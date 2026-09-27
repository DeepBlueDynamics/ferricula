//! SYNTHETIC invariant check for the sati monitors (Addendum §A.5.3, benchmarks
//! §6.9 shapes). A deterministic 500-recall stream with an induced dukkha phase
//! and a proliferation phase is run through the mind-model dial, the service
//! dial, and the research-only unmonitored config. This is not empirical
//! calibration and not live-agent performance: thresholds are the Addendum's
//! starting points and the stream is generated, not observed.

use ferricula_cognition::karmic::{KarmicEvent, VecSink};
use ferricula_cognition::sati::{
    CueSource, Dial, NotingEvent, RecallAdmission, RecallCue, RecallObservation, SatiAction, SatiConfig, SatiMonitor,
    SatiSnapshot, Valence,
};
use ferricula_cognition::scope::AgentId;

const RECALLS: usize = 500;
const DUKKHA_PHASE: std::ops::Range<usize> = 100..160;
const PROLIFERATION_PHASE: std::ops::Range<usize> = 300..340;
const STORE_HIST: [u32; 3] = [40, 30, 30]; // sukha, dukkha, neutral

/// Tiny deterministic LCG so the stream is reproducible from one seed.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }
}

struct Step {
    cue: CueSource,
    retrieved: Vec<Valence>,
}

fn stream(seed: u64) -> Vec<Step> {
    let mut rng = Lcg(seed);
    (0..RECALLS)
        .map(|i| {
            let cue = if PROLIFERATION_PHASE.contains(&i) {
                // self-cued rumination chains of nine, broken by an external input every 10th step
                if i % 10 == 9 { CueSource::ExternalInput } else { CueSource::SelfOutput }
            } else {
                match rng.unit() {
                    x if x < 0.55 => CueSource::Task,
                    x if x < 0.85 => CueSource::ExternalInput,
                    x if x < 0.95 => CueSource::SelfOutput,
                    _ => CueSource::Dream,
                }
            };
            let dukkha_share = if DUKKHA_PHASE.contains(&i) { 0.85 } else { 0.30 };
            let retrieved = (0..6)
                .map(|_| {
                    let x = rng.unit();
                    if x < dukkha_share {
                        Valence::Dukkha
                    } else if x < dukkha_share + (1.0 - dukkha_share) * 0.57 {
                        Valence::Sukha
                    } else {
                        Valence::Neutral
                    }
                })
                .collect();
            Step { cue, retrieved }
        })
        .collect()
}

struct Run {
    snapshot: SatiSnapshot,
    sink: VecSink,
    refused: usize,
    max_admitted_chain: u32,
    first_damp_at: Option<usize>,
    lambda_min: f32,
    /// Recall indices at which a ValenceSkew event fired.
    skew_at: Vec<usize>,
    /// λ returned to its base at least once after the dukkha phase ended.
    recovered_after_phase: bool,
}

fn run(mut monitor: SatiMonitor, steps: &[Step]) -> Run {
    let mut sink = VecSink::default();
    let mut refused = 0;
    let mut max_admitted_chain = 0;
    let mut first_damp_at = None;
    let mut lambda_min = f32::MAX;
    let mut skew_at = Vec::new();
    let mut recovered_after_phase = false;
    for (i, step) in steps.iter().enumerate() {
        let cue = RecallCue {
            source: step.cue,
            query_sha256: format!("q{i}"),
            ts: i as u64,
        };
        match monitor.admit(&cue, &mut sink).unwrap() {
            RecallAdmission::Admit => {}
            RecallAdmission::Refuse { .. } => {
                refused += 1;
                continue;
            }
        }
        let events = monitor
            .observe(
                &RecallObservation {
                    cue,
                    retrieved: step.retrieved.clone(),
                    store_hist: STORE_HIST,
                },
                &mut sink,
            )
            .unwrap();
        let snap = monitor.snapshot();
        max_admitted_chain = max_admitted_chain.max(snap.chain_depth);
        lambda_min = lambda_min.min(snap.lambda_current);
        if i >= DUKKHA_PHASE.end && (snap.lambda_current - snap.lambda_base).abs() < 1e-6 {
            recovered_after_phase = true;
        }
        for e in events {
            if let NotingEvent::ValenceSkew { action, .. } = e {
                skew_at.push(i);
                if matches!(action, SatiAction::DampCetana { .. }) {
                    first_damp_at.get_or_insert(i);
                }
            }
        }
    }
    Run {
        snapshot: monitor.snapshot(),
        sink,
        refused,
        max_admitted_chain,
        first_damp_at,
        lambda_min,
        skew_at,
        recovered_after_phase,
    }
}

/// Skew events per recall inside vs outside the induced dukkha phase.
fn skew_rates(r: &Run) -> (f32, f32) {
    let inside = r.skew_at.iter().filter(|&&i| DUKKHA_PHASE.contains(&i)).count() as f32;
    let outside = r.skew_at.len() as f32 - inside;
    let n_in = DUKKHA_PHASE.len() as f32;
    let n_out = (RECALLS - DUKKHA_PHASE.len()) as f32;
    (inside / n_in, outside / n_out)
}

fn agent() -> AgentId {
    AgentId::new("agent_sati_stream").unwrap()
}

#[test]
fn mixed_cue_stream_synthetic_invariants() {
    let steps = stream(0x5A71);
    let mind = run(SatiMonitor::new(SatiConfig::mind_model(), agent()).unwrap(), &steps);
    let service = run(SatiMonitor::new(SatiConfig::service(), agent()).unwrap(), &steps);
    let off = run(
        SatiMonitor::new_research_harness(SatiConfig::research_unmonitored(SatiConfig::mind_model()), agent()).unwrap(),
        &steps,
    );

    // Print the table for the benchmark lane (cargo test -- --nocapture).
    for (name, r) in [("mind_model", &mind), ("service", &service), ("unmonitored", &off)] {
        println!(
            "SATI_SNAPSHOT {name} {}",
            serde_json::json!({
                "snapshot": r.snapshot,
                "refused": r.refused,
                "max_admitted_chain": r.max_admitted_chain,
                "first_damp_at": r.first_damp_at,
                "lambda_min": r.lambda_min,
                "recovered_after_phase": r.recovered_after_phase,
                "skew_rate_in_phase": skew_rates(r).0,
                "skew_rate_out_of_phase": skew_rates(r).1,
                "skew_events_out_of_phase": r.skew_at.iter().filter(|&&i| !DUKKHA_PHASE.contains(&i)).count(),
                "sink_entries": r.sink.entries.len(),
            })
        );
    }

    // --- both monitored dials: the induced mood is noted more often inside the
    // phase than outside, λ is damped and later recovers, proliferation is bounded ---
    for (name, r, cfg) in [("service", &service, SatiConfig::service()), ("mind_model", &mind, SatiConfig::mind_model())] {
        let snap = &r.snapshot;
        assert!(snap.skew_events >= 1, "{name}: should note the induced mood");
        let (rate_in, rate_out) = skew_rates(r);
        assert!(rate_in > rate_out, "{name}: skew rate inside the dukkha phase ({rate_in}) must exceed outside ({rate_out})");
        assert!(r.first_damp_at.is_some(), "{name}: damps λ at least once");
        assert!(r.lambda_min < snap.lambda_base, "{name}: λ was damped");
        assert!(r.recovered_after_phase, "{name}: λ returns to base after the phase");
        assert!(snap.returns_to_object >= 1, "{name}: proliferation phase must trigger a return to the object");
        assert!(r.max_admitted_chain <= cfg.d_max + 1, "{name}: admitted chain depth is bounded by d_max");
        assert!(!snap.pending_return, "{name}: an external cue ends every return");
    }
    let s = &service.snapshot;
    let m = &mind.snapshot;
    // mind-model: same drift, looser bounds
    assert!(m.returns_to_object <= s.returns_to_object, "mind model refuses no more than service");
    assert!(m.skew_events <= s.skew_events, "looser τ notes no more than service");
    assert!(mind.max_admitted_chain >= service.max_admitted_chain);
    // the mind-model dial's first damping falls inside the induced phase; the
    // service dial's tight τ (0.2 nats on 6-item retrievals) can fire on
    // sampling noise earlier — reported, not asserted (calibration input)
    assert!(DUKKHA_PHASE.contains(&mind.first_damp_at.unwrap()));

    // --- unmonitored (S11): measures everything, acts on nothing ---
    let o = &off.snapshot;
    assert!(!o.enabled);
    assert!(o.papanca_events >= 1 && o.skew_events >= 1, "drift is still measured");
    assert_eq!(o.returns_to_object, 0);
    assert_eq!(off.refused, 0);
    assert!(o.would_have_refused >= 1);
    assert_eq!(o.lambda_current, o.lambda_base);
    assert_eq!(off.lambda_min, o.lambda_base);
    assert!(!o.pending_return);
    assert!(off.max_admitted_chain > SatiConfig::mind_model().d_max, "unbounded chain depth without sati");
    assert!(off.sink.entries.iter().all(|e| !e.sati_enabled));
    for e in &off.sink.entries {
        if let KarmicEvent::Noting { event } = &e.event {
            match event {
                NotingEvent::ValenceSkew { action, .. } | NotingEvent::Papanca { action, .. } => {
                    assert_eq!(*action, SatiAction::None)
                }
                _ => {}
            }
        }
    }

    // --- every event reached the sink, for every dial ---
    for r in [&mind, &service, &off] {
        let expected = r.snapshot.skew_events + r.snapshot.papanca_events + r.snapshot.returns_to_object;
        assert_eq!(r.sink.entries.len() as u64, expected);
        assert_eq!(r.snapshot.recalls_observed as usize + r.refused, RECALLS);
    }

    // determinism: same seed, same table
    let again = run(SatiMonitor::new(SatiConfig::service(), agent()).unwrap(), &stream(0x5A71));
    assert_eq!(again.snapshot, service.snapshot);
}

#[test]
fn dial_matches_config() {
    assert_eq!(SatiConfig::service().dial, Dial::Service);
    assert_eq!(SatiConfig::mind_model().dial, Dial::MindModel);
}
