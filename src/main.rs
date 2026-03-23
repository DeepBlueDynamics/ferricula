use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Result, bail};
use ferricula::clock::{self, ClockConfig, ClockEvent, ClockTelemetry};
use ferricula::http::HttpCommand;
use ferricula::identity::IdentityState;
use ferricula::inversion;
use ferricula::memory::{Emotion, MemoryRecord};
use ferricula::planner::{Planner, PlannerResult};
use ferricula::corpus::SearchEngine;
use ferricula::tokenizer;
use ferricula::{DistanceMetric, DurableEngine, EdgeKind, Row};

/// Format a Unix epoch (seconds) as ISO 8601 UTC string.
fn epoch_to_iso8601(epoch: u64) -> String {
    const SECS_PER_MIN: u64 = 60;
    const SECS_PER_HOUR: u64 = 3600;
    const SECS_PER_DAY: u64 = 86400;

    let days = epoch / SECS_PER_DAY;
    let day_secs = epoch % SECS_PER_DAY;
    let hour = day_secs / SECS_PER_HOUR;
    let min = (day_secs % SECS_PER_HOUR) / SECS_PER_MIN;
    let sec = day_secs % SECS_PER_MIN;

    // Days since 1970-01-01 to Y-M-D (civil calendar)
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, hour, min, sec)
}

/// A recall that's waiting for the planner thread to finish LLM rewriting.
struct PendingRecall {
    rx: mpsc::Receiver<PlannerResult>,
    reply: mpsc::SyncSender<String>,
}

/// Load .env file from cwd or binary's directory. Sets vars only if not already in env.
fn load_dotenv() {
    let candidates = [
        std::env::current_dir().ok().map(|p| p.join(".env")),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(".env"))),
    ];
    for path in candidates.iter().flatten() {
        if let Ok(contents) = std::fs::read_to_string(path) {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, val)) = line.split_once('=') {
                    let key = key.trim();
                    let val = val.trim().trim_matches('"').trim_matches('\'');
                    if std::env::var(key).is_err() {
                        // SAFETY: called before any threads are spawned
                        unsafe { std::env::set_var(key, val) };
                    }
                }
            }
            break; // use first .env found
        }
    }
}

fn main() -> Result<()> {
    // Load .env file (sibling to binary or cwd) — sets vars only if not already in env
    load_dotenv();

    // Parse args: ferricula [data_dir] [--serve [port]]
    let args: Vec<String> = std::env::args().collect();
    let mut data_dir = "./data".to_string();
    let mut serve_mode = false;
    let mut serve_port: u16 = 8765;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--serve" => {
                serve_mode = true;
                if i + 1 < args.len() {
                    if let Ok(p) = args[i + 1].parse::<u16>() {
                        serve_port = p;
                        i += 1;
                    }
                }
            }
            other => {
                if !other.starts_with('-') {
                    data_dir = other.to_string();
                }
            }
        }
        i += 1;
    }

    let mut db = DurableEngine::open(&data_dir)?;

    let agent_key = std::env::var("AGENT_KEY").ok();
    let planner = Planner::new(agent_key);

    // Load or create identity
    let chonk_url =
        std::env::var("CHONK_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
    let identity_entropy = get_identity_entropy();
    let (mut identity, is_new) = ferricula::identity::load_or_create(&data_dir, &identity_entropy);
    if is_new {
        let (row, record) = ferricula::identity::create_anchor(&identity);
        // Best-effort: anchor vector is 4d, engine might have different dim
        if let Err(e) = db.remember(row, record) {
            eprintln!("[identity] anchor memory skipped (dim mismatch ok): {e}");
        }
    }

    if !serve_mode {
        show_splash();
    }

    let should_stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&should_stop);
    ctrlc::set_handler(move || {
        stop_flag.store(true, Ordering::SeqCst);
    })?;

    // Spawn clock thread
    let clock_config = ClockConfig::from_env();
    let (clock_rx, clock_telemetry) = clock::spawn_clock(clock_config);

    if serve_mode {
        // HTTP service mode
        eprintln!("ferricula v{} (serve mode)", env!("CARGO_PKG_VERSION"));
        eprintln!("data dir: {data_dir}");
        eprintln!("identity: {} ({})", identity.agent_id, identity.name);

        let (http_tx, http_rx) = mpsc::channel::<HttpCommand>();
        let _http_running = Arc::clone(&should_stop);
        // Invert the flag: http thread checks `running` (true=keep going)
        let http_flag = Arc::new(AtomicBool::new(true));
        let http_flag_clone = Arc::clone(&http_flag);

        let _http_handle = ferricula::http::spawn_http(serve_port, http_tx, http_flag.clone());

        let mut pending_recalls: Vec<PendingRecall> = Vec::new();

        // Build search engine from existing memories
        let mut search_engine = SearchEngine::new();
        for row in db.engine().rows_iter() {
            if let Some(text) = row.tags.get("text") {
                search_engine.add_document(row.id, text);
            }
        }
        if search_engine.corpus.n_docs > 0 {
            search_engine.recalibrate();
            eprintln!("[search] indexed {} docs, avgdl={:.1}", search_engine.corpus.n_docs, search_engine.corpus.avgdl());
        }

        loop {
            if should_stop.load(Ordering::SeqCst) {
                eprintln!("[serve] signal received, checkpointing...");
                http_flag_clone.store(false, Ordering::SeqCst);
                db.checkpoint()?;
                eprintln!("[serve] checkpoint complete");
                break;
            }

            process_clock_events(&mut db, &mut identity, &clock_rx, &chonk_url);
            process_http_commands(
                &mut db,
                &planner,
                &clock_telemetry,
                &mut identity,
                &chonk_url,
                &http_rx,
                &mut pending_recalls,
                &mut search_engine,
            );
            process_pending_recalls(&mut db, &mut pending_recalls, &chonk_url);

            // Small sleep to avoid busy-spinning when no events
            std::thread::sleep(Duration::from_millis(10));
        }
    } else {
        // REPL mode (original behavior)
        println!("ferricula v{}", env!("CARGO_PKG_VERSION"));
        println!("data dir: {data_dir}");
        println!("identity: {} ({})", identity.agent_id, identity.name);
        print_help();

        let (stdin_tx, stdin_rx) = mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("stdin-reader".into())
            .spawn(move || {
                let stdin = io::stdin();
                for line in stdin.lock().lines() {
                    match line {
                        Ok(l) => {
                            if stdin_tx.send(l).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
            .expect("failed to spawn stdin reader");

        print!("\n> ");
        io::stdout().flush()?;

        loop {
            if should_stop.load(Ordering::SeqCst) {
                println!("signal received, checkpointing...");
                db.checkpoint()?;
                println!("checkpoint complete");
                break;
            }

            process_clock_events(&mut db, &mut identity, &clock_rx, &chonk_url);

            match stdin_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(line) => {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        print!("\n> ");
                        io::stdout().flush()?;
                        continue;
                    }
                    if line.eq_ignore_ascii_case("exit") || line.eq_ignore_ascii_case("quit") {
                        db.checkpoint()?;
                        break;
                    }
                    if line.eq_ignore_ascii_case("checkpoint") {
                        db.checkpoint()?;
                        println!("checkpoint complete");
                        print!("\n> ");
                        io::stdout().flush()?;
                        continue;
                    }
                    if line.eq_ignore_ascii_case("help") {
                        print_help();
                        print!("\n> ");
                        io::stdout().flush()?;
                        continue;
                    }

                    match handle_command(&mut db, &planner, &mut identity, &clock_telemetry, &chonk_url, &line) {
                        Ok(output) => println!("{output}"),
                        Err(err) => eprintln!("error: {err:#}"),
                    }
                    print!("\n> ");
                    io::stdout().flush()?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    db.checkpoint()?;
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Get entropy bytes for identity casting.
/// Tries timestamp-derived bytes as fallback.
fn get_identity_entropy() -> Vec<u8> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut bytes = vec![0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = ((now >> (i * 4)) & 0xFF) as u8;
    }
    bytes
}

/// Drain all pending clock events and act on them.
fn process_clock_events(db: &mut DurableEngine, identity: &mut IdentityState, clock_rx: &mpsc::Receiver<ClockEvent>, chonk_url: &str) {
    let chonk = if inversion::chonk_available(chonk_url) { Some(chonk_url) } else { None };
    while let Ok(event) = clock_rx.try_recv() {
        match event {
            ClockEvent::DreamTrigger {
                epoch,
                intensity,
                entropy_bytes,
            } => {
                let report = db.dream_with_intensity(intensity, &[], chonk);
                identity.activate_from_report(&report);
                eprintln!(
                    "[clock] dream epoch={epoch} intensity={intensity:.2} entropy={entropy_bytes}B \
                     decayed={} forgiven={} consolidated={} pruned={} ghosts={} edges={} archetypes=[{}]",
                    report.decayed, report.forgiven, report.consolidated, report.pruned,
                    report.ghost_echoes, report.edges_created,
                    report.active_archetypes.join(","),
                );
            }
            ClockEvent::RadioStatus { available, message } => {
                let status = if available {
                    "connected"
                } else {
                    "disconnected"
                };
                eprintln!("[clock] radio {status}: {message}");
            }
            ClockEvent::Tick { .. } => {
                // Silent — telemetry atomics updated by clock thread
            }
        }
    }
}

/// Drain completed planner results — execute SQL and reply to HTTP clients.
fn process_pending_recalls(
    db: &mut DurableEngine,
    pending: &mut Vec<PendingRecall>,
    chonk_url: &str,
) {
    let mut completed = Vec::new();
    for (i, pr) in pending.iter().enumerate() {
        match pr.rx.try_recv() {
            Ok(result) => {
                let output = cmd_recall_sql_embed(db, &result.sql, Some(chonk_url))
                    .unwrap_or_else(|e| json_error(&e));
                let _ = pr.reply.send(json_wrap("result", &output));
                if result.llm_used {
                    eprintln!("[planner] LLM rewrite completed: {:?} -> {:?}",
                        &result.input[..result.input.len().min(40)],
                        &result.sql[..result.sql.len().min(60)]);
                }
                completed.push(i);
            }
            Err(mpsc::TryRecvError::Empty) => {} // still waiting
            Err(mpsc::TryRecvError::Disconnected) => {
                // Planner thread died — send error reply
                let _ = pr.reply.send(json_wrap("result", "error: planner thread died"));
                completed.push(i);
            }
        }
    }
    // Remove completed in reverse order to preserve indices
    for i in completed.into_iter().rev() {
        pending.swap_remove(i);
    }
}

/// Process pending HTTP commands (non-blocking drain).
/// Recalls that need LLM rewriting are deferred to `pending_recalls`.
fn process_http_commands(
    db: &mut DurableEngine,
    planner: &Planner,
    telemetry: &Arc<ClockTelemetry>,
    identity: &mut IdentityState,
    chonk_url: &str,
    http_rx: &mpsc::Receiver<HttpCommand>,
    pending_recalls: &mut Vec<PendingRecall>,
    search_engine: &mut SearchEngine,
) {
    while let Ok(cmd) = http_rx.try_recv() {
        match cmd {
            HttpCommand::Remember { body, reply } => {
                // Parse body to extract text for search indexing before cmd_remember
                let text_for_index = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| {
                        let id = v.get("id")?.as_u64()? as u32;
                        let text = v.get("tags")?.get("text")?.as_str()?.to_string();
                        Some((id, text))
                    });
                let result = cmd_remember(db, &body).unwrap_or_else(|e| json_error(&e));
                // Update search engine with new doc
                if let Some((id, text)) = text_for_index {
                    search_engine.add_document(id, &text);
                }
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Recall { body, reply } => {
                // Extract "query" field from JSON body, fall back to raw body
                let query_text = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("query")?.as_str().map(|s| s.to_string()))
                    .unwrap_or(body);

                if planner.needs_llm(&query_text) {
                    // Defer: spawn LLM rewrite on background thread
                    let rx = planner.spawn_llm_rewrite(&query_text);
                    pending_recalls.push(PendingRecall { rx, reply });
                    eprintln!("[planner] deferred recall to LLM thread ({} pending)", pending_recalls.len());
                } else {
                    // Fast path: sync rewrite (SQL passthrough or rule-based)
                    let sql = planner.rewrite_query_sync(&query_text)
                        .unwrap_or_else(|e| format!("error: {e}"));
                    let result = cmd_recall_sql_embed(db, &sql, Some(chonk_url)).unwrap_or_else(|e| json_error(&e));
                    let _ = reply.send(json_wrap("result", &result));
                }
            }
            HttpCommand::Dream { body: _, reply } => {
                let result = cmd_dream(db, identity, chonk_url).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Status { reply } => {
                let result = cmd_status(db, identity).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Inspect { id, reply } => {
                let result = cmd_inspect(db, &id.to_string()).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Offer { body, reply } => {
                let result = cmd_offer(db, identity, &body, chonk_url).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Keystone { id, reply } => {
                let result = cmd_keystone(db, &id.to_string()).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Connect { body, reply } => {
                let result = handle_connect_json(db, &body).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Disconnect { body, reply } => {
                let result = handle_disconnect_json(db, &body).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Neighbors { id, reply } => {
                let result = cmd_neighbors(db, &id.to_string()).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Clock { reply } => {
                let result = cmd_clock(telemetry).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Checkpoint { reply } => {
                let result = match db.checkpoint() {
                    Ok(()) => "checkpoint complete".to_string(),
                    Err(e) => format!("error: {e}"),
                };
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Identity { reply } => {
                // Inject runtime version into identity JSON
                let mut val: serde_json::Value = serde_json::from_str(&identity.to_json())
                    .unwrap_or_else(|_| serde_json::json!({}));
                val["version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
                let _ = reply.send(serde_json::to_string_pretty(&val).unwrap_or_default());
            }
            HttpCommand::GetRow { id, reply } => {
                let result = cmd_get(db, &id.to_string()).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(result); // already JSON
            }
            HttpCommand::Terms { reply } => {
                let result = cmd_terms(db).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::InversionCheck { id, reply } => {
                let result = handle_inversion_check(db, chonk_url, id);
                let _ = reply.send(result);
            }
            HttpCommand::Skg { reply } => {
                let result = cmd_skg(db);
                let _ = reply.send(result);
            }
            HttpCommand::SkgTerm { term, reply } => {
                let result = cmd_skg_term(db, &term);
                let _ = reply.send(result);
            }
            HttpCommand::Delete { id, reply } => {
                search_engine.remove_document(id);
                let result = match db.remove_memory(id) {
                    Ok(true) => serde_json::json!({"deleted": true, "id": id}).to_string(),
                    Ok(false) => serde_json::json!({"deleted": false, "id": id, "error": "not found"}).to_string(),
                    Err(e) => json_error(&e),
                };
                let _ = reply.send(result);
            }
            HttpCommand::MaxId { reply } => {
                let result = cmd_maxid(db).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("maxid", &result));
            }
            HttpCommand::Search { body, reply } => {
                let result = handle_search(db, search_engine, &body);
                let _ = reply.send(result);
            }
            HttpCommand::Hybrid { body, reply } => {
                let result = handle_hybrid(db, search_engine, &body, chonk_url);
                let _ = reply.send(result);
            }
            HttpCommand::Glossary { reply } => {
                let _ = reply.send(ferricula::pali::glossary_json());
            }
            HttpCommand::Dashboard { reply } => {
                let _ = reply.send(build_dashboard(db, identity, telemetry, chonk_url));
            }
        }
    }
}

/// Handle JSON connect body: {"a": N, "b": N, "label": "..."}
fn handle_connect_json(db: &mut DurableEngine, body: &str) -> Result<String> {
    let val: serde_json::Value = serde_json::from_str(body)?;
    let a = val
        .get("a")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("missing a"))? as u32;
    let b = val
        .get("b")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("missing b"))? as u32;
    let label = val
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("related");
    let kind = match val.get("kind").and_then(|v| v.as_str()) {
        Some("causal") => EdgeKind::Causal,
        _ => EdgeKind::Semantic,
    };
    let arrow = if kind == EdgeKind::Causal { "->" } else { "<->" };
    db.connect(a, b, label.to_string(), 1.0, kind)?;
    Ok(format!("connected {a} {arrow} {b} [{label}]"))
}

/// Handle JSON disconnect body: {"a": N, "b": N}
fn handle_disconnect_json(db: &mut DurableEngine, body: &str) -> Result<String> {
    let val: serde_json::Value = serde_json::from_str(body)?;
    let a = val
        .get("a")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("missing a"))? as u32;
    let b = val
        .get("b")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("missing b"))? as u32;
    db.disconnect(a, b)?;
    Ok(format!("disconnected {a} <-> {b}"))
}

/// Handle inversion check for a memory.
fn handle_inversion_check(db: &DurableEngine, chonk_url: &str, id: u32) -> String {
    use ferricula::inversion;

    if !inversion::chonk_available(chonk_url) {
        return serde_json::json!({
            "error": "chonk not reachable",
            "memory_id": id,
        })
        .to_string();
    }

    let row = match db.engine().get(id) {
        Some(r) => r,
        None => return serde_json::json!({"error": "no row", "memory_id": id}).to_string(),
    };

    let original_text = row.tags.get("text").cloned().unwrap_or_default();
    if original_text.is_empty() {
        return serde_json::json!({
            "error": "no text tag",
            "memory_id": id,
        })
        .to_string();
    }

    match inversion::check_inversion_with_data(chonk_url, id, &original_text, &row.vector) {
        Some(check) => serde_json::to_string(&check).unwrap_or_else(|_| "{}".to_string()),
        None => serde_json::json!({
            "error": "inversion failed",
            "memory_id": id,
        })
        .to_string(),
    }
}

/// Handle POST /search — BM25 ranked search.
fn handle_search(db: &DurableEngine, search_engine: &SearchEngine, body: &str) -> String {
    let val: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({"error": format!("{e}")}).to_string(),
    };
    let query = val.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let k = val.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let hits = search_engine.bm25_search(query, k, db.prime_tree());

    let results: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| {
            let text = db
                .engine()
                .get(h.id)
                .and_then(|r| r.tags.get("text").cloned())
                .unwrap_or_default();
            serde_json::json!({
                "id": h.id,
                "probability": (h.probability * 10000.0).round() / 10000.0,
                "bm25": (h.bm25_score * 1000.0).round() / 1000.0,
                "text": text,
            })
        })
        .collect();

    serde_json::json!({"results": results}).to_string()
}

/// Handle POST /hybrid — fused vector + BM25 search.
fn handle_hybrid(db: &DurableEngine, search_engine: &SearchEngine, body: &str, chonk_url: &str) -> String {
    let val: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({"error": format!("{e}")}).to_string(),
    };
    let query = val.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let k = val.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let weight = val.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.5);

    // Get vector hits from chonk embed + cosine search
    let vector_hits: Vec<(u32, f32)> = if inversion::chonk_available(chonk_url) {
        // Embed query via chonk
        match inversion::embed_text(chonk_url, query) {
            Some(qvec) => {
                let hits = db.engine().vector_topk(&qvec, k * 2, DistanceMetric::Cosine, None);
                hits.into_iter().map(|h| (h.id, h.score)).collect()
            }
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let hits = search_engine.hybrid_search(query, k, weight, db.prime_tree(), &vector_hits);

    let results: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| {
            let text = db
                .engine()
                .get(h.id)
                .and_then(|r| r.tags.get("text").cloned())
                .unwrap_or_default();
            serde_json::json!({
                "id": h.id,
                "probability": (h.probability * 10000.0).round() / 10000.0,
                "bm25": (h.bm25_score * 1000.0).round() / 1000.0,
                "text": text,
            })
        })
        .collect();

    serde_json::json!({"results": results}).to_string()
}

/// Wrap a string result in JSON.
fn json_wrap(key: &str, value: &str) -> String {
    serde_json::json!({ key: value }).to_string()
}

/// Format an error as JSON.
fn json_error(err: &anyhow::Error) -> String {
    format!("error: {err:#}")
}

fn print_help() {
    println!("commands:");
    println!("  remember <json>              ingest memory (creates Row + MemoryRecord)");
    println!("  recall <sql|freeform>        search + update recall stats");
    println!("  dream                        run dream cycle (decay/consolidate/prune)");
    println!("  status                       show memory statistics");
    println!("  inspect <id>                 show memory record details");
    println!("  keystone <id>                toggle keystone status");
    println!("  connect <id1> <id2> <label>  create graph edge");
    println!("  disconnect <id1> <id2>       remove graph edge");
    println!("  neighbors <id>               show graph neighbors");
    println!("  terms                        list prime tree terms");
    println!("  skg [term]                   semantic knowledge graph (Weber bracket)");
    println!("  upsert <json-row>            raw row upsert (no memory envelope)");
    println!("  delete <id>                  delete row");
    println!("  query <sql>                  raw SQL query");
    println!("  rewrite <freeform>           show planner rewrite");
    println!("  topk <metric> <vec> <k>      vector similarity search");
    println!("  maxid                        highest row ID in engine");
    println!("  get <id>                     return row as JSON");
    println!("  touch <id>                   update recall stats on memory");
    println!("  clock                        show clock telemetry");
    println!("  offer <hex>                  inject entropy manually, trigger dream");
    println!("  checkpoint                   flush to disk");
    println!("  help                         this message");
    println!("  exit                         checkpoint and quit");
}

fn handle_command(
    db: &mut DurableEngine,
    planner: &Planner,
    identity: &mut IdentityState,
    telemetry: &Arc<ClockTelemetry>,
    chonk_url: &str,
    line: &str,
) -> Result<String> {
    let mut parts = line.splitn(2, ' ');
    let cmd = parts
        .next()
        .map(str::to_lowercase)
        .ok_or_else(|| anyhow::anyhow!("empty command"))?;
    let tail = parts.next().unwrap_or("").trim();

    match cmd.as_str() {
        "remember" => cmd_remember(db, tail),
        "recall" => cmd_recall(db, planner, chonk_url, tail),
        "dream" => cmd_dream(db, identity, chonk_url),
        "status" => cmd_status(db, identity),
        "inspect" => cmd_inspect(db, tail),
        "keystone" => cmd_keystone(db, tail),
        "connect" => cmd_connect(db, tail),
        "disconnect" => cmd_disconnect(db, tail),
        "neighbors" => cmd_neighbors(db, tail),
        "terms" => cmd_terms(db),
        "skg" => {
            if tail.is_empty() {
                Ok(cmd_skg(db))
            } else {
                Ok(cmd_skg_term(db, tail))
            }
        }
        "clock" => cmd_clock(telemetry),
        "offer" => cmd_offer(db, identity, tail, chonk_url),
        "upsert" => {
            let row = parse_row_json(tail)?;
            db.upsert(row)?;
            Ok("ok".to_string())
        }
        "delete" => {
            let id: u32 = tail.parse()?;
            let deleted = db.delete(id)?;
            Ok(format!("deleted={deleted}"))
        }
        "query" => {
            let canonical = planner.rewrite_query(tail)?;
            let result = db.execute_sql_with_embed(&canonical, Some(chonk_url))?;
            Ok(format!("ids={:?}", result.ids))
        }
        "rewrite" => {
            let rewritten = planner.rewrite_query(tail)?;
            Ok(format!("rewritten: {rewritten}"))
        }
        "topk" => {
            let mut args = tail.splitn(3, ' ');
            let metric_text = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("topk: missing metric"))?;
            let vector_text = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("topk: missing vector"))?;
            let k_text = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("topk: missing k"))?;

            let metric = parse_metric(metric_text)?;
            let vector = parse_vector_literal(vector_text)?;
            let k: usize = k_text.parse()?;
            let hits = db.engine().vector_topk(&vector, k, metric, None);
            Ok(format!("hits={hits:?}"))
        }
        "maxid" => cmd_maxid(db),
        "get" => cmd_get(db, tail),
        "touch" => cmd_touch(db, tail),
        _ => bail!("unknown command (type 'help' for list)"),
    }
}

// --- Commands ---

fn cmd_remember(db: &mut DurableEngine, tail: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(tail)?;
    let row = parse_row_json(tail)?;
    let id = row.id;

    let mut record = MemoryRecord::new(id);

    if let Some(emo) = value.get("emotion") {
        let primary = emo
            .get("primary")
            .and_then(|v| v.as_str())
            .unwrap_or("neutral")
            .to_string();
        let secondary = emo
            .get("secondary")
            .and_then(|v| v.as_str())
            .map(String::from);
        record.emotion = Some(Emotion { primary, secondary });
    }

    if let Some(imp) = value.get("importance").and_then(|v| v.as_f64()) {
        record.importance = imp as f32;
    }

    if let Some(ks) = value.get("keystone").and_then(|v| v.as_bool()) {
        record.keystone = ks;
    }

    if let Some(alpha) = value.get("decay_alpha").and_then(|v| v.as_f64()) {
        record.decay_alpha =
            (alpha as f32).clamp(ferricula::memory::ALPHA_MIN, ferricula::memory::ALPHA_MAX);
    }

    db.remember(row.clone(), record)?;

    // Word-level stemmed terms from text tag (with Pali expansion)
    let mut all_terms = Vec::new();
    if let Some(text) = row.tags.get("text") {
        let expanded = ferricula::pali::expand(text);
        let terms = tokenizer::extract_terms(&expanded);
        for term in &terms {
            db.insert_term(term, id)?;
        }
        all_terms.extend(terms);
    }
    // Also index other tags as structured terms (channel:value, author:value, etc.)
    for (key, val) in &row.tags {
        if key != "text" {
            let tag_term = format!("{}:{}", key, val.to_lowercase());
            db.insert_term(&tag_term, id)?;
            all_terms.push(tag_term);
        }
    }

    Ok(format!("remembered id={id} terms={} indexed", all_terms.len()))
}

/// Recall with planner rewrite (REPL mode — blocking LLM is acceptable).
fn cmd_recall(db: &mut DurableEngine, planner: &Planner, chonk_url: &str, tail: &str) -> Result<String> {
    let canonical = planner.rewrite_query(tail)?;
    cmd_recall_sql_embed(db, &canonical, Some(chonk_url))
}

/// Recall with embed support — resolves embed('text') via chonk.
fn cmd_recall_sql_embed(db: &mut DurableEngine, sql: &str, chonk_url: Option<&str>) -> Result<String> {
    let result = db.execute_sql_with_embed(sql, chonk_url)?;

    for &id in &result.ids {
        if let Some(record) = db.memory_store_mut().get_mut(id) {
            record.on_recall();
        }
    }

    let mut lines = Vec::new();
    for &id in &result.ids {
        let fidelity = db
            .memory_store()
            .get(id)
            .map(|r| format!(" fidelity={:.3} recalls={}", r.fidelity, r.recall_count))
            .unwrap_or_default();
        let text = db.engine().get(id)
            .and_then(|row| row.tags.get("text").cloned())
            .unwrap_or_default();
        let text_trunc: &str = if text.len() > 200 {
            // Find a char boundary at or before byte 200
            let mut end = 200;
            while end > 0 && !text.is_char_boundary(end) { end -= 1; }
            &text[..end]
        } else {
            &text
        };
        lines.push(format!("  id={id}{fidelity} text={text_trunc}"));
    }

    Ok(format!(
        "sql: {sql}\nrecalled {} memories:\n{}",
        result.ids.len(),
        lines.join("\n")
    ))
}

fn cmd_dream(db: &mut DurableEngine, identity: &mut IdentityState, chonk_url: &str) -> Result<String> {
    let chonk = if inversion::chonk_available(chonk_url) { Some(chonk_url) } else { None };
    let report = db.dream(chonk);
    identity.activate_from_report(&report);
    Ok(format_dream_report(&report))
}

fn cmd_clock(telemetry: &Arc<ClockTelemetry>) -> Result<String> {
    let ticks = telemetry.tick_count.load(Ordering::Relaxed);
    let dreams = telemetry.dream_count.load(Ordering::Relaxed);
    let entropy = telemetry.entropy_lifetime.load(Ordering::Relaxed);
    let available = telemetry.radio_available.load(Ordering::Relaxed);
    let reservoir = telemetry.reservoir_bytes.load(Ordering::Relaxed);

    Ok(format!(
        "clock:\n  ticks={ticks}\n  dreams={dreams}\n  entropy_lifetime={entropy}B\n  reservoir={reservoir}B\n  radio={}",
        if available {
            "connected"
        } else {
            "disconnected"
        },
    ))
}

fn cmd_offer(db: &mut DurableEngine, identity: &mut IdentityState, tail: &str, chonk_url: &str) -> Result<String> {
    let hex = tail.trim();
    if hex.is_empty() {
        bail!("offer: provide hex-encoded entropy (e.g. 'offer deadbeef')");
    }
    let bytes = clock::hex_decode(hex);
    if bytes.is_empty() {
        bail!("offer: invalid hex string");
    }
    let chonk = if inversion::chonk_available(chonk_url) { Some(chonk_url) } else { None };
    let intensity = (bytes.len() as f32 / 64.0).min(1.0);
    let report = db.dream_with_intensity(intensity, &bytes, chonk);
    identity.activate_from_report(&report);
    Ok(format!(
        "offer accepted: {}B entropy, intensity={intensity:.2}\n{}",
        bytes.len(),
        format_dream_report(&report),
    ))
}

fn format_dream_report(report: &ferricula::DreamReport) -> String {
    let skg = &report.skg_summary;
    let emerging: Vec<String> = skg.top_emerging.iter().map(|e| format!("{}~{}", e.term_a, e.term_b)).collect();
    let decaying: Vec<String> = skg.top_decaying.iter().map(|e| format!("{}~{}", e.term_a, e.term_b)).collect();
    format!(
        "dream complete:\n  ticks={}\n  decayed={}\n  forgiven={}\n  archived={}\n  consolidated={}\n  pruned={}\n  ghost_echoes={}\n  keystones_reviewed={}\n  edges_created={}\n  keystones_promoted={}\n  active_archetypes=[{}]\n  skg: sampled={} tracked={} emerging=[{}] decaying=[{}]",
        report.ticks,
        report.decayed,
        report.forgiven,
        report.archived,
        report.consolidated,
        report.pruned,
        report.ghost_echoes,
        report.keystones_reviewed,
        report.edges_created,
        report.keystones_promoted,
        report.active_archetypes.join(","),
        skg.pairs_sampled,
        skg.pairs_tracked,
        emerging.join(","),
        decaying.join(","),
    )
}

fn build_dashboard(
    db: &DurableEngine,
    identity: &IdentityState,
    telemetry: &Arc<ClockTelemetry>,
    chonk_url: &str,
) -> String {
    let store = db.memory_store();
    let active = store.in_state(ferricula::LifecycleState::Active).len();
    let forgiven = store.in_state(ferricula::LifecycleState::Forgiven).len();
    let archived = store.in_state(ferricula::LifecycleState::Archived).len();
    let keystones = store.keystones().len();
    let rows = db.engine().row_count();
    let graph_nodes = db.graph().node_count();
    let graph_edges = db.graph().edge_count();
    let terms = db.prime_tree().root_count();

    let id = &identity.name;
    let hexagram = format!("hexagram {} &mdash; {}", identity.hexagram.number, identity.hexagram.name);
    let horoscope = &identity.horoscope.sign_name;
    let primary_emotion = &identity.primary_emotion;
    let secondary_emotion = &identity.secondary_emotion;

    // Archetype info
    let archetypes_html: String = identity
        .archetypes
        .iter()
        .map(|a| {
            let state_class = if a.active { "ok" } else { "off" };
            format!(
                r#"<span class="arch {state_class}">{}</span>"#,
                a.role.name()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Clock / radio status
    let ticks = telemetry.tick_count.load(Ordering::Relaxed);
    let dreams = telemetry.dream_count.load(Ordering::Relaxed);
    let entropy_life = telemetry.entropy_lifetime.load(Ordering::Relaxed);
    let radio_up = telemetry.radio_available.load(Ordering::Relaxed);
    let reservoir = telemetry.reservoir_bytes.load(Ordering::Relaxed);

    // Service checks
    let chonk_status = check_service(chonk_url, "/health");
    let radio_url = std::env::var("RADIO_URL").unwrap_or_default();
    let agent_key_set = std::env::var("AGENT_KEY").is_ok();

    // Agent config from data volume
    let agent_config = load_agent_toml();

    // Determine state: fresh (no memories beyond anchor), trained, or active
    let brain_state = if rows <= 1 {
        "fresh"
    } else if dreams == 0 {
        "loaded"
    } else {
        "active"
    };

    // Build setup checklist
    let chonk_check = if chonk_status {
        r#"<div class="check ok">Embedding service (chonk)</div>"#
    } else {
        r#"<div class="check fail">Embedding service (chonk) &mdash; not reachable</div>"#
    };
    let radio_check = if radio_up {
        r#"<div class="check ok">Entropy source (sdr-random)</div>"#
    } else if radio_url.is_empty() {
        r#"<div class="check warn">Entropy source &mdash; RADIO_URL not set (dreams are manual only)</div>"#
    } else {
        r#"<div class="check warn">Entropy source &mdash; not connected yet (waiting for first tick)</div>"#
    };
    let key_check = if agent_key_set {
        r#"<div class="check ok">LLM query planner (AGENT_KEY)</div>"#
    } else {
        r#"<div class="check warn">LLM query planner &mdash; AGENT_KEY not set (rule-based fallback)</div>"#
    };

    // Agent name — from agent.toml if present, FERRICULA_NAME env var, otherwise default
    let agent_name = agent_config
        .as_ref()
        .map(|c| c.name.clone())
        .or_else(|| std::env::var("FERRICULA_NAME").ok())
        .unwrap_or_else(|| "FERRICULA".to_string());
    let agent_role_line = agent_config
        .as_ref()
        .map(|c| format!(r#"<div class="role" style="margin-top:.5rem">{}</div>"#, c.role))
        .unwrap_or_default();

    // Agent personality block (legacy — now merged into identity block)
    let _agent_block = if let Some(ref cfg) = agent_config {
        format!(
            r#"<div class="id-block">
<h2>Agent</h2>
<div class="id-name">{}</div>
<div class="role">{}</div>
<div class="row" style="margin-top:.5rem">
<span class="tag">{}</span>
</div>
</div>"#,
            cfg.name, cfg.role, cfg.voice
        )
    } else {
        String::new()
    };

    // Next steps based on state
    let next_steps = match brain_state {
        "fresh" => r#"<div class="next">
<h2>Next Steps</h2>
<ol>
<li>Feed me documents via <code>POST /remember</code> or the MCP <code>ferricula_remember</code> tool</li>
<li>Seed the knowledge graph with <code>POST /connect</code> to link related memories</li>
<li>Run dream cycles via <code>POST /dream</code> or <code>POST /offer</code> with entropy</li>
<li>Talk to me via <code>POST /recall</code> or the MCP <code>ferricula_recall</code> tool</li>
</ol>
</div>"#,
        "loaded" => r#"<div class="next">
<h2>Next Steps</h2>
<ol>
<li>Run dream cycles to consolidate: <code>POST /dream</code> or <code>POST /offer</code></li>
<li>Connect an entropy source for automatic dreams (set <code>RADIO_URL</code>)</li>
<li>Start chatting via <code>POST /recall</code></li>
</ol>
</div>"#,
        _ => "",
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>ferricula &mdash; {id}</title>
<style>
*{{margin:0;padding:0;box-sizing:border-box}}
body{{background:#0c0a09;color:#e7e5e4;font-family:'Courier New',monospace;padding:2rem;max-width:900px;margin:0 auto}}
h1{{color:#b91c1c;font-size:1.5rem;margin-bottom:.25rem}}
.ver{{color:#78716c;font-size:.75rem;margin-bottom:1.5rem}}
.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:.75rem;margin-bottom:1.5rem}}
.card{{background:#1c1917;border:1px solid #292524;border-radius:8px;padding:1rem}}
.card h2,.id-block h2,.checks h2,.next h2{{color:#78716c;font-size:.6rem;text-transform:uppercase;letter-spacing:.15em;margin-bottom:.5rem}}
.val{{font-size:1.5rem;font-weight:bold}}
.active{{color:#059669}} .forgiven{{color:#d97706}} .archived{{color:#78716c}} .keystone{{color:#7c3aed}}
.row{{display:flex;gap:.5rem;flex-wrap:wrap;margin-top:.25rem}}
.tag{{background:#292524;border-radius:4px;padding:.15rem .4rem;font-size:.7rem;color:#a8a29e}}
.id-block{{background:#1c1917;border:1px solid #292524;border-radius:8px;padding:1rem;margin-bottom:1rem}}
.id-name{{font-size:1.1rem;font-weight:bold;margin-bottom:.25rem}}
.role{{color:#a8a29e;font-size:.75rem}}
.checks{{background:#1c1917;border:1px solid #292524;border-radius:8px;padding:1rem;margin-bottom:1rem}}
.check{{font-size:.75rem;padding:.3rem 0;padding-left:1.5rem;position:relative}}
.check::before{{position:absolute;left:0;width:1rem;text-align:center}}
.check.ok{{color:#059669}} .check.ok::before{{content:"+"}}
.check.fail{{color:#b91c1c}} .check.fail::before{{content:"x"}}
.check.warn{{color:#d97706}} .check.warn::before{{content:"~"}}
.arch{{display:inline-block;background:#292524;border-radius:4px;padding:.15rem .5rem;font-size:.65rem;color:#78716c;margin:.15rem .25rem .15rem 0}}
.arch.ok{{color:#059669;border:1px solid #059669}}
.arch.off{{color:#44403c;border:1px solid #292524}}
.next{{background:#1c1917;border:1px solid #292524;border-radius:8px;padding:1rem;margin-bottom:1rem}}
.next ol{{padding-left:1.25rem;font-size:.75rem;color:#a8a29e;line-height:1.8}}
.next code{{background:#292524;padding:.1rem .3rem;border-radius:3px;font-size:.7rem;color:#e7e5e4}}
.clock{{font-size:.7rem;color:#78716c;margin-top:.25rem}}
a{{color:#b91c1c;text-decoration:none}} a:hover{{text-decoration:underline}}
footer{{margin-top:2rem;color:#44403c;font-size:.65rem}}
.state-badge{{display:inline-block;font-size:.6rem;text-transform:uppercase;letter-spacing:.1em;padding:.2rem .5rem;border-radius:4px;margin-left:.5rem}}
.state-fresh{{background:#292524;color:#d97706}}
.state-loaded{{background:#292524;color:#7c3aed}}
.state-active{{background:#292524;color:#059669}}
</style>
<meta http-equiv="refresh" content="30">
</head>
<body>
<h1>{agent_name} <span class="state-badge state-{brain_state}">{brain_state}</span></h1>
<div class="ver">{id} &bull; v{ver}</div>

<div class="id-block">
<h2>Identity</h2>
<div class="row">
<span class="tag">{hexagram}</span>
<span class="tag">{horoscope}</span>
<span class="tag">{primary_emotion} / {secondary_emotion}</span>
</div>
<div class="row" style="margin-top:.5rem">
{archetypes_html}
</div>
{agent_role_line}
</div>

<div class="checks">
<h2>Services</h2>
{chonk_check}
{radio_check}
{key_check}
</div>

{next_steps}

<div class="grid">
<div class="card">
<h2>Memories</h2>
<div class="val">{rows}</div>
<div class="row">
<span class="tag active">active {active}</span>
<span class="tag forgiven">forgiven {forgiven}</span>
<span class="tag archived">archived {archived}</span>
</div>
</div>
<div class="card">
<h2>Keystones</h2>
<div class="val keystone">{keystones}</div>
</div>
<div class="card">
<h2>Graph</h2>
<div class="val">{graph_nodes}</div>
<div class="row">
<span class="tag">{graph_edges} edges</span>
</div>
</div>
<div class="card">
<h2>Terms</h2>
<div class="val">{terms}</div>
</div>
<div class="card">
<h2>Clock</h2>
<div class="val">{dreams}</div>
<div class="clock">ticks={ticks} entropy={entropy_life}B reservoir={reservoir}B</div>
</div>
</div>

<footer>
<a href="/status">status</a> &bull;
<a href="/identity">identity</a> &bull;
<a href="/clock">clock</a> &bull;
<a href="/skg">skg</a> &bull;
<a href="/terms">terms</a> &bull;
<a href="https://ferricula.com">ferricula.com</a> &bull;
<a href="https://github.com/DeepBlueDynamics/ferricula">source</a>
</footer>
</body>
</html>"#,
        ver = env!("CARGO_PKG_VERSION"),
    )
}

/// Check if a service is reachable (quick TCP probe).
fn check_service(url: &str, _path: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let stripped = url.strip_prefix("http://").unwrap_or(url);
    let stripped = stripped.strip_prefix("https://").unwrap_or(stripped);
    let addr = stripped.trim_end_matches('/');
    TcpStream::connect_timeout(
        &addr
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// Minimal agent config parsed from /data/agent.toml (if present).
struct AgentConfig {
    name: String,
    role: String,
    voice: String,
}

/// Try to load agent.toml from the data directory.
fn load_agent_toml() -> Option<AgentConfig> {
    // Check common data paths
    for dir in &["./data", "/data"] {
        let path = std::path::Path::new(dir).join("agent.toml");
        if let Ok(contents) = std::fs::read_to_string(&path) {
            return parse_agent_toml(&contents);
        }
    }
    None
}

/// Minimal TOML parser — extracts name, role, voice from agent config.
/// No toml crate dependency; just finds key = "value" lines.
fn extract_toml_string(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = ");
    let line = line.trim();
    if line.starts_with(&prefix) || line.starts_with(&format!("{key}=")) {
        let val = line.split_once('=')?.1.trim();
        let val = val.trim_matches('"').trim_matches('\'');
        Some(val.to_string())
    } else {
        None
    }
}

fn parse_agent_toml(contents: &str) -> Option<AgentConfig> {
    let mut name = None;
    let mut role = None;
    let mut voice = None;

    for line in contents.lines() {
        let line = line.trim();
        if let Some(val) = extract_toml_string(line, "name") {
            name = Some(val);
        } else if let Some(val) = extract_toml_string(line, "role") {
            role = Some(val);
        } else if let Some(val) = extract_toml_string(line, "voice") {
            voice = Some(val);
        }
    }

    Some(AgentConfig {
        name: name.unwrap_or_else(|| "unnamed".to_string()),
        role: role.unwrap_or_else(|| "general agent".to_string()),
        voice: voice.unwrap_or_else(|| "default".to_string()),
    })
}

fn cmd_status(db: &DurableEngine, identity: &IdentityState) -> Result<String> {
    let store = db.memory_store();
    let active = store.in_state(ferricula::LifecycleState::Active).len();
    let forgiven = store.in_state(ferricula::LifecycleState::Forgiven).len();
    let archived = store.in_state(ferricula::LifecycleState::Archived).len();
    let keystones = store.keystones().len();

    let agent_name = load_agent_toml()
        .map(|a| a.name)
        .unwrap_or_default();
    let name_line = if agent_name.is_empty() {
        identity.name.clone()
    } else {
        format!("{} ({})", agent_name, identity.name)
    };

    Ok(format!(
        "{name_line}:\n  rows={}\n  memories={} (active={active} forgiven={forgiven} archived={archived})\n  keystones={keystones}\n  graph: {} nodes, {} edges\n  prime_tree: {} terms, {} nodes, {} members",
        db.engine().row_count(),
        store.len(),
        db.graph().node_count(),
        db.graph().edge_count(),
        db.prime_tree().root_count(),
        db.prime_tree().node_count(),
        db.prime_tree().total_members(),
    ))
}

fn cmd_inspect(db: &DurableEngine, tail: &str) -> Result<String> {
    let id: u32 = tail.parse()?;
    let Some(record) = db.memory_store().get(id) else {
        bail!("no memory record for id={id}");
    };

    let neighbors = db.graph().neighbors(id);
    let degree = db.graph().degree(id);

    let emotion_str = record
        .emotion
        .as_ref()
        .map(|e| {
            let sec = e.secondary.as_deref().unwrap_or("-");
            format!("{}/{}", e.primary, sec)
        })
        .unwrap_or_else(|| "-".to_string());

    Ok(format!(
        "memory id={id}:\n  state={:?}\n  fidelity={:.4}\n  decay_alpha={:.5} (effective={:.5})\n  keystone={}\n  recalls={}\n  consolidation_depth={}\n  importance={:.2}\n  emotion={emotion_str}\n  provenance={:?}\n  created_at={}\n  last_recalled={}\n  age={}s\n  staleness={}s\n  graph: degree={degree} neighbors={:?}",
        record.state,
        record.fidelity,
        record.decay_alpha,
        record.effective_alpha(),
        record.keystone,
        record.recall_count,
        record.consolidation_depth,
        record.importance,
        record.provenance,
        epoch_to_iso8601(record.created_at),
        epoch_to_iso8601(record.last_recalled),
        record.age(),
        record.staleness(),
        neighbors.iter().collect::<Vec<_>>(),
    ))
}

fn cmd_keystone(db: &mut DurableEngine, tail: &str) -> Result<String> {
    let id: u32 = tail.parse()?;
    let Some(record) = db.memory_store_mut().get_mut(id) else {
        bail!("no memory record for id={id}");
    };
    record.keystone = !record.keystone;
    let status = if record.keystone { "set" } else { "unset" };
    Ok(format!("keystone {status} for id={id}"))
}

fn cmd_connect(db: &mut DurableEngine, tail: &str) -> Result<String> {
    let parts: Vec<&str> = tail.split_whitespace().collect();
    if parts.len() < 2 {
        anyhow::bail!("connect: need at least two IDs");
    }
    let a: u32 = parts[0].parse()?;
    let b: u32 = parts[1].parse()?;
    let label = parts.get(2).copied().unwrap_or("related").to_string();
    let kind = match parts.get(3).copied() {
        Some("causal") => EdgeKind::Causal,
        _ => EdgeKind::Semantic,
    };
    let arrow = if kind == EdgeKind::Causal { "->" } else { "<->" };
    db.connect(a, b, label.clone(), 1.0, kind)?;
    Ok(format!("connected {a} {arrow} {b} [{label}]"))
}

fn cmd_disconnect(db: &mut DurableEngine, tail: &str) -> Result<String> {
    let mut args = tail.splitn(2, ' ');
    let a: u32 = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("disconnect: missing id1"))?
        .parse()?;
    let b: u32 = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("disconnect: missing id2"))?
        .parse()?;
    db.disconnect(a, b)?;
    Ok(format!("disconnected {a} <-> {b}"))
}

fn cmd_neighbors(db: &DurableEngine, tail: &str) -> Result<String> {
    let id: u32 = tail.parse()?;
    let neighbors = db.graph().neighbors(id);
    let predecessors = db.graph().predecessors(id);
    let mut lines = Vec::new();
    for nid in neighbors.iter() {
        let edge_info = db
            .graph()
            .edge(id, nid)
            .map(|e| {
                let arrow = if e.kind == EdgeKind::Causal { "->" } else { "<->" };
                format!(" [{} {}] w={:.2}", e.label, arrow, e.weight)
            })
            .unwrap_or_default();
        let fidelity_info = db
            .memory_store()
            .get(nid)
            .map(|r| format!(" fidelity={:.3}", r.fidelity))
            .unwrap_or_default();
        lines.push(format!("  {nid}{edge_info}{fidelity_info}"));
    }
    if !predecessors.is_empty() {
        lines.push("  --- causal predecessors ---".to_string());
        for pid in predecessors.iter() {
            let edge_info = db
                .graph()
                .edge(pid, id)
                .map(|e| format!(" [{} <-] w={:.2}", e.label, e.weight))
                .unwrap_or_default();
            let fidelity_info = db
                .memory_store()
                .get(pid)
                .map(|r| format!(" fidelity={:.3}", r.fidelity))
                .unwrap_or_default();
            lines.push(format!("  {pid}{edge_info}{fidelity_info}"));
        }
    }
    let total = neighbors.len() + predecessors.len();
    Ok(format!(
        "neighbors of {id} ({total}):\n{}",
        lines.join("\n")
    ))
}

fn cmd_terms(db: &DurableEngine) -> Result<String> {
    let terms = db.prime_tree().terms();
    if terms.is_empty() {
        return Ok("no terms".to_string());
    }
    let mut lines = Vec::new();
    for term in &terms {
        let members = db.prime_tree().search_exact(term);
        lines.push(format!("  {term}: {} members", members.len()));
    }
    Ok(format!("terms ({}):\n{}", terms.len(), lines.join("\n")))
}

fn cmd_skg(db: &DurableEngine) -> String {
    let skg = db.skg();
    let mut edges: Vec<ferricula::SkgEdge> = skg
        .histories
        .iter()
        .map(|(key, hist)| ferricula::SkgEdge {
            term_a: key.a.clone(),
            term_b: key.b.clone(),
            jaccard: hist.current_jaccard().unwrap_or(0.0),
            velocity: hist.velocity(),
            acceleration: hist.acceleration(),
            weber_bracket: hist.weber_bracket(),
            history_len: hist.len(),
        })
        .collect();

    // Sort by |weber_bracket| desc
    edges.sort_by(|a, b| {
        let abs_a = a.weber_bracket.unwrap_or(0.0).abs();
        let abs_b = b.weber_bracket.unwrap_or(0.0).abs();
        abs_b.total_cmp(&abs_a)
    });

    let emerging: Vec<&ferricula::SkgEdge> = edges.iter().filter(|e| e.weber_bracket.map_or(false, |w| w > 0.0)).take(5).collect();
    let decaying: Vec<&ferricula::SkgEdge> = edges.iter().filter(|e| e.weber_bracket.map_or(false, |w| w < 0.0)).take(5).collect();

    serde_json::json!({
        "dream_tick": skg.dream_tick,
        "tracked_pairs": skg.histories.len(),
        "top_emerging": emerging,
        "top_decaying": decaying,
    })
    .to_string()
}

fn cmd_skg_term(db: &DurableEngine, term: &str) -> String {
    let edges = db.skg().edges_for_term(term);
    serde_json::json!({
        "term": term,
        "edges": edges,
    })
    .to_string()
}

fn cmd_maxid(db: &DurableEngine) -> Result<String> {
    let max = db.engine().rows_iter().map(|r| r.id).max().unwrap_or(0);
    Ok(format!("{max}"))
}

fn cmd_get(db: &DurableEngine, tail: &str) -> Result<String> {
    let id: u32 = tail.parse()?;
    let Some(row) = db.engine().get(id) else {
        bail!("no row for id={id}");
    };
    let tags: serde_json::Map<String, serde_json::Value> = row
        .tags
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let obj = serde_json::json!({
        "id": row.id,
        "tags": tags,
        "vector_dim": row.vector.len(),
    });
    Ok(serde_json::to_string(&obj)?)
}

fn cmd_touch(db: &mut DurableEngine, tail: &str) -> Result<String> {
    let id: u32 = tail.parse()?;
    let Some(record) = db.memory_store_mut().get_mut(id) else {
        bail!("no memory record for id={id}");
    };
    record.on_recall();
    Ok(format!("touched id={id} recalls={}", record.recall_count))
}

// --- Parsing helpers ---

fn parse_row_json(text: &str) -> Result<Row> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let id = value
        .get("id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("row.id is required"))? as u32;

    let tags_obj = value
        .get("tags")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow::anyhow!("row.tags must be an object"))?;
    let mut tags = BTreeMap::new();
    for (k, v) in tags_obj {
        tags.insert(
            k.to_string(),
            v.as_str()
                .ok_or_else(|| anyhow::anyhow!("tag values must be strings"))?
                .to_string(),
        );
    }

    let vector_arr = value
        .get("vector")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("row.vector must be an array"))?;
    let mut vector = Vec::with_capacity(vector_arr.len());
    for x in vector_arr {
        vector.push(
            x.as_f64()
                .ok_or_else(|| anyhow::anyhow!("vector values must be numbers"))?
                as f32,
        );
    }
    Ok(Row { id, tags, vector })
}

fn parse_metric(text: &str) -> Result<DistanceMetric> {
    match text.to_lowercase().as_str() {
        "cosine" => Ok(DistanceMetric::Cosine),
        "l2" => Ok(DistanceMetric::L2),
        "jaccard" => Ok(DistanceMetric::Jaccard),
        _ => bail!("unknown metric: {text}"),
    }
}

fn parse_vector_literal(text: &str) -> Result<Vec<f32>> {
    let trimmed = text.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        bail!("vector cannot be empty");
    }
    let mut out = Vec::new();
    for part in trimmed.split(',') {
        out.push(part.trim().parse::<f32>()?);
    }
    Ok(out)
}

fn show_splash() {
    if std::env::var_os("NO_COLOR").is_some() || std::env::var("TERM").unwrap_or_default() == "dumb"
    {
        return;
    }
    if let Err(err) = render_splash() {
        eprintln!("splash skipped: {err}");
    }
}

fn render_splash() -> Result<()> {
    use crossterm::event::{Event, poll, read};
    use crossterm::execute;
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::{Alignment, Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph};

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|f| {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Percentage(35),
                    Constraint::Length(7),
                    Constraint::Percentage(58),
                ]
                .as_ref(),
            )
            .split(f.size());

        let title = Paragraph::new(vec![
            Line::from(Span::styled(
                "FERRICULA",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            )),
            Line::from(Span::styled(
                "COGNITIVE MEMORY",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::NONE));

        let tagline = Paragraph::new(Line::from(vec![
            Span::styled("contact ", Style::default().fg(Color::White)),
            Span::styled("→", Style::default().fg(Color::Gray)),
            Span::styled(" feeling ", Style::default().fg(Color::LightCyan)),
            Span::styled("→", Style::default().fg(Color::Gray)),
            Span::styled(" perception ", Style::default().fg(Color::LightMagenta)),
            Span::styled("→", Style::default().fg(Color::Gray)),
            Span::styled(" thought ", Style::default().fg(Color::White)),
            Span::styled("→", Style::default().fg(Color::Gray)),
            Span::styled(" memory", Style::default().fg(Color::Yellow)),
        ]))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Gray)),
        );

        f.render_widget(title, chunks[0]);
        f.render_widget(tagline, chunks[1]);
    })?;

    let wait = Duration::from_millis(1200);
    if poll(wait)? {
        if let Event::Key(_) = read()? {
            // consume one key press
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
