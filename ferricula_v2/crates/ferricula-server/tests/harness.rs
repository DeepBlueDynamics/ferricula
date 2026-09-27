use ferricula_server::harness::*;
use ferricula_server::model::{ModelRouter, ProviderResponse, RecordingTransport};
use ferricula_server::model_config::{ModelCapability, ModelRoutingConfig, RouteStep, TaskClass};
use serde_json::json;

fn router() -> ModelRouter {
    let mut config = ModelRoutingConfig::steve_safe_defaults();
    for (id, model, class) in [
        ("generator", "small-llm", TaskClass::Summarize),
        ("judge", "small-decision", TaskClass::Scan),
        ("escalation", "larger-decision", TaskClass::Deliberate),
    ] {
        let mut profile = config.profile("local_ollama").unwrap().clone();
        profile.id = id.into();
        profile.model = model.into();
        profile.context_tokens = 65536;
        config.profiles.push(profile);
        config.routes.iter_mut().find(|r| r.task_class == class).unwrap().steps =
            vec![
                RouteStep { profile_id: id.into(), daily_budget_usd: 0.0 },
                RouteStep { profile_id: "no_model".into(), daily_budget_usd: 0.0 },
            ];
    }
    ModelRouter::new(config).unwrap()
}

fn response(text: &str) -> ProviderResponse {
    ProviderResponse {
        text: text.into(), input_tokens: 17, output_tokens: 9,
        raw: json!({"usage":{"prompt_tokens":17,"completion_tokens":9}}),
    }
}

fn task() -> HarnessTask {
    HarnessTask {
        task: "Report the test outcome.".into(),
        evidence: vec![Evidence { id: "test-1".into(), text: "cargo test exited 0.".into() }],
    }
}

fn backend<'a>(router: &'a ModelRouter, transport: &'a RecordingTransport, task_class: TaskClass)
    -> RoutedBackend<'a>
{
    RoutedBackend { router, transport, task_class, daily_budget_usd: 0.0, private_context: true }
}

fn execute(router: &ModelRouter, transport: &RecordingTransport, escalate: bool)
    -> anyhow::Result<HarnessResult>
{
    let generator = backend(router, transport, TaskClass::Summarize);
    let judge = backend(router, transport, TaskClass::Scan);
    let larger = backend(router, transport, TaskClass::Deliberate);
    run_task(&task(), &generator, &judge,
        if escalate { Some(&larger as &dyn DecisionBackend) } else { None },
        &HarnessLimits::default())
}

#[test]
fn small_models_complete_without_escalation_and_report_actual_usage() {
    let router = router();
    let transport = RecordingTransport::default();
    transport.script("small-llm", response("The tests passed."));
    transport.script("small-decision", response(
        r#"{"verdict":"accept","confidence":0.95,"evidence_ids":["test-1"]}"#));
    let result = execute(&router, &transport, true).unwrap();
    assert_eq!(result.decision.verdict, Verdict::Accept);
    assert!(!result.escalated);
    assert_eq!(result.calls.len(), 2);
    assert_eq!(result.calls[0].model, "small-llm");
    assert_eq!(result.calls[1].model, "small-decision");
    assert_eq!(result.calls[0].input_tokens, Some(17));
    assert_eq!(result.calls[1].output_tokens, Some(9));
    assert_eq!(router.ledger().entries().len(), 2);
}

#[test]
fn invalid_json_escalates_once_and_both_judgments_are_accounted() {
    let router = router();
    let transport = RecordingTransport::default();
    transport.script("small-llm", response("The tests passed."));
    transport.script("small-decision", response("sure, accepted!"));
    transport.script("larger-decision", response(
        r#"{"verdict":"reject","confidence":0.97,"evidence_ids":["test-1"]}"#));
    let result = execute(&router, &transport, true).unwrap();
    assert_eq!(result.decision.verdict, Verdict::Reject);
    assert!(result.escalated);
    assert_eq!(result.calls.len(), 3);
    assert_eq!(router.ledger().entries().len(), 3);
    assert_eq!(transport.calls.lock().unwrap().len(), 3);
}

#[test]
fn uncertainty_after_escalation_stops_with_abstention() {
    let router = router();
    let transport = RecordingTransport::default();
    transport.script("small-llm", response("Maybe the tests passed."));
    transport.script("small-decision", response(
        r#"{"verdict":"accept","confidence":0.4,"evidence_ids":["test-1"]}"#));
    transport.script("larger-decision", response(
        r#"{"verdict":"accept","confidence":0.5,"evidence_ids":["test-1"]}"#));
    let result = execute(&router, &transport, true).unwrap();
    assert_eq!(result.decision.verdict, Verdict::Abstain);
    assert_eq!(transport.calls.lock().unwrap().len(), 3);
}

#[test]
fn invented_evidence_missing_evidence_and_invalid_confidence_cannot_accept() {
    for judgment in [
        r#"{"verdict":"accept","confidence":0.99,"evidence_ids":["invented"]}"#,
        r#"{"verdict":"accept","confidence":0.99,"evidence_ids":[]}"#,
        r#"{"verdict":"accept","confidence":1.1,"evidence_ids":["test-1"]}"#,
        r#"{"verdict":"accept","confidence":null,"evidence_ids":["test-1"]}"#,
        r#"{"verdict":"accept","confidence":0.99,"evidence_ids":["test-1"],"extra":true}"#,
    ] {
        let router = router();
        let transport = RecordingTransport::default();
        transport.script("small-llm", response("I say I succeeded."));
        transport.script("small-decision", response(judgment));
        let result = execute(&router, &transport, false).unwrap();
        assert_eq!(result.decision.verdict, Verdict::Abstain, "{judgment}");
        assert_eq!(result.calls.len(), 2);
    }
}

#[test]
fn unreported_paid_usage_is_reserved_and_blocks_repeated_calls_after_restart() {
    use std::time::SystemTime;
    use ferricula_server::model::{EscalationReason, InferenceRequest};
    let original = router();
    let mut config = original.config().clone();
    let profile = config.profiles.iter_mut().find(|p| p.id == "generator").unwrap();
    profile.cost.input_per_mtok = 1.0;
    profile.cost.output_per_mtok = 2.0;
    config.routes.iter_mut().find(|r| r.task_class == TaskClass::Summarize).unwrap()
        .steps[0].daily_budget_usd = 1.0;
    let router = ModelRouter::new(config.clone()).unwrap();
    let transport = RecordingTransport::default();
    transport.script("small-llm", ProviderResponse {
        text: "candidate".into(), input_tokens: 0, output_tokens: 0, raw: json!({}),
    });
    let mut request = InferenceRequest::new(TaskClass::Summarize, "task").with_tokens(100, 20);
    request.max_tokens = Some(30);
    let now = SystemTime::now();
    let (decision, _) = router.complete_with_budget(&request, &transport, now, 1.0).unwrap();
    assert!(!decision.no_model);
    let entries = router.ledger().entries();
    assert_eq!(entries[0].input_tokens, 100);
    assert_eq!(entries[0].output_tokens, 30);
    assert!(entries[0].usage_estimated);
    assert!(entries[0].cost_usd > 0.0);
    let persisted = serde_json::to_vec(&entries).unwrap();
    let restored = ModelRouter::with_usage(config, serde_json::from_slice(&persisted).unwrap()).unwrap();
    let decision = restored.select(&request, now).unwrap();
    assert!(decision.no_model);
    assert!(decision.skipped.iter().any(|reason| matches!(
        reason, EscalationReason::UsageUnreported { profile_id } if profile_id == "generator"
    )));
    assert_eq!(transport.calls.lock().unwrap().len(), 1);
}

#[test]
fn missing_provider_usage_is_unknown_not_free() {
    let router = router();
    let transport = RecordingTransport::default();
    let mut generated = response("The tests passed.");
    generated.raw = json!({});
    transport.script("small-llm", generated);
    transport.script("small-decision", response(
        r#"{"verdict":"accept","confidence":0.9,"evidence_ids":["test-1"]}"#));
    let result = execute(&router, &transport, false).unwrap();
    assert_eq!(result.calls[0].input_tokens, None);
    assert_eq!(result.calls[0].output_tokens, None);
    assert_eq!(result.calls[0].estimated_cost_usd, None);
}

#[test]
fn invalid_inputs_are_rejected_before_provider_calls() {
    let router = router();
    let transport = RecordingTransport::default();
    let generator = backend(&router, &transport, TaskClass::Summarize);
    let judge = backend(&router, &transport, TaskClass::Scan);
    let mut duplicate = task();
    duplicate.evidence.push(duplicate.evidence[0].clone());
    assert!(run_task(&duplicate, &generator, &judge, None, &HarnessLimits::default()).is_err());
    let mut limits = HarnessLimits::default();
    limits.max_input_bytes = 1;
    assert!(run_task(&task(), &generator, &judge, None, &limits).is_err());
    limits.max_input_bytes = 32768;
    limits.minimum_confidence = f64::NAN;
    assert!(run_task(&task(), &generator, &judge, None, &limits).is_err());
    assert!(transport.calls.lock().unwrap().is_empty());
}

#[test]
fn no_model_echo_is_never_a_generated_answer_or_judgment() {
    let router = router();
    let transport = RecordingTransport::default();
    let generator = backend(&router, &transport, TaskClass::Mechanical);
    let judge = backend(&router, &transport, TaskClass::Scan);
    assert!(run_task(&task(), &generator, &judge, None, &HarnessLimits::default()).is_err());
}

#[test]
fn private_evidence_never_reaches_unapproved_profile() {
    let original = router();
    let mut config = original.config().clone();
    config.profiles.iter_mut().find(|p| p.id == "generator").unwrap()
        .capabilities.retain(|c| *c != ModelCapability::PrivateContext);
    let router = ModelRouter::new(config).unwrap();
    let transport = RecordingTransport::default();
    assert!(execute(&router, &transport, false).is_err());
    // The router can return its explicit no-model sentinel, but no remote
    // model sees the task or evidence.
    assert!(transport.calls.lock().unwrap().iter().all(|request|
        matches!(request, ferricula_server::model::ProviderRequest::NoModel { .. })));
}
