//! Embeddable server — run Ferricula's HTTP service inside another binary.
//!
//! Call `spawn_embedded(data_dir, port)` to start the full Ferricula engine
//! with HTTP service on a background thread. Returns a handle to stop it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::clock::{self, ClockConfig, ClockEvent, ClockTelemetry};
use crate::corpus::SearchEngine;
use crate::http::HttpCommand;
use crate::identity::IdentityState;
use crate::inversion;
use crate::memory::{Emotion, MemoryRecord};
use crate::planner::{Planner, PlannerResult};
use crate::tokenizer;
use crate::{DurableEngine, EdgeKind, Row};

/// Handle returned by `spawn_embedded`. Drop it or call `stop()` to shut down.
pub struct EmbeddedHandle {
    stop_flag: Arc<AtomicBool>,
    _thread: JoinHandle<()>,
}

impl EmbeddedHandle {
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }
}

impl Drop for EmbeddedHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

struct PendingRecall {
    rx: mpsc::Receiver<PlannerResult>,
    reply: mpsc::SyncSender<String>,
}

/// Spawn the full Ferricula engine + HTTP server on background threads.
pub fn spawn_embedded(data_dir: &str, port: u16) -> Result<EmbeddedHandle, String> {
    let data_dir = data_dir.to_string();
    let should_stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&should_stop);

    let thread = thread::Builder::new()
        .name("ferricula-core".into())
        .spawn(move || {
            if let Err(e) = run_server(&data_dir, port, &should_stop) {
                eprintln!("[ferricula-embed] error: {e}");
            }
        })
        .map_err(|e| format!("failed to spawn ferricula thread: {e}"))?;

    Ok(EmbeddedHandle {
        stop_flag,
        _thread: thread,
    })
}

fn run_server(data_dir: &str, port: u16, should_stop: &Arc<AtomicBool>) -> anyhow::Result<()> {
    let _ = std::fs::create_dir_all(data_dir);
    let mut db = DurableEngine::open(data_dir)?;

    let identity_entropy = get_identity_entropy();
    let (mut identity, is_new) = crate::identity::load_or_create(data_dir, &identity_entropy);
    if is_new {
        let (row, record) = crate::identity::create_anchor(&identity);
        if let Err(e) = db.remember(row, record) {
            eprintln!("[ferricula-embed] anchor memory skipped: {e}");
        }
    }

    let agent_key = std::env::var("AGENT_KEY").ok();
    let planner = Planner::new(agent_key);
    let shivvr_url = std::env::var("SHIVVR_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());

    let clock_config = ClockConfig::from_env();
    let (clock_rx, clock_telemetry) = clock::spawn_clock(clock_config);

    let (http_tx, http_rx) = mpsc::channel::<HttpCommand>();
    let http_flag = Arc::new(AtomicBool::new(true));
    let http_flag_clone = Arc::clone(&http_flag);
    let _http_handle = crate::http::spawn_http(port, http_tx, http_flag.clone());

    let mut search_engine = SearchEngine::new();
    for row in db.engine().rows_iter() {
        if let Some(text) = row.tags.get("text") {
            search_engine.add_document(row.id, text);
        }
    }
    if search_engine.corpus.n_docs > 0 {
        search_engine.recalibrate();
    }

    eprintln!(
        "[ferricula-embed] v{} on :{} ({} memories, identity: {})",
        env!("CARGO_PKG_VERSION"),
        port,
        db.engine().row_count(),
        identity.name,
    );

    let mut pending_recalls: Vec<PendingRecall> = Vec::new();
    let mut last_dream_report = String::new();

    loop {
        if should_stop.load(Ordering::SeqCst) {
            eprintln!("[ferricula-embed] shutting down...");
            http_flag_clone.store(false, Ordering::SeqCst);
            let _ = db.checkpoint();
            break;
        }

        process_clock_events(&mut db, &mut identity, &clock_rx, &shivvr_url, &mut last_dream_report);
        process_http_commands(
            &mut db, &planner, &clock_telemetry, &mut identity, &shivvr_url,
            &http_rx, &mut pending_recalls, &mut search_engine, &mut last_dream_report,
        );
        process_pending_recalls(&mut db, &mut identity, &mut pending_recalls, &shivvr_url);

        std::thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}

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

fn json_wrap(key: &str, value: &str) -> String {
    serde_json::json!({ key: value }).to_string()
}

fn json_error(err: &anyhow::Error) -> String {
    format!("error: {err:#}")
}

fn encrypt_vector_in_body(body: &str, vt: &crate::transform::VectorTransform) -> String {
    let mut val: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return body.to_string(),
    };
    if let Some(arr) = val.get("vector").and_then(|v| v.as_array()) {
        let plain: Vec<f32> = arr.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
        if !plain.is_empty() {
            let encrypted = vt.encrypt(&plain);
            val["vector"] = serde_json::json!(encrypted);
        }
    }
    val.to_string()
}

fn process_clock_events(
    db: &mut DurableEngine, identity: &mut IdentityState,
    clock_rx: &mpsc::Receiver<ClockEvent>, shivvr_url: &str,
    last_dream_report: &mut String,
) {
    let shivvr = if inversion::shivvr_available(shivvr_url) { Some(shivvr_url) } else { None };
    while let Ok(event) = clock_rx.try_recv() {
        match event {
            ClockEvent::DreamTrigger { epoch, intensity, entropy_bytes, seed } => {
                let _ = entropy_bytes;
                let report = db.dream_with_intensity(intensity, &seed, shivvr);
                identity.activate_from_report(&report);
                identity.dream_cool();
                *last_dream_report = format_dream_report(&report);
                eprintln!("[ferricula-embed] dream epoch={epoch} intensity={intensity:.2}");
            }
            ClockEvent::RadioStatus { available, message } => {
                let status = if available { "connected" } else { "disconnected" };
                eprintln!("[ferricula-embed] radio {status}: {message}");
            }
            ClockEvent::Tick { .. } => {}
        }
    }
}

fn process_pending_recalls(
    db: &mut DurableEngine, identity: &mut IdentityState,
    pending: &mut Vec<PendingRecall>, shivvr_url: &str,
) {
    let mut completed = Vec::new();
    for (i, pr) in pending.iter().enumerate() {
        match pr.rx.try_recv() {
            Ok(result) => {
                let output = db.execute_sql_with_embed(&result.sql, Some(shivvr_url))
                    .map(|r| {
                        let rows: Vec<serde_json::Value> = r.ids.iter()
                            .filter_map(|id| db.engine().get(*id).map(|row| {
                                serde_json::json!({"id": row.id, "tags": row.tags})
                            }))
                            .collect();
                        serde_json::to_string(&rows).unwrap_or_default()
                    })
                    .unwrap_or_else(|e| json_error(&e));
                let _ = pr.reply.send(json_wrap("result", &output));
                completed.push(i);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                let _ = pr.reply.send(json_wrap("result", "error: planner thread died"));
                completed.push(i);
            }
        }
    }
    for i in completed.into_iter().rev() {
        pending.swap_remove(i);
    }
}

fn process_http_commands(
    db: &mut DurableEngine, planner: &Planner, telemetry: &Arc<ClockTelemetry>,
    identity: &mut IdentityState, shivvr_url: &str,
    http_rx: &mpsc::Receiver<HttpCommand>, pending_recalls: &mut Vec<PendingRecall>,
    search_engine: &mut SearchEngine, last_dream_report: &mut String,
) {
    while let Ok(cmd) = http_rx.try_recv() {
        match cmd {
            HttpCommand::Remember { body, reply } => {
                let text_for_index = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| {
                        let id = v.get("id")?.as_u64()? as u32;
                        let text = v.get("tags")?.get("text")?.as_str()?.to_string();
                        Some((id, text))
                    });
                let encrypted_body = if let Some(ref vt) = identity.vector_transform {
                    encrypt_vector_in_body(&body, vt)
                } else {
                    body.clone()
                };
                let result = cmd_remember(db, &encrypted_body).unwrap_or_else(|e| json_error(&e));
                if let Some((id, text)) = text_for_index {
                    search_engine.add_document(id, &text);
                }
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Recall { body, reply } => {
                let query_text = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("query")?.as_str().map(|s| s.to_string()))
                    .unwrap_or(body);
                if planner.needs_llm(&query_text) {
                    let rx = planner.spawn_llm_rewrite(&query_text);
                    pending_recalls.push(PendingRecall { rx, reply });
                } else {
                    let sql = planner.rewrite_query_sync(&query_text)
                        .unwrap_or_else(|e| format!("error: {e}"));
                    let result = db.execute_sql_with_embed(&sql, Some(shivvr_url))
                        .map(|r| {
                            let rows: Vec<serde_json::Value> = r.ids.iter()
                                .filter_map(|id| db.engine().get(*id).map(|row| {
                                    serde_json::json!({"id": row.id, "tags": row.tags})
                                }))
                                .collect();
                            serde_json::to_string(&rows).unwrap_or_default()
                        })
                        .unwrap_or_else(|e| json_error(&e));
                    let _ = reply.send(json_wrap("result", &result));
                }
            }
            HttpCommand::Dream { body: _, reply } => {
                let shivvr = if inversion::shivvr_available(shivvr_url) { Some(shivvr_url) } else { None };
                let report = db.dream(shivvr);
                identity.activate_from_report(&report);
                identity.dream_cool();
                let result = format_dream_report(&report);
                *last_dream_report = result.clone();
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Status { reply } => {
                let result = serde_json::json!({
                    "memories": db.engine().row_count(),
                    "identity": identity.name,
                    "agent_id": identity.agent_id,
                    "heat": identity.cognitive_heat,
                    "ticks": telemetry.tick_count.load(Ordering::Relaxed),
                    "dreams": telemetry.dream_count.load(Ordering::Relaxed),
                }).to_string();
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Inspect { id, reply } => {
                let result = if let Some(row) = db.engine().get(id) {
                    let record = db.memory_store().get(id);
                    serde_json::json!({
                        "id": id, "tags": row.tags,
                        "record": record.map(|r| serde_json::json!({
                            "importance": r.importance,
                            "keystone": r.keystone,
                            "state": format!("{:?}", r.state),
                        })),
                    }).to_string()
                } else {
                    json_error(&anyhow::anyhow!("row {id} not found"))
                };
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Offer { body: _, reply } => {
                let shivvr = if inversion::shivvr_available(shivvr_url) { Some(shivvr_url) } else { None };
                let report = db.dream(shivvr);
                identity.activate_from_report(&report);
                let result = format_dream_report(&report);
                *last_dream_report = result.clone();
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Keystone { id, reply } => {
                let result = if let Some(record) = db.memory_store_mut().get_mut(id) {
                    record.keystone = true;
                    serde_json::json!({"keystone": true, "id": id}).to_string()
                } else {
                    json_error(&anyhow::anyhow!("row {id} not found"))
                };
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Connect { body, reply } => {
                let result = handle_connect(db, &body).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Disconnect { body, reply } => {
                let result = handle_disconnect(db, &body).unwrap_or_else(|e| json_error(&e));
                let _ = reply.send(json_wrap("result", &result));
            }
            HttpCommand::Neighbors { id, reply } => {
                let edges = db.graph().all_edges().into_iter()
                    .filter(|e| e.from == id || e.to == id)
                    .collect::<Vec<_>>();
                let _ = reply.send(serde_json::to_string(&edges).unwrap_or_default());
            }
            HttpCommand::Clock { reply } => {
                let result = serde_json::json!({
                    "ticks": telemetry.tick_count.load(Ordering::Relaxed),
                    "dreams": telemetry.dream_count.load(Ordering::Relaxed),
                    "entropy_lifetime": telemetry.entropy_lifetime.load(Ordering::Relaxed),
                    "radio_available": telemetry.radio_available.load(Ordering::Relaxed),
                }).to_string();
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
                let mut val: serde_json::Value = serde_json::from_str(&identity.to_json())
                    .unwrap_or_else(|_| serde_json::json!({}));
                val["version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
                let _ = reply.send(serde_json::to_string_pretty(&val).unwrap_or_default());
            }
            HttpCommand::GetRow { id, reply } => {
                let result = if let Some(row) = db.engine().get(id) {
                    serde_json::json!({"id": row.id, "tags": row.tags}).to_string()
                } else {
                    json_error(&anyhow::anyhow!("row {id} not found"))
                };
                let _ = reply.send(result);
            }
            HttpCommand::Terms { reply } => {
                let terms: Vec<String> = db.engine().rows_iter()
                    .flat_map(|r| r.tags.get("text").map(|t| tokenizer::extract_terms(t)))
                    .flatten()
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                let _ = reply.send(serde_json::to_string(&terms).unwrap_or_default());
            }
            HttpCommand::InversionCheck { id, reply } => {
                let _ = reply.send(serde_json::json!({"inversion_check": id}).to_string());
            }
            HttpCommand::Skg { reply } => {
                let _ = reply.send("{}".to_string());
            }
            HttpCommand::SkgTerm { term: _, reply } => {
                let _ = reply.send("[]".to_string());
            }
            HttpCommand::Delete { id, reply } => {
                search_engine.remove_document(id);
                let result = match db.remove_memory(id) {
                    Ok(true) => serde_json::json!({"deleted": true, "id": id}).to_string(),
                    Ok(false) => serde_json::json!({"deleted": false, "id": id}).to_string(),
                    Err(e) => json_error(&e),
                };
                let _ = reply.send(result);
            }
            HttpCommand::MaxId { reply } => {
                let max = db.engine().rows_iter().map(|r| r.id).max().unwrap_or(0);
                let _ = reply.send(json_wrap("maxid", &max.to_string()));
            }
            HttpCommand::Search { body, reply } => {
                let val: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let query = val["query"].as_str().unwrap_or("");
                let k = val["k"].as_u64().unwrap_or(10) as usize;
                let results = search_engine.bm25_search(query, k, db.prime_tree());
                let rows: Vec<serde_json::Value> = results.iter()
                    .filter_map(|hit| db.engine().get(hit.id).map(|r| {
                        serde_json::json!({"id": r.id, "tags": r.tags, "score": hit.bm25_score})
                    }))
                    .collect();
                let _ = reply.send(serde_json::to_string(&rows).unwrap_or_default());
            }
            HttpCommand::Hybrid { body, reply } => {
                // Simplified — same as search for now
                let val: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let query = val["query"].as_str().unwrap_or("");
                let k = val["k"].as_u64().unwrap_or(10) as usize;
                let results = search_engine.bm25_search(query, k, db.prime_tree());
                let rows: Vec<serde_json::Value> = results.iter()
                    .filter_map(|hit| db.engine().get(hit.id).map(|r| {
                        serde_json::json!({"id": r.id, "tags": r.tags, "score": hit.bm25_score})
                    }))
                    .collect();
                let _ = reply.send(serde_json::to_string(&rows).unwrap_or_default());
            }
            HttpCommand::Glossary { reply } => {
                let _ = reply.send(crate::pali::glossary_json());
            }
            HttpCommand::Dashboard { reply } => {
                let result = serde_json::json!({
                    "memories": db.engine().row_count(),
                    "identity": identity.name,
                    "agent_id": identity.agent_id,
                    "heat": identity.cognitive_heat,
                    "ticks": telemetry.tick_count.load(Ordering::Relaxed),
                    "dreams": telemetry.dream_count.load(Ordering::Relaxed),
                }).to_string();
                let _ = reply.send(result);
            }
            HttpCommand::Confer { body: _, reply } => {
                let _ = reply.send(serde_json::json!({"confer": "ok"}).to_string());
            }
            HttpCommand::LastDream { reply } => {
                if last_dream_report.is_empty() {
                    let _ = reply.send(json_wrap("result", "no dreams yet"));
                } else {
                    let _ = reply.send(json_wrap("result", last_dream_report));
                }
            }
            HttpCommand::Tools { reply } => {
                let result = serde_json::json!({
                    "tier": "NOMINAL",
                    "heat": identity.cognitive_heat,
                    "escalate": false,
                    "active_tools": [],
                    "suppressed": [],
                    "reason": format!("heat={:.2}", identity.cognitive_heat),
                }).to_string();
                let _ = reply.send(result);
            }
            HttpCommand::Query { reply, .. } => {
                let _ = reply.send(serde_json::json!({"rows": []}).to_string());
            }
            HttpCommand::RefsDistinct { field, reply } => {
                let values = db.engine().distinct_tag_values(&field);
                let result = serde_json::json!({ "field": field, "values": values, "count": values.len() });
                let _ = reply.send(result.to_string());
            }
        }
    }
}

fn cmd_remember(db: &mut DurableEngine, body: &str) -> anyhow::Result<String> {
    let value: serde_json::Value = serde_json::from_str(body)?;
    let row = parse_row_json(body)?;
    let id = row.id;
    let mut record = MemoryRecord::new(id);
    if let Some(emo) = value.get("emotion") {
        let primary = emo.get("primary").and_then(|v| v.as_str()).unwrap_or("neutral").to_string();
        let secondary = emo.get("secondary").and_then(|v| v.as_str()).map(String::from);
        record.emotion = Some(Emotion { primary, secondary });
    }
    if let Some(imp) = value.get("importance").and_then(|v| v.as_f64()) {
        record.importance = imp as f32;
    }
    if let Some(ks) = value.get("keystone").and_then(|v| v.as_bool()) {
        record.keystone = ks;
    }
    if let Some(alpha) = value.get("decay_alpha").and_then(|v| v.as_f64()) {
        record.decay_alpha = (alpha as f32).clamp(crate::memory::ALPHA_MIN, crate::memory::ALPHA_MAX);
    }
    db.remember(row, record)?;
    Ok(serde_json::json!({"remembered": id}).to_string())
}

fn handle_connect(db: &mut DurableEngine, body: &str) -> anyhow::Result<String> {
    let val: serde_json::Value = serde_json::from_str(body)?;
    let from = val["from"].as_u64().ok_or_else(|| anyhow::anyhow!("from required"))? as u32;
    let to = val["to"].as_u64().ok_or_else(|| anyhow::anyhow!("to required"))? as u32;
    let kind_str = val["kind"].as_str().unwrap_or("semantic");
    let kind = match kind_str {
        "causal" => EdgeKind::Causal,
        _ => EdgeKind::Semantic,
    };
    let label = val["label"].as_str().unwrap_or("").to_string();
    let weight = val["weight"].as_f64().unwrap_or(1.0) as f32;
    db.connect(from, to, label, weight, kind)?;
    Ok(serde_json::json!({"connected": true, "from": from, "to": to}).to_string())
}

fn handle_disconnect(db: &mut DurableEngine, body: &str) -> anyhow::Result<String> {
    let val: serde_json::Value = serde_json::from_str(body)?;
    let from = val["from"].as_u64().ok_or_else(|| anyhow::anyhow!("from required"))? as u32;
    let to = val["to"].as_u64().ok_or_else(|| anyhow::anyhow!("to required"))? as u32;
    db.disconnect(from, to)?;
    Ok(serde_json::json!({"disconnected": true, "from": from, "to": to}).to_string())
}

fn format_dream_report(report: &crate::DreamReport) -> String {
    serde_json::json!({
        "decayed": report.decayed,
        "forgiven": report.forgiven,
        "consolidated": report.consolidated,
        "pruned": report.pruned,
        "ghost_echoes": report.ghost_echoes,
        "edges_created": report.edges_created,
        "active_archetypes": report.active_archetypes,
    }).to_string()
}

fn parse_row_json(text: &str) -> anyhow::Result<Row> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let id = value.get("id").and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("row.id is required"))? as u32;
    let tags_obj = value.get("tags").and_then(|v| v.as_object())
        .ok_or_else(|| anyhow::anyhow!("row.tags must be an object"))?;
    let mut tags = std::collections::BTreeMap::new();
    for (k, v) in tags_obj {
        tags.insert(k.to_string(), v.as_str()
            .ok_or_else(|| anyhow::anyhow!("tag values must be strings"))?.to_string());
    }
    let vector_arr = value.get("vector").and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("row.vector must be an array"))?;
    let mut vector = Vec::with_capacity(vector_arr.len());
    for x in vector_arr {
        vector.push(x.as_f64().ok_or_else(|| anyhow::anyhow!("vector values must be numbers"))? as f32);
    }
    Ok(Row { id, tags, vector, refs: None })
}
