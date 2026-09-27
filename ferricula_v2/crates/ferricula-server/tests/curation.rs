use ferricula_cognition::curator::{CurationRequest, MemoryKind, RawMemory};
use ferricula_cognition::outcome::Pool;
use ferricula_cognition::sati::{CueSource, RecallCue};
use ferricula_cognition::scope::AgentId;
use ferricula_core::LifecycleState;
use ferricula_server::curation::curate_with_router;
use ferricula_server::model::{ModelRouter, ProviderResponse, RecordingTransport};
use serde_json::json;

fn request() -> CurationRequest {
    let agent = AgentId::new("a").unwrap();
    CurationRequest {
        agent: agent.clone(), task: "How should this deployment be checked?".into(),
        cue: RecallCue { source: CueSource::Task, query_sha256: "task-sha".into(), ts: 1 },
        candidates: vec![RawMemory {
            id: 7, agent, text: "Run tests and inspect the exit status before claiming completion.".into(),
            oja: 1.0, state: LifecycleState::Active, vedana: None,
            pool: Pool::Success, kind: MemoryKind::Raw, sealed: false, keystone: false,
            retrieval_score: 1.0, sati_recall: None,
        }],
        k: 3, include_failures: false,
    }
}

#[test]
fn briefing_is_ephemeral_and_receipt_contains_no_source_or_generated_text() {
    let router = ModelRouter::with_defaults().unwrap();
    let transport = RecordingTransport::default();
    transport.script("gemma4:e2b", ProviderResponse {
        text: "Check the test exit status before reporting success.".into(),
        input_tokens: 100, output_tokens: 10,
        raw: json!({"usage":{"prompt_tokens":100,"completion_tokens":10}}),
    });
    let req = request();
    let original = req.candidates[0].text.clone();
    let briefing = curate_with_router(&req, &router, &transport, 0.0).unwrap();
    assert!(briefing.render().contains("test exit status"));
    assert_eq!(req.candidates[0].text, original);
    let receipt = serde_json::to_string(&briefing.receipt()).unwrap();
    assert!(!receipt.contains(&original));
    assert!(!receipt.contains(&briefing.guidance));
    assert_eq!(router.ledger().entries().len(), 1);
    assert_eq!(transport.calls.lock().unwrap().len(), 1);
}

#[test]
fn wrong_agent_is_rejected_before_any_model_call() {
    let router = ModelRouter::with_defaults().unwrap();
    let transport = RecordingTransport::default();
    let mut req = request();
    req.candidates[0].agent = AgentId::new("other").unwrap();
    assert!(curate_with_router(&req, &router, &transport, 0.0).is_err());
    assert!(transport.calls.lock().unwrap().is_empty());
}

#[test]
fn released_sealed_and_failed_records_do_not_reach_provider() {
    for mode in 0..3 {
        let router = ModelRouter::with_defaults().unwrap();
        let transport = RecordingTransport::default();
        let mut req = request();
        match mode {
            0 => req.candidates[0].state = LifecycleState::Forgiven,
            1 => req.candidates[0].sealed = true,
            _ => req.candidates[0].pool = Pool::Failure,
        }
        let briefing = curate_with_router(&req, &router, &transport, 0.0).unwrap();
        assert!(briefing.used.is_empty());
        assert!(transport.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn truncated_generation_is_not_used_as_a_complete_briefing() {
    let router = ModelRouter::with_defaults().unwrap();
    let transport = RecordingTransport::default();
    transport.script("gemma4:e2b", ProviderResponse {
        text: "Partial advice".into(), input_tokens: 100, output_tokens: 512,
        raw: json!({"usage":{"prompt_tokens":100,"completion_tokens":512},
                    "choices":[{"finish_reason":"length"}]}),
    });
    assert!(curate_with_router(&request(), &router, &transport, 0.0).is_err());
    assert_eq!(router.ledger().entries().len(), 1);
}

#[test]
fn oversized_source_is_not_silently_truncated_or_sent() {
    let router = ModelRouter::with_defaults().unwrap();
    let transport = RecordingTransport::default();
    let mut req = request();
    req.candidates[0].text = "x".repeat(20000);
    assert!(curate_with_router(&req, &router, &transport, 0.0).is_err());
    assert_eq!(req.candidates[0].text.len(), 20000);
    assert!(transport.calls.lock().unwrap().is_empty());
}
