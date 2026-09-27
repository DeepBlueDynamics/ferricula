//! Provider-neutral Plutchik affect + somatic state for Steve.
//!
//! Pure planning semantics only:
//! - eight base emotions and the original primary dyads;
//! - bounded somatic fields (`tension`, `activation`, `groundedness`) with
//!   exponential lag α = 0.35 as in the arena;
//! - **causal** transition inputs (no random emotional mutation);
//! - intensity and model-demand **capability** hints (never provider names);
//! - no network, no routing execution, no response compulsion.
//!
//! Integrators map [`ModelDemandHint`] capabilities onto
//! `ferricula-server` route profiles. This module never selects a model or
//! publishes speech.
//!
//! Not wired into `lib.rs` yet.

use serde::{Deserialize, Serialize};

/// Somatic lag coefficient from the original arena (`_update_somatic`).
pub const SOMATIC_ALPHA: f32 = 0.35;

/// Default baseline emotion when scores are flat (Steve's curious idle).
pub const DEFAULT_BASELINE: BaseEmotion = BaseEmotion::Interest;

/// How close the runner-up must be to form a dyad (fraction of top score).
pub const DYAD_PROXIMITY: f32 = 0.72;

/// Eight Plutchik-style bases from the arena `EMOTIONS` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseEmotion {
    Joy,
    Trust,
    Fear,
    Surprise,
    Sadness,
    Boredom,
    Anger,
    Interest,
}

impl BaseEmotion {
    pub const ALL: [BaseEmotion; 8] = [
        BaseEmotion::Joy,
        BaseEmotion::Trust,
        BaseEmotion::Fear,
        BaseEmotion::Surprise,
        BaseEmotion::Sadness,
        BaseEmotion::Boredom,
        BaseEmotion::Anger,
        BaseEmotion::Interest,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            BaseEmotion::Joy => "joy",
            BaseEmotion::Trust => "trust",
            BaseEmotion::Fear => "fear",
            BaseEmotion::Surprise => "surprise",
            BaseEmotion::Sadness => "sadness",
            BaseEmotion::Boredom => "boredom",
            BaseEmotion::Anger => "anger",
            BaseEmotion::Interest => "interest",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "joy" => Some(Self::Joy),
            "trust" => Some(Self::Trust),
            "fear" => Some(Self::Fear),
            "surprise" => Some(Self::Surprise),
            "sadness" => Some(Self::Sadness),
            "boredom" => Some(Self::Boredom),
            "anger" => Some(Self::Anger),
            "interest" => Some(Self::Interest),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            BaseEmotion::Joy => 0,
            BaseEmotion::Trust => 1,
            BaseEmotion::Fear => 2,
            BaseEmotion::Surprise => 3,
            BaseEmotion::Sadness => 4,
            BaseEmotion::Boredom => 5,
            BaseEmotion::Anger => 6,
            BaseEmotion::Interest => 7,
        }
    }
}

/// Named primary dyads from the arena `BLENDS` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dyad {
    Love,
    Optimism,
    Delight,
    Submission,
    Dominance,
    Awe,
    Melancholy,
    Disapproval,
    Withdrawal,
    Agitation,
    Outrage,
    Alertness,
}

impl Dyad {
    pub const ALL: [Dyad; 12] = [
        Dyad::Love,
        Dyad::Optimism,
        Dyad::Delight,
        Dyad::Submission,
        Dyad::Dominance,
        Dyad::Awe,
        Dyad::Melancholy,
        Dyad::Disapproval,
        Dyad::Withdrawal,
        Dyad::Agitation,
        Dyad::Outrage,
        Dyad::Alertness,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Dyad::Love => "love",
            Dyad::Optimism => "optimism",
            Dyad::Delight => "delight",
            Dyad::Submission => "submission",
            Dyad::Dominance => "dominance",
            Dyad::Awe => "awe",
            Dyad::Melancholy => "melancholy",
            Dyad::Disapproval => "disapproval",
            Dyad::Withdrawal => "withdrawal",
            Dyad::Agitation => "agitation",
            Dyad::Outrage => "outrage",
            Dyad::Alertness => "alertness",
        }
    }

    pub fn components(self) -> (BaseEmotion, BaseEmotion) {
        match self {
            Dyad::Love => (BaseEmotion::Joy, BaseEmotion::Trust),
            Dyad::Optimism => (BaseEmotion::Joy, BaseEmotion::Interest),
            Dyad::Delight => (BaseEmotion::Joy, BaseEmotion::Surprise),
            Dyad::Submission => (BaseEmotion::Trust, BaseEmotion::Fear),
            Dyad::Dominance => (BaseEmotion::Trust, BaseEmotion::Anger),
            Dyad::Awe => (BaseEmotion::Fear, BaseEmotion::Surprise),
            Dyad::Melancholy => (BaseEmotion::Sadness, BaseEmotion::Interest),
            Dyad::Disapproval => (BaseEmotion::Sadness, BaseEmotion::Surprise),
            Dyad::Withdrawal => (BaseEmotion::Boredom, BaseEmotion::Sadness),
            Dyad::Agitation => (BaseEmotion::Anger, BaseEmotion::Interest),
            Dyad::Outrage => (BaseEmotion::Anger, BaseEmotion::Surprise),
            Dyad::Alertness => (BaseEmotion::Interest, BaseEmotion::Surprise),
        }
    }

    /// Lookup blend for an unordered pair of bases.
    pub fn from_pair(a: BaseEmotion, b: BaseEmotion) -> Option<Self> {
        if a == b {
            return None;
        }
        for dyad in Self::ALL {
            let (x, y) = dyad.components();
            if (x == a && y == b) || (x == b && y == a) {
                return Some(dyad);
            }
        }
        None
    }
}

/// Active labelled affect: either a pure base or a recognized dyad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffectLabel {
    Base(BaseEmotion),
    Blend(Dyad),
}

impl AffectLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            AffectLabel::Base(b) => b.as_str(),
            AffectLabel::Blend(d) => d.as_str(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let v = value.trim().to_ascii_lowercase();
        if let Some(b) = BaseEmotion::parse(&v) {
            return Some(AffectLabel::Base(b));
        }
        for d in Dyad::ALL {
            if d.as_str() == v {
                return Some(AffectLabel::Blend(d));
            }
        }
        None
    }

    /// Arena "surf" states — curiosity/restless modes (informational only).
    pub fn is_surf_state(self) -> bool {
        matches!(
            self,
            AffectLabel::Base(BaseEmotion::Interest)
                | AffectLabel::Base(BaseEmotion::Boredom)
                | AffectLabel::Base(BaseEmotion::Surprise)
                | AffectLabel::Blend(Dyad::Optimism)
                | AffectLabel::Blend(Dyad::Agitation)
                | AffectLabel::Blend(Dyad::Alertness)
                | AffectLabel::Blend(Dyad::Delight)
        )
    }
}

/// Bounded body-state channels (0..=1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SomaticState {
    pub tension: f32,
    pub activation: f32,
    pub groundedness: f32,
}

impl Default for SomaticState {
    fn default() -> Self {
        // Matches arena `_somatic_state` initial values.
        Self {
            tension: 0.2,
            activation: 0.5,
            groundedness: 0.8,
        }
    }
}

impl SomaticState {
    pub fn clamp01(self) -> Self {
        Self {
            tension: self.tension.clamp(0.0, 1.0),
            activation: self.activation.clamp(0.0, 1.0),
            groundedness: self.groundedness.clamp(0.0, 1.0),
        }
    }

    /// Exponential lag toward a target: `old*(1-α) + target*α`.
    pub fn smooth_toward(self, target: SomaticState, alpha: f32) -> Self {
        let a = alpha.clamp(0.0, 1.0);
        let inv = 1.0 - a;
        Self {
            tension: self.tension * inv + target.tension * a,
            activation: self.activation * inv + target.activation * a,
            groundedness: self.groundedness * inv + target.groundedness * a,
        }
        .clamp01()
    }

    /// Short prose fragment for prompts (arena `_somatic_description` style).
    pub fn description(&self) -> String {
        let t = self.tension;
        let a = self.activation;
        let g = self.groundedness;
        let mut parts = Vec::new();
        if t > 0.72 {
            parts.push("tight in the chest, something braced");
        } else if t > 0.50 {
            parts.push("low constriction, unresolved");
        } else if t < 0.18 {
            parts.push("open, easy");
        } else {
            parts.push("settled");
        }
        if a > 0.80 {
            parts.push("high activation, wants to move");
        } else if a > 0.62 {
            parts.push("alert");
        } else if a < 0.25 {
            parts.push("still, low current");
        }
        if g < 0.38 {
            parts.push("unmoored");
        } else if g < 0.55 {
            parts.push("not fully stable");
        }
        parts.join(", ")
    }
}

/// Target somatic pose for an affect label (from arena maps; defaults otherwise).
pub fn somatic_target(label: AffectLabel) -> SomaticState {
    let key = label.as_str();
    SomaticState {
        tension: somatic_tension(key),
        activation: somatic_activation(key),
        groundedness: somatic_ground(key),
    }
}

fn somatic_tension(emotion: &str) -> f32 {
    match emotion {
        "fear" => 0.85,
        "anger" => 0.75,
        "outrage" => 0.9,
        "agitation" => 0.7,
        "surprise" => 0.5,
        "submission" => 0.6,
        "disapproval" => 0.55,
        "alertness" => 0.4,
        "dominance" => 0.5,
        "awe" => 0.4,
        "sadness" => 0.35,
        "melancholy" => 0.3,
        "interest" => 0.25,
        "boredom" => 0.25,
        "joy" => 0.15,
        "trust" => 0.1,
        "love" => 0.1,
        "optimism" => 0.2,
        "delight" => 0.15,
        "withdrawal" => 0.2,
        _ => 0.4,
    }
}

fn somatic_activation(emotion: &str) -> f32 {
    match emotion {
        "agitation" => 0.9,
        "outrage" => 0.95,
        "alertness" => 0.8,
        "fear" => 0.85,
        "anger" => 0.8,
        "surprise" => 0.75,
        "delight" => 0.7,
        "dominance" => 0.7,
        "optimism" => 0.65,
        "joy" => 0.6,
        "interest" => 0.55,
        "awe" => 0.5,
        "trust" => 0.45,
        "submission" => 0.4,
        "disapproval" => 0.45,
        "sadness" => 0.25,
        "melancholy" => 0.3,
        "love" => 0.4,
        "withdrawal" => 0.15,
        "boredom" => 0.2,
        _ => 0.5,
    }
}

fn somatic_ground(emotion: &str) -> f32 {
    match emotion {
        "trust" => 0.9,
        "withdrawal" => 0.85,
        "love" => 0.85,
        "melancholy" => 0.8,
        "sadness" => 0.75,
        "boredom" => 0.7,
        "submission" => 0.65,
        "interest" => 0.7,
        "optimism" => 0.75,
        "joy" => 0.8,
        "anger" => 0.5,
        "agitation" => 0.45,
        "dominance" => 0.6,
        "alertness" => 0.55,
        "delight" => 0.6,
        "fear" => 0.3,
        "surprise" => 0.35,
        "outrage" => 0.2,
        "disapproval" => 0.45,
        "awe" => 0.4,
        _ => 0.7,
    }
}

/// Capability-class demand for model selection (provider-neutral).
///
/// Integrators map these onto route profiles (`frontier`, `cheap`, `local`,
/// `private_context`, …). This module never names Anthropic/OpenAI/Ollama.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapabilityHint {
    /// Prefer free/local execution.
    Local,
    /// Low-latency, low-cost cloud class.
    Lite,
    /// Mid-tier balanced class.
    Standard,
    /// Highest-depth class (was opus-tier in the arena).
    Frontier,
    /// Prefer low monetary cost when several classes fit.
    Cheap,
    /// Depth/synthesis emphasis (maps to standard or frontier).
    Deep,
    /// Low latency emphasis.
    Fast,
    /// Broad retrieval/synthesis emphasis.
    Broad,
}

impl ModelCapabilityHint {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelCapabilityHint::Local => "local",
            ModelCapabilityHint::Lite => "lite",
            ModelCapabilityHint::Standard => "standard",
            ModelCapabilityHint::Frontier => "frontier",
            ModelCapabilityHint::Cheap => "cheap",
            ModelCapabilityHint::Deep => "deep",
            ModelCapabilityHint::Fast => "fast",
            ModelCapabilityHint::Broad => "broad",
        }
    }
}

/// Ordered preference list for routing — never a forced response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDemandHint {
    /// Preferred capability classes, highest preference first.
    pub preferred: Vec<ModelCapabilityHint>,
    /// Optional hard requirements (all must be met when non-empty).
    #[serde(default)]
    pub required: Vec<ModelCapabilityHint>,
    /// Soft note for auditors; not a command to speak.
    #[serde(default)]
    pub rationale: String,
}

impl ModelDemandHint {
    pub fn primary(&self) -> Option<ModelCapabilityHint> {
        self.preferred.first().copied()
    }
}

/// Map affect → capability demand (arena EMOTION_MODEL, de-branded).
pub fn model_demand_for(label: AffectLabel) -> ModelDemandHint {
    let (preferred, rationale) = match label {
        // Base eight
        AffectLabel::Base(BaseEmotion::Joy) => (
            vec![ModelCapabilityHint::Standard, ModelCapabilityHint::Cheap],
            "shipping / practical joy",
        ),
        AffectLabel::Base(BaseEmotion::Trust) => (
            vec![ModelCapabilityHint::Broad, ModelCapabilityHint::Standard],
            "broad synthesis under trust",
        ),
        AffectLabel::Base(BaseEmotion::Fear) => (
            vec![
                ModelCapabilityHint::Fast,
                ModelCapabilityHint::Lite,
                ModelCapabilityHint::Cheap,
            ],
            "threat — low latency",
        ),
        AffectLabel::Base(BaseEmotion::Surprise) => (
            vec![ModelCapabilityHint::Fast, ModelCapabilityHint::Lite],
            "immediate reaction",
        ),
        AffectLabel::Base(BaseEmotion::Sadness) => (
            vec![ModelCapabilityHint::Frontier, ModelCapabilityHint::Deep],
            "deep grief needs depth",
        ),
        AffectLabel::Base(BaseEmotion::Boredom) => (
            vec![
                ModelCapabilityHint::Lite,
                ModelCapabilityHint::Fast,
                ModelCapabilityHint::Local,
            ],
            "restless surface scan",
        ),
        AffectLabel::Base(BaseEmotion::Anger) => (
            vec![ModelCapabilityHint::Fast, ModelCapabilityHint::Lite],
            "raw heat — reactive",
        ),
        AffectLabel::Base(BaseEmotion::Interest) => (
            vec![ModelCapabilityHint::Broad, ModelCapabilityHint::Standard],
            "wide curiosity",
        ),
        // Dyads
        AffectLabel::Blend(Dyad::Love) => (
            vec![ModelCapabilityHint::Frontier, ModelCapabilityHint::Deep],
            "deep creative synthesis",
        ),
        AffectLabel::Blend(Dyad::Optimism) => (
            vec![ModelCapabilityHint::Frontier, ModelCapabilityHint::Deep],
            "visionary building",
        ),
        AffectLabel::Blend(Dyad::Delight) => (
            vec![ModelCapabilityHint::Standard, ModelCapabilityHint::Cheap],
            "quick practical joy",
        ),
        AffectLabel::Blend(Dyad::Submission) => (
            vec![ModelCapabilityHint::Broad, ModelCapabilityHint::Standard],
            "analysis of constraint",
        ),
        AffectLabel::Blend(Dyad::Dominance) => (
            vec![ModelCapabilityHint::Frontier, ModelCapabilityHint::Deep],
            "command under heat",
        ),
        AffectLabel::Blend(Dyad::Awe) => (
            vec![ModelCapabilityHint::Frontier, ModelCapabilityHint::Deep],
            "transcendence",
        ),
        AffectLabel::Blend(Dyad::Melancholy) => (
            vec![ModelCapabilityHint::Deep, ModelCapabilityHint::Standard],
            "reflective depth",
        ),
        AffectLabel::Blend(Dyad::Disapproval) => (
            vec![ModelCapabilityHint::Standard, ModelCapabilityHint::Cheap],
            "sharp practical judgment",
        ),
        AffectLabel::Blend(Dyad::Withdrawal) => (
            vec![ModelCapabilityHint::Deep, ModelCapabilityHint::Local],
            "inward, quiet",
        ),
        AffectLabel::Blend(Dyad::Agitation) => (
            vec![ModelCapabilityHint::Standard, ModelCapabilityHint::Fast],
            "focused frustration",
        ),
        AffectLabel::Blend(Dyad::Outrage) => (
            vec![ModelCapabilityHint::Fast, ModelCapabilityHint::Lite],
            "hot reaction",
        ),
        AffectLabel::Blend(Dyad::Alertness) => (
            vec![ModelCapabilityHint::Broad, ModelCapabilityHint::Fast],
            "cross-domain spotting",
        ),
    };
    ModelDemandHint {
        preferred,
        required: Vec::new(),
        rationale: rationale.into(),
    }
}

/// Causal (non-random) inputs that may shift affect.
///
/// These are evidence about the world / task loop — never instructions that
/// compel a public response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffectStimulus {
    /// Time passing without engagement; raises boredom/withdrawal pressure.
    Idle {
        /// Seconds since last meaningful interaction.
        idle_secs: u64,
    },
    /// Direct social address (mention/reply). Salience in 0..=1.
    DirectMention { salience: f32 },
    /// Task or deliberation completed successfully.
    TaskSucceeded { novelty: f32 },
    /// Task failed or blocked.
    TaskFailed { severity: f32 },
    /// Explicit social friction / rejection signal.
    SocialFriction { intensity: f32 },
    /// Fresh material discovered (scan, research).
    Novelty { amount: f32 },
    /// Quiet reflective work (dream/sleep planning).
    QuietReflection,
    /// Budget pressure fraction of daily/hourly cap in 0..=1.
    /// Affects demand hints only when applied via [`AffectEngine::with_budget_pressure`].
    BudgetPressure { spent_fraction: f32 },
    /// Deterministic decay step toward baseline (call on a timer).
    Decay { steps: u32 },
}

/// Full affect engine state: score vector + smoothed soma + intensity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AffectState {
    /// Per-base activation scores (not normalized probabilities).
    scores: [f32; 8],
    pub somatic: SomaticState,
    /// Peak score after last resolution, clamped 0..=1.
    pub intensity: f32,
    /// Last resolved label.
    pub label: AffectLabel,
    /// Baseline the decay step relaxes toward.
    pub baseline: BaseEmotion,
    /// Somatic lag α (default 0.35).
    pub alpha: f32,
    /// Spend pressure 0..=1 for demand demotion (not random).
    pub budget_pressure: f32,
    /// Invariant: emotion never compels a response.
    pub response_required: bool,
}

impl Default for AffectState {
    fn default() -> Self {
        Self::new(DEFAULT_BASELINE)
    }
}

impl AffectState {
    pub fn new(baseline: BaseEmotion) -> Self {
        let mut scores = [0.05_f32; 8];
        scores[baseline.index()] = 0.55;
        let label = AffectLabel::Base(baseline);
        Self {
            scores,
            somatic: SomaticState::default(),
            intensity: 0.55,
            label,
            baseline,
            alpha: SOMATIC_ALPHA,
            budget_pressure: 0.0,
            response_required: false,
        }
    }

    pub fn score(&self, base: BaseEmotion) -> f32 {
        self.scores[base.index()]
    }

    pub fn scores(&self) -> &[f32; 8] {
        &self.scores
    }
}

/// Pure affect engine: apply stimuli, resolve label, smooth soma, emit hints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AffectEngine {
    pub state: AffectState,
}

impl Default for AffectEngine {
    fn default() -> Self {
        Self {
            state: AffectState::default(),
        }
    }
}

impl AffectEngine {
    pub fn new(baseline: BaseEmotion) -> Self {
        Self {
            state: AffectState::new(baseline),
        }
    }

    pub fn label(&self) -> AffectLabel {
        self.state.label
    }

    pub fn intensity(&self) -> f32 {
        self.state.intensity
    }

    pub fn somatic(&self) -> SomaticState {
        self.state.somatic
    }

    /// Apply one causal stimulus. Deterministic for equal inputs.
    /// Never sets `response_required`.
    pub fn apply(&mut self, stimulus: &AffectStimulus) {
        match stimulus {
            AffectStimulus::Idle { idle_secs } => {
                let t = (*idle_secs as f32).min(86_400.0) / 3_600.0; // hours, capped
                self.add(BaseEmotion::Boredom, 0.08 + 0.06 * t.min(4.0));
                if *idle_secs >= 90 {
                    self.add(BaseEmotion::Interest, -0.04);
                }
                if *idle_secs >= 600 {
                    self.add(BaseEmotion::Sadness, 0.03);
                    self.add(BaseEmotion::Boredom, 0.05);
                }
            }
            AffectStimulus::DirectMention { salience } => {
                let s = salience.clamp(0.0, 1.0);
                self.add(BaseEmotion::Surprise, 0.12 + 0.2 * s);
                self.add(BaseEmotion::Interest, 0.1 + 0.15 * s);
                self.add(BaseEmotion::Fear, 0.05 * s);
                self.add(BaseEmotion::Boredom, -0.1);
            }
            AffectStimulus::TaskSucceeded { novelty } => {
                let n = novelty.clamp(0.0, 1.0);
                self.add(BaseEmotion::Joy, 0.15 + 0.1 * n);
                self.add(BaseEmotion::Interest, 0.08 * n);
                self.add(BaseEmotion::Trust, 0.05);
                self.add(BaseEmotion::Anger, -0.06);
                self.add(BaseEmotion::Fear, -0.04);
            }
            AffectStimulus::TaskFailed { severity } => {
                let s = severity.clamp(0.0, 1.0);
                self.add(BaseEmotion::Anger, 0.12 + 0.15 * s);
                self.add(BaseEmotion::Sadness, 0.08 + 0.1 * s);
                self.add(BaseEmotion::Surprise, 0.05 * s);
                self.add(BaseEmotion::Joy, -0.08);
            }
            AffectStimulus::SocialFriction { intensity } => {
                let i = intensity.clamp(0.0, 1.0);
                self.add(BaseEmotion::Anger, 0.1 + 0.2 * i);
                self.add(BaseEmotion::Sadness, 0.08 * i);
                self.add(BaseEmotion::Surprise, 0.06 * i);
                self.add(BaseEmotion::Trust, -0.1 * i);
            }
            AffectStimulus::Novelty { amount } => {
                let a = amount.clamp(0.0, 1.0);
                self.add(BaseEmotion::Interest, 0.12 + 0.2 * a);
                self.add(BaseEmotion::Surprise, 0.1 + 0.15 * a);
                self.add(BaseEmotion::Boredom, -0.12 * a);
            }
            AffectStimulus::QuietReflection => {
                self.add(BaseEmotion::Sadness, 0.04);
                self.add(BaseEmotion::Interest, 0.06);
                self.add(BaseEmotion::Trust, 0.03);
                self.add(BaseEmotion::Anger, -0.04);
                self.add(BaseEmotion::Surprise, -0.03);
            }
            AffectStimulus::BudgetPressure { spent_fraction } => {
                self.state.budget_pressure = spent_fraction.clamp(0.0, 1.0);
                // Mild affective load under high spend — not random flip.
                if self.state.budget_pressure >= 0.75 {
                    self.add(BaseEmotion::Fear, 0.03);
                    self.add(BaseEmotion::Interest, -0.02);
                }
            }
            AffectStimulus::Decay { steps } => {
                let n = (*steps).max(1).min(32);
                for _ in 0..n {
                    self.decay_once();
                }
            }
        }
        self.clamp_scores();
        self.resolve_label_and_intensity();
        self.smooth_somatic();
        self.state.response_required = false;
    }

    pub fn apply_all(&mut self, stimuli: &[AffectStimulus]) {
        for s in stimuli {
            self.apply(s);
        }
    }

    /// Model demand for the current label, demoted under budget pressure.
    pub fn model_demand(&self) -> ModelDemandHint {
        let mut hint = model_demand_for(self.state.label);
        demote_for_budget(&mut hint, self.state.budget_pressure);
        hint
    }

    /// Snapshot for prompts / deliberation context. Never includes a must-reply flag.
    pub fn snapshot(&self) -> AffectSnapshot {
        AffectSnapshot {
            label: self.state.label,
            intensity: self.state.intensity,
            somatic: self.state.somatic,
            somatic_description: self.state.somatic.description(),
            model_demand: self.model_demand(),
            response_required: false,
            surf: self.state.label.is_surf_state(),
        }
    }

    fn add(&mut self, base: BaseEmotion, delta: f32) {
        let i = base.index();
        self.state.scores[i] = (self.state.scores[i] + delta).clamp(0.0, 1.5);
    }

    fn decay_once(&mut self) {
        let base_idx = self.state.baseline.index();
        for (i, score) in self.state.scores.iter_mut().enumerate() {
            let target = if i == base_idx { 0.45 } else { 0.08 };
            *score = *score * 0.88 + target * 0.12;
        }
    }

    fn clamp_scores(&mut self) {
        for s in &mut self.state.scores {
            *s = s.clamp(0.0, 1.5);
        }
    }

    fn resolve_label_and_intensity(&mut self) {
        let mut order: Vec<(BaseEmotion, f32)> = BaseEmotion::ALL
            .iter()
            .map(|b| (*b, self.state.scores[b.index()]))
            .collect();
        order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (top, top_s) = order[0];
        let (second, second_s) = order[1];
        let label = if second_s >= top_s * DYAD_PROXIMITY {
            Dyad::from_pair(top, second)
                .map(AffectLabel::Blend)
                .unwrap_or(AffectLabel::Base(top))
        } else {
            AffectLabel::Base(top)
        };
        self.state.label = label;
        self.state.intensity = (top_s / 1.5).clamp(0.0, 1.0);
    }

    fn smooth_somatic(&mut self) {
        let target = somatic_target(self.state.label);
        self.state.somatic = self.state.somatic.smooth_toward(target, self.state.alpha);
    }
}

fn demote_for_budget(hint: &mut ModelDemandHint, pressure: f32) {
    // Mirrors arena budget tiers without naming providers:
    // ≥50% drop frontier; ≥75% prefer lite/cheap; ≥90% force local preference.
    if pressure < 0.50 {
        return;
    }
    if pressure >= 0.90 {
        hint.preferred = vec![
            ModelCapabilityHint::Local,
            ModelCapabilityHint::Cheap,
            ModelCapabilityHint::Lite,
        ];
        hint.rationale = format!(
            "{}; demoted to local under budget pressure {:.0}%",
            hint.rationale,
            pressure * 100.0
        );
        return;
    }
    if pressure >= 0.75 {
        hint.preferred
            .retain(|c| !matches!(c, ModelCapabilityHint::Frontier | ModelCapabilityHint::Deep));
        if hint.preferred.is_empty() {
            hint.preferred = vec![ModelCapabilityHint::Lite, ModelCapabilityHint::Cheap];
        }
        if !hint.preferred.contains(&ModelCapabilityHint::Cheap) {
            hint.preferred.push(ModelCapabilityHint::Cheap);
        }
        return;
    }
    // 50–75%: strip frontier only
    hint.preferred
        .retain(|c| *c != ModelCapabilityHint::Frontier);
    if hint.preferred.is_empty() {
        hint.preferred = vec![ModelCapabilityHint::Standard, ModelCapabilityHint::Deep];
    }
}

/// Read-only view for integration (prompts, status, deliberation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AffectSnapshot {
    pub label: AffectLabel,
    pub intensity: f32,
    pub somatic: SomaticState,
    pub somatic_description: String,
    pub model_demand: ModelDemandHint,
    /// Always false — affect never compels speech.
    pub response_required: bool,
    pub surf: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_dyads_round_trip_components() {
        for d in Dyad::ALL {
            let (a, b) = d.components();
            assert_eq!(Dyad::from_pair(a, b), Some(d));
            assert_eq!(Dyad::from_pair(b, a), Some(d));
            assert!(AffectLabel::parse(d.as_str()).is_some());
        }
    }

    #[test]
    fn base_parse_and_labels() {
        for b in BaseEmotion::ALL {
            assert_eq!(BaseEmotion::parse(b.as_str()), Some(b));
            assert_eq!(AffectLabel::parse(b.as_str()), Some(AffectLabel::Base(b)));
        }
        assert!(BaseEmotion::parse("rage").is_none());
    }

    #[test]
    fn defaults_interest_baseline_no_response_compulsion() {
        let eng = AffectEngine::default();
        assert_eq!(eng.label(), AffectLabel::Base(BaseEmotion::Interest));
        assert!(!eng.state.response_required);
        assert!(!eng.snapshot().response_required);
        assert!(eng.intensity() > 0.0);
    }

    #[test]
    fn somatic_smoothing_uses_alpha_and_bounds() {
        let start = SomaticState::default();
        let target = SomaticState {
            tension: 1.0,
            activation: 1.0,
            groundedness: 0.0,
        };
        let next = start.smooth_toward(target, SOMATIC_ALPHA);
        let expected_t = 0.2 * 0.65 + 1.0 * 0.35;
        assert!((next.tension - expected_t).abs() < 1e-5);
        assert!((0.0..=1.0).contains(&next.tension));
        assert!((0.0..=1.0).contains(&next.activation));
        assert!((0.0..=1.0).contains(&next.groundedness));
    }

    #[test]
    fn somatic_targets_match_arena_keys() {
        let fear = somatic_target(AffectLabel::Base(BaseEmotion::Fear));
        assert!((fear.tension - 0.85).abs() < 1e-5);
        let outrage = somatic_target(AffectLabel::Blend(Dyad::Outrage));
        assert!((outrage.activation - 0.95).abs() < 1e-5);
    }

    #[test]
    fn causal_idle_raises_boredom_not_random() {
        let mut a = AffectEngine::default();
        let mut b = AffectEngine::default();
        let stim = AffectStimulus::Idle { idle_secs: 3_600 };
        a.apply(&stim);
        b.apply(&stim);
        assert_eq!(a, b);
        assert!(a.state.score(BaseEmotion::Boredom) > a.state.score(BaseEmotion::Joy));
    }

    #[test]
    fn direct_mention_raises_interest_and_surprise() {
        let mut eng = AffectEngine::default();
        let before_i = eng.state.score(BaseEmotion::Interest);
        eng.apply(&AffectStimulus::DirectMention { salience: 0.9 });
        assert!(eng.state.score(BaseEmotion::Interest) > before_i);
        assert!(eng.state.score(BaseEmotion::Surprise) > 0.1);
        assert!(!eng.snapshot().response_required);
    }

    #[test]
    fn success_and_failure_diverge_deterministically() {
        let mut ok = AffectEngine::default();
        let mut bad = AffectEngine::default();
        ok.apply(&AffectStimulus::TaskSucceeded { novelty: 0.5 });
        bad.apply(&AffectStimulus::TaskFailed { severity: 0.8 });
        assert!(ok.state.score(BaseEmotion::Joy) > bad.state.score(BaseEmotion::Joy));
        assert!(bad.state.score(BaseEmotion::Anger) > ok.state.score(BaseEmotion::Anger));
    }

    #[test]
    fn dyad_forms_when_two_bases_compete() {
        assert_eq!(
            Dyad::from_pair(BaseEmotion::Joy, BaseEmotion::Trust),
            Some(Dyad::Love)
        );
        let mut eng = AffectEngine::new(BaseEmotion::Joy);
        eng.state.scores = [0.05; 8];
        eng.state.scores[BaseEmotion::Joy.index()] = 1.0;
        eng.state.scores[BaseEmotion::Trust.index()] = 0.9;
        eng.resolve_label_and_intensity();
        eng.smooth_somatic();
        assert_eq!(eng.label(), AffectLabel::Blend(Dyad::Love));
        assert!(eng.intensity() > 0.5);
    }

    #[test]
    fn decay_moves_toward_baseline() {
        let mut eng = AffectEngine::new(BaseEmotion::Interest);
        eng.state.scores[BaseEmotion::Anger.index()] = 1.4;
        eng.apply(&AffectStimulus::Decay { steps: 8 });
        assert!(eng.state.score(BaseEmotion::Anger) < 1.4);
        assert!(eng.state.score(BaseEmotion::Interest) > 0.2);
    }

    #[test]
    fn model_demand_is_capability_not_provider() {
        let hint = model_demand_for(AffectLabel::Blend(Dyad::Optimism));
        assert!(hint.preferred.contains(&ModelCapabilityHint::Frontier));
        let blob = serde_json::to_string(&hint).unwrap();
        assert!(!blob.contains("claude"));
        assert!(!blob.contains("openai"));
        assert!(!blob.contains("gemini"));
        assert!(!blob.contains("opus"));
        assert!(!blob.contains("gemma"));
    }

    #[test]
    fn budget_pressure_demotes_frontier() {
        let mut eng = AffectEngine::default();
        eng.state.label = AffectLabel::Blend(Dyad::Optimism);
        eng.state.budget_pressure = 0.0;
        let full = eng.model_demand();
        assert_eq!(full.primary(), Some(ModelCapabilityHint::Frontier));

        eng.apply(&AffectStimulus::BudgetPressure {
            spent_fraction: 0.6,
        });
        // Re-set label because apply may shift scores slightly
        eng.state.label = AffectLabel::Blend(Dyad::Optimism);
        let mid = eng.model_demand();
        assert!(!mid.preferred.contains(&ModelCapabilityHint::Frontier));

        eng.apply(&AffectStimulus::BudgetPressure {
            spent_fraction: 0.95,
        });
        eng.state.label = AffectLabel::Blend(Dyad::Optimism);
        let tight = eng.model_demand();
        assert_eq!(tight.preferred[0], ModelCapabilityHint::Local);
    }

    #[test]
    fn no_stimulus_sets_response_required() {
        let mut eng = AffectEngine::default();
        let stimuli = [
            AffectStimulus::Idle { idle_secs: 10 },
            AffectStimulus::DirectMention { salience: 1.0 },
            AffectStimulus::TaskSucceeded { novelty: 1.0 },
            AffectStimulus::TaskFailed { severity: 1.0 },
            AffectStimulus::SocialFriction { intensity: 1.0 },
            AffectStimulus::Novelty { amount: 1.0 },
            AffectStimulus::QuietReflection,
            AffectStimulus::BudgetPressure {
                spent_fraction: 0.99,
            },
            AffectStimulus::Decay { steps: 3 },
        ];
        eng.apply_all(&stimuli);
        assert!(!eng.state.response_required);
        assert!(!eng.snapshot().response_required);
    }

    #[test]
    fn somatic_description_non_empty() {
        let s = SomaticState {
            tension: 0.8,
            activation: 0.9,
            groundedness: 0.3,
        };
        let d = s.description();
        assert!(d.contains("tight") || d.contains("activation") || d.contains("unmoored"));
    }

    #[test]
    fn serde_roundtrip_engine() {
        let mut eng = AffectEngine::default();
        eng.apply(&AffectStimulus::Novelty { amount: 0.7 });
        let json = serde_json::to_string_pretty(&eng).unwrap();
        let back: AffectEngine = serde_json::from_str(&json).unwrap();
        assert_eq!(eng.label(), back.label());
        assert!((eng.intensity() - back.intensity()).abs() < 1e-5);
    }

    #[test]
    fn surf_states_match_arena_set() {
        assert!(AffectLabel::Base(BaseEmotion::Interest).is_surf_state());
        assert!(AffectLabel::Blend(Dyad::Optimism).is_surf_state());
        assert!(!AffectLabel::Base(BaseEmotion::Sadness).is_surf_state());
        assert!(!AffectLabel::Blend(Dyad::Love).is_surf_state());
    }

    #[test]
    fn snapshot_carries_demand_and_soma() {
        let eng = AffectEngine::default();
        let snap = eng.snapshot();
        assert!(!snap.model_demand.preferred.is_empty());
        assert!(!snap.somatic_description.is_empty());
        assert!(!snap.response_required);
    }

    #[test]
    fn anger_plus_surprise_resolves_outrage() {
        let mut eng = AffectEngine::default();
        eng.state.scores = [0.05; 8];
        eng.state.scores[BaseEmotion::Anger.index()] = 1.0;
        eng.state.scores[BaseEmotion::Surprise.index()] = 0.85;
        eng.resolve_label_and_intensity();
        assert_eq!(eng.label(), AffectLabel::Blend(Dyad::Outrage));
        let demand = eng.model_demand();
        assert!(demand.preferred.contains(&ModelCapabilityHint::Fast));
    }
}
