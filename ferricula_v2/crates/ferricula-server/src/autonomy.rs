//! Bounded, provider-neutral **autonomy** state machine for Steve.
//!
//! Implements the V2 event-driven redesign of the arena think loop
//! (`research/original-ferricula/AUTONOMY_LOOP_SPEC.md`):
//!
//! - states: [`AutonomyState::Asleep`], [`Awake`](AutonomyState::Awake),
//!   [`Deliberating`](AutonomyState::Deliberating),
//!   [`Dreaming`](AutonomyState::Dreaming), [`Paused`](AutonomyState::Paused);
//! - typed [`WakeTrigger`]s and legal transitions;
//! - pause/stop precedence over all other events;
//! - bounded [`WorkLease`]s (time + step budgets);
//! - budget and conversational quiet-period **gates** (not random pop-ins);
//! - crash/restart recovery via durable [`AutonomySnapshot`];
//! - hard invariants: **no trigger compels a response or a write**.
//!
//! Pure state logic only: no network, no model calls, no memory I/O.
//! Integrators map outcomes onto the task worker and model router.
//!
//! Not wired into `lib.rs` yet.

use serde::{Deserialize, Serialize};

/// Default think-loop cadence from the arena (`THINK_INTERVAL = 45`).
pub const DEFAULT_TICK_SECS: u64 = 45;

/// Quiet window after user chat (`_CHAT_WINDOW_SECS = 30`).
pub const DEFAULT_QUIET_SECS: u64 = 30;

/// Silence before idle resume (`_CHAT_IDLE_SECS = 90`).
pub const DEFAULT_IDLE_RESUME_SECS: u64 = 90;

/// Default max wall-clock seconds for one work lease.
pub const DEFAULT_LEASE_SECS: u64 = 120;

/// Default max logical steps inside one lease.
pub const DEFAULT_LEASE_STEPS: u32 = 8;

/// High budget pressure at which model work is gated (arena ~0.90).
pub const DEFAULT_BUDGET_GATE: f32 = 0.90;

/// Autonomy phase. Names match the V2 acceptance diagram (Awake ≡ Engaged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyState {
    Asleep,
    Awake,
    Deliberating,
    Dreaming,
    Paused,
}

impl AutonomyState {
    pub const ALL: [AutonomyState; 5] = [
        AutonomyState::Asleep,
        AutonomyState::Awake,
        AutonomyState::Deliberating,
        AutonomyState::Dreaming,
        AutonomyState::Paused,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AutonomyState::Asleep => "asleep",
            AutonomyState::Awake => "awake",
            AutonomyState::Deliberating => "deliberating",
            AutonomyState::Dreaming => "dreaming",
            AutonomyState::Paused => "paused",
        }
    }

    pub fn is_working(self) -> bool {
        matches!(
            self,
            AutonomyState::Awake | AutonomyState::Deliberating | AutonomyState::Dreaming
        )
    }

    pub fn is_paused(self) -> bool {
        matches!(self, AutonomyState::Paused)
    }
}

/// Why the machine was asked to leave sleep or advance work.
///
/// Triggers are **stimuli**, never commands to speak or write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeTrigger {
    /// Direct social address / mention consideration ready.
    DirectMention { consideration_id: String },
    /// Operator or API wake control.
    ApiWake,
    /// Scheduler decided a wake is due.
    ScheduledWake,
    /// Quiet period ended and idle threshold elapsed.
    IdleResume { idle_secs: u64 },
    /// Operator explicitly started after pause/stop.
    OperatorStart,
    /// Queue still has deliberative work.
    ProcessInput,
    /// Idle threshold while deliberating → dream/consolidation window.
    IdleThresholdForDream { idle_secs: u64 },
    /// Dream/consolidation finished.
    DreamComplete,
    /// Task queue drained.
    QueueEmpty,
    /// Active work lease expired (time or steps).
    LeaseExpired { lease_id: u64 },
    /// Operator pause (soft).
    OperatorPause,
    /// Operator stop (hard; same sink as pause, recorded distinctly).
    OperatorStop,
    /// Periodic tick for lease/quiet evaluation.
    Tick,
}

impl WakeTrigger {
    pub fn kind_name(&self) -> &'static str {
        match self {
            WakeTrigger::DirectMention { .. } => "direct_mention",
            WakeTrigger::ApiWake => "api_wake",
            WakeTrigger::ScheduledWake => "scheduled_wake",
            WakeTrigger::IdleResume { .. } => "idle_resume",
            WakeTrigger::OperatorStart => "operator_start",
            WakeTrigger::ProcessInput => "process_input",
            WakeTrigger::IdleThresholdForDream { .. } => "idle_threshold_for_dream",
            WakeTrigger::DreamComplete => "dream_complete",
            WakeTrigger::QueueEmpty => "queue_empty",
            WakeTrigger::LeaseExpired { .. } => "lease_expired",
            WakeTrigger::OperatorPause => "operator_pause",
            WakeTrigger::OperatorStop => "operator_stop",
            WakeTrigger::Tick => "tick",
        }
    }

    /// True for operator halt intents (highest precedence).
    pub fn is_halt(&self) -> bool {
        matches!(self, WakeTrigger::OperatorPause | WakeTrigger::OperatorStop)
    }
}

/// Policy knobs for gates and leases (all pure data).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonomyPolicy {
    pub tick_secs: u64,
    pub quiet_secs: u64,
    pub idle_resume_secs: u64,
    pub lease_secs: u64,
    pub lease_steps: u32,
    /// Budget pressure ∈ [0,1] at/above which model work is refused.
    pub budget_gate: f32,
    /// Minimum idle seconds in Deliberating before Dreaming is legal.
    pub dream_idle_secs: u64,
    /// When true, quiet period blocks new leases (never random override).
    pub honor_quiet_period: bool,
}

impl Default for AutonomyPolicy {
    fn default() -> Self {
        Self {
            tick_secs: DEFAULT_TICK_SECS,
            quiet_secs: DEFAULT_QUIET_SECS,
            idle_resume_secs: DEFAULT_IDLE_RESUME_SECS,
            lease_secs: DEFAULT_LEASE_SECS,
            lease_steps: DEFAULT_LEASE_STEPS,
            budget_gate: DEFAULT_BUDGET_GATE,
            dream_idle_secs: DEFAULT_IDLE_RESUME_SECS,
            honor_quiet_period: true,
        }
    }
}

impl AutonomyPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.tick_secs == 0 {
            return Err("autonomy.tick_secs must be positive".into());
        }
        if self.lease_secs == 0 || self.lease_steps == 0 {
            return Err("autonomy lease bounds must be positive".into());
        }
        if !(0.0..=1.0).contains(&self.budget_gate) {
            return Err("autonomy.budget_gate must be in 0..=1".into());
        }
        Ok(())
    }
}

/// Bounded permission to do one unit of background work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkLease {
    pub id: u64,
    pub state: AutonomyState,
    pub started_at: u64,
    pub expires_at: u64,
    pub max_steps: u32,
    pub steps_used: u32,
    /// Why the lease was granted (audit).
    pub trigger: String,
    /// Model calls allowed under current gates (integrator still enforces).
    pub allow_model: bool,
    /// Overlay memory writes allowed (never core/read-only volume).
    pub allow_overlay_write: bool,
}

impl WorkLease {
    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.expires_at || self.steps_used >= self.max_steps
    }

    pub fn remaining_steps(&self) -> u32 {
        self.max_steps.saturating_sub(self.steps_used)
    }
}

/// Hard sovereignty flags on every transition outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutonomyInvariants {
    /// Triggers never compel public/private speech.
    pub compel_response: bool,
    /// Triggers never compel memory/graph/WAL writes.
    pub compel_write: bool,
    /// Self-authored Engage still required for publication (integrator).
    pub requires_self_authored_engage_for_publish: bool,
}

impl AutonomyInvariants {
    pub const SOVEREIGN: Self = Self {
        compel_response: false,
        compel_write: false,
        requires_self_authored_engage_for_publish: true,
    };

    pub fn holds(self) -> bool {
        !self.compel_response
            && !self.compel_write
            && self.requires_self_authored_engage_for_publish
    }
}

/// Result of applying one event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitionOutcome {
    pub from: AutonomyState,
    pub to: AutonomyState,
    pub accepted: bool,
    pub reason: String,
    pub trigger: String,
    /// Lease granted, retained, or cleared.
    pub lease: Option<WorkLease>,
    pub quiet_active: bool,
    pub budget_blocks_model: bool,
    pub invariants: AutonomyInvariants,
}

/// Durable snapshot for crash/restart recovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutonomySnapshot {
    pub state: AutonomyState,
    /// State to restore when leaving Paused (if pause interrupted work).
    pub resume_state: Option<AutonomyState>,
    pub lease: Option<WorkLease>,
    pub last_chat_at: Option<u64>,
    pub quiet_until: u64,
    pub budget_pressure: f32,
    pub daily_budget_exhausted: bool,
    pub idle_secs: u64,
    pub queue_depth: u32,
    pub last_transition_at: u64,
    pub next_lease_id: u64,
    pub policy: AutonomyPolicy,
    pub transition_count: u64,
}

impl Default for AutonomySnapshot {
    fn default() -> Self {
        Self {
            state: AutonomyState::Asleep,
            resume_state: None,
            lease: None,
            last_chat_at: None,
            quiet_until: 0,
            budget_pressure: 0.0,
            daily_budget_exhausted: false,
            idle_secs: 0,
            queue_depth: 0,
            last_transition_at: 0,
            next_lease_id: 1,
            policy: AutonomyPolicy::default(),
            transition_count: 0,
        }
    }
}

/// Pure autonomy machine.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AutonomyMachine {
    snap: AutonomySnapshot,
}

impl AutonomyMachine {
    pub fn new(policy: AutonomyPolicy) -> Result<Self, String> {
        policy.validate()?;
        Ok(Self {
            snap: AutonomySnapshot {
                policy,
                ..AutonomySnapshot::default()
            },
        })
    }

    pub fn with_defaults() -> Self {
        Self::default()
    }

    /// Restore after crash. Invalid, mismatched, or expired leases at `now`
    /// are dropped, and the state is normalized so the machine can never
    /// come back in a shape it could not have reached itself:
    ///
    /// - `Paused` never carries a lease (halt cancels work authority);
    /// - a working state requires a live lease *for that state* — otherwise
    ///   it collapses to `Asleep` (incomplete work compels nothing, and a
    ///   working state without a lease would deadlock: nothing expires it);
    /// - `next_lease_id` always moves past any surviving lease id, so a
    ///   restart cannot mint duplicate lease ids.
    pub fn recover(mut snapshot: AutonomySnapshot, now: u64) -> Result<Self, String> {
        snapshot.policy.validate()?;
        if snapshot.state == AutonomyState::Paused {
            snapshot.lease = None;
        }
        if let Some(lease) = snapshot.lease.clone() {
            snapshot.next_lease_id = snapshot.next_lease_id.max(lease.id.saturating_add(1));
            if lease.is_expired(now) || lease.state != snapshot.state {
                snapshot.lease = None;
            }
        }
        if snapshot.state.is_working() && snapshot.lease.is_none() {
            // Incomplete work does not compel anything; return to sleep.
            snapshot.state = AutonomyState::Asleep;
            snapshot.resume_state = None;
        }
        // Paused always wins over a recovered working state if both set oddly.
        if snapshot.resume_state == Some(AutonomyState::Paused) {
            snapshot.resume_state = None;
        }
        snapshot.budget_pressure = snapshot.budget_pressure.clamp(0.0, 1.0);
        snapshot.last_transition_at = now;
        Ok(Self { snap: snapshot })
    }

    pub fn snapshot(&self) -> &AutonomySnapshot {
        &self.snap
    }

    pub fn into_snapshot(self) -> AutonomySnapshot {
        self.snap
    }

    pub fn state(&self) -> AutonomyState {
        self.snap.state
    }

    pub fn lease(&self) -> Option<&WorkLease> {
        self.snap.lease.as_ref()
    }

    pub fn policy(&self) -> &AutonomyPolicy {
        &self.snap.policy
    }

    /// Record user chat activity (starts quiet period). Does not change state.
    pub fn note_chat(&mut self, now: u64) {
        self.snap.last_chat_at = Some(now);
        if self.snap.policy.honor_quiet_period {
            self.snap.quiet_until = now.saturating_add(self.snap.policy.quiet_secs);
        }
        self.snap.idle_secs = 0;
    }

    pub fn set_budget_pressure(&mut self, pressure: f32) {
        self.snap.budget_pressure = pressure.clamp(0.0, 1.0);
    }

    pub fn set_daily_budget_exhausted(&mut self, exhausted: bool) {
        self.snap.daily_budget_exhausted = exhausted;
    }

    pub fn set_queue_depth(&mut self, depth: u32) {
        self.snap.queue_depth = depth;
    }

    pub fn set_idle_secs(&mut self, idle: u64) {
        self.snap.idle_secs = idle;
    }

    pub fn quiet_active(&self, now: u64) -> bool {
        self.snap.policy.honor_quiet_period && now < self.snap.quiet_until
    }

    pub fn budget_blocks_model(&self) -> bool {
        self.snap.daily_budget_exhausted
            || self.snap.budget_pressure >= self.snap.policy.budget_gate
    }

    /// Ledger Safeguard Invariant, checked at **execution** time: the spend
    /// ledger must gate every model call, not just lease grants. A lease's
    /// `allow_model` reflects the gate at grant time; budget pressure can
    /// rise mid-lease, so integrators must call this immediately before
    /// each model execution.
    pub fn may_call_model(&self, now: u64) -> bool {
        !self.snap.state.is_paused()
            && self
                .snap
                .lease
                .as_ref()
                .is_some_and(|lease| lease.allow_model && !lease.is_expired(now))
            && !self.budget_blocks_model()
    }

    /// Overlay-write permission at execution time. Never grants access to
    /// the read-only base; the flag only covers the overlay sink.
    pub fn may_write_overlay(&self, now: u64) -> bool {
        !self.snap.state.is_paused()
            && self
                .snap
                .lease
                .as_ref()
                .is_some_and(|lease| lease.allow_overlay_write && !lease.is_expired(now))
    }

    /// Idle claims are stimuli, not authority: clamp a caller-supplied idle
    /// duration against the machine's own record of the last chat, so a
    /// forged or stale claim cannot bypass the quiet/dream idle gates.
    fn effective_idle(&self, claimed_idle_secs: u64, now: u64) -> u64 {
        match self.snap.last_chat_at {
            Some(last_chat) => claimed_idle_secs.min(now.saturating_sub(last_chat)),
            None => claimed_idle_secs,
        }
    }

    /// Consume one step on the active lease if present.
    pub fn record_step(&mut self, now: u64) -> Result<(), String> {
        let Some(lease) = self.snap.lease.as_mut() else {
            return Err("no active lease".into());
        };
        if lease.is_expired(now) {
            return Err("lease expired".into());
        }
        lease.steps_used = lease.steps_used.saturating_add(1);
        Ok(())
    }

    /// Apply a trigger at time `now`. Pure transition function + state update.
    pub fn apply(&mut self, trigger: WakeTrigger, now: u64) -> TransitionOutcome {
        let from = self.snap.state;
        let trigger_name = trigger.kind_name().to_string();

        // Pause/stop always wins.
        if trigger.is_halt() {
            return self.enter_paused(from, trigger_name, now, trigger.kind_name());
        }

        // While paused, only OperatorStart (and halt, handled above) matter.
        if from == AutonomyState::Paused {
            return match trigger {
                WakeTrigger::OperatorStart => self.leave_paused(now, trigger_name),
                WakeTrigger::Tick => self.noop(from, trigger_name, now, "paused: tick ignored"),
                _ => self.noop(
                    from,
                    trigger_name,
                    now,
                    "paused: only operator_start resumes",
                ),
            };
        }

        // Lease expiry check on tick / explicit event. The machine verifies
        // expiry itself: a LeaseExpired claim for a lease that is still live
        // at `now` (or for a different lease) is ignored, so a stale or
        // buggy external timer cannot cancel active work early.
        if matches!(
            trigger,
            WakeTrigger::Tick | WakeTrigger::LeaseExpired { .. }
        ) && let Some(lease) = self.snap.lease.clone()
        {
            if lease.is_expired(now) {
                return self.expire_lease(from, trigger_name, now);
            }
            if let WakeTrigger::LeaseExpired { lease_id } = trigger {
                let reason = if lease_id == lease.id {
                    "lease_expired claim rejected: lease is still live"
                } else {
                    "lease_expired claim rejected: not the active lease"
                };
                return self.noop(from, trigger_name, now, reason);
            }
        }

        match (from, &trigger) {
            // --- From Asleep ---
            // Direct mention / scheduled / idle honor quiet period (arena chat window).
            // Explicit API wake may proceed — still never compels response/write.
            (AutonomyState::Asleep, WakeTrigger::DirectMention { .. }) => {
                self.enter_work(AutonomyState::Awake, from, trigger_name, now, true)
            }
            (AutonomyState::Asleep, WakeTrigger::ApiWake) => {
                self.enter_work(AutonomyState::Awake, from, trigger_name, now, false)
            }
            (AutonomyState::Asleep, WakeTrigger::ScheduledWake) => {
                self.enter_work(AutonomyState::Deliberating, from, trigger_name, now, true)
            }
            (AutonomyState::Asleep, WakeTrigger::IdleResume { idle_secs })
                if self.effective_idle(*idle_secs, now) >= self.snap.policy.idle_resume_secs =>
            {
                self.enter_work(AutonomyState::Deliberating, from, trigger_name, now, true)
            }
            (AutonomyState::Asleep, WakeTrigger::OperatorStart) => self.noop(
                from,
                trigger_name,
                now,
                "already asleep; operator_start is for pause resume",
            ),
            (AutonomyState::Asleep, WakeTrigger::Tick) => {
                self.noop(from, trigger_name, now, "asleep: tick idle")
            }

            // --- From Awake ---
            // ProcessInput is the active session itself and is never
            // quiet-gated; autonomous stimuli (mention consideration,
            // scheduled wake) honor the quiet period in every state.
            (AutonomyState::Awake, WakeTrigger::ProcessInput) => {
                self.enter_work(AutonomyState::Deliberating, from, trigger_name, now, false)
            }
            (AutonomyState::Awake, WakeTrigger::DirectMention { .. })
            | (AutonomyState::Awake, WakeTrigger::ScheduledWake) => {
                self.enter_work(AutonomyState::Deliberating, from, trigger_name, now, true)
            }
            (AutonomyState::Awake, WakeTrigger::QueueEmpty) => {
                self.sleep_clear(from, trigger_name, now, "awake: queue empty")
            }
            (AutonomyState::Awake, WakeTrigger::Tick) => {
                self.noop(from, trigger_name, now, "awake: tick")
            }

            // --- From Deliberating ---
            (AutonomyState::Deliberating, WakeTrigger::IdleThresholdForDream { idle_secs })
                if self.effective_idle(*idle_secs, now) >= self.snap.policy.dream_idle_secs =>
            {
                self.enter_work(AutonomyState::Dreaming, from, trigger_name, now, false)
            }
            (AutonomyState::Deliberating, WakeTrigger::QueueEmpty) => {
                self.sleep_clear(from, trigger_name, now, "deliberating: queue empty")
            }
            (AutonomyState::Deliberating, WakeTrigger::ProcessInput) => {
                // Stay deliberating; refresh lease if needed.
                self.enter_work(AutonomyState::Deliberating, from, trigger_name, now, false)
            }
            (AutonomyState::Deliberating, WakeTrigger::Tick) => {
                self.noop(from, trigger_name, now, "deliberating: tick")
            }

            // --- From Dreaming ---
            (AutonomyState::Dreaming, WakeTrigger::DreamComplete)
            | (AutonomyState::Dreaming, WakeTrigger::QueueEmpty) => {
                self.sleep_clear(from, trigger_name, now, "dream complete / queue empty")
            }
            (AutonomyState::Dreaming, WakeTrigger::Tick) => {
                self.noop(from, trigger_name, now, "dreaming: tick")
            }

            // Illegal / ignored
            _ => self.noop(
                from,
                trigger_name,
                now,
                &format!(
                    "no legal transition from {} on {}",
                    from.as_str(),
                    trigger.kind_name()
                ),
            ),
        }
    }

    fn enter_paused(
        &mut self,
        from: AutonomyState,
        trigger_name: String,
        now: u64,
        halt_kind: &str,
    ) -> TransitionOutcome {
        if from != AutonomyState::Paused {
            self.snap.resume_state = Some(from);
        }
        self.snap.state = AutonomyState::Paused;
        self.snap.lease = None; // halt cancels work authority
        self.snap.last_transition_at = now;
        self.snap.transition_count = self.snap.transition_count.saturating_add(1);
        TransitionOutcome {
            from,
            to: AutonomyState::Paused,
            accepted: true,
            reason: format!("{halt_kind}: halt precedence; lease cleared"),
            trigger: trigger_name,
            lease: None,
            quiet_active: self.quiet_active(now),
            budget_blocks_model: self.budget_blocks_model(),
            invariants: AutonomyInvariants::SOVEREIGN,
        }
    }

    fn leave_paused(&mut self, now: u64, trigger_name: String) -> TransitionOutcome {
        let from = AutonomyState::Paused;
        let dest = match self.snap.resume_state.take() {
            Some(AutonomyState::Paused) | None => AutonomyState::Asleep,
            Some(s) => s,
        };
        // Never resume into a working state without a fresh lease decision.
        let dest = if dest.is_working() {
            AutonomyState::Asleep
        } else {
            dest
        };
        self.snap.state = dest;
        self.snap.lease = None;
        self.snap.last_transition_at = now;
        self.snap.transition_count = self.snap.transition_count.saturating_add(1);
        TransitionOutcome {
            from,
            to: dest,
            accepted: true,
            reason: "operator_start: left pause into safe state".into(),
            trigger: trigger_name,
            lease: None,
            quiet_active: self.quiet_active(now),
            budget_blocks_model: self.budget_blocks_model(),
            invariants: AutonomyInvariants::SOVEREIGN,
        }
    }

    fn sleep_clear(
        &mut self,
        from: AutonomyState,
        trigger_name: String,
        now: u64,
        reason: &str,
    ) -> TransitionOutcome {
        self.snap.state = AutonomyState::Asleep;
        self.snap.lease = None;
        self.snap.resume_state = None;
        self.snap.last_transition_at = now;
        self.snap.transition_count = self.snap.transition_count.saturating_add(1);
        TransitionOutcome {
            from,
            to: AutonomyState::Asleep,
            accepted: true,
            reason: reason.into(),
            trigger: trigger_name,
            lease: None,
            quiet_active: self.quiet_active(now),
            budget_blocks_model: self.budget_blocks_model(),
            invariants: AutonomyInvariants::SOVEREIGN,
        }
    }

    fn expire_lease(
        &mut self,
        from: AutonomyState,
        trigger_name: String,
        now: u64,
    ) -> TransitionOutcome {
        self.snap.lease = None;
        // Expiry returns to asleep; does not force completion of work.
        self.snap.state = AutonomyState::Asleep;
        self.snap.last_transition_at = now;
        self.snap.transition_count = self.snap.transition_count.saturating_add(1);
        TransitionOutcome {
            from,
            to: AutonomyState::Asleep,
            accepted: true,
            reason: "work lease expired; returning to asleep without compelled action".into(),
            trigger: trigger_name,
            lease: None,
            quiet_active: self.quiet_active(now),
            budget_blocks_model: self.budget_blocks_model(),
            invariants: AutonomyInvariants::SOVEREIGN,
        }
    }

    fn enter_work(
        &mut self,
        to: AutonomyState,
        from: AutonomyState,
        trigger_name: String,
        now: u64,
        check_quiet: bool,
    ) -> TransitionOutcome {
        if check_quiet && self.quiet_active(now) {
            return self.noop(
                from,
                trigger_name,
                now,
                "quiet period active: wake deferred (no compel)",
            );
        }

        // Same-state re-entry retains the active lease: a stream of
        // ProcessInput events must consume ONE step budget, not mint a
        // fresh lease each time (which would make the work bound infinite).
        if to == from
            && let Some(existing) = self.snap.lease.clone()
            && !existing.is_expired(now)
            && existing.state == to
        {
            self.snap.last_transition_at = now;
            self.snap.transition_count = self.snap.transition_count.saturating_add(1);
            return TransitionOutcome {
                from,
                to,
                accepted: true,
                reason: format!(
                    "continuing {} under existing lease {} ({} steps left)",
                    to.as_str(),
                    existing.id,
                    existing.remaining_steps()
                ),
                trigger: trigger_name,
                lease: Some(existing),
                quiet_active: self.quiet_active(now),
                budget_blocks_model: self.budget_blocks_model(),
                invariants: AutonomyInvariants::SOVEREIGN,
            };
        }

        let allow_model = !self.budget_blocks_model();
        // Overlay writes are a permission bit for the integrator; never a compel.
        let allow_overlay_write = true;

        let lease = self.issue_lease(to, now, &trigger_name, allow_model, allow_overlay_write);
        self.snap.state = to;
        self.snap.lease = Some(lease.clone());
        self.snap.last_transition_at = now;
        self.snap.transition_count = self.snap.transition_count.saturating_add(1);

        TransitionOutcome {
            from,
            to,
            accepted: true,
            reason: format!(
                "entered {}; model_allowed={allow_model}; compel_response=false; compel_write=false",
                to.as_str()
            ),
            trigger: trigger_name,
            lease: Some(lease),
            quiet_active: self.quiet_active(now),
            budget_blocks_model: self.budget_blocks_model(),
            invariants: AutonomyInvariants::SOVEREIGN,
        }
    }

    fn issue_lease(
        &mut self,
        state: AutonomyState,
        now: u64,
        trigger: &str,
        allow_model: bool,
        allow_overlay_write: bool,
    ) -> WorkLease {
        let id = self.snap.next_lease_id;
        self.snap.next_lease_id = self.snap.next_lease_id.saturating_add(1);
        WorkLease {
            id,
            state,
            started_at: now,
            expires_at: now.saturating_add(self.snap.policy.lease_secs),
            max_steps: self.snap.policy.lease_steps,
            steps_used: 0,
            trigger: trigger.into(),
            allow_model,
            allow_overlay_write,
        }
    }

    fn noop(
        &mut self,
        from: AutonomyState,
        trigger_name: String,
        now: u64,
        reason: &str,
    ) -> TransitionOutcome {
        TransitionOutcome {
            from,
            to: from,
            accepted: false,
            reason: reason.into(),
            trigger: trigger_name,
            lease: self.snap.lease.clone(),
            quiet_active: self.quiet_active(now),
            budget_blocks_model: self.budget_blocks_model(),
            invariants: AutonomyInvariants::SOVEREIGN,
        }
    }
}

/// Static table of diagram-legal edges (for tests / docs).
pub fn legal_edges() -> &'static [(AutonomyState, &'static str, AutonomyState)] {
    &[
        (
            AutonomyState::Asleep,
            "direct_mention|api_wake",
            AutonomyState::Awake,
        ),
        (
            AutonomyState::Asleep,
            "scheduled_wake|idle_resume",
            AutonomyState::Deliberating,
        ),
        (
            AutonomyState::Awake,
            "process_input|mention|scheduled",
            AutonomyState::Deliberating,
        ),
        (
            AutonomyState::Deliberating,
            "idle_threshold_for_dream",
            AutonomyState::Dreaming,
        ),
        (
            AutonomyState::Dreaming,
            "dream_complete|queue_empty",
            AutonomyState::Asleep,
        ),
        (
            AutonomyState::Deliberating,
            "queue_empty",
            AutonomyState::Asleep,
        ),
        // Halt has precedence from EVERY state, not just Asleep.
        (
            AutonomyState::Asleep,
            "operator_pause|stop",
            AutonomyState::Paused,
        ),
        (
            AutonomyState::Awake,
            "operator_pause|stop",
            AutonomyState::Paused,
        ),
        (
            AutonomyState::Deliberating,
            "operator_pause|stop",
            AutonomyState::Paused,
        ),
        (
            AutonomyState::Dreaming,
            "operator_pause|stop",
            AutonomyState::Paused,
        ),
        (
            AutonomyState::Paused,
            "operator_start",
            AutonomyState::Asleep,
        ),
        // Lease expiry returns any working state to Asleep.
        (
            AutonomyState::Awake,
            "lease_expired|tick",
            AutonomyState::Asleep,
        ),
        (
            AutonomyState::Deliberating,
            "lease_expired|tick",
            AutonomyState::Asleep,
        ),
        (
            AutonomyState::Dreaming,
            "lease_expired|tick",
            AutonomyState::Asleep,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> AutonomyMachine {
        AutonomyMachine::with_defaults()
    }

    #[test]
    fn defaults_start_asleep_sovereign() {
        let m = machine();
        assert_eq!(m.state(), AutonomyState::Asleep);
        assert!(AutonomyInvariants::SOVEREIGN.holds());
        m.policy().validate().unwrap();
    }

    #[test]
    fn direct_mention_wakes_to_awake_without_compel() {
        let mut m = machine();
        let out = m.apply(
            WakeTrigger::DirectMention {
                consideration_id: "c1".into(),
            },
            100,
        );
        assert!(out.accepted);
        assert_eq!(out.to, AutonomyState::Awake);
        assert!(out.lease.is_some());
        assert!(!out.invariants.compel_response);
        assert!(!out.invariants.compel_write);
        assert!(out.invariants.requires_self_authored_engage_for_publish);
    }

    #[test]
    fn scheduled_wake_enters_deliberating() {
        let mut m = machine();
        let out = m.apply(WakeTrigger::ScheduledWake, 1);
        assert_eq!(out.to, AutonomyState::Deliberating);
        assert!(out.accepted);
        assert!(!out.invariants.compel_response);
    }

    #[test]
    fn awake_to_deliberating_on_process_input() {
        let mut m = machine();
        m.apply(WakeTrigger::ApiWake, 1);
        let out = m.apply(WakeTrigger::ProcessInput, 2);
        assert_eq!(out.from, AutonomyState::Awake);
        assert_eq!(out.to, AutonomyState::Deliberating);
    }

    #[test]
    fn deliberating_to_dreaming_on_idle_threshold() {
        let mut m = machine();
        m.apply(WakeTrigger::ScheduledWake, 10);
        let out = m.apply(
            WakeTrigger::IdleThresholdForDream {
                idle_secs: DEFAULT_IDLE_RESUME_SECS,
            },
            20,
        );
        assert!(out.accepted);
        assert_eq!(out.to, AutonomyState::Dreaming);
    }

    #[test]
    fn dream_complete_returns_asleep() {
        let mut m = machine();
        m.apply(WakeTrigger::ScheduledWake, 1);
        m.apply(WakeTrigger::IdleThresholdForDream { idle_secs: 10_000 }, 2);
        let out = m.apply(WakeTrigger::DreamComplete, 3);
        assert_eq!(out.to, AutonomyState::Asleep);
        assert!(m.lease().is_none());
    }

    #[test]
    fn queue_empty_from_deliberating_sleeps() {
        let mut m = machine();
        m.apply(WakeTrigger::ScheduledWake, 1);
        let out = m.apply(WakeTrigger::QueueEmpty, 2);
        assert_eq!(out.to, AutonomyState::Asleep);
    }

    #[test]
    fn pause_has_precedence_from_any_working_state() {
        let mut m = machine();
        m.apply(WakeTrigger::ApiWake, 1);
        m.apply(WakeTrigger::ProcessInput, 2);
        assert_eq!(m.state(), AutonomyState::Deliberating);
        let out = m.apply(WakeTrigger::OperatorPause, 3);
        assert_eq!(out.to, AutonomyState::Paused);
        assert!(m.lease().is_none());
        // Triggers while paused are ignored except start/halt
        let blocked = m.apply(
            WakeTrigger::DirectMention {
                consideration_id: "x".into(),
            },
            4,
        );
        assert!(!blocked.accepted);
        assert_eq!(m.state(), AutonomyState::Paused);
    }

    #[test]
    fn stop_also_pauses_and_start_resumes_safely() {
        let mut m = machine();
        m.apply(WakeTrigger::ApiWake, 1);
        m.apply(WakeTrigger::OperatorStop, 2);
        assert_eq!(m.state(), AutonomyState::Paused);
        let out = m.apply(WakeTrigger::OperatorStart, 3);
        // Working resume_state is collapsed to Asleep for safety
        assert_eq!(out.to, AutonomyState::Asleep);
        assert!(!out.invariants.compel_response);
    }

    #[test]
    fn quiet_period_defers_wake() {
        let mut m = machine();
        m.note_chat(100);
        assert!(m.quiet_active(110));
        let out = m.apply(
            WakeTrigger::DirectMention {
                consideration_id: "c".into(),
            },
            110,
        );
        assert!(!out.accepted);
        assert_eq!(m.state(), AutonomyState::Asleep);
        assert!(out.quiet_active);
        // After quiet window
        let later = 100 + DEFAULT_QUIET_SECS + 1;
        let out2 = m.apply(
            WakeTrigger::DirectMention {
                consideration_id: "c2".into(),
            },
            later,
        );
        assert!(out2.accepted);
        assert_eq!(out2.to, AutonomyState::Awake);
    }

    #[test]
    fn budget_gate_blocks_model_on_lease_not_compel() {
        let mut m = machine();
        m.set_budget_pressure(0.95);
        let out = m.apply(WakeTrigger::ScheduledWake, 1);
        assert!(out.accepted);
        assert!(out.budget_blocks_model);
        let lease = out.lease.unwrap();
        assert!(!lease.allow_model);
        assert!(!out.invariants.compel_write);
        assert!(!out.invariants.compel_response);
    }

    #[test]
    fn daily_budget_exhausted_same_gate() {
        let mut m = machine();
        m.set_daily_budget_exhausted(true);
        let out = m.apply(WakeTrigger::ApiWake, 1);
        assert!(!out.lease.unwrap().allow_model);
    }

    #[test]
    fn work_lease_bounds_and_step_accounting() {
        let mut policy = AutonomyPolicy::default();
        policy.lease_steps = 2;
        policy.lease_secs = 50;
        let mut m = AutonomyMachine::new(policy).unwrap();
        m.apply(WakeTrigger::ApiWake, 1000);
        let lease = m.lease().unwrap().clone();
        assert_eq!(lease.max_steps, 2);
        assert_eq!(lease.expires_at, 1050);
        m.record_step(1001).unwrap();
        m.record_step(1002).unwrap();
        assert!(m.lease().unwrap().is_expired(1002));
        let out = m.apply(WakeTrigger::Tick, 1002);
        assert_eq!(out.to, AutonomyState::Asleep);
    }

    #[test]
    fn lease_expired_trigger() {
        let mut m = machine();
        m.apply(WakeTrigger::ScheduledWake, 10);
        let id = m.lease().unwrap().id;
        // Force expiry by time
        let exp = m.lease().unwrap().expires_at;
        let out = m.apply(WakeTrigger::LeaseExpired { lease_id: id }, exp);
        assert_eq!(out.to, AutonomyState::Asleep);
        assert!(m.lease().is_none());
    }

    #[test]
    fn crash_recovery_drops_expired_lease() {
        let mut m = machine();
        m.apply(WakeTrigger::ApiWake, 100);
        let mut snap = m.snapshot().clone();
        // Simulate crash long after lease expiry
        let now = snap.lease.as_ref().unwrap().expires_at + 5;
        let recovered = AutonomyMachine::recover(snap.clone(), now).unwrap();
        assert_eq!(recovered.state(), AutonomyState::Asleep);
        assert!(recovered.lease().is_none());

        // Fresh lease survives recovery before expiry
        snap = {
            let mut m2 = machine();
            m2.apply(WakeTrigger::ApiWake, 200);
            m2.into_snapshot()
        };
        let mid = snap.lease.as_ref().unwrap().started_at + 1;
        let ok = AutonomyMachine::recover(snap, mid).unwrap();
        assert_eq!(ok.state(), AutonomyState::Awake);
        assert!(ok.lease().is_some());
    }

    #[test]
    fn no_trigger_sets_compel_flags() {
        let mut m = machine();
        let triggers = [
            WakeTrigger::DirectMention {
                consideration_id: "a".into(),
            },
            WakeTrigger::ApiWake,
            WakeTrigger::ScheduledWake,
            WakeTrigger::IdleResume { idle_secs: 10_000 },
            WakeTrigger::ProcessInput,
            WakeTrigger::IdleThresholdForDream { idle_secs: 10_000 },
            WakeTrigger::DreamComplete,
            WakeTrigger::QueueEmpty,
            WakeTrigger::OperatorPause,
            WakeTrigger::OperatorStart,
            WakeTrigger::Tick,
        ];
        for (index, t) in triggers.into_iter().enumerate() {
            let now = index as u64 + 1;
            let out = m.apply(t, now);
            assert!(out.invariants.holds(), "{}", out.reason);
            assert!(!out.invariants.compel_response);
            assert!(!out.invariants.compel_write);
        }
    }

    #[test]
    fn illegal_edge_rejected() {
        let mut m = machine();
        // Dreaming from asleep without path
        let out = m.apply(WakeTrigger::DreamComplete, 1);
        assert!(!out.accepted);
        assert_eq!(m.state(), AutonomyState::Asleep);
    }

    #[test]
    fn serde_roundtrip_snapshot() {
        let mut m = machine();
        m.note_chat(50);
        m.set_budget_pressure(0.2);
        m.apply(WakeTrigger::ApiWake, 100);
        let json = serde_json::to_string_pretty(m.snapshot()).unwrap();
        let back: AutonomySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(m.snapshot(), &back);
    }

    #[test]
    fn idle_resume_from_asleep() {
        let mut m = machine();
        let short = m.apply(WakeTrigger::IdleResume { idle_secs: 10 }, 1);
        assert!(!short.accepted);
        let long = m.apply(
            WakeTrigger::IdleResume {
                idle_secs: DEFAULT_IDLE_RESUME_SECS,
            },
            2,
        );
        assert!(long.accepted);
        assert_eq!(long.to, AutonomyState::Deliberating);
    }

    #[test]
    fn policy_validation() {
        let mut p = AutonomyPolicy::default();
        p.validate().unwrap();
        p.lease_steps = 0;
        assert!(AutonomyMachine::new(p).is_err());
    }

    #[test]
    fn legal_edges_table_non_empty() {
        assert!(!legal_edges().is_empty());
    }

    #[test]
    fn process_input_retains_lease_instead_of_minting_fresh_budget() {
        let mut m = machine();
        m.apply(WakeTrigger::ApiWake, 1);
        m.apply(WakeTrigger::ProcessInput, 2); // Awake -> Deliberating (new lease)
        let lease_id = m.lease().unwrap().id;
        m.record_step(3).unwrap();
        m.record_step(4).unwrap();
        let steps_before = m.lease().unwrap().steps_used;
        // Repeated ProcessInput in Deliberating must NOT reset the budget.
        let out = m.apply(WakeTrigger::ProcessInput, 5);
        assert!(out.accepted);
        assert_eq!(out.to, AutonomyState::Deliberating);
        let lease = m.lease().unwrap();
        assert_eq!(lease.id, lease_id, "lease must be retained, not re-minted");
        assert_eq!(
            lease.steps_used, steps_before,
            "step accounting must survive"
        );
    }

    #[test]
    fn stale_or_early_lease_expired_claims_are_rejected() {
        let mut m = machine();
        m.apply(WakeTrigger::ScheduledWake, 10);
        let id = m.lease().unwrap().id;
        // Correct id but the lease is still live: claim rejected, work kept.
        let early = m.apply(WakeTrigger::LeaseExpired { lease_id: id }, 11);
        assert!(!early.accepted);
        assert_eq!(m.state(), AutonomyState::Deliberating);
        assert!(m.lease().is_some());
        // Wrong id: also rejected.
        let wrong = m.apply(WakeTrigger::LeaseExpired { lease_id: id + 99 }, 12);
        assert!(!wrong.accepted);
        assert!(m.lease().is_some());
        // Genuinely expired: accepted regardless of which id was claimed.
        let exp = m.lease().unwrap().expires_at;
        let out = m.apply(WakeTrigger::LeaseExpired { lease_id: id }, exp);
        assert!(out.accepted);
        assert_eq!(out.to, AutonomyState::Asleep);
    }

    #[test]
    fn forged_idle_claims_cannot_bypass_gates() {
        let mut m = machine();
        m.note_chat(100);
        // Quiet window (30s) has passed, but real idle is only 31s < 90s:
        // a forged claim of 10_000 idle seconds must not wake the machine.
        let out = m.apply(WakeTrigger::IdleResume { idle_secs: 10_000 }, 131);
        assert!(!out.accepted);
        assert_eq!(m.state(), AutonomyState::Asleep);
        // Same defense for the dream idle gate.
        let mut m = machine();
        m.apply(WakeTrigger::ScheduledWake, 1);
        m.note_chat(50);
        let dream = m.apply(WakeTrigger::IdleThresholdForDream { idle_secs: 10_000 }, 60);
        assert!(!dream.accepted);
        assert_eq!(m.state(), AutonomyState::Deliberating);
        // With genuine silence the same triggers pass.
        let mut m = machine();
        m.note_chat(100);
        let now = 100 + DEFAULT_IDLE_RESUME_SECS;
        let ok = m.apply(
            WakeTrigger::IdleResume {
                idle_secs: DEFAULT_IDLE_RESUME_SECS,
            },
            now,
        );
        assert!(ok.accepted);
    }

    #[test]
    fn autonomous_triggers_honor_quiet_in_awake_too() {
        let mut m = machine();
        m.apply(WakeTrigger::ApiWake, 1);
        assert_eq!(m.state(), AutonomyState::Awake);
        m.note_chat(100);
        // Scheduled wake and mention consideration defer during quiet…
        assert!(!m.apply(WakeTrigger::ScheduledWake, 110).accepted);
        assert!(
            !m.apply(
                WakeTrigger::DirectMention {
                    consideration_id: "c".into()
                },
                111,
            )
            .accepted
        );
        assert_eq!(m.state(), AutonomyState::Awake);
        // …but processing the active session's own input is never gated.
        let out = m.apply(WakeTrigger::ProcessInput, 112);
        assert!(out.accepted);
        assert_eq!(out.to, AutonomyState::Deliberating);
    }

    #[test]
    fn recovery_normalizes_impossible_snapshots() {
        // Working state without a lease would deadlock: collapses to Asleep.
        let mut snap = AutonomySnapshot::default();
        snap.state = AutonomyState::Deliberating;
        snap.lease = None;
        let m = AutonomyMachine::recover(snap, 100).unwrap();
        assert_eq!(m.state(), AutonomyState::Asleep);

        // Paused never carries work authority: lease dropped.
        let mut working = machine();
        working.apply(WakeTrigger::ApiWake, 10);
        let live_lease = working.lease().unwrap().clone();
        let mut snap = AutonomySnapshot::default();
        snap.state = AutonomyState::Paused;
        snap.lease = Some(live_lease.clone());
        let m = AutonomyMachine::recover(snap, 11).unwrap();
        assert_eq!(m.state(), AutonomyState::Paused);
        assert!(m.lease().is_none());

        // Lease whose state disagrees with the snapshot state is dropped,
        // and the working state collapses with it.
        let mut snap = AutonomySnapshot::default();
        snap.state = AutonomyState::Dreaming;
        snap.lease = Some(live_lease.clone()); // lease.state == Awake
        let m = AutonomyMachine::recover(snap, 11).unwrap();
        assert_eq!(m.state(), AutonomyState::Asleep);
        assert!(m.lease().is_none());

        // next_lease_id always moves past a surviving lease id.
        let mut snap = AutonomySnapshot::default();
        snap.state = AutonomyState::Awake;
        snap.lease = Some(live_lease.clone());
        snap.next_lease_id = 1; // stale counter behind the lease id
        let m = AutonomyMachine::recover(snap, 11).unwrap();
        assert!(m.snapshot().next_lease_id > live_lease.id);
    }

    #[test]
    fn ledger_invariant_holds_at_execution_time_not_just_grant() {
        let mut m = machine();
        m.apply(WakeTrigger::ApiWake, 1);
        assert!(m.lease().unwrap().allow_model);
        assert!(m.may_call_model(2));
        // Budget exhausts mid-lease: the lease bit is stale, the live check
        // must refuse before any token is spent.
        m.set_daily_budget_exhausted(true);
        assert!(m.lease().unwrap().allow_model, "grant-time bit is stale");
        assert!(!m.may_call_model(3), "execution-time ledger check must win");
        m.set_daily_budget_exhausted(false);
        m.set_budget_pressure(0.99);
        assert!(!m.may_call_model(4));
        // Expired lease refuses both model and overlay work.
        let exp = m.lease().unwrap().expires_at;
        assert!(!m.may_call_model(exp));
        assert!(!m.may_write_overlay(exp));
        // No lease at all (asleep/paused) refuses everything.
        let mut asleep = machine();
        assert!(!asleep.may_call_model(1));
        assert!(!asleep.may_write_overlay(1));
        asleep.apply(WakeTrigger::OperatorPause, 2);
        assert!(!asleep.may_call_model(3));
    }
}
