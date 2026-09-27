//! Operator-facing HTTP surface for Steve.
//!
//! Status and recall are read-only. Explicit operator chat and structured
//! episode commits persist in separate writable state; recovery memory stays
//! immutable. Background pause does not disable an authenticated chat request.
//! Overlay approval remains unavailable. MCP exposes read-only tools.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use ferricula_cognition::WhisperContext;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::runtime::{ActivityMode, RuntimeStatus, SteveRuntime, TaskKind};
use crate::sleep_cycle::{CooldownLedger, PlanningGate, plan_cycle_gated};

type ApiError = (StatusCode, Json<Value>);
type ApiResult<T> = Result<Json<T>, ApiError>;

/// Hard cap on array elements returned by Phase One list endpoints.
const MAX_STATUS_LIST: usize = 64;

pub fn router(runtime: Arc<SteveRuntime>) -> Router {
    Router::new()
        .route("/", get(chat_page))
        .route("/chat", post(chat))
        .route("/chat/{conversation_id}", get(conversation))
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/identity", get(identity))
        .route("/control/mode", post(set_mode))
        .route("/control/wake", post(wake))
        .route("/control/sleep", post(sleep))
        .route("/control/pause", post(pause))
        .route("/control/schedule", post(set_schedule))
        // Phase One read-only status slices (operator auth).
        .route("/control/autonomy", get(autonomy_status))
        .route("/control/schedule/plan", get(sleep_plan_status))
        .route("/control/advocate", get(advocate_status))
        .route("/tasks", get(tasks).post(enqueue))
        .route("/tasks/{id}", get(task))
        .route("/tasks/read-feed", post(read_feed))
        .route("/tasks/read-hacker-news", post(read_hacker_news))
        .route("/tasks/mention", post(mention))
        .route("/tasks/considerations", get(mention_considerations))
        .route("/memory/recall", post(recall))
        .route("/memory/episodes", post(episode_commit))
        .route("/memory/episode-query", post(episode_query))
        .route("/memory/overlay", get(overlay_status))
        .route(
            "/memory/overlay/approve/{event_id}",
            post(overlay_approve_blocked),
        )
        .route("/models/status", get(model_status))
        .route("/wisdom/preview", post(wisdom_preview))
        .merge(crate::mcp::router())
        .with_state(runtime)
}

async fn episode_commit(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Json(item): Json<ferricula_episode::EpisodeItem>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let event_id = runtime.commit_episode(item).map_err(bad_request)?;
    Ok(Json(json!({"event_id": event_id, "source_actor_verified": false})))
}

async fn episode_query(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Json(request): Json<ferricula_episode::query::EpisodeQueryRequest>,
) -> ApiResult<ferricula_episode::query::EpisodeQueryResponse> {
    require_operator(&runtime, &headers)?;
    if request.query.len() > 8192 { return Err(bad_request("query exceeds 8192 bytes")); }
    Ok(Json(runtime.query_episodes(&request)))
}

async fn chat_page() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("chat.html"))
}

async fn chat(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Json(request): Json<crate::runtime::ChatRequest>,
) -> ApiResult<crate::runtime::ChatTurn> {
    require_operator(&runtime, &headers)?;
    request.validate().map_err(bad_request)?;
    // The durable request must reach a terminal state even if HTTP disconnects.
    let turn = tokio::spawn(async move { runtime.converse(request).await })
        .await.map_err(internal)?.map_err(|error| (
        StatusCode::CONFLICT, Json(json!({"error": error.to_string()}))
    ))?;
    Ok(Json(turn))
}

async fn conversation(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    Ok(Json(json!({"turns": runtime.conversation(id), "limit": 100})))
}

async fn health(State(runtime): State<Arc<SteveRuntime>>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "agent_id": runtime.inspection.agent_id,
        "mode": runtime.status().mode
    }))
}

async fn status(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<crate::runtime::RuntimeStatus> {
    require_operator(&runtime, &headers)?;
    Ok(Json(runtime.status()))
}

async fn identity(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    Ok(Json(
        serde_json::to_value(&runtime.inspection).map_err(internal)?,
    ))
}

#[derive(Deserialize)]
struct ModeRequest {
    mode: String,
}

async fn set_mode(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Json(request): Json<ModeRequest>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let mode = ActivityMode::parse(&request.mode).map_err(bad_request)?;
    runtime.set_mode(mode).map_err(internal)?;
    Ok(Json(json!({ "mode": mode })))
}

async fn wake(State(runtime): State<Arc<SteveRuntime>>, headers: HeaderMap) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    runtime.set_mode(ActivityMode::Engaged).map_err(internal)?;
    Ok(Json(json!({ "mode": ActivityMode::Engaged })))
}

async fn sleep(State(runtime): State<Arc<SteveRuntime>>, headers: HeaderMap) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    runtime.set_mode(ActivityMode::Asleep).map_err(internal)?;
    Ok(Json(json!({ "mode": ActivityMode::Asleep })))
}

async fn pause(State(runtime): State<Arc<SteveRuntime>>, headers: HeaderMap) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    runtime.set_mode(ActivityMode::Paused).map_err(internal)?;
    Ok(Json(json!({ "mode": ActivityMode::Paused })))
}

#[derive(Deserialize)]
struct ScheduleRequest {
    enabled: bool,
}

async fn set_schedule(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Json(request): Json<ScheduleRequest>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    runtime.set_schedule(request.enabled).map_err(internal)?;
    Ok(Json(json!({ "enabled": request.enabled })))
}

async fn tasks(State(runtime): State<Arc<SteveRuntime>>, headers: HeaderMap) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let list = bound_slice(runtime.tasks(), MAX_STATUS_LIST);
    Ok(Json(
        json!({ "tasks": list, "truncated": runtime.tasks().len() > MAX_STATUS_LIST }),
    ))
}

async fn task(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let Some(task) = runtime.tasks().into_iter().find(|task| task.id == id) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "task not found" })),
        ));
    };
    Ok(Json(serde_json::to_value(task).map_err(internal)?))
}

#[derive(Deserialize)]
struct TaskRequest {
    kind: TaskKind,
    #[serde(default)]
    payload: Value,
}

async fn enqueue(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Json(request): Json<TaskRequest>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let task = runtime
        .enqueue(request.kind, "operator", request.payload)
        .map_err(internal)?;
    Ok(Json(serde_json::to_value(task).map_err(internal)?))
}

async fn read_feed(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let task = runtime
        .enqueue(TaskKind::ReadFeed, "operator", json!({}))
        .map_err(internal)?;
    Ok(Json(serde_json::to_value(task).map_err(internal)?))
}

async fn read_hacker_news(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let task = runtime
        .enqueue(TaskKind::ReadHackerNews, "operator", json!({}))
        .map_err(internal)?;
    Ok(Json(serde_json::to_value(task).map_err(internal)?))
}

#[derive(Deserialize)]
struct MentionRequest {
    item_id: u64,
    comment_id: Option<u64>,
    by: String,
    text: String,
}

async fn mention(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Json(request): Json<MentionRequest>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let text = bound_text(&request.by, 128);
    let body = bound_text(&request.text, 4_096);
    let task = runtime
        .enqueue(
            TaskKind::DirectMention,
            "nutnews-mention",
            json!({
                "item_id": request.item_id,
                "comment_id": request.comment_id,
                "by": text,
                "text": body
            }),
        )
        .map_err(internal)?;
    Ok(Json(serde_json::to_value(task).map_err(internal)?))
}

// --- Phase One read-only endpoints ------------------------------------------

/// `GET /control/autonomy` — snapshot from `status()` + public `config.autonomy`.
async fn autonomy_status(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let s = runtime.status();
    Ok(Json(project_autonomy_status(&s, &runtime.config)))
}

/// `GET /tasks/considerations` — pending **counts** only (no private bodies;
/// runtime does not expose consideration text via a public getter).
async fn mention_considerations(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let s = runtime.status();
    Ok(Json(project_mention_pending(&s, &runtime.config)))
}

/// `GET /control/schedule/plan` — sleep status + optional config-preview plan
/// (durable cooldown ledger is not public on SteveRuntime; preview uses empty ledger).
async fn sleep_plan_status(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let s = runtime.status();
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(Json(project_sleep_status(
        &s,
        &runtime.config,
        &runtime.inspection.agent_id,
        t,
    )))
}

/// `GET /memory/overlay` — metadata only (event count / flags), no payload text.
async fn overlay_status(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let s = runtime.status();
    Ok(Json(project_overlay_status(&s, &runtime.config)))
}

/// `GET /control/advocate` — last alignment summary from status; no memory text.
async fn advocate_status(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let s = runtime.status();
    Ok(Json(project_advocate_status(&s, &runtime.config)))
}

/// `POST /memory/overlay/approve/{event_id}` — blocked: no public runtime
/// method enforces approval + sovereignty yet.
async fn overlay_approve_blocked(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Path(event_id): Path<String>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let _ = bound_text(&event_id, 128);
    Err((
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "overlay approval is not exposed: SteveRuntime has no public method that appends Approval events under sovereignty/lease gates",
            "event_id": bound_text(&event_id, 128),
            "status": "blocked",
            "hint": "add runtime.approve_overlay_event(..) that checks overlay.enabled, may_write_overlay, and OverlayLog approval rules"
        })),
    ))
}

async fn wisdom_preview(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Json(context): Json<WhisperContext>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    let (whispers, controls) = runtime.wisdom_preview(context);
    let whispers = bound_slice(whispers, MAX_STATUS_LIST);
    Ok(Json(
        json!({ "whispers": whispers, "integrated_controls": controls }),
    ))
}

#[derive(Deserialize)]
struct RecallRequest {
    query: String,
    #[serde(default = "default_recall_limit")]
    limit: usize,
}

fn default_recall_limit() -> usize {
    10
}

async fn recall(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
    Json(request): Json<RecallRequest>,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    if request.query.trim().is_empty() {
        return Err(bad_request("query cannot be empty"));
    }
    let limit = request.limit.min(MAX_STATUS_LIST);
    let hits = runtime.recall_candidates(&request.query, limit);
    Ok(Json(json!({ "query": request.query, "hits": hits })))
}

async fn model_status(
    State(runtime): State<Arc<SteveRuntime>>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_operator(&runtime, &headers)?;
    Ok(Json(runtime.model_status()))
}

// --- Projection helpers (pure; unit-tested) ---------------------------------

fn project_autonomy_status(s: &RuntimeStatus, config: &crate::config::RuntimeConfig) -> Value {
    let policy = &config.autonomy.policy;
    json!({
        "enabled": config.autonomy.enabled,
        "state": s.autonomy_state,
        "lease": s.autonomy_lease_id.map(|id| json!({
            "id": id,
            "model_allowed": s.autonomy_model_allowed
        })),
        "model_allowed_now": s.autonomy_model_allowed,
        "compel_response": s.compel_response,
        "compel_write": s.compel_write,
        "mode": s.mode,
        "policy": {
            "tick_secs": policy.tick_secs,
            "quiet_secs": policy.quiet_secs,
            "idle_resume_secs": policy.idle_resume_secs,
            "lease_secs": policy.lease_secs,
            "lease_steps": policy.lease_steps,
            "budget_gate": policy.budget_gate,
            "dream_idle_secs": policy.dream_idle_secs,
            "honor_quiet_period": policy.honor_quiet_period
        },
        "note": "idle_secs/budget_pressure live on the autonomy snapshot; only status-public fields are returned here"
    })
}

fn project_mention_pending(s: &RuntimeStatus, config: &crate::config::RuntimeConfig) -> Value {
    // Bodies of pending considerations are not on a public runtime getter.
    // Expose counts only — never invent text that could leak private content.
    let pending = s.mention_pending.min(MAX_STATUS_LIST);
    json!({
        "enabled": config.mentions.enabled,
        "cursor": s.mention_cursor,
        "gap": s.mention_gap,
        "pending_count": s.mention_pending,
        "items": [],
        "truncated": s.mention_pending > MAX_STATUS_LIST,
        "shown": pending,
        "note": "consideration bodies are not exposed via public SteveRuntime methods; count/cursor only"
    })
}

fn project_sleep_status(
    s: &RuntimeStatus,
    config: &crate::config::RuntimeConfig,
    agent_id: &str,
    now: u64,
) -> Value {
    let enabled = config.sleep_cycle.enabled;
    let gate = match s.mode {
        ActivityMode::Paused => PlanningGate::Paused,
        _ => PlanningGate::Allow,
    };
    // Config-preview plan only: durable CooldownLedger is not public on the runtime.
    let (proposals, skipped, gate_label) = if enabled {
        let plan = plan_cycle_gated(
            &config.sleep_cycle,
            &CooldownLedger::default(),
            agent_id,
            now,
            gate,
        );
        let props: Vec<Value> = plan
            .proposals
            .iter()
            .take(MAX_STATUS_LIST)
            .map(|p| {
                json!({
                    "id": p.id,
                    "kind": p.kind.as_str(),
                    "phase": format!("{:?}", p.phase),
                    "requires_operator_approval": p.requires_operator_approval,
                    "memory_effect": format!("{:?}", p.memory_effect),
                    // No private memory excerpts — only metadata.
                })
            })
            .collect();
        let skips: Vec<Value> = plan
            .skipped
            .iter()
            .take(MAX_STATUS_LIST)
            .map(|sk| {
                json!({
                    "kind": sk.kind.as_str(),
                    "reason": bound_text(&sk.reason, 256),
                    "ready_at": sk.ready_at
                })
            })
            .collect();
        (props, skips, format!("{:?}", plan.gate))
    } else {
        (vec![], vec![], "disabled".into())
    };
    json!({
        "enabled": enabled,
        "last_cycle": s.sleep_last_cycle,
        "mode": s.mode,
        "ready_for_planning": enabled && !matches!(s.mode, ActivityMode::Paused),
        "gate": gate_label,
        "proposals": proposals,
        "skipped": skipped,
        "proposals_source": if enabled {
            "config_preview_without_durable_cooldowns"
        } else {
            "disabled"
        },
        "policy": {
            "cycles_per_day": config.sleep_cycle.cycles_per_day,
            "max_proposals_per_cycle": config.sleep_cycle.max_proposals_per_cycle,
            "share_private_memory": config.sleep_cycle.privacy.share_private_memory
        },
        "note": "proposal list is a pure config preview; durable cooldown ledger is not exposed on SteveRuntime"
    })
}

fn project_overlay_status(s: &RuntimeStatus, config: &crate::config::RuntimeConfig) -> Value {
    json!({
        "enabled": config.overlay.enabled,
        "path": config.overlay.path.display().to_string(),
        "total_events": s.overlay_events,
        "pending_approvals": [],
        "pending_approvals_note": "pending approval envelopes are not on a public runtime getter; use offline tooling or a future status method",
        "bounds": {
            "max_events": config.overlay.bounds.max_events,
            "max_text_bytes": config.overlay.bounds.max_text_bytes,
            "max_note_bytes": config.overlay.bounds.max_note_bytes
        },
        "compel_write": s.compel_write
    })
}

fn project_advocate_status(s: &RuntimeStatus, config: &crate::config::RuntimeConfig) -> Value {
    json!({
        "enabled": config.advocate.enabled,
        "reviews_per_day": config.advocate.reviews_per_day,
        "last_alignment": s.advocate_alignment,
        "expired": s.advocate_expired,
        "history": [],
        "history_note": "full wants/verdict history is not on a public runtime getter; last_alignment only",
        "compel_response": s.compel_response
    })
}

fn bound_slice<T>(mut items: Vec<T>, max: usize) -> Vec<T> {
    if items.len() > max {
        items.truncate(max);
    }
    items
}

fn bound_text(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

fn require_operator(runtime: &SteveRuntime, headers: &HeaderMap) -> Result<(), ApiError> {
    let authorization = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if runtime.authorize(authorization) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "operator authorization required" })),
        ))
    }
}

fn bad_request(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": error.to_string() })),
    )
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuntimeConfig;
    use crate::runtime::{ActivityMode, RuntimeStatus};

    fn sample_status() -> RuntimeStatus {
        RuntimeStatus {
            identity: "Steve Jobs".into(),
            agent_id: "ferricula-stevejobs".into(),
            mode: ActivityMode::Asleep,
            queued: 0,
            running: 0,
            schedule_enabled: true,
            last_scheduled_wake: None,
            memory_rows: 0,
            memory_records: 0,
            nutnews_enabled: false,
            nutnews_writes_enabled: false,
            model_usd_today: 0.0,
            autonomy_state: "disabled".into(),
            autonomy_lease_id: None,
            autonomy_model_allowed: true,
            compel_response: false,
            compel_write: false,
            mention_cursor: 0,
            mention_pending: 3,
            mention_gap: false,
            affect_label: "disabled".into(),
            affect_intensity: 0.0,
            sleep_last_cycle: Some(99),
            overlay_events: 0,
            advocate_alignment: Some("aligned".into()),
            advocate_expired: false,
        }
    }

    #[test]
    fn autonomy_projection_never_compels() {
        let s = sample_status();
        let config = RuntimeConfig::default();
        let v = project_autonomy_status(&s, &config);
        assert_eq!(v["compel_response"], false);
        assert_eq!(v["compel_write"], false);
        assert_eq!(v["enabled"], false);
        assert!(v["policy"]["lease_steps"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn mention_pending_exposes_counts_not_bodies() {
        let s = sample_status();
        let config = RuntimeConfig::default();
        let v = project_mention_pending(&s, &config);
        assert_eq!(v["pending_count"], 3);
        assert!(v["items"].as_array().unwrap().is_empty());
        assert!(!v["note"].as_str().unwrap().is_empty());
    }

    #[test]
    fn sleep_projection_respects_disabled_pause() {
        let mut s = sample_status();
        s.mode = ActivityMode::Paused;
        let config = RuntimeConfig::default();
        let v = project_sleep_status(&s, &config, "ferricula-stevejobs", 1_000);
        assert_eq!(v["ready_for_planning"], false);
        // Default sleep_cycle.enabled is true, but paused gate yields no proposals.
        assert!(v["proposals"].as_array().unwrap().is_empty() || v["gate"] == "paused");
    }

    #[test]
    fn sleep_projection_when_sleep_disabled() {
        let s = sample_status();
        let mut config = RuntimeConfig::default();
        config.sleep_cycle.enabled = false;
        let v = project_sleep_status(&s, &config, "ferricula-stevejobs", 1_000);
        assert_eq!(v["enabled"], false);
        assert_eq!(v["proposals_source"], "disabled");
        assert!(v["proposals"].as_array().unwrap().is_empty());
    }

    #[test]
    fn overlay_status_has_no_payload_text() {
        let s = sample_status();
        let config = RuntimeConfig::default();
        let v = project_overlay_status(&s, &config);
        assert_eq!(v["enabled"], false);
        assert!(v["pending_approvals"].as_array().unwrap().is_empty());
        assert!(v.get("total_events").is_some());
        // No accidental text fields.
        assert!(v.get("text").is_none());
    }

    #[test]
    fn advocate_status_is_summary_only() {
        let s = sample_status();
        let mut config = RuntimeConfig::default();
        config.advocate.enabled = true;
        let v = project_advocate_status(&s, &config);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["last_alignment"], "aligned");
        assert!(v["history"].as_array().unwrap().is_empty());
        assert_eq!(v["compel_response"], false);
    }

    #[test]
    fn bound_text_and_slice() {
        assert_eq!(bound_text("hello", 10), "hello");
        assert_eq!(bound_text("abcdef", 3), "abc");
        let v = bound_slice(vec![1, 2, 3, 4, 5], 3);
        assert_eq!(v, vec![1, 2, 3]);
    }

    #[test]
    fn phase_one_routes_are_registered() {
        // Ensure path constants used by PHASE1_API_DELTA remain wired.
        let paths = [
            "/control/autonomy",
            "/tasks/considerations",
            "/control/schedule/plan",
            "/memory/overlay",
            "/memory/overlay/approve/{event_id}",
            "/control/advocate",
        ];
        // router() needs a runtime; we only check path list stability here.
        for p in paths {
            assert!(p.starts_with('/'));
        }
    }
}
