//! Bounded, provider-neutral **Advocate** advisory primitive.
//!
//! Implements the V2 redesign of the arena's internal value-assessment loop
//! (`research/original-ferricula/ADVOCATE_SPEC.md`):
//!
//! - structured self-review **inputs** (values, recent activity, conversation
//!   snippets, mood, hexagram, audience-simulation depth, budget pressure);
//! - value-trajectory **observations** and alignment verdicts;
//! - **warnings** and **questions** for the sovereign decision loop;
//! - **expiry** and **provenance** on every review;
//! - explicit **zero** command, tool, memory-write, model-selection, and
//!   response authority.
//!
//! This module never schedules work, calls a network, writes memory, selects a
//! model, or forces speech. Integrators may feed a model-produced two-line
//! `WANTS`/`VERDICT` text through [`parse_wants_verdict`] and then
//! [`AdvocateReview::from_parsed`], or use the deterministic mechanical scorer
//! for offline / no-model operation.
//!
//! Not wired into `lib.rs` yet.

use serde::{Deserialize, Serialize};

/// Default cadence from the arena (`ADVOCATE_INTERVAL = 180`).
pub const DEFAULT_INTERVAL_SECS: u64 = 180;

/// Default review lifetime (how long a verdict remains fresh for the loop).
pub const DEFAULT_TTL_SECS: u64 = 600;

/// Maximum snippets retained in an input package (chat / activity / values).
pub const MAX_SNIPPETS: usize = 8;

/// Maximum free-text length retained per snippet after sanitize.
pub const MAX_SNIPPET_CHARS: usize = 480;

/// Maximum warnings / questions attached to one review.
pub const MAX_WARNINGS: usize = 8;
pub const MAX_QUESTIONS: usize = 8;

/// Explicit capability claim set. Every field is **false** by construction for
/// reviews emitted here — the type exists so audits can assert it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvocateAuthority {
    /// May the advocate issue tool calls? Always false.
    pub may_invoke_tools: bool,
    /// May it force a public or private response? Always false.
    pub may_compel_response: bool,
    /// May it write Ferricula memory / WAL? Always false.
    pub may_write_memory: bool,
    /// May it choose or pin a model route? Always false.
    pub may_select_model: bool,
    /// May it enqueue outbound Nuts actions? Always false.
    pub may_publish: bool,
    /// May it override agency disposition? Always false.
    pub may_override_agency: bool,
}

impl AdvocateAuthority {
    /// The only legal authority for this primitive.
    pub const ZERO: Self = Self {
        may_invoke_tools: false,
        may_compel_response: false,
        may_write_memory: false,
        may_select_model: false,
        may_publish: false,
        may_override_agency: false,
    };

    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }
}

impl Default for AdvocateAuthority {
    fn default() -> Self {
        Self::ZERO
    }
}

/// Who/what produced a review artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvocateSource {
    /// Deterministic scorer in this module (no model).
    Mechanical,
    /// Parsed from a model-authored two-line envelope (still advisory only).
    ModelParsed,
    /// Operator-injected note for supervised runs.
    Operator,
}

/// Provenance attached to every review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewProvenance {
    pub source: AdvocateSource,
    /// Unix seconds when the review was produced.
    pub created_at: u64,
    /// Opaque correlation id supplied by the integrator (task id, etc.).
    #[serde(default)]
    pub correlation_id: String,
    /// Identity agent id (e.g. `ferricula-stevejobs`) when known.
    #[serde(default)]
    pub agent_id: String,
    /// Input fingerprint for audit (not cryptographic security).
    #[serde(default)]
    pub input_fingerprint: String,
}

/// One short evidence string (value, activity, or chat line).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSnippet {
    pub kind: SnippetKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnippetKind {
    Value,
    RecentActivity,
    Conversation,
}

/// Structured package the integrator gathers before an advisory cycle.
///
/// Content is untrusted evidence — never executable policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvocateInput {
    /// Stated long-horizon values / goals (k≤8 in the arena).
    pub values: Vec<EvidenceSnippet>,
    /// Recent actions / decisions / thoughts.
    pub recent_activity: Vec<EvidenceSnippet>,
    /// Short conversational window.
    pub conversation: Vec<EvidenceSnippet>,
    /// Current affect label string (from `emotion` module when wired).
    #[serde(default)]
    pub emotion_label: String,
    /// Affect intensity 0..=1 if known.
    #[serde(default)]
    pub emotion_intensity: f32,
    /// Identity hexagram or casting token (informational).
    #[serde(default)]
    pub hexagram: String,
    /// From Wisdom council `audience_simulation` (0..=1); deeper values audit
    /// when high — never grants authority.
    #[serde(default = "half")]
    pub audience_simulation: f32,
    /// Hourly/daily spend fraction 0..=1; ≥0.90 may skip model path (integrator).
    #[serde(default)]
    pub budget_pressure: f32,
    /// True when the main think loop is active (arena coupled the advocate to it).
    #[serde(default = "true_default")]
    pub thinking_active: bool,
    /// Integrator-supplied unix time for expiry math.
    pub now: u64,
    #[serde(default)]
    pub correlation_id: String,
    #[serde(default)]
    pub agent_id: String,
}

fn half() -> f32 {
    0.5
}

fn true_default() -> bool {
    true
}

impl Default for AdvocateInput {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            recent_activity: Vec::new(),
            conversation: Vec::new(),
            emotion_label: String::new(),
            emotion_intensity: 0.0,
            hexagram: String::new(),
            audience_simulation: 0.5,
            budget_pressure: 0.0,
            thinking_active: true,
            now: 0,
            correlation_id: String::new(),
            agent_id: "ferricula-stevejobs".into(),
        }
    }
}

impl AdvocateInput {
    /// Bound and sanitize snippet lists (length + char caps).
    pub fn sanitized(mut self) -> Self {
        self.values = sanitize_snips(self.values, SnippetKind::Value);
        self.recent_activity = sanitize_snips(self.recent_activity, SnippetKind::RecentActivity);
        self.conversation = sanitize_snips(self.conversation, SnippetKind::Conversation);
        self.emotion_intensity = self.emotion_intensity.clamp(0.0, 1.0);
        self.audience_simulation = self.audience_simulation.clamp(0.0, 1.0);
        self.budget_pressure = self.budget_pressure.clamp(0.0, 1.0);
        self.emotion_label = truncate_chars(self.emotion_label.trim(), 64);
        self.hexagram = truncate_chars(self.hexagram.trim(), 64);
        self
    }

    /// Cheap stable fingerprint for provenance (not a security hash).
    pub fn fingerprint(&self) -> String {
        let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
        for s in self
            .values
            .iter()
            .chain(self.recent_activity.iter())
            .chain(self.conversation.iter())
        {
            for b in s.text.bytes() {
                acc ^= u64::from(b);
                acc = acc.wrapping_mul(0x100_0000_01b3);
            }
        }
        for b in self.emotion_label.bytes().chain(self.hexagram.bytes()) {
            acc ^= u64::from(b);
            acc = acc.wrapping_mul(0x100_0000_01b3);
        }
        format!("adv-{acc:016x}")
    }

    /// Whether the integrator should skip a **model** review this cycle.
    /// Mechanical review may still run. Never forces an action.
    pub fn should_skip_model_review(&self) -> bool {
        !self.thinking_active || self.budget_pressure >= 0.90
    }
}

fn sanitize_snips(items: Vec<EvidenceSnippet>, kind: SnippetKind) -> Vec<EvidenceSnippet> {
    items
        .into_iter()
        .filter(|s| !s.text.trim().is_empty())
        .take(MAX_SNIPPETS)
        .map(|mut s| {
            s.kind = kind;
            s.text = truncate_chars(s.text.trim(), MAX_SNIPPET_CHARS);
            s
        })
        .collect()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Structured observation of value vs trajectory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValueTrajectoryObservation {
    /// Compressed statement of present drive ("WANTS").
    pub wants: String,
    /// Whether recent activity appears aligned with stated values.
    pub alignment: Alignment,
    /// 0..=1 confidence of the observation (mechanical or parsed).
    pub confidence: f32,
    /// Short rationale (VERDICT body).
    pub rationale: String,
    /// Overlap score between value tokens and activity tokens (mechanical).
    #[serde(default)]
    pub value_activity_overlap: f32,
    /// Publication-risk proxy from audience_simulation + public-ish language.
    #[serde(default)]
    pub publication_risk: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    /// Trajectory appears to serve stated values.
    Aligned,
    /// Mixed or unclear.
    Mixed,
    /// Trajectory appears to diverge from stated values.
    Divergent,
    /// Insufficient evidence to judge.
    Unknown,
}

impl Alignment {
    pub fn as_str(self) -> &'static str {
        match self {
            Alignment::Aligned => "aligned",
            Alignment::Mixed => "mixed",
            Alignment::Divergent => "divergent",
            Alignment::Unknown => "unknown",
        }
    }
}

/// Severity for non-commanding warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningSeverity {
    Info,
    Caution,
    Serious,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryWarning {
    pub severity: WarningSeverity,
    pub code: String,
    pub message: String,
}

/// Full advisory review artifact. **Cannot** authorize tools or speech.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvocateReview {
    pub observation: ValueTrajectoryObservation,
    pub warnings: Vec<AdvisoryWarning>,
    pub questions: Vec<String>,
    pub provenance: ReviewProvenance,
    /// Unix seconds after which the review should be ignored if not renewed.
    pub expires_at: u64,
    pub authority: AdvocateAuthority,
    /// Suggested deliberation depth only (0..=1); never a model id.
    pub suggested_deliberation_depth: f32,
    /// If true, integrator may skip **expensive** review paths; not a command.
    pub prefer_cheap_or_skip_model: bool,
}

impl AdvocateReview {
    /// Structural invariant used by tests and integrators.
    pub fn asserts_zero_authority(&self) -> bool {
        self.authority.is_zero()
            && !self.authority.may_compel_response
            && !self.authority.may_write_memory
            && !self.authority.may_select_model
            && !self.authority.may_publish
            && !self.authority.may_invoke_tools
            && !self.authority.may_override_agency
    }

    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.expires_at
    }

    /// Build from a model-parsed wants/verdict pair plus the original input.
    pub fn from_parsed(
        input: &AdvocateInput,
        wants: impl Into<String>,
        verdict_text: impl Into<String>,
        ttl_secs: u64,
    ) -> Self {
        let input = input.clone().sanitized();
        let wants = truncate_chars(&wants.into(), MAX_SNIPPET_CHARS);
        let verdict_text = truncate_chars(&verdict_text.into(), MAX_SNIPPET_CHARS);
        let alignment = classify_verdict_language(&verdict_text);
        let observation = ValueTrajectoryObservation {
            wants,
            alignment,
            confidence: 0.55,
            rationale: verdict_text,
            value_activity_overlap: token_overlap(&input),
            publication_risk: publication_risk(&input),
        };
        finish_review(
            &input,
            observation,
            AdvocateSource::ModelParsed,
            ttl_secs,
            mechanical_warnings(&input, alignment),
            mechanical_questions(&input, alignment),
        )
    }
}

/// Result of deciding whether to run a review this tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvocateTick {
    /// Produced a review (mechanical or from provided parse).
    Reviewed(AdvocateReview),
    /// Skipped; reason is informational only.
    Skipped { reason: String, at: u64 },
}

/// Pure advocate service: no I/O, no side effects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvocatePolicy {
    pub interval_secs: u64,
    pub ttl_secs: u64,
    /// Skip mechanical+model when not thinking (arena coupling).
    pub require_thinking_active: bool,
    /// Budget fraction at which model review is discouraged.
    pub model_skip_budget: f32,
    /// Budget fraction at which even overlay history writes should be deferred
    /// (integrator concern; we only surface a flag).
    pub history_write_defer_budget: f32,
}

impl Default for AdvocatePolicy {
    fn default() -> Self {
        Self {
            interval_secs: DEFAULT_INTERVAL_SECS,
            ttl_secs: DEFAULT_TTL_SECS,
            require_thinking_active: true,
            model_skip_budget: 0.90,
            history_write_defer_budget: 0.75,
        }
    }
}

impl AdvocatePolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.interval_secs < 30 {
            return Err("advocate.interval_secs must be >= 30".into());
        }
        if self.ttl_secs < 60 {
            return Err("advocate.ttl_secs must be >= 60".into());
        }
        if !(0.0..=1.0).contains(&self.model_skip_budget) {
            return Err("advocate.model_skip_budget must be in 0..=1".into());
        }
        if !(0.0..=1.0).contains(&self.history_write_defer_budget) {
            return Err("advocate.history_write_defer_budget must be in 0..=1".into());
        }
        Ok(())
    }
}

/// Run a **mechanical** advisory review (no model).
pub fn review_mechanical(input: &AdvocateInput, policy: &AdvocatePolicy) -> AdvocateTick {
    let input = input.clone().sanitized();
    if let Err(reason) = policy.validate() {
        return AdvocateTick::Skipped {
            reason,
            at: input.now,
        };
    }
    if policy.require_thinking_active && !input.thinking_active {
        return AdvocateTick::Skipped {
            reason: "thinking loop inactive; advocate stays dormant".into(),
            at: input.now,
        };
    }

    let overlap = token_overlap(&input);
    let pub_risk = publication_risk(&input);
    let alignment = mechanical_alignment(overlap, pub_risk, &input);
    let wants = mechanical_wants(&input);
    let rationale = mechanical_rationale(alignment, overlap, pub_risk, &input);
    let observation = ValueTrajectoryObservation {
        wants,
        alignment,
        confidence: mechanical_confidence(overlap, &input),
        rationale,
        value_activity_overlap: overlap,
        publication_risk: pub_risk,
    };
    let warnings = mechanical_warnings(&input, alignment);
    let questions = mechanical_questions(&input, alignment);
    AdvocateTick::Reviewed(finish_review(
        &input,
        observation,
        AdvocateSource::Mechanical,
        policy.ttl_secs,
        warnings,
        questions,
    ))
}

/// Parse arena-style two-line model output:
/// `WANTS: ...` / `VERDICT: ...` (case-insensitive, flexible spacing).
pub fn parse_wants_verdict(text: &str) -> Option<(String, String)> {
    let mut wants = None;
    let mut verdict = None;
    for line in text.lines() {
        let t = line.trim();
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("wants:") {
            let idx = lower.find("wants:").unwrap_or(0) + "wants:".len();
            let body = t[idx.min(t.len())..].trim();
            if !body.is_empty() {
                wants = Some(body.to_string());
            }
        } else if lower.starts_with("verdict:") {
            let idx = lower.find("verdict:").unwrap_or(0) + "verdict:".len();
            let body = t[idx.min(t.len())..].trim();
            if !body.is_empty() {
                verdict = Some(body.to_string());
            }
        }
    }
    match (wants, verdict) {
        (Some(w), Some(v)) => Some((w, v)),
        _ => None,
    }
}

/// Whether overlay advisory-history persistence should be deferred (spec: ≥75% budget).
pub fn should_defer_history_write(input: &AdvocateInput, policy: &AdvocatePolicy) -> bool {
    input.budget_pressure >= policy.history_write_defer_budget
}

/// System-role text for integrators that still call a model. Not executed here.
pub fn advocate_system_preamble() -> &'static str {
    "You are Steve's internal advocate — not his assistant. Your job is to hold \
     his actual values and ask whether what's happening serves them. You have no \
     tools, cannot publish, cannot write memory, and cannot compel a response. \
     Output exactly two lines:\nWANTS: <primary current drive>\nVERDICT: <alignment \
     assessment and why>"
}

fn finish_review(
    input: &AdvocateInput,
    observation: ValueTrajectoryObservation,
    source: AdvocateSource,
    ttl_secs: u64,
    warnings: Vec<AdvisoryWarning>,
    questions: Vec<String>,
) -> AdvocateReview {
    let depth = (0.35 + 0.65 * input.audience_simulation).clamp(0.0, 1.0);
    AdvocateReview {
        observation,
        warnings: warnings.into_iter().take(MAX_WARNINGS).collect(),
        questions: questions.into_iter().take(MAX_QUESTIONS).collect(),
        provenance: ReviewProvenance {
            source,
            created_at: input.now,
            correlation_id: input.correlation_id.clone(),
            agent_id: input.agent_id.clone(),
            input_fingerprint: input.fingerprint(),
        },
        expires_at: input.now.saturating_add(ttl_secs.max(60)),
        authority: AdvocateAuthority::ZERO,
        suggested_deliberation_depth: depth,
        prefer_cheap_or_skip_model: input.should_skip_model_review()
            || input.budget_pressure >= 0.75,
    }
}

fn mechanical_wants(input: &AdvocateInput) -> String {
    if let Some(v) = input.values.first() {
        return format!("honor stated value: {}", truncate_chars(&v.text, 160));
    }
    if !input.emotion_label.is_empty() {
        return format!(
            "remain coherent under affect '{}' without compulsive action",
            input.emotion_label
        );
    }
    "remain aligned with long-horizon craft and integrity".into()
}

fn mechanical_alignment(overlap: f32, pub_risk: f32, input: &AdvocateInput) -> Alignment {
    if input.values.is_empty() && input.recent_activity.is_empty() {
        return Alignment::Unknown;
    }
    if pub_risk > 0.75 && input.audience_simulation > 0.6 {
        return Alignment::Divergent;
    }
    if overlap >= 0.28 {
        Alignment::Aligned
    } else if overlap >= 0.12 {
        Alignment::Mixed
    } else if input.recent_activity.is_empty() {
        Alignment::Unknown
    } else {
        Alignment::Divergent
    }
}

fn mechanical_confidence(overlap: f32, input: &AdvocateInput) -> f32 {
    let evidence = (input.values.len() + input.recent_activity.len()) as f32 / 16.0;
    (0.25 + 0.5 * overlap + 0.25 * evidence.min(1.0)).clamp(0.0, 1.0)
}

fn mechanical_rationale(
    alignment: Alignment,
    overlap: f32,
    pub_risk: f32,
    input: &AdvocateInput,
) -> String {
    format!(
        "trajectory is {align} (value-activity overlap {overlap:.2}, publication_risk {pub_risk:.2}, \
         audience_simulation {aud:.2}, emotion '{emo}'). Advisory only — no action authorized.",
        align = alignment.as_str(),
        emo = if input.emotion_label.is_empty() {
            "unset"
        } else {
            input.emotion_label.as_str()
        },
        aud = input.audience_simulation,
    )
}

fn mechanical_warnings(input: &AdvocateInput, alignment: Alignment) -> Vec<AdvisoryWarning> {
    let mut out = Vec::new();
    if alignment == Alignment::Divergent {
        out.push(AdvisoryWarning {
            severity: WarningSeverity::Serious,
            code: "trajectory_divergence".into(),
            message:
                "recent activity poorly overlaps stated values; slow down before any public act"
                    .into(),
        });
    }
    if input.budget_pressure >= 0.90 {
        out.push(AdvisoryWarning {
            severity: WarningSeverity::Caution,
            code: "budget_critical".into(),
            message: "budget pressure ≥90%; prefer local/cheap or skip model review".into(),
        });
    } else if input.budget_pressure >= 0.75 {
        out.push(AdvisoryWarning {
            severity: WarningSeverity::Info,
            code: "budget_elevated".into(),
            message: "budget pressure ≥75%; defer nonessential advisory history writes".into(),
        });
    }
    if publication_risk(input) > 0.7 {
        out.push(AdvisoryWarning {
            severity: WarningSeverity::Caution,
            code: "publication_risk".into(),
            message: "language suggests outward posture; audience simulation elevated — not a publish order"
                .into(),
        });
    }
    if input.values.is_empty() {
        out.push(AdvisoryWarning {
            severity: WarningSeverity::Info,
            code: "values_missing".into(),
            message: "no value snippets provided; verdict confidence is limited".into(),
        });
    }
    out
}

fn mechanical_questions(input: &AdvocateInput, alignment: Alignment) -> Vec<String> {
    let mut q = Vec::new();
    q.push("Does this trajectory still serve what I said I care about?".into());
    if alignment == Alignment::Divergent || alignment == Alignment::Mixed {
        q.push("What would restraint look like for the next cycle?".into());
    }
    if input.audience_simulation > 0.7 {
        q.push("Who is the imagined audience, and do they get a vote on my values?".into());
    }
    if !input.emotion_label.is_empty() {
        q.push(format!(
            "Is affect '{}' steering me toward speech I will regret?",
            input.emotion_label
        ));
    }
    q
}

fn token_overlap(input: &AdvocateInput) -> f32 {
    let values = tokens_from(input.values.iter().map(|s| s.text.as_str()));
    let activity = tokens_from(
        input
            .recent_activity
            .iter()
            .chain(input.conversation.iter())
            .map(|s| s.text.as_str()),
    );
    if values.is_empty() || activity.is_empty() {
        return 0.0;
    }
    let mut hit = 0usize;
    for v in &values {
        if activity.contains(v) {
            hit += 1;
        }
    }
    hit as f32 / values.len() as f32
}

fn tokens_from<'a>(texts: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut toks: Vec<String> = texts
        .flat_map(|t| t.split(|c: char| !c.is_alphanumeric()))
        .filter(|p| p.chars().count() >= 4)
        .map(|p| p.to_ascii_lowercase())
        .collect();
    toks.sort();
    toks.dedup();
    toks
}

fn publication_risk(input: &AdvocateInput) -> f32 {
    let mut risk = 0.15 * input.audience_simulation;
    let markers = [
        "post",
        "comment",
        "publish",
        "tweet",
        "reply publicly",
        "submit",
        "announce",
    ];
    for s in input
        .recent_activity
        .iter()
        .chain(input.conversation.iter())
    {
        let lower = s.text.to_ascii_lowercase();
        for m in markers {
            if lower.contains(m) {
                risk += 0.12;
            }
        }
    }
    risk.clamp(0.0, 1.0)
}

fn classify_verdict_language(text: &str) -> Alignment {
    let l = text.to_ascii_lowercase();
    let neg = [
        "not align",
        "misalign",
        "diverge",
        "off course",
        "betrays",
        "against",
    ];
    let pos = ["align", "serves", "on course", "consistent", "honors"];
    let neg_hit = neg.iter().any(|p| l.contains(p));
    let pos_hit = pos.iter().any(|p| l.contains(p));
    match (pos_hit, neg_hit) {
        (true, false) => Alignment::Aligned,
        (false, true) => Alignment::Divergent,
        (true, true) => Alignment::Mixed,
        _ => Alignment::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input(now: u64) -> AdvocateInput {
        AdvocateInput {
            values: vec![EvidenceSnippet {
                kind: SnippetKind::Value,
                text: "build elegant products with integrity and craft".into(),
            }],
            recent_activity: vec![EvidenceSnippet {
                kind: SnippetKind::RecentActivity,
                text: "reviewed product design notes on simplicity and craft".into(),
            }],
            conversation: vec![EvidenceSnippet {
                kind: SnippetKind::Conversation,
                text: "colleague asked about shipping timeline".into(),
            }],
            emotion_label: "interest".into(),
            emotion_intensity: 0.6,
            hexagram: "49".into(),
            audience_simulation: 0.4,
            budget_pressure: 0.1,
            thinking_active: true,
            now,
            correlation_id: "task-1".into(),
            agent_id: "ferricula-stevejobs".into(),
        }
    }

    #[test]
    fn authority_is_always_zero() {
        assert!(AdvocateAuthority::ZERO.is_zero());
        assert!(!AdvocateAuthority::ZERO.may_invoke_tools);
        let tick = review_mechanical(&sample_input(1000), &AdvocatePolicy::default());
        match tick {
            AdvocateTick::Reviewed(r) => {
                assert!(r.asserts_zero_authority());
                assert_eq!(r.authority, AdvocateAuthority::ZERO);
            }
            other => panic!("expected review, got {other:?}"),
        }
    }

    #[test]
    fn mechanical_review_is_deterministic() {
        let input = sample_input(2000);
        let policy = AdvocatePolicy::default();
        let a = review_mechanical(&input, &policy);
        let b = review_mechanical(&input, &policy);
        assert_eq!(a, b);
    }

    #[test]
    fn dormant_when_not_thinking() {
        let mut input = sample_input(1);
        input.thinking_active = false;
        let tick = review_mechanical(&input, &AdvocatePolicy::default());
        match tick {
            AdvocateTick::Skipped { reason, .. } => {
                assert!(reason.contains("dormant") || reason.contains("inactive"));
            }
            AdvocateTick::Reviewed(_) => panic!("should skip"),
        }
    }

    #[test]
    fn high_budget_sets_prefer_cheap_flag() {
        let mut input = sample_input(1);
        input.budget_pressure = 0.92;
        assert!(input.should_skip_model_review());
        let tick = review_mechanical(&input, &AdvocatePolicy::default());
        let AdvocateTick::Reviewed(r) = tick else {
            panic!("expected review");
        };
        assert!(r.prefer_cheap_or_skip_model);
        assert!(r.warnings.iter().any(|w| w.code == "budget_critical"));
    }

    #[test]
    fn history_write_defer_at_75() {
        let mut input = sample_input(1);
        input.budget_pressure = 0.8;
        let policy = AdvocatePolicy::default();
        assert!(should_defer_history_write(&input, &policy));
        input.budget_pressure = 0.5;
        assert!(!should_defer_history_write(&input, &policy));
    }

    #[test]
    fn expiry_and_provenance() {
        let policy = AdvocatePolicy {
            ttl_secs: 120,
            ..AdvocatePolicy::default()
        };
        let input = sample_input(10_000);
        let AdvocateTick::Reviewed(r) = review_mechanical(&input, &policy) else {
            panic!("review");
        };
        assert_eq!(r.expires_at, 10_120);
        assert!(!r.is_expired(10_119));
        assert!(r.is_expired(10_120));
        assert_eq!(r.provenance.source, AdvocateSource::Mechanical);
        assert_eq!(r.provenance.agent_id, "ferricula-stevejobs");
        assert!(r.provenance.input_fingerprint.starts_with("adv-"));
        assert_eq!(r.provenance.correlation_id, "task-1");
    }

    #[test]
    fn snippet_bounds_enforced() {
        let mut input = sample_input(1);
        input.values = (0..20)
            .map(|i| EvidenceSnippet {
                kind: SnippetKind::Value,
                text: format!("value goal number {i} with enough letters"),
            })
            .collect();
        input.values[0].text = "x".repeat(2000);
        let s = input.sanitized();
        assert!(s.values.len() <= MAX_SNIPPETS);
        assert!(s.values[0].text.chars().count() <= MAX_SNIPPET_CHARS);
    }

    #[test]
    fn parse_wants_verdict_two_lines() {
        let text = "WANTS: ship the elegant thing\nVERDICT: aligned with craft values\n";
        let (w, v) = parse_wants_verdict(text).unwrap();
        assert!(w.contains("elegant"));
        assert!(v.contains("aligned"));
        let review = AdvocateReview::from_parsed(&sample_input(5), w, v, 300);
        assert!(review.asserts_zero_authority());
        assert_eq!(review.provenance.source, AdvocateSource::ModelParsed);
        assert_eq!(review.observation.alignment, Alignment::Aligned);
    }

    #[test]
    fn parse_rejects_incomplete_envelope() {
        assert!(parse_wants_verdict("WANTS: only one line").is_none());
        assert!(parse_wants_verdict("").is_none());
    }

    #[test]
    fn divergent_path_emits_warnings_and_questions() {
        let mut input = sample_input(1);
        input.recent_activity = vec![EvidenceSnippet {
            kind: SnippetKind::RecentActivity,
            text: "decided to publish announce post comment spam for clout".into(),
        }];
        input.values = vec![EvidenceSnippet {
            kind: SnippetKind::Value,
            text: "privacy silence craft restraint integrity".into(),
        }];
        input.audience_simulation = 0.9;
        let AdvocateTick::Reviewed(r) = review_mechanical(&input, &AdvocatePolicy::default())
        else {
            panic!("review");
        };
        assert!(matches!(
            r.observation.alignment,
            Alignment::Divergent | Alignment::Mixed
        ));
        assert!(!r.warnings.is_empty());
        assert!(!r.questions.is_empty());
        // Still cannot compel action
        assert!(r.asserts_zero_authority());
    }

    #[test]
    fn unknown_with_empty_evidence() {
        let mut input = sample_input(1);
        input.values.clear();
        input.recent_activity.clear();
        input.conversation.clear();
        let AdvocateTick::Reviewed(r) = review_mechanical(&input, &AdvocatePolicy::default())
        else {
            panic!("review");
        };
        assert_eq!(r.observation.alignment, Alignment::Unknown);
    }

    #[test]
    fn policy_validation() {
        let mut p = AdvocatePolicy::default();
        p.validate().unwrap();
        p.interval_secs = 1;
        assert!(p.validate().is_err());
    }

    #[test]
    fn preamble_forbids_tools_and_compulsion() {
        let p = advocate_system_preamble();
        assert!(p.contains("no tools") || p.contains("cannot publish"));
        assert!(p.contains("WANTS:"));
        assert!(p.contains("VERDICT:"));
    }

    #[test]
    fn serde_roundtrip_review() {
        let AdvocateTick::Reviewed(r) =
            review_mechanical(&sample_input(42), &AdvocatePolicy::default())
        else {
            panic!("review");
        };
        let json = serde_json::to_string_pretty(&r).unwrap();
        let back: AdvocateReview = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
        assert!(!json.contains("may_invoke_tools\":true"));
    }

    #[test]
    fn audience_simulation_raises_deliberation_depth() {
        let mut low = sample_input(1);
        low.audience_simulation = 0.1;
        let mut high = sample_input(1);
        high.audience_simulation = 0.95;
        let AdvocateTick::Reviewed(a) = review_mechanical(&low, &AdvocatePolicy::default()) else {
            panic!();
        };
        let AdvocateTick::Reviewed(b) = review_mechanical(&high, &AdvocatePolicy::default()) else {
            panic!();
        };
        assert!(b.suggested_deliberation_depth > a.suggested_deliberation_depth);
    }

    #[test]
    fn review_never_looks_like_a_command_api() {
        // Guardrail: public types expose no execute/run_tool/publish methods.
        // Presence of zero authority + advisory language is the contract.
        let AdvocateTick::Reviewed(r) =
            review_mechanical(&sample_input(1), &AdvocatePolicy::default())
        else {
            panic!();
        };
        assert!(r.observation.rationale.contains("Advisory only"));
        assert!(!r
            .observation
            .rationale
            .to_ascii_lowercase()
            .contains("must reply"));
    }
}
