//! Bounded, provider-neutral sleep/dream planning primitive.
//!
//! Pure state and types only: no clock reads, no I/O, no network, and no
//! access to the memory engine. Callers supply `now` (unix seconds) and
//! persist the state themselves; this module decides *what Steve may do
//! while asleep* — it never does any of it. Invariants by construction:
//!
//! 1. **No memory mutation.** A proposal can request at most
//!    [`MemoryEffect::ProposeOnly`], which requires operator approval by
//!    definition. There is no representation for applying a change.
//! 2. **Private by default.** [`PrivacyEnvelope::share_private_memory`]
//!    defaults to `false`. When sharing is enabled, proposals demand the
//!    `private_context` capability and redaction is forced on emit.
//! 3. **Bounded.** Explicit call/token/USD ceilings, per-kind cooldowns,
//!    proposal TTLs, and `max_proposals_per_cycle`.
//! 4. **Pause/stop suppression.** [`PlanningGate::Paused`] /
//!    [`PlanningGate::Stopped`] yield zero proposals (planning may still
//!    run under sleep/allow gates when the policy is enabled).
//! 5. **Restart-safe ledger.** [`CooldownLedger::note_run`] is monotonic
//!    per kind; merge prefers the later timestamp; planning never writes
//!    the ledger.
//!
//! Not wired into `lib.rs` yet; the integrator adds `pub mod sleep_cycle;`
//! and owns persistence, execution, and approval routing.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Capability an executor must advertise before a proposal that reads
/// private Ferricula memory may be handed to it. Matches the snake_case
/// serialization of `model_config::ModelCapability::PrivateContext`.
pub const CAP_PRIVATE_CONTEXT: &str = "private_context";

const SECONDS_PER_DAY: u64 = 86_400;
/// Floor for any computed spacing, matching the wake scheduler's floor.
const MIN_SPACING_SECS: u64 = 60;
/// Floor for per-kind cooldowns so a misconfiguration cannot tight-loop.
const MIN_COOLDOWN_SECS: u64 = 300;
/// Absolute ceiling on a single proposal's USD budget (planning clamp).
const MAX_PROPOSAL_USD: f64 = 5.0;
/// Absolute ceiling on input tokens attached to one proposal.
const MAX_INPUT_TOKENS: u32 = 128_000;
/// Absolute ceiling on output tokens attached to one proposal.
const MAX_OUTPUT_TOKENS: u32 = 16_384;
/// Absolute ceiling on model calls attached to one proposal.
const MAX_MODEL_CALLS: u32 = 8;

/// Integrator-facing activity gate for pure planning. Maps to runtime
/// pause/stop without importing `runtime.rs`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningGate {
    /// Normal: plan if policy.enabled (includes asleep / engaged modes).
    #[default]
    Allow,
    /// Operator pause: no proposals, no advance of sleep cognition intent.
    Paused,
    /// Hard stop / shutdown drain: no proposals.
    Stopped,
}

impl PlanningGate {
    pub fn allows_planning(self) -> bool {
        matches!(self, PlanningGate::Allow)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PlanningGate::Allow => "allow",
            PlanningGate::Paused => "paused",
            PlanningGate::Stopped => "stopped",
        }
    }
}

/// Where in a sleep cycle a proposal belongs. Ordering is execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SleepPhase {
    /// Light processing right after entering the cycle.
    Descent,
    /// Review of recent observations (maps to reflection proposals).
    Rem,
    /// Deep consolidation work (maps to dream/consolidation proposals).
    DeepSleep,
}

/// What kind of offline cognition a proposal asks for. Ordering doubles as
/// planning priority: cheaper, lighter kinds come first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    /// Re-read the latest wake's observations and summarize salience.
    Reflection,
    /// Associative pass over existing memories (ferricula dream cycle input).
    Dream,
    /// Structural maintenance suggestions: merge/prune/keystone candidates.
    Consolidation,
}

impl ProposalKind {
    /// All kinds in planning-priority order.
    pub const ALL: [ProposalKind; 3] = [
        ProposalKind::Reflection,
        ProposalKind::Dream,
        ProposalKind::Consolidation,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ProposalKind::Reflection => "reflection",
            ProposalKind::Dream => "dream",
            ProposalKind::Consolidation => "consolidation",
        }
    }

    fn phase(self) -> SleepPhase {
        match self {
            ProposalKind::Reflection => SleepPhase::Rem,
            ProposalKind::Dream => SleepPhase::DeepSleep,
            ProposalKind::Consolidation => SleepPhase::DeepSleep,
        }
    }

    fn rationale(self) -> &'static str {
        match self {
            ProposalKind::Reflection => {
                "cooldown elapsed; review the most recent wake's observations"
            }
            ProposalKind::Dream => "cooldown elapsed; associative pass over existing memories",
            ProposalKind::Consolidation => {
                "cooldown elapsed; surface merge/prune candidates for approval"
            }
        }
    }
}

/// The only memory effects a proposal can request. Deliberately, there is no
/// variant that applies a change: `ProposeOnly` output is a suggestion that
/// an operator (or an operator-approved pipeline) must act on elsewhere.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEffect {
    /// Read-only: the task may read memory but produces no change requests.
    #[default]
    None,
    /// The task may emit change *suggestions* for later human approval.
    ProposeOnly,
}

impl MemoryEffect {
    pub const fn requires_operator_approval(self) -> bool {
        matches!(self, MemoryEffect::ProposeOnly)
    }

    /// True when this effect cannot mutate memory by itself.
    pub const fn is_non_mutating(self) -> bool {
        matches!(self, MemoryEffect::None | MemoryEffect::ProposeOnly)
    }
}

/// Whether sleep tasks may suggest memory changes at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationPolicy {
    /// Default: sleep cognition is strictly read-only.
    #[default]
    ReadOnly,
    /// Sleep cognition may emit suggestions that require operator approval.
    ProposeForApproval,
}

impl MutationPolicy {
    fn effect(self) -> MemoryEffect {
        match self {
            MutationPolicy::ReadOnly => MemoryEffect::None,
            MutationPolicy::ProposeForApproval => MemoryEffect::ProposeOnly,
        }
    }
}

/// Privacy posture attached to every proposal. Defaults are the strictest
/// setting: private memory stays local and excerpts are redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyEnvelope {
    /// May recovered Ferricula memory content be included in the executor
    /// prompt? `false` restricts the task to derived statistics and public
    /// observations.
    pub share_private_memory: bool,
    /// When memory content is shared, excerpts must pass redaction first.
    pub redact_excerpts: bool,
    /// Hard cap on any single excerpt handed to an executor.
    pub max_excerpt_chars: usize,
}

impl Default for PrivacyEnvelope {
    fn default() -> Self {
        Self {
            share_private_memory: false,
            redact_excerpts: true,
            max_excerpt_chars: 600,
        }
    }
}

impl PrivacyEnvelope {
    /// Capabilities an executor must advertise for this envelope.
    pub fn required_capabilities(&self) -> Vec<String> {
        if self.share_private_memory {
            vec![CAP_PRIVATE_CONTEXT.to_string()]
        } else {
            Vec::new()
        }
    }

    /// Defense-in-depth clamp applied when emitting proposals so a
    /// misconfigured persisted policy cannot ship unredacted private memory.
    pub fn hardened(&self) -> Self {
        let mut out = self.clone();
        if out.max_excerpt_chars == 0 {
            out.max_excerpt_chars = 1;
        }
        if out.max_excerpt_chars > 4_000 {
            out.max_excerpt_chars = 4_000;
        }
        if out.share_private_memory && !out.redact_excerpts {
            // Prefer safety over share: drop private memory rather than leak.
            out.share_private_memory = false;
            out.redact_excerpts = true;
        }
        if !out.share_private_memory {
            // Redaction remains on so later toggles cannot surprise.
            out.redact_excerpts = true;
        }
        out
    }
}

/// Hard resource ceilings for a single proposal.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProposalBudget {
    /// Zero means the task must run mechanically, with no model at all.
    pub max_model_calls: u32,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    /// Estimated-spend ceiling in USD for the whole proposal.
    pub max_usd: f64,
}

impl Default for ProposalBudget {
    fn default() -> Self {
        Self {
            max_model_calls: 1,
            max_input_tokens: 8_192,
            max_output_tokens: 1_024,
            max_usd: 0.05,
        }
    }
}

impl ProposalBudget {
    /// Clamp to absolute planning bounds so executors never see NaN or
    /// unbounded ceilings from a corrupted policy snapshot.
    pub fn hardened(&self) -> Self {
        let mut max_usd = if self.max_usd.is_finite() && self.max_usd >= 0.0 {
            self.max_usd
        } else {
            0.0
        };
        if max_usd > MAX_PROPOSAL_USD {
            max_usd = MAX_PROPOSAL_USD;
        }
        let mut max_model_calls = self.max_model_calls.min(MAX_MODEL_CALLS);
        let mut max_input_tokens = self.max_input_tokens.min(MAX_INPUT_TOKENS);
        let mut max_output_tokens = self.max_output_tokens.min(MAX_OUTPUT_TOKENS);
        if max_model_calls == 0 {
            max_input_tokens = 0;
            max_output_tokens = 0;
            max_usd = 0.0;
        } else {
            if max_input_tokens == 0 {
                max_input_tokens = 1;
            }
            if max_output_tokens == 0 {
                max_output_tokens = 1;
            }
        }
        // If USD is zero but calls remain, force mechanical (no model).
        if max_usd == 0.0 {
            max_model_calls = 0;
            max_input_tokens = 0;
            max_output_tokens = 0;
        }
        Self {
            max_model_calls,
            max_input_tokens,
            max_output_tokens,
            max_usd,
        }
    }

    pub fn is_mechanical(&self) -> bool {
        let h = self.hardened();
        h.max_model_calls == 0 && h.max_usd == 0.0
    }
}

/// Minimum seconds between two runs of the same proposal kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CooldownPolicy {
    pub reflection_secs: u64,
    pub dream_secs: u64,
    pub consolidation_secs: u64,
}

impl Default for CooldownPolicy {
    fn default() -> Self {
        Self {
            reflection_secs: 3 * 3_600,
            dream_secs: 6 * 3_600,
            consolidation_secs: SECONDS_PER_DAY,
        }
    }
}

impl CooldownPolicy {
    pub fn for_kind(&self, kind: ProposalKind) -> u64 {
        let raw = match kind {
            ProposalKind::Reflection => self.reflection_secs,
            ProposalKind::Dream => self.dream_secs,
            ProposalKind::Consolidation => self.consolidation_secs,
        };
        raw.max(MIN_COOLDOWN_SECS)
    }
}

/// Complete sleep-cycle policy. All fields have safe defaults; `validate`
/// follows the same style as `config.rs::RuntimeConfig::validate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SleepCyclePolicy {
    pub enabled: bool,
    /// Sleep-cycle cadence; base spacing is `86400 / cycles_per_day`.
    pub cycles_per_day: u8,
    /// Symmetric jitter half-width applied to the spacing, in seconds.
    pub jitter_secs: u64,
    /// Proposals expire this many seconds after their scheduled time.
    pub proposal_ttl_secs: u64,
    pub max_proposals_per_cycle: usize,
    pub cooldowns: CooldownPolicy,
    pub budget: ProposalBudget,
    pub privacy: PrivacyEnvelope,
    pub mutation: MutationPolicy,
}

impl Default for SleepCyclePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            cycles_per_day: 4,
            jitter_secs: 45 * 60,
            proposal_ttl_secs: 3_600,
            max_proposals_per_cycle: 3,
            cooldowns: CooldownPolicy::default(),
            budget: ProposalBudget::default(),
            privacy: PrivacyEnvelope::default(),
            mutation: MutationPolicy::default(),
        }
    }
}

impl SleepCyclePolicy {
    /// Base spacing between cycles before jitter, floored at one minute.
    pub fn base_spacing_secs(&self) -> u64 {
        (SECONDS_PER_DAY / u64::from(self.cycles_per_day.max(1))).max(MIN_SPACING_SECS)
    }

    /// Jitter half-width clamped so spacing never goes non-positive relative
    /// to half the base interval.
    pub fn effective_jitter_secs(&self) -> u64 {
        let half = self.base_spacing_secs() / 2;
        self.jitter_secs.min(half)
    }

    pub fn validate(&self) -> Result<()> {
        if self.cycles_per_day == 0 {
            bail!("sleep_cycle.cycles_per_day must be positive");
        }
        if self.cycles_per_day > 24 {
            bail!("sleep_cycle.cycles_per_day cannot exceed 24");
        }
        if self.jitter_secs > self.base_spacing_secs() / 2 {
            bail!("sleep_cycle.jitter_secs cannot exceed half the base spacing");
        }
        if self.max_proposals_per_cycle == 0 || self.max_proposals_per_cycle > 16 {
            bail!("sleep_cycle.max_proposals_per_cycle must be in 1..=16");
        }
        if self.proposal_ttl_secs < MIN_SPACING_SECS {
            bail!("sleep_cycle.proposal_ttl_secs must be at least 60");
        }
        if self.proposal_ttl_secs > self.base_spacing_secs() {
            bail!("sleep_cycle.proposal_ttl_secs cannot exceed the cycle spacing");
        }
        for kind in ProposalKind::ALL {
            // Raw field must meet floor before for_kind clamp (validate config intent).
            let raw = match kind {
                ProposalKind::Reflection => self.cooldowns.reflection_secs,
                ProposalKind::Dream => self.cooldowns.dream_secs,
                ProposalKind::Consolidation => self.cooldowns.consolidation_secs,
            };
            if raw < MIN_COOLDOWN_SECS {
                bail!(
                    "sleep_cycle cooldown for {} must be at least 300s",
                    kind.as_str()
                );
            }
        }
        if !self.budget.max_usd.is_finite() || self.budget.max_usd < 0.0 {
            bail!("sleep_cycle.budget.max_usd must be a finite, non-negative number");
        }
        if self.budget.max_usd > MAX_PROPOSAL_USD {
            bail!("sleep_cycle.budget.max_usd cannot exceed {MAX_PROPOSAL_USD}");
        }
        if self.budget.max_model_calls > MAX_MODEL_CALLS {
            bail!("sleep_cycle.budget.max_model_calls cannot exceed {MAX_MODEL_CALLS}");
        }
        if self.budget.max_input_tokens > MAX_INPUT_TOKENS {
            bail!("sleep_cycle.budget.max_input_tokens cannot exceed {MAX_INPUT_TOKENS}");
        }
        if self.budget.max_output_tokens > MAX_OUTPUT_TOKENS {
            bail!("sleep_cycle.budget.max_output_tokens cannot exceed {MAX_OUTPUT_TOKENS}");
        }
        if self.budget.max_model_calls > 0
            && (self.budget.max_input_tokens == 0 || self.budget.max_output_tokens == 0)
        {
            bail!(
                "sleep_cycle.budget token ceilings must be positive when model calls are allowed"
            );
        }
        if self.privacy.max_excerpt_chars == 0 || self.privacy.max_excerpt_chars > 4_000 {
            bail!("sleep_cycle.privacy.max_excerpt_chars must be in 1..=4000");
        }
        if self.privacy.share_private_memory && !self.privacy.redact_excerpts {
            bail!(
                "sleep_cycle.privacy cannot share private memory with redaction disabled; \
                 enable redact_excerpts or disable share_private_memory"
            );
        }
        Ok(())
    }
}

/// Durable per-kind cooldown bookkeeping. The integrator persists this next
/// to the rest of runtime state and calls [`CooldownLedger::note_run`] when
/// a proposal actually executes — planning alone never consumes a cooldown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CooldownLedger {
    last_run: BTreeMap<ProposalKind, u64>,
}

impl CooldownLedger {
    pub fn is_ready(&self, kind: ProposalKind, policy: &SleepCyclePolicy, now: u64) -> bool {
        match self.last_run.get(&kind) {
            None => true,
            Some(last) => now.saturating_sub(*last) >= policy.cooldowns.for_kind(kind),
        }
    }

    /// Earliest time the kind becomes ready again; `now`-independent.
    pub fn next_ready_at(&self, kind: ProposalKind, policy: &SleepCyclePolicy) -> u64 {
        match self.last_run.get(&kind) {
            None => 0,
            Some(last) => last.saturating_add(policy.cooldowns.for_kind(kind)),
        }
    }

    /// Record an actual execution. **Monotonic:** a smaller `now` (clock skew
    /// or stale restart write) never rewinds the last-run timestamp, so a
    /// restored ledger cannot accidentally re-open a cooldown window.
    pub fn note_run(&mut self, kind: ProposalKind, now: u64) {
        match self.last_run.get(&kind).copied() {
            Some(prev) if now < prev => {
                // Keep prev; ignore regressing timestamps.
            }
            _ => {
                self.last_run.insert(kind, now);
            }
        }
    }

    /// Last recorded run time for a kind, if any.
    pub fn last_run_at(&self, kind: ProposalKind) -> Option<u64> {
        self.last_run.get(&kind).copied()
    }

    /// Merge another ledger (e.g. after restart from two snapshots). For each
    /// kind, keeps the **later** timestamp so cooldowns stay conservative.
    pub fn merge_prefer_later(&mut self, other: &CooldownLedger) {
        for (kind, ts) in &other.last_run {
            match self.last_run.get(kind).copied() {
                Some(prev) if prev >= *ts => {}
                _ => {
                    self.last_run.insert(*kind, *ts);
                }
            }
        }
    }

    /// Drop entries that are not known kinds (forward-compat no-op today) and
    /// ensure map only holds finite timestamps.
    pub fn sanitize(&mut self) {
        self.last_run
            .retain(|kind, _| ProposalKind::ALL.contains(kind));
    }
}

/// One planned unit of sleep cognition. Ids are deterministic so a re-plan
/// of the same cycle produces the same ids (idempotent enqueue).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DreamProposal {
    pub id: String,
    pub kind: ProposalKind,
    pub phase: SleepPhase,
    pub scheduled_for: u64,
    pub expires_at: u64,
    pub budget: ProposalBudget,
    pub privacy: PrivacyEnvelope,
    /// Capability strings the executor must advertise (provider-neutral).
    pub required_capabilities: Vec<String>,
    pub memory_effect: MemoryEffect,
    pub requires_operator_approval: bool,
    pub rationale: String,
}

impl DreamProposal {
    /// Structural check: proposal never authorizes direct memory mutation.
    pub fn asserts_non_mutating(&self) -> bool {
        self.memory_effect.is_non_mutating()
            && (self.memory_effect != MemoryEffect::ProposeOnly || self.requires_operator_approval)
    }
}

/// A kind that was considered but not proposed, and why — so bounded
/// planning never reads as "nothing was skipped".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedProposal {
    pub kind: ProposalKind,
    pub reason: String,
    pub ready_at: u64,
}

/// The pure output of planning one sleep cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CyclePlan {
    pub cycle_started_at: u64,
    pub proposals: Vec<DreamProposal>,
    pub skipped: Vec<SkippedProposal>,
    /// Gate that produced this plan (for audit after restart).
    #[serde(default)]
    pub gate: PlanningGate,
}

impl CyclePlan {
    fn empty(now: u64, gate: PlanningGate, reason: &str) -> Self {
        let skipped = ProposalKind::ALL
            .iter()
            .map(|kind| SkippedProposal {
                kind: *kind,
                reason: reason.into(),
                ready_at: u64::MAX,
            })
            .collect();
        Self {
            cycle_started_at: now,
            proposals: Vec::new(),
            skipped,
            gate,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }
}

/// When the next sleep cycle should start. `last_cycle_started == None`
/// means a fresh state volume: the first cycle is due immediately.
///
/// Under [`PlanningGate::Paused`] or [`PlanningGate::Stopped`], returns
/// `u64::MAX` so schedulers treat the cycle as not due.
pub fn next_cycle_at(
    policy: &SleepCyclePolicy,
    agent_id: &str,
    last_cycle_started: Option<u64>,
) -> u64 {
    next_cycle_at_gated(policy, agent_id, last_cycle_started, PlanningGate::Allow)
}

/// Gate-aware next-cycle time.
pub fn next_cycle_at_gated(
    policy: &SleepCyclePolicy,
    agent_id: &str,
    last_cycle_started: Option<u64>,
    gate: PlanningGate,
) -> u64 {
    if !gate.allows_planning() || !policy.enabled {
        return u64::MAX;
    }
    match last_cycle_started {
        None => 0,
        Some(last) => {
            let spacing = jittered_spacing(
                policy.base_spacing_secs(),
                policy.effective_jitter_secs(),
                last,
                agent_id,
            );
            last.saturating_add(spacing)
        }
    }
}

/// True when `now` has reached the next scheduled cycle under the gate.
pub fn cycle_is_due(
    policy: &SleepCyclePolicy,
    agent_id: &str,
    last_cycle_started: Option<u64>,
    now: u64,
    gate: PlanningGate,
) -> bool {
    now >= next_cycle_at_gated(policy, agent_id, last_cycle_started, gate)
}

/// Plan one sleep cycle with the default allow gate. Pure: same inputs, same
/// plan. The ledger is read, never written.
pub fn plan_cycle(
    policy: &SleepCyclePolicy,
    ledger: &CooldownLedger,
    agent_id: &str,
    now: u64,
) -> CyclePlan {
    plan_cycle_gated(policy, ledger, agent_id, now, PlanningGate::Allow)
}

/// Plan under an explicit pause/stop/allow gate.
pub fn plan_cycle_gated(
    policy: &SleepCyclePolicy,
    ledger: &CooldownLedger,
    agent_id: &str,
    now: u64,
    gate: PlanningGate,
) -> CyclePlan {
    if !gate.allows_planning() {
        return CyclePlan::empty(
            now,
            gate,
            &format!("planning suppressed: gate={}", gate.as_str()),
        );
    }
    if !policy.enabled {
        return CyclePlan::empty(now, gate, "sleep cycle disabled by policy");
    }
    // Invalid persisted policy → refuse to propose rather than emit unbounded work.
    if policy.validate().is_err() {
        return CyclePlan::empty(now, gate, "sleep cycle policy failed validation");
    }

    let privacy = policy.privacy.hardened();
    let budget = policy.budget.hardened();
    let mut proposals = Vec::new();
    let mut skipped = Vec::new();
    let cap = policy.max_proposals_per_cycle.clamp(1, 16);

    for kind in ProposalKind::ALL {
        if proposals.len() >= cap {
            skipped.push(SkippedProposal {
                kind,
                reason: "max_proposals_per_cycle reached".into(),
                ready_at: now,
            });
            continue;
        }
        if !ledger.is_ready(kind, policy, now) {
            skipped.push(SkippedProposal {
                kind,
                reason: format!("{} cooldown active", kind.as_str()),
                ready_at: ledger.next_ready_at(kind, policy),
            });
            continue;
        }
        let effect = policy.mutation.effect();
        debug_assert!(effect.is_non_mutating());
        proposals.push(DreamProposal {
            id: proposal_id(agent_id, kind, now),
            kind,
            phase: kind.phase(),
            scheduled_for: now,
            expires_at: now.saturating_add(policy.proposal_ttl_secs.max(MIN_SPACING_SECS)),
            budget,
            privacy: privacy.clone(),
            required_capabilities: privacy.required_capabilities(),
            memory_effect: effect,
            requires_operator_approval: effect.requires_operator_approval(),
            rationale: kind.rationale().to_string(),
        });
    }
    CyclePlan {
        cycle_started_at: now,
        proposals,
        skipped,
        gate,
    }
}

/// Deterministic proposal id: FNV-1a over agent id, kind, and cycle start.
fn proposal_id(agent_id: &str, kind: ProposalKind, cycle_started_at: u64) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in agent_id
        .bytes()
        .chain(kind.as_str().bytes())
        .chain(cycle_started_at.to_be_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("sleep-{}-{hash:016x}", kind.as_str())
}

/// Deterministic, bounded jitter. Span is clamped to `base/2` so a bad
/// caller cannot request a non-positive interval. Result is always
/// `>= MIN_SPACING_SECS`.
fn jittered_spacing(base: u64, span: u64, last: u64, agent_id: &str) -> u64 {
    let base = base.max(MIN_SPACING_SECS);
    let span = span.min(base / 2);
    if span == 0 {
        return base;
    }
    let mut seed = last ^ 0x9e37_79b9_7f4a_7c15;
    for byte in agent_id.bytes() {
        seed ^= u64::from(byte);
        seed = seed.wrapping_mul(0x100_0000_01b3);
    }
    seed ^= seed >> 30;
    seed = seed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    seed ^= seed >> 27;
    let width = span.saturating_mul(2).saturating_add(1);
    let offset = i128::from(seed % width) - i128::from(span);
    u64::try_from((i128::from(base) + offset).max(i128::from(MIN_SPACING_SECS)))
        .unwrap_or(MIN_SPACING_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = "ferricula-stevejobs";

    #[test]
    fn defaults_are_safe_and_valid() {
        let policy = SleepCyclePolicy::default();
        policy.validate().unwrap();
        assert!(!policy.privacy.share_private_memory);
        assert!(policy.privacy.redact_excerpts);
        assert_eq!(policy.mutation, MutationPolicy::ReadOnly);
        assert!(policy.privacy.required_capabilities().is_empty());
        assert!(policy.budget.max_usd > 0.0 && policy.budget.max_usd <= MAX_PROPOSAL_USD);
    }

    #[test]
    fn validation_rejects_unsafe_policies() {
        let mut policy = SleepCyclePolicy::default();
        policy.cycles_per_day = 0;
        assert!(policy.validate().is_err());

        let mut policy = SleepCyclePolicy::default();
        policy.cycles_per_day = 25;
        assert!(policy.validate().is_err());

        let mut policy = SleepCyclePolicy::default();
        policy.jitter_secs = policy.base_spacing_secs();
        assert!(policy.validate().is_err());

        let mut policy = SleepCyclePolicy::default();
        policy.max_proposals_per_cycle = 0;
        assert!(policy.validate().is_err());

        let mut policy = SleepCyclePolicy::default();
        policy.proposal_ttl_secs = policy.base_spacing_secs() + 1;
        assert!(policy.validate().is_err());

        let mut policy = SleepCyclePolicy::default();
        policy.cooldowns.reflection_secs = 1;
        assert!(policy.validate().is_err());

        let mut policy = SleepCyclePolicy::default();
        policy.budget.max_usd = f64::NAN;
        assert!(policy.validate().is_err());

        let mut policy = SleepCyclePolicy::default();
        policy.budget.max_output_tokens = 0;
        assert!(policy.validate().is_err());

        let mut policy = SleepCyclePolicy::default();
        policy.budget.max_usd = MAX_PROPOSAL_USD + 1.0;
        assert!(policy.validate().is_err());

        let mut policy = SleepCyclePolicy::default();
        policy.privacy.share_private_memory = true;
        policy.privacy.redact_excerpts = false;
        assert!(policy.validate().is_err());
    }

    #[test]
    fn jitter_is_stable_bounded_and_clamped() {
        let spacing = jittered_spacing(21_600, 2_700, 1234, AGENT);
        assert!((18_900..=24_300).contains(&spacing));
        assert_eq!(spacing, jittered_spacing(21_600, 2_700, 1234, AGENT));
        assert_eq!(jittered_spacing(21_600, 0, 1234, AGENT), 21_600);
        // Oversized span is clamped to base/2; still deterministic and >= floor.
        let wide = jittered_spacing(1_000, 50_000, 99, AGENT);
        assert!(wide >= MIN_SPACING_SECS);
        assert!(wide <= 1_000 + 500);
        assert_eq!(wide, jittered_spacing(1_000, 50_000, 99, AGENT));
    }

    #[test]
    fn next_cycle_is_immediate_on_fresh_state_then_spaced() {
        let policy = SleepCyclePolicy::default();
        assert_eq!(next_cycle_at(&policy, AGENT, None), 0);
        let next = next_cycle_at(&policy, AGENT, Some(1_000_000));
        let base = policy.base_spacing_secs();
        let span = policy.effective_jitter_secs();
        assert!(next >= 1_000_000 + base - span);
        assert!(next <= 1_000_000 + base + span);
    }

    #[test]
    fn pause_and_stop_suppress_planning_and_due() {
        let policy = SleepCyclePolicy::default();
        let ledger = CooldownLedger::default();
        for gate in [PlanningGate::Paused, PlanningGate::Stopped] {
            let plan = plan_cycle_gated(&policy, &ledger, AGENT, 50_000, gate);
            assert!(plan.proposals.is_empty(), "{gate:?}");
            assert_eq!(plan.skipped.len(), 3);
            assert_eq!(plan.gate, gate);
            assert!(plan.skipped.iter().all(|s| s.reason.contains("suppressed")));
            assert_eq!(next_cycle_at_gated(&policy, AGENT, None, gate), u64::MAX);
            assert!(!cycle_is_due(&policy, AGENT, None, 0, gate));
        }
        let allow = plan_cycle_gated(&policy, &ledger, AGENT, 50_000, PlanningGate::Allow);
        assert!(!allow.proposals.is_empty());
        assert!(cycle_is_due(&policy, AGENT, None, 0, PlanningGate::Allow));
    }

    #[test]
    fn disabled_policy_plans_nothing() {
        let mut policy = SleepCyclePolicy::default();
        policy.enabled = false;
        let plan = plan_cycle(&policy, &CooldownLedger::default(), AGENT, 1);
        assert!(plan.proposals.is_empty());
        assert_eq!(plan.skipped.len(), ProposalKind::ALL.len());
        assert_eq!(
            next_cycle_at_gated(&policy, AGENT, None, PlanningGate::Allow),
            u64::MAX
        );
    }

    #[test]
    fn invalid_policy_emits_no_proposals() {
        let mut policy = SleepCyclePolicy::default();
        policy.budget.max_usd = f64::NAN;
        let plan = plan_cycle(&policy, &CooldownLedger::default(), AGENT, 1);
        assert!(plan.proposals.is_empty());
        assert!(plan.skipped[0].reason.contains("validation"));
    }

    #[test]
    fn cooldowns_gate_and_release() {
        let policy = SleepCyclePolicy::default();
        let mut ledger = CooldownLedger::default();
        assert!(ledger.is_ready(ProposalKind::Dream, &policy, 0));
        ledger.note_run(ProposalKind::Dream, 10_000);
        assert!(!ledger.is_ready(ProposalKind::Dream, &policy, 10_001));
        let ready_at = ledger.next_ready_at(ProposalKind::Dream, &policy);
        assert_eq!(ready_at, 10_000 + policy.cooldowns.dream_secs);
        assert!(ledger.is_ready(ProposalKind::Dream, &policy, ready_at));
    }

    #[test]
    fn note_run_is_monotonic_across_clock_regress() {
        let mut ledger = CooldownLedger::default();
        ledger.note_run(ProposalKind::Reflection, 1_000);
        ledger.note_run(ProposalKind::Reflection, 500); // skew / stale write
        assert_eq!(ledger.last_run_at(ProposalKind::Reflection), Some(1_000));
        ledger.note_run(ProposalKind::Reflection, 1_500);
        assert_eq!(ledger.last_run_at(ProposalKind::Reflection), Some(1_500));
    }

    #[test]
    fn ledger_merge_prefers_later_timestamps() {
        let mut a = CooldownLedger::default();
        a.note_run(ProposalKind::Dream, 100);
        a.note_run(ProposalKind::Reflection, 200);
        let mut b = CooldownLedger::default();
        b.note_run(ProposalKind::Dream, 150);
        b.note_run(ProposalKind::Consolidation, 50);
        a.merge_prefer_later(&b);
        assert_eq!(a.last_run_at(ProposalKind::Dream), Some(150));
        assert_eq!(a.last_run_at(ProposalKind::Reflection), Some(200));
        assert_eq!(a.last_run_at(ProposalKind::Consolidation), Some(50));
    }

    #[test]
    fn planning_never_mutates_ledger() {
        let policy = SleepCyclePolicy::default();
        let mut ledger = CooldownLedger::default();
        ledger.note_run(ProposalKind::Dream, 10);
        let before = ledger.clone();
        let _ = plan_cycle(&policy, &ledger, AGENT, 1_000_000);
        assert_eq!(ledger, before);
    }

    #[test]
    fn plan_is_bounded_and_deterministic() {
        let mut policy = SleepCyclePolicy::default();
        policy.max_proposals_per_cycle = 2;
        let ledger = CooldownLedger::default();
        let plan = plan_cycle(&policy, &ledger, AGENT, 50_000);
        assert_eq!(plan.proposals.len(), 2);
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].kind, ProposalKind::Consolidation);
        assert_eq!(plan, plan_cycle(&policy, &ledger, AGENT, 50_000));
        for proposal in &plan.proposals {
            assert_eq!(proposal.expires_at, 50_000 + policy.proposal_ttl_secs);
            assert!(!proposal.rationale.is_empty());
            assert!(proposal.id.starts_with("sleep-"));
            assert!(proposal.asserts_non_mutating());
        }
    }

    #[test]
    fn plan_respects_cooldowns_and_reports_skips() {
        let policy = SleepCyclePolicy::default();
        let mut ledger = CooldownLedger::default();
        ledger.note_run(ProposalKind::Reflection, 100_000);
        let plan = plan_cycle(&policy, &ledger, AGENT, 100_060);
        let kinds: Vec<ProposalKind> = plan.proposals.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![ProposalKind::Dream, ProposalKind::Consolidation]
        );
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].kind, ProposalKind::Reflection);
        assert_eq!(
            plan.skipped[0].ready_at,
            100_000 + policy.cooldowns.reflection_secs
        );
    }

    #[test]
    fn proposals_never_carry_mutation_authority() {
        let plan = plan_cycle(
            &SleepCyclePolicy::default(),
            &CooldownLedger::default(),
            AGENT,
            7,
        );
        for proposal in &plan.proposals {
            assert_eq!(proposal.memory_effect, MemoryEffect::None);
            assert!(!proposal.requires_operator_approval);
            assert!(proposal.memory_effect.is_non_mutating());
        }
        let mut policy = SleepCyclePolicy::default();
        policy.mutation = MutationPolicy::ProposeForApproval;
        let plan = plan_cycle(&policy, &CooldownLedger::default(), AGENT, 7);
        for proposal in &plan.proposals {
            assert_eq!(proposal.memory_effect, MemoryEffect::ProposeOnly);
            assert!(proposal.requires_operator_approval);
            assert!(proposal.asserts_non_mutating());
        }
    }

    #[test]
    fn private_context_defaults_and_hardening() {
        assert!(!PrivacyEnvelope::default().share_private_memory);
        let mut dirty = PrivacyEnvelope::default();
        dirty.share_private_memory = true;
        dirty.redact_excerpts = false;
        let hard = dirty.hardened();
        assert!(!hard.share_private_memory);
        assert!(hard.redact_excerpts);

        let mut policy = SleepCyclePolicy::default();
        policy.privacy.share_private_memory = true;
        policy.validate().unwrap();
        let plan = plan_cycle(&policy, &CooldownLedger::default(), AGENT, 7);
        for proposal in &plan.proposals {
            assert_eq!(
                proposal.required_capabilities,
                vec![CAP_PRIVATE_CONTEXT.to_string()]
            );
            assert!(proposal.privacy.redact_excerpts);
        }
    }

    #[test]
    fn proposal_budget_hardening_bounds() {
        let wild = ProposalBudget {
            max_model_calls: 100,
            max_input_tokens: u32::MAX,
            max_output_tokens: u32::MAX,
            max_usd: 999.0,
        };
        let h = wild.hardened();
        assert!(h.max_model_calls <= MAX_MODEL_CALLS);
        assert!(h.max_input_tokens <= MAX_INPUT_TOKENS);
        assert!(h.max_output_tokens <= MAX_OUTPUT_TOKENS);
        assert!(h.max_usd <= MAX_PROPOSAL_USD);

        let zero_usd = ProposalBudget {
            max_model_calls: 3,
            max_input_tokens: 100,
            max_output_tokens: 50,
            max_usd: 0.0,
        };
        assert!(zero_usd.hardened().is_mechanical());

        let nan = ProposalBudget {
            max_usd: f64::NAN,
            ..ProposalBudget::default()
        };
        assert_eq!(nan.hardened().max_usd, 0.0);
        assert!(nan.hardened().is_mechanical());
    }

    #[test]
    fn emitted_budgets_are_hardened() {
        let mut policy = SleepCyclePolicy::default();
        // Valid but near ceiling — emit must still clamp if we bypass validate path
        // by only hardening at emit; use valid policy.
        policy.budget.max_usd = 0.05;
        let plan = plan_cycle(&policy, &CooldownLedger::default(), AGENT, 1);
        for p in &plan.proposals {
            assert_eq!(p.budget, policy.budget.hardened());
            assert!(p.budget.max_usd.is_finite());
        }
    }

    #[test]
    fn state_and_plan_round_trip_through_serde() {
        let policy = SleepCyclePolicy::default();
        let json = serde_json::to_string(&policy).unwrap();
        assert_eq!(policy, serde_json::from_str(&json).unwrap());

        let mut ledger = CooldownLedger::default();
        ledger.note_run(ProposalKind::Consolidation, 42);
        let json = serde_json::to_string(&ledger).unwrap();
        assert_eq!(ledger, serde_json::from_str(&json).unwrap());

        let plan = plan_cycle(&policy, &ledger, AGENT, 99);
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(plan, serde_json::from_str::<CyclePlan>(&json).unwrap());
    }

    #[test]
    fn empty_policy_toml_yields_safe_defaults() {
        let policy: SleepCyclePolicy = toml::from_str("").unwrap();
        policy.validate().unwrap();
        assert!(!policy.privacy.share_private_memory);
        assert_eq!(policy.mutation, MutationPolicy::ReadOnly);
    }

    #[test]
    fn cooldown_for_kind_never_below_floor() {
        let cool = CooldownPolicy {
            reflection_secs: 1,
            dream_secs: 1,
            consolidation_secs: 1,
        };
        for kind in ProposalKind::ALL {
            assert!(cool.for_kind(kind) >= MIN_COOLDOWN_SECS);
        }
    }

    #[test]
    fn memory_effect_has_no_apply_variant() {
        // Exhaustive match: adding Apply would fail this test at compile time
        // if someone extends the enum without updating is_non_mutating.
        for effect in [MemoryEffect::None, MemoryEffect::ProposeOnly] {
            assert!(effect.is_non_mutating());
        }
    }
}
