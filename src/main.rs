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
use ferricula::planner::Planner;
use ferricula::{DistanceMetric, DurableEngine, Row};

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
    let (identity, is_new) = ferricula::identity::load_or_create(&data_dir, &identity_entropy);
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
        eprintln!("ferricula v0.3.0 (serve mode)");
        eprintln!("data dir: {data_dir}");
        eprintln!("identity: {} ({})", identity.agent_id, identity.name);

        let (http_tx, http_rx) = mpsc::channel::<HttpCommand>();
        let _http_running = Arc::clone(&should_stop);
        // Invert the flag: http thread checks `running` (true=keep going)
        let http_flag = Arc::new(AtomicBool::new(true));
        let http_flag_clone = Arc::clone(&http_flag);

        let _http_handle = ferricula::http::spawn_http(serve_port, http_tx, http_flag.clone());

        loop {
            if should_stop.load(Ordering::SeqCst) {
                eprintln!("[serve] signal received, checkpointing...");
                http_flag_clone.store(false, Ordering::SeqCst);
                db.checkpoint()?;
                eprintln!("[serve] checkpoint complete");
                break;
            }

            process_clock_events(&mut db, &clock_rx, &chonk_url);
            process_http_commands(
                &mut db,
                &planner,
                &clock_telemetry,
                &identity,
                &chonk_url,
                &http_rx,
            );

            // Small sleep to avoid busy-spinning when no events
            std::thread::sleep(Duration::from_millis(10));
        }
    } else {
        // REPL mode (original behavior)
        println!("ferricula v0.3.0");
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

            process_clock_events(&mut db, &clock_rx, &chonk_url);

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

                    match handle_command(&mut db, &planner, &clock_telemetry, &chonk_url, &line) {
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
fn process_clock_events(db: &mut DurableEngine, clock_rx: &mpsc::Receiver<ClockEvent>, chonk_url: &str) {
    let chonk = if inversion::chonk_available(chonk_url) { Some(chonk_url) } else { None };
    while let Ok(event) = clock_rx.try_recv() {
        match event {
            ClockEvent::DreamTrigger {
                epoch,
                intensity,
                entropy_bytes,
            } => {
                let report = db.dream_with_intensity(intensity, &[], chonk);
                eprintln!(
                    "[clock] dream epoch={epoch} intensity={intensity:.2} entropy={entropy_bytes}B \
                     decayed={} forgiven={} consolidated={} pruned={} ghosts={}",
                    report.decayed, report.forgiven, report.consolidated, report.pruned, report.ghost_echoes,
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

/// Process pending HTTP commands (non-blocking drain).
fn process_http_commands(
    db: &mut DurableEngine,
    planner: &Planner,
    telemetry: &Arc<ClockTelemetry>,
    identity: &IdentityState,
    chonk_url: &str,
    http_rx: &mpsc::Receiver<HttpCommand>,
) {
    while let Ok(cmd) = http_rx.try_recv() {
        match cmd {
            HttpCommand::Remember { body, reply } => {
                let result = cmd_remember(db, &body).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Recall { body, reply } => {
                // Extract "query" field from JSON body, fall back to raw body
                let query_text = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("query")?.as_str().map(|s| s.to_string()))
                    .unwrap_or(body);
                let result = cmd_recall(db, planner, &query_text).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Dream { body: _, reply } => {
                let result = cmd_dream(db, chonk_url).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Status { reply } => {
                let result = cmd_status(db).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Inspect { id, reply } => {
                let result = cmd_inspect(db, &id.to_string()).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Offer { body, reply } => {
                let result = cmd_offer(db, &body, chonk_url).unwrap_or_else(|e| json_error(&e));
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
                let _ = reply.send(identity.to_json());
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
    db.connect(a, b, label.to_string(), 1.0)?;
    Ok(format!("connected {a} <-> {b} [{label}]"))
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
        "recall" => cmd_recall(db, planner, tail),
        "dream" => cmd_dream(db, chonk_url),
        "status" => cmd_status(db),
        "inspect" => cmd_inspect(db, tail),
        "keystone" => cmd_keystone(db, tail),
        "connect" => cmd_connect(db, tail),
        "disconnect" => cmd_disconnect(db, tail),
        "neighbors" => cmd_neighbors(db, tail),
        "terms" => cmd_terms(db),
        "clock" => cmd_clock(telemetry),
        "offer" => cmd_offer(db, tail, chonk_url),
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
            let result = db.execute_sql(&canonical)?;
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

    let tag_values: Vec<String> = row.tags.values().map(|v| v.to_lowercase()).collect();

    db.remember(row, record)?;

    for term in &tag_values {
        db.insert_term(term, id)?;
    }

    Ok(format!("remembered id={id} terms={tag_values:?}"))
}

fn cmd_recall(db: &mut DurableEngine, planner: &Planner, tail: &str) -> Result<String> {
    let canonical = planner.rewrite_query(tail)?;
    let result = db.execute_sql(&canonical)?;

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
        lines.push(format!("  id={id}{fidelity}"));
    }

    Ok(format!(
        "recalled {} memories:\n{}",
        result.ids.len(),
        lines.join("\n")
    ))
}

fn cmd_dream(db: &mut DurableEngine, chonk_url: &str) -> Result<String> {
    let chonk = if inversion::chonk_available(chonk_url) { Some(chonk_url) } else { None };
    let report = db.dream(chonk);
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

fn cmd_offer(db: &mut DurableEngine, tail: &str, chonk_url: &str) -> Result<String> {
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
    Ok(format!(
        "offer accepted: {}B entropy, intensity={intensity:.2}\n{}",
        bytes.len(),
        format_dream_report(&report),
    ))
}

fn format_dream_report(report: &ferricula::DreamReport) -> String {
    format!(
        "dream complete:\n  ticks={}\n  decayed={}\n  forgiven={}\n  archived={}\n  consolidated={}\n  pruned={}\n  ghost_echoes={}\n  keystones_reviewed={}",
        report.ticks,
        report.decayed,
        report.forgiven,
        report.archived,
        report.consolidated,
        report.pruned,
        report.ghost_echoes,
        report.keystones_reviewed,
    )
}

fn cmd_status(db: &DurableEngine) -> Result<String> {
    let store = db.memory_store();
    let active = store.in_state(ferricula::LifecycleState::Active).len();
    let forgiven = store.in_state(ferricula::LifecycleState::Forgiven).len();
    let archived = store.in_state(ferricula::LifecycleState::Archived).len();
    let keystones = store.keystones().len();

    Ok(format!(
        "ferricula:\n  rows={}\n  memories={} (active={active} forgiven={forgiven} archived={archived})\n  keystones={keystones}\n  graph: {} nodes, {} edges\n  prime_tree: {} terms, {} nodes, {} members",
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
        "memory id={id}:\n  state={:?}\n  fidelity={:.4}\n  decay_alpha={:.5} (effective={:.5})\n  keystone={}\n  recalls={}\n  consolidation_depth={}\n  importance={:.2}\n  emotion={emotion_str}\n  provenance={:?}\n  age={}s\n  staleness={}s\n  graph: degree={degree} neighbors={:?}",
        record.state,
        record.fidelity,
        record.decay_alpha,
        record.effective_alpha(),
        record.keystone,
        record.recall_count,
        record.consolidation_depth,
        record.importance,
        record.provenance,
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
    let mut args = tail.splitn(3, ' ');
    let a: u32 = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("connect: missing id1"))?
        .parse()?;
    let b: u32 = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("connect: missing id2"))?
        .parse()?;
    let label = args.next().unwrap_or("related").to_string();
    db.connect(a, b, label.clone(), 1.0)?;
    Ok(format!("connected {a} <-> {b} [{label}]"))
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
    let mut lines = Vec::new();
    for nid in neighbors.iter() {
        let edge_info = db
            .graph()
            .edge(id, nid)
            .map(|e| format!(" [{}] w={:.2}", e.label, e.weight))
            .unwrap_or_default();
        let fidelity_info = db
            .memory_store()
            .get(nid)
            .map(|r| format!(" fidelity={:.3}", r.fidelity))
            .unwrap_or_default();
        lines.push(format!("  {nid}{edge_info}{fidelity_info}"));
    }
    Ok(format!(
        "neighbors of {id} ({}):\n{}",
        neighbors.len(),
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
