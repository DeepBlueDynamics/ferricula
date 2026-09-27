//! Independent review tests for the operator chat surface (chat.rs).
//!
//! Read-only vs the chat implementation — do NOT edit `src/chat.rs`. This file
//! only exercises the public surface the crate exports
//! (`ChatRequest`, `ChatTurn`, `InputOrigin`) plus behavior reachable without
//! touching internals. Runtime-lifecycle defects (cancellation reconciliation,
//! failed-retry terminality, provider-error masking) cannot be unit-tested here
//! because `ChatStore` / `SteveRuntime::converse` are private; those are listed
//! as concrete findings with proposed fixes + live-Docker acceptance cases.
//!
//! No host run by me; tests compile only (queued via Coordinator -> Piranha).

use ferricula_server::runtime::{ChatRequest, ChatTurn, InputOrigin};

fn req() -> ChatRequest {
    ChatRequest {
        request_id: uuid::Uuid::new_v4(),
        conversation_id: uuid::Uuid::new_v4(),
        message: "Plastic clatter; limited search found nothing.".into(),
        reported_origin: InputOrigin::Scheduler,
    }
}

// ---------------------------------------------------------------------------
// 1. Input bounds (validation contract)
// ---------------------------------------------------------------------------

#[test]
fn empty_message_rejected() {
    let mut r = req();
    r.message = "   ".into();
    assert!(r.validate().is_err(), "blank message must be rejected");
}

#[test]
fn oversized_message_rejected() {
    let mut r = req();
    r.message = "x".repeat(8193);
    assert!(r.validate().is_err(), "> 8192 bytes must be rejected");
}

#[test]
fn max_size_message_accepted() {
    let mut r = req();
    r.message = "x".repeat(8192);
    assert!(r.validate().is_ok(), "8192 bytes must be accepted at the boundary");
}

#[test]
fn unknown_fields_rejected_by_serde() {
    // deny_unknown_fields: a caller cannot smuggle envelope fields through.
    let json = format!(
        r#"{{"request_id":"{}","conversation_id":"{}","message":"hi","reported_origin":"human","origin_verified":true}}"#,
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    );
    let res: Result<ChatRequest, _> = serde_json::from_str(&json);
    assert!(res.is_err(), "origin_verified must not be a settable request field");
}

// ---------------------------------------------------------------------------
// 2. Origin attribution round-trip (caller claim, distinct variants)
// ---------------------------------------------------------------------------

#[test]
fn origin_variants_round_trip() {
    for (s, expected) in [
        ("human", InputOrigin::Human),
        ("agent", InputOrigin::Agent),
        ("scheduler", InputOrigin::Scheduler),
        ("tool", InputOrigin::Tool),
        ("unknown", InputOrigin::Unknown),
    ] {
        let v: InputOrigin = serde_json::from_value(serde_json::json!(s)).unwrap();
        assert_eq!(v, expected, "parse {s}");
        let back: serde_json::Value = serde_json::to_value(&v).unwrap();
        assert_eq!(back, serde_json::json!(s), "serialize {s}");
    }
}

#[test]
fn turn_retains_reported_origin_and_can_be_failed() {
    let mut t = ChatTurn {
        request: req(),
        received_at: 1,
        completed_at: None,
        status: "pending".into(),
        reply: None,
        model: None,
        memory_candidates: serde_json::json!([]),
        episode_candidates: serde_json::json!([]),
        evidence_cards: serde_json::json!([]),
        error: None,
    };
    t.status = "failed".into();
    t.error = Some("no eligible local private-context model".into());
    let json = serde_json::to_string(&t).unwrap();
    let back: ChatTurn = serde_json::from_str(&json).unwrap();
    assert_eq!(back.status, "failed");
    assert_eq!(back.request.reported_origin, InputOrigin::Scheduler);
    assert_eq!(back.request.request_id, t.request.request_id);
    assert!(back.completed_at.is_none(), "failed turn keeps completed_at None only if not set");
}

// ---------------------------------------------------------------------------
// 3. Findings that need runtime/live acceptance (cannot unit-test privately)
//
// F-C1 (cancellation reconcile): `converse` saves a "pending" turn, then awaits
//   `spawn_blocking` inference. A dropped HTTP request cancels the outer future
//   but NOT the blocking task (spawn_blocking keeps running; its result is
//   never observed). The "pending" record is never reconciled to failed/
//   interrupted until a restart, and the admission semaphore is held until the
//   detached inference finishes. Proposed fix: attach a cancellable watchdog /
//   oneshot that, on request drop, re-locks and marks the turn "interrupted"
//   (and releases the permit immediately). Acceptance: start inference, drop
//   the client, GET /chat/{id} within N seconds -> status != "pending".
//
// F-C2 (failed retry is terminal): a turn whose inference fails is saved as
//   "failed" *before* the caller can retry; a subsequent request with the SAME
//   request_id returns the failed clone (idempotency treats failure as
//   replay-answer), so a transient model outage cannot be retried under the
//   same id — the caller must mint a new id. Proposed fix: document failed as
//   terminal, or return the "different input / reissue" error on retry of a
//   failed id, or allow forced re-inference. Acceptance: force a failing model,
//   POST twice with same id -> second returns the failed turn, not a retry.
//
// F-C3 (provider error masked): the durable `turn.error` is a fixed generic
//   string; the actual provider error (URL/body/cause) is dropped. Good for
//   no-secret hygiene, but diagnostics for a misconfigured model are lost and
//   a bug in `complete_with_budget` is invisible. Proposed fix: keep the generic
//   durable value but log the cause in the runtime trace. Acceptance: enable a
//   broken route, confirm the generic error is durable and the cause is in
//   server logs, never in the JSON response.
//
// F-C4 (context estimate is a rough /3): `estimated` = bytes/3, metadata is
//   truncated at 16 KB, history holds up to 8 turns each with a 768-token reply;
//   if a small local model has a tighter context the estimate can be low and the
//   router's ContextTooSmall check or the provider truncates/errors, surfacing
//   only as F-C3's generic failure. Proposed fix: cap aggregate history by a
//   token budget (not fixed 8 turns) and compute the estimate from the same
//   budget the router enforces. Acceptance: 8 turns of 8 KB messages against a
//   small-context local model -> either a clean ContextTooSmall, never a silent
//   truncation or a misleading generic error.
// ---------------------------------------------------------------------------
