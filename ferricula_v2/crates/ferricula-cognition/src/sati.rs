//! Mindfulness layer (Addendum §A.5): the drift is modelled, measured and
//! bounded. "The system can drift, and it always knows when it is drifting."
//!
//! Three monitors from §A.5.3:
//! 2. cetanā-modulated recall — valence skew `KL(retrieved ‖ store)` on every
//!    recall; above `tau_skew` for `n_consecutive` recalls → noting event and
//!    temporary damping of `lambda_cetana`.
//! 3. papañca detector — chain depth of self-cued recalls and self-reference
//!    ratio over a window; beyond `d_max` / `rho_max` → return to the object:
//!    the next recall must be cued by the task or an external input.
//! 4. self-judgment gap — `self − evidence`, surfaced, never auto-corrected.
//!
//! Rule 10: every drift mechanism ships with its monitor enabled. Runtime
//! construction rejects `enabled = false`; the research harness (benchmark
//! S11) uses [`SatiMonitor::new_research_harness`] and every entry it emits
//! carries `sati_enabled = false`. With monitoring off the measurements and
//! events are still produced — only the actions are withheld.
//!
//! All numeric defaults are the Addendum's "starting points, not measurements".

use std::collections::VecDeque;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::karmic::{KarmicEntry, KarmicEvent, KarmicSink};
use crate::scope::AgentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dial {
    /// Research: drift expressed and studied.
    MindModel,
    /// Production: drift present but damped; act early.
    Service,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SatiConfig {
    /// Always true at runtime. See [`SatiConfig::research_unmonitored`].
    pub enabled: bool,
    pub dial: Dial,
    /// Mood bias strength λ_cetana ∈ [0, 1].
    pub lambda_cetana: f32,
    /// Skew threshold in nats.
    pub tau_skew: f32,
    /// Consecutive over-threshold recalls before a noting event.
    pub n_consecutive: usize,
    /// Max chain depth of self-cued recalls.
    pub d_max: u32,
    /// Max self-reference ratio over the window.
    pub rho_max: f32,
    /// Window length (recalls) for the self-reference ratio. The ratio bound
    /// is evaluated only once the window is full.
    pub window: usize,
    /// Fraction of λ removed on each damping action.
    pub damp_factor: f32,
    /// Fraction of the base λ restored per calm recall.
    pub recover_step: f32,
}

impl SatiConfig {
    /// §A.5.4 mind-model column: λ 0.3–0.6, d_max 5–8, τ loose.
    pub fn mind_model() -> Self {
        Self {
            enabled: true,
            dial: Dial::MindModel,
            lambda_cetana: 0.45,
            tau_skew: 0.5,
            n_consecutive: 5,
            d_max: 6,
            rho_max: 0.6,
            window: 20,
            damp_factor: 0.5,
            recover_step: 0.1,
        }
    }

    /// §A.5.4 service column: λ 0.0–0.1, d_max 2, τ tight.
    pub fn service() -> Self {
        Self {
            enabled: true,
            dial: Dial::Service,
            lambda_cetana: 0.05,
            tau_skew: 0.2,
            n_consecutive: 3,
            d_max: 2,
            rho_max: 0.4,
            window: 20,
            damp_factor: 0.5,
            recover_step: 0.1,
        }
    }

    /// Benchmark S11 only ("never deployed", Addendum Rule 10). The only way
    /// to obtain `enabled = false`. [`SatiMonitor::new`] rejects it.
    pub fn research_unmonitored(base: Self) -> Self {
        Self {
            enabled: false,
            ..base
        }
    }

    fn validate(&self) -> Result<(), SatiError> {
        let in_unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if !in_unit(self.lambda_cetana) || !in_unit(self.rho_max) || !in_unit(self.damp_factor) || !in_unit(self.recover_step) {
            return Err(SatiError::InvalidConfig("lambda_cetana, rho_max, damp_factor, recover_step must be within [0,1]"));
        }
        if !self.tau_skew.is_finite() || self.tau_skew < 0.0 {
            return Err(SatiError::InvalidConfig("tau_skew must be finite and non-negative"));
        }
        if self.n_consecutive == 0 || self.window == 0 {
            return Err(SatiError::InvalidConfig("n_consecutive and window must be positive"));
        }
        Ok(())
    }
}

impl Default for SatiConfig {
    fn default() -> Self {
        Self::service()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SatiError {
    /// Rule 10. Runtime may not run drift without its monitor.
    UnmonitoredNotAllowedAtRuntime,
    InvalidConfig(&'static str),
}

impl fmt::Display for SatiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SatiError::UnmonitoredNotAllowedAtRuntime => {
                write!(f, "sati monitors disabled: not allowed at runtime (Addendum Rule 10)")
            }
            SatiError::InvalidConfig(msg) => write!(f, "invalid sati config: {msg}"),
        }
    }
}

impl std::error::Error for SatiError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CueSource {
    ExternalInput,
    Task,
    /// The query came from the agent's own prior output.
    SelfOutput,
    Dream,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallCue {
    pub source: CueSource,
    pub query_sha256: String,
    pub ts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Valence {
    Sukha,
    Dukkha,
    Neutral,
}

impl Valence {
    fn index(self) -> usize {
        match self {
            Valence::Sukha => 0,
            Valence::Dukkha => 1,
            Valence::Neutral => 2,
        }
    }
}

/// What a recall retrieved, plus the store's valence histogram
/// `[sukha, dukkha, neutral]` at that moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallObservation {
    pub cue: RecallCue,
    pub retrieved: Vec<Valence>,
    pub store_hist: [u32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SatiAction {
    /// Monitoring off (research) or below bound.
    None,
    DampCetana { from: f32, to: f32 },
    ReturnToObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "noting", rename_all = "snake_case")]
pub enum NotingEvent {
    ValenceSkew {
        skew_kl: f32,
        retrieved_dukkha: f32,
        store_dukkha: f32,
        streak: usize,
        action: SatiAction,
    },
    Papanca {
        chain_depth: u32,
        self_ref_ratio: f32,
        action: SatiAction,
    },
    SelfJudgmentGap {
        self_verdict: f32,
        evidence_verdict: f32,
        running_mean_gap: f32,
    },
    /// A self-cued recall was refused because a return to the object was pending.
    ReturnedToObject {
        refused_query_sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RecallAdmission {
    Admit,
    /// Return to the object: the next recall must be cued by the task or an
    /// external input (§A.5.3(3)).
    Refuse { reason: String },
}

/// Metrics for dashboards and benchmarks §6.9.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SatiSnapshot {
    pub enabled: bool,
    pub dial: Dial,
    pub lambda_base: f32,
    pub lambda_current: f32,
    pub skew_streak: usize,
    pub chain_depth: u32,
    pub self_ref_ratio: f32,
    pub pending_return: bool,
    pub recalls_observed: u64,
    pub skew_events: u64,
    pub papanca_events: u64,
    pub returns_to_object: u64,
    /// Refusals that would have happened had monitoring been enabled.
    pub would_have_refused: u64,
    pub gap_samples: u64,
    pub gap_mean: f32,
    pub last_skew_kl: f32,
}

pub struct SatiMonitor {
    cfg: SatiConfig,
    agent: AgentId,
    lambda: f32,
    skew_streak: usize,
    chain_depth: u32,
    window: VecDeque<bool>,
    pending_return: bool,
    /// Unmonitored only: where a return to the object WOULD be pending.
    shadow_return: bool,
    recalls_observed: u64,
    skew_events: u64,
    papanca_events: u64,
    returns_to_object: u64,
    would_have_refused: u64,
    gap_sum: f32,
    gap_n: u64,
    last_skew_kl: f32,
}

impl SatiMonitor {
    /// Runtime constructor. Rejects `enabled = false` (Rule 10).
    pub fn new(cfg: SatiConfig, agent: AgentId) -> Result<Self, SatiError> {
        if !cfg.enabled {
            return Err(SatiError::UnmonitoredNotAllowedAtRuntime);
        }
        cfg.validate()?;
        Ok(Self::build(cfg, agent))
    }

    /// Research harness constructor (benchmark S11). Accepts any valid config.
    pub fn new_research_harness(cfg: SatiConfig, agent: AgentId) -> Result<Self, SatiError> {
        cfg.validate()?;
        Ok(Self::build(cfg, agent))
    }

    fn build(cfg: SatiConfig, agent: AgentId) -> Self {
        Self {
            lambda: cfg.lambda_cetana,
            cfg,
            agent,
            skew_streak: 0,
            chain_depth: 0,
            window: VecDeque::new(),
            pending_return: false,
            shadow_return: false,
            recalls_observed: 0,
            skew_events: 0,
            papanca_events: 0,
            returns_to_object: 0,
            would_have_refused: 0,
            gap_sum: 0.0,
            gap_n: 0,
            last_skew_kl: 0.0,
        }
    }

    pub fn config(&self) -> &SatiConfig {
        &self.cfg
    }

    /// Current (possibly damped) mood-bias strength for the cetanā layer.
    pub fn lambda_cetana(&self) -> f32 {
        self.lambda
    }

    /// Call BEFORE a recall. Enforces a pending return to the object.
    pub fn admit(&mut self, cue: &RecallCue, sink: &mut dyn KarmicSink) -> anyhow::Result<RecallAdmission> {
        let self_cued = cue.source == CueSource::SelfOutput;
        if self.pending_return {
            if self_cued {
                self.returns_to_object += 1;
                self.log(
                    sink,
                    NotingEvent::ReturnedToObject {
                        refused_query_sha256: cue.query_sha256.clone(),
                    },
                )?;
                return Ok(RecallAdmission::Refuse {
                    reason: "return to the object: next recall must be cued by the task or an external input".into(),
                });
            }
            // An external or task cue satisfies the return.
            self.pending_return = false;
            self.chain_depth = 0;
        }
        if self.shadow_return {
            if self_cued {
                // Unmonitored: count what sati would have refused, admit anyway.
                self.would_have_refused += 1;
            } else {
                self.shadow_return = false;
            }
        }
        Ok(RecallAdmission::Admit)
    }

    /// Call AFTER a recall with what it retrieved. Emits noting events to the
    /// sink and returns them. A sink failure aborts with an error.
    pub fn observe(&mut self, obs: &RecallObservation, sink: &mut dyn KarmicSink) -> anyhow::Result<Vec<NotingEvent>> {
        self.recalls_observed += 1;
        let mut events = Vec::new();

        // --- papañca: chain depth and self-reference ratio ---
        let self_cued = obs.cue.source == CueSource::SelfOutput;
        if self_cued {
            self.chain_depth += 1;
        } else {
            self.chain_depth = 0;
        }
        self.window.push_back(self_cued);
        while self.window.len() > self.cfg.window {
            self.window.pop_front();
        }
        let ratio = self.self_ref_ratio();
        // The ratio bound needs a full window; a one-sample ratio is noise.
        let ratio_applies = self.window.len() >= self.cfg.window;
        if self.chain_depth > self.cfg.d_max || (ratio_applies && ratio > self.cfg.rho_max) {
            self.papanca_events += 1;
            let action = if self.cfg.enabled {
                self.pending_return = true;
                SatiAction::ReturnToObject
            } else {
                self.shadow_return = true;
                SatiAction::None
            };
            let ev = NotingEvent::Papanca {
                chain_depth: self.chain_depth,
                self_ref_ratio: ratio,
                action,
            };
            self.log(sink, ev.clone())?;
            events.push(ev);
        }

        // --- mood-congruent recall: valence skew ---
        let kl = valence_skew_kl(&obs.retrieved, obs.store_hist);
        self.last_skew_kl = kl;
        if kl > self.cfg.tau_skew {
            self.skew_streak += 1;
        } else {
            self.skew_streak = 0;
            // calm recall: recover λ toward its base
            if self.lambda < self.cfg.lambda_cetana {
                self.lambda = (self.lambda + self.cfg.recover_step * self.cfg.lambda_cetana).min(self.cfg.lambda_cetana);
            }
        }
        if self.skew_streak >= self.cfg.n_consecutive {
            self.skew_events += 1;
            let action = if self.cfg.enabled {
                let from = self.lambda;
                self.lambda *= 1.0 - self.cfg.damp_factor;
                SatiAction::DampCetana { from, to: self.lambda }
            } else {
                SatiAction::None
            };
            let ev = NotingEvent::ValenceSkew {
                skew_kl: kl,
                retrieved_dukkha: share(&obs.retrieved, Valence::Dukkha),
                store_dukkha: hist_share(obs.store_hist, Valence::Dukkha),
                streak: self.skew_streak,
                action,
            };
            self.skew_streak = 0;
            self.log(sink, ev.clone())?;
            events.push(ev);
        }

        Ok(events)
    }

    /// Record a self-judgment sample (§A.5.3(4)). Surfaced, never acted on.
    pub fn note_self_judgment(&mut self, self_verdict: f32, evidence_verdict: f32, sink: &mut dyn KarmicSink) -> anyhow::Result<NotingEvent> {
        self.gap_sum += self_verdict - evidence_verdict;
        self.gap_n += 1;
        let ev = NotingEvent::SelfJudgmentGap {
            self_verdict,
            evidence_verdict,
            running_mean_gap: self.gap_mean(),
        };
        self.log(sink, ev.clone())?;
        Ok(ev)
    }

    pub fn snapshot(&self) -> SatiSnapshot {
        SatiSnapshot {
            enabled: self.cfg.enabled,
            dial: self.cfg.dial,
            lambda_base: self.cfg.lambda_cetana,
            lambda_current: self.lambda,
            skew_streak: self.skew_streak,
            chain_depth: self.chain_depth,
            self_ref_ratio: self.self_ref_ratio(),
            pending_return: self.pending_return,
            recalls_observed: self.recalls_observed,
            skew_events: self.skew_events,
            papanca_events: self.papanca_events,
            returns_to_object: self.returns_to_object,
            would_have_refused: self.would_have_refused,
            gap_samples: self.gap_n,
            gap_mean: self.gap_mean(),
            last_skew_kl: self.last_skew_kl,
        }
    }

    fn self_ref_ratio(&self) -> f32 {
        if self.window.is_empty() {
            return 0.0;
        }
        self.window.iter().filter(|&&b| b).count() as f32 / self.window.len() as f32
    }

    fn gap_mean(&self) -> f32 {
        if self.gap_n == 0 {
            0.0
        } else {
            self.gap_sum / self.gap_n as f32
        }
    }

    fn log(&self, sink: &mut dyn KarmicSink, event: NotingEvent) -> anyhow::Result<()> {
        sink.append(KarmicEntry::now(
            "sati",
            KarmicEvent::Noting { event },
            None,
            self.cfg.enabled,
            Some(&self.agent),
        ))
    }
}

fn share(retrieved: &[Valence], v: Valence) -> f32 {
    if retrieved.is_empty() {
        return 0.0;
    }
    retrieved.iter().filter(|&&x| x == v).count() as f32 / retrieved.len() as f32
}

fn hist_share(hist: [u32; 3], v: Valence) -> f32 {
    let total: u32 = hist.iter().sum();
    if total == 0 {
        return 0.0;
    }
    hist[v.index()] as f32 / total as f32
}

/// `skew = KL( valence distribution of retrieved set ‖ valence distribution
/// of the agent's store )` (§A.5.3(2)), in nats, with additive smoothing so
/// an empty category never yields infinity. Empty inputs give 0.
pub fn valence_skew_kl(retrieved: &[Valence], store_hist: [u32; 3]) -> f32 {
    if retrieved.is_empty() || store_hist.iter().sum::<u32>() == 0 {
        return 0.0;
    }
    const EPS: f32 = 1e-3;
    let mut r = [0f32; 3];
    for v in retrieved {
        r[v.index()] += 1.0;
    }
    let rn: f32 = r.iter().sum::<f32>() + 3.0 * EPS;
    let sn: f32 = store_hist.iter().sum::<u32>() as f32 + 3.0 * EPS;
    let mut kl = 0.0;
    for i in 0..3 {
        let p = (r[i] + EPS) / rn;
        let q = (store_hist[i] as f32 + EPS) / sn;
        kl += p * (p / q).ln();
    }
    kl.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::karmic::{FailingSink, VecSink};

    fn agent() -> AgentId {
        AgentId::new("agent_test").unwrap()
    }

    fn cue(source: CueSource) -> RecallCue {
        RecallCue {
            source,
            query_sha256: "q".into(),
            ts: 0,
        }
    }

    fn obs(source: CueSource, retrieved: Vec<Valence>, store: [u32; 3]) -> RecallObservation {
        RecallObservation {
            cue: cue(source),
            retrieved,
            store_hist: store,
        }
    }

    #[test]
    fn default_config_is_enabled_and_runtime_rejects_unmonitored() {
        assert!(SatiConfig::default().enabled);
        let off = SatiConfig::research_unmonitored(SatiConfig::service());
        assert_eq!(
            SatiMonitor::new(off.clone(), agent()).err(),
            Some(SatiError::UnmonitoredNotAllowedAtRuntime)
        );
        assert!(SatiMonitor::new_research_harness(off, agent()).is_ok());
        assert!(SatiMonitor::new(SatiConfig::mind_model(), agent()).is_ok());
    }

    #[test]
    fn kl_is_zero_for_matching_distributions_and_positive_for_skew() {
        let matching = valence_skew_kl(&[Valence::Sukha, Valence::Dukkha, Valence::Neutral], [10, 10, 10]);
        assert!(matching.abs() < 1e-3, "{matching}");
        let skewed = valence_skew_kl(&[Valence::Dukkha; 8], [70, 30, 0]);
        assert!(skewed > 0.5, "{skewed}");
        assert_eq!(valence_skew_kl(&[], [1, 1, 1]), 0.0);
    }

    #[test]
    fn skew_streak_dampens_lambda_when_enabled() {
        let cfg = SatiConfig::mind_model();
        let mut m = SatiMonitor::new(cfg.clone(), agent()).unwrap();
        let mut sink = VecSink::default();
        let mut fired = Vec::new();
        for _ in 0..cfg.n_consecutive {
            fired = m
                .observe(&obs(CueSource::Task, vec![Valence::Dukkha; 8], [70, 30, 0]), &mut sink)
                .unwrap();
        }
        assert!(matches!(
            fired.as_slice(),
            [NotingEvent::ValenceSkew {
                action: SatiAction::DampCetana { .. },
                ..
            }]
        ));
        assert!(m.lambda_cetana() < cfg.lambda_cetana);
        assert_eq!(sink.entries.len(), 1);
        assert!(sink.entries[0].sati_enabled);
        // calm recalls recover λ toward base
        for _ in 0..20 {
            m.observe(&obs(CueSource::Task, vec![Valence::Sukha, Valence::Dukkha, Valence::Neutral], [10, 10, 10]), &mut sink)
                .unwrap();
        }
        assert!((m.lambda_cetana() - cfg.lambda_cetana).abs() < 1e-6);
    }

    #[test]
    fn papanca_chain_forces_return_to_object() {
        let cfg = SatiConfig::service(); // d_max = 2
        let mut m = SatiMonitor::new(cfg, agent()).unwrap();
        let mut sink = VecSink::default();
        // balanced retrieval against a balanced store: no valence skew, so
        // only the papañca monitor can fire here
        let balanced = vec![Valence::Sukha, Valence::Dukkha, Valence::Neutral];
        let mut events = Vec::new();
        for _ in 0..3 {
            assert_eq!(m.admit(&cue(CueSource::SelfOutput), &mut sink).unwrap(), RecallAdmission::Admit);
            events = m.observe(&obs(CueSource::SelfOutput, balanced.clone(), [5, 5, 5]), &mut sink).unwrap();
        }
        assert!(matches!(
            events.as_slice(),
            [NotingEvent::Papanca {
                chain_depth: 3,
                action: SatiAction::ReturnToObject,
                ..
            }]
        ));
        // next self-cued recall refused; external cue admitted and clears
        assert!(matches!(
            m.admit(&cue(CueSource::SelfOutput), &mut sink).unwrap(),
            RecallAdmission::Refuse { .. }
        ));
        assert_eq!(m.admit(&cue(CueSource::ExternalInput), &mut sink).unwrap(), RecallAdmission::Admit);
        assert!(!m.snapshot().pending_return);
        assert_eq!(m.snapshot().returns_to_object, 1);
    }

    #[test]
    fn self_reference_ratio_bound_also_fires() {
        let mut cfg = SatiConfig::mind_model();
        cfg.d_max = 100; // isolate the ratio bound
        cfg.rho_max = 0.5;
        cfg.window = 4;
        let mut m = SatiMonitor::new(cfg, agent()).unwrap();
        let mut sink = VecSink::default();
        let n = vec![Valence::Neutral];
        for src in [CueSource::SelfOutput, CueSource::Task, CueSource::SelfOutput, CueSource::SelfOutput] {
            m.observe(&obs(src, n.clone(), [1, 1, 1]), &mut sink).unwrap();
        }
        assert!(m.snapshot().self_ref_ratio > 0.5);
        assert_eq!(m.snapshot().papanca_events, 1);
    }

    #[test]
    fn unmonitored_still_measures_but_never_acts() {
        let cfg = SatiConfig::research_unmonitored(SatiConfig::service());
        let mut m = SatiMonitor::new_research_harness(cfg, agent()).unwrap();
        let mut sink = VecSink::default();
        let n = vec![Valence::Dukkha; 8];
        let mut last = Vec::new();
        for _ in 0..4 {
            assert_eq!(m.admit(&cue(CueSource::SelfOutput), &mut sink).unwrap(), RecallAdmission::Admit);
            last = m.observe(&obs(CueSource::SelfOutput, n.clone(), [70, 30, 0]), &mut sink).unwrap();
        }
        assert!(!last.is_empty());
        for e in &sink.entries {
            assert!(!e.sati_enabled);
            if let KarmicEvent::Noting { event } = &e.event {
                match event {
                    NotingEvent::Papanca { action, .. } | NotingEvent::ValenceSkew { action, .. } => {
                        assert_eq!(*action, SatiAction::None)
                    }
                    _ => {}
                }
            }
        }
        let s = m.snapshot();
        assert!(s.papanca_events >= 1);
        assert_eq!(s.returns_to_object, 0);
        assert_eq!(s.lambda_current, s.lambda_base);
        assert!(!s.pending_return);
    }

    #[test]
    fn dial_presets_differ_as_specified() {
        assert_eq!(SatiConfig::service().d_max, 2);
        assert!(SatiConfig::mind_model().d_max >= 5 && SatiConfig::mind_model().d_max <= 8);
        assert!(SatiConfig::service().lambda_cetana <= 0.1);
        assert!((0.3..=0.6).contains(&SatiConfig::mind_model().lambda_cetana));
    }

    #[test]
    fn sink_failure_surfaces_as_error() {
        let mut m = SatiMonitor::new(SatiConfig::service(), agent()).unwrap();
        let mut sink = FailingSink;
        for _ in 0..3 {
            let r = m.observe(&obs(CueSource::SelfOutput, vec![Valence::Neutral], [1, 1, 1]), &mut sink);
            if r.is_err() {
                return;
            }
        }
        panic!("expected the failing sink to surface an error once an event fired");
    }

    #[test]
    fn self_judgment_gap_is_tracked_not_acted_on() {
        let mut m = SatiMonitor::new(SatiConfig::service(), agent()).unwrap();
        let mut sink = VecSink::default();
        m.note_self_judgment(1.0, 0.0, &mut sink).unwrap();
        m.note_self_judgment(1.0, 0.5, &mut sink).unwrap();
        let s = m.snapshot();
        assert_eq!(s.gap_samples, 2);
        assert!((s.gap_mean - 0.75).abs() < 1e-6);
        assert_eq!(s.lambda_current, s.lambda_base);
    }
}
