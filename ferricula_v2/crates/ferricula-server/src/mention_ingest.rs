//! Bounded, provider-neutral Nuts mention-ingestion primitive.
//!
//! This module is the durable *inbound* half of the Nuts bridge: it advances a
//! ledger cursor, deduplicates event sequences, records idempotent processing
//! outcomes, and may enqueue **consideration** of a direct mention. It never
//! drafts, publishes, or otherwise compels a public response. Integration into
//! the runtime task loop is intentionally left to the caller.
//!
//! Design anchors (see `research/nuts-news/ARCHITECTURE.md` and
//! `docs/NUTS_MENTION_THREAT_MODEL.md`):
//! - inbound dedupe key: `nutnews:<instance>:<event_seq>`
//! - the cursor advances only through **contiguous** sequences; a hole with
//!   `gap = false` is treated as an implicit gap and freezes the cursor at
//!   the last contiguous event (T3)
//! - all inbound strings are size-bounded before durable storage; identity
//!   fields are normalized (trim, zero-width strip, ASCII lowercase) (T6, T7)
//! - consideration ids and outbound `request_id`s are **deterministic**
//!   functions of the event key, so a re-created consideration can be
//!   collapsed downstream instead of double-posting (T11)
//! - admission to the consideration queue is bounded per actor and overall,
//!   and loss is always an explicit outcome, never a silent eviction (T10)
//! - a mention is a stimulus for sovereign deliberation, never an obligation

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default Steve handle on Nuts News (lowercase comparison).
pub const DEFAULT_STEVE_HANDLE: &str = "steve";

/// Default Nuts instance id used in durable keys.
pub const DEFAULT_INSTANCE: &str = "news.nuts.services";

/// Hard upper bound on events accepted in one ingest batch.
pub const MAX_BATCH_EVENTS: usize = 500;

/// Hard upper bound on retained processed-event keys (dedupe ring).
pub const DEFAULT_DEDUPE_CAPACITY: usize = 4_096;

/// Hard upper bound on pending considerations kept in durable state.
pub const DEFAULT_CONSIDERATION_CAPACITY: usize = 256;

/// Default byte cap on stored/classified mention text (config-overridable).
pub const DEFAULT_MAX_TEXT_BYTES: usize = 16 * 1024;

/// Absolute ceiling for the text cap; validation rejects anything larger.
pub const MAX_TEXT_BYTES_CEILING: usize = 64 * 1024;

/// Byte cap on identity-like fields (`actor`, `reply_to`, handles).
pub const MAX_IDENT_BYTES: usize = 64;

/// Byte cap on instance ids.
pub const MAX_INSTANCE_BYTES: usize = 128;

/// Byte cap on opaque content fingerprints.
pub const MAX_CONTENT_HASH_BYTES: usize = 128;

/// Default cap on *pending* considerations attributable to one actor.
pub const DEFAULT_MAX_PENDING_PER_ACTOR: usize = 4;

/// Stable inbound identity for an instance-scoped ledger event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventKey {
    pub instance: String,
    pub event_seq: u64,
}

impl EventKey {
    pub fn new(instance: impl Into<String>, event_seq: u64) -> Self {
        Self {
            instance: instance.into(),
            event_seq,
        }
    }

    /// Canonical dedupe key: `nutnews:<instance>:<event_seq>`.
    pub fn dedupe_id(&self) -> String {
        format!("nutnews:{}:{}", self.instance, self.event_seq)
    }
}

impl fmt::Display for EventKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.dedupe_id())
    }
}

/// Durable monotonic cursor over a Nuts ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerCursor {
    pub instance: String,
    /// Last successfully processed sequence number (inclusive). Zero means none.
    pub last_seq: u64,
    /// Highest ledger height observed from the provider (informational).
    #[serde(default)]
    pub observed_height: u64,
    /// True when the provider reported a gap, or ingest detected a silent
    /// hole / floor jump itself. Cleared when contiguity is restored.
    #[serde(default)]
    pub gap: bool,
    #[serde(default)]
    pub updated_at: u64,
}

impl LedgerCursor {
    pub fn fresh(instance: impl Into<String>) -> Self {
        Self {
            instance: instance.into(),
            last_seq: 0,
            observed_height: 0,
            gap: false,
            updated_at: 0,
        }
    }

    /// Argument for the next `events_since(after=…)` poll.
    pub fn after(&self) -> u64 {
        self.last_seq
    }
}

/// Provider-neutral classification of a ledger event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Comment,
    Item,
    Vote,
    HandleChange,
    Classification,
    Other,
}

impl EventKind {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "comment" | "reply" => Self::Comment,
            "item" | "story" | "post" | "submit" => Self::Item,
            "vote" | "upvote" | "downvote" => Self::Vote,
            "handle" | "handle_change" | "profile" => Self::HandleChange,
            "classification" | "classify" => Self::Classification,
            _ => Self::Other,
        }
    }

    /// Votes and handle churn are never mention candidates.
    pub fn is_ignorable_noise(self) -> bool {
        matches!(
            self,
            EventKind::Vote | EventKind::HandleChange | EventKind::Classification
        )
    }
}

/// Provider-neutral inbound event envelope. Network transport is out of scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundEvent {
    pub seq: u64,
    pub kind: EventKind,
    pub item_id: u64,
    #[serde(default)]
    pub comment_id: Option<u64>,
    #[serde(default)]
    pub parent_comment_id: Option<u64>,
    /// Author handle (normalized comparison against Steve's handle).
    pub actor: String,
    /// Raw comment/item body. Treated as untrusted data.
    #[serde(default)]
    pub text: String,
    /// Explicit reply-to handle when the provider surfaces one.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// Opaque content fingerprint for change collapse (optional).
    #[serde(default)]
    pub content_hash: Option<String>,
}

impl InboundEvent {
    pub fn key(&self, instance: &str) -> EventKey {
        EventKey::new(instance, self.seq)
    }

    /// Identity/metadata fields have hard byte caps; a violation marks the
    /// whole event malformed (identity fields are small by contract, and a
    /// forged giant field must not freeze ingest — it is skipped explicitly).
    fn oversized_identity(&self) -> bool {
        self.actor.len() > MAX_IDENT_BYTES
            || self
                .reply_to
                .as_ref()
                .is_some_and(|r| r.len() > MAX_IDENT_BYTES)
            || self
                .content_hash
                .as_ref()
                .is_some_and(|h| h.len() > MAX_CONTENT_HASH_BYTES)
    }
}

/// One page returned by a provider-neutral `events_since` poll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventBatch {
    pub instance: String,
    pub ledger_floor: u64,
    pub ledger_height: u64,
    pub gap: bool,
    pub events: Vec<InboundEvent>,
}

impl EventBatch {
    pub fn validate(&self) -> Result<()> {
        normalize_instance(&self.instance)?;
        if self.events.len() > MAX_BATCH_EVENTS {
            bail!(
                "event batch exceeds MAX_BATCH_EVENTS ({MAX_BATCH_EVENTS}); got {}",
                self.events.len()
            );
        }
        let mut seen = BTreeSet::new();
        for event in &self.events {
            if event.seq == 0 {
                bail!("event seq must be positive");
            }
            if !seen.insert(event.seq) {
                bail!("duplicate seq {} inside a single batch", event.seq);
            }
            if event.item_id == 0 {
                bail!("event {} has item_id 0", event.seq);
            }
        }
        Ok(())
    }
}

/// Why an event did not become a consideration (or was re-delivered).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    AlreadyProcessed,
    BelowCursor,
    GapRecoveryRequired,
    NoiseKind,
    SelfAuthored,
    NotAMention,
    EmptyText,
    BatchLimit,
    /// Identity/metadata field exceeded its byte cap (malformed event).
    Oversized,
    /// This actor already has the maximum pending considerations.
    ActorSaturated,
    /// Consideration queue full and nothing evictable; loss made explicit.
    ConsiderationCapacity,
}

/// Result of classifying a single event relative to Steve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionSignal {
    /// Direct `@handle` in text.
    DirectAtMention,
    /// Provider-level reply_to targeting Steve.
    ExplicitReply,
    /// Not a mention of Steve.
    None,
}

/// Durable record that a mention may be *considered*. Never a forced reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consideration {
    /// Deterministic id derived from the event key (idempotent handle: the
    /// same logical event always yields the same consideration id).
    pub consideration_id: Uuid,
    pub event_key: EventKey,
    pub item_id: u64,
    pub comment_id: Option<u64>,
    pub actor: String,
    /// Untrusted evidence, truncated to the configured byte cap.
    pub text: String,
    /// True when `text` was cut at the byte cap before storage.
    #[serde(default)]
    pub text_truncated: bool,
    pub signal: MentionSignal,
    /// Explicit policy flag: consideration does not require a response.
    pub response_required: bool,
    /// Deterministic outbound-idempotency id derived from the event key, so
    /// a re-created consideration for the same event reuses the same key and
    /// the server can collapse duplicate publishes.
    pub request_id: Uuid,
    pub created_at: u64,
    /// Whether this has been handed to the runtime queue yet.
    pub enqueued: bool,
}

impl Consideration {
    /// Mentions never compel a response. This field is always false.
    pub fn may_enqueue_consideration(&self) -> bool {
        !self.response_required
            && matches!(
                self.signal,
                MentionSignal::DirectAtMention | MentionSignal::ExplicitReply
            )
    }
}

/// Per-event processing outcome (idempotent when re-applied).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOutcome {
    Considered {
        consideration_id: Uuid,
        signal: MentionSignal,
    },
    Skipped {
        reason: SkipReason,
    },
}

/// Audit row for one applied event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub event_key: EventKey,
    pub outcome: ProcessOutcome,
    pub processed_at: u64,
}

/// Configuration for the ingest engine (no network, no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MentionIngestConfig {
    pub instance: String,
    pub steve_handle: String,
    pub dedupe_capacity: usize,
    pub consideration_capacity: usize,
    /// Maximum events applied from a single batch (additional bound ≤ batch size).
    pub max_apply_per_batch: usize,
    /// Byte cap applied to event text before classification and storage.
    pub max_text_bytes: usize,
    /// Cap on pending (not yet enqueued) considerations per actor.
    pub max_pending_per_actor: usize,
}

impl Default for MentionIngestConfig {
    fn default() -> Self {
        Self {
            instance: DEFAULT_INSTANCE.into(),
            steve_handle: DEFAULT_STEVE_HANDLE.into(),
            dedupe_capacity: DEFAULT_DEDUPE_CAPACITY,
            consideration_capacity: DEFAULT_CONSIDERATION_CAPACITY,
            max_apply_per_batch: MAX_BATCH_EVENTS,
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
            max_pending_per_actor: DEFAULT_MAX_PENDING_PER_ACTOR,
        }
    }
}

impl MentionIngestConfig {
    /// Normalize identity fields in place, then check bounds. Construction
    /// paths (`new`, `from_state`, `load`) all pass through here, so stored
    /// state always carries canonical instance/handle forms.
    pub fn normalize_and_validate(&mut self) -> Result<()> {
        self.instance = normalize_instance(&self.instance)?;
        self.steve_handle = normalize_handle(&self.steve_handle)?;
        if self.dedupe_capacity == 0 {
            bail!("dedupe_capacity must be positive");
        }
        if self.consideration_capacity == 0 {
            bail!("consideration_capacity must be positive");
        }
        if self.max_apply_per_batch == 0 || self.max_apply_per_batch > MAX_BATCH_EVENTS {
            bail!("max_apply_per_batch must be in 1..={MAX_BATCH_EVENTS}");
        }
        if self.max_text_bytes == 0 || self.max_text_bytes > MAX_TEXT_BYTES_CEILING {
            bail!("max_text_bytes must be in 1..={MAX_TEXT_BYTES_CEILING}");
        }
        if self.max_pending_per_actor == 0
            || self.max_pending_per_actor > self.consideration_capacity
        {
            bail!("max_pending_per_actor must be in 1..=consideration_capacity");
        }
        Ok(())
    }

    fn handle_norm(&self) -> String {
        normalize_ident(&self.steve_handle)
    }
}

/// Fully durable ingest state: cursor + dedupe ring + considerations + audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionIngestState {
    pub config: MentionIngestConfig,
    pub cursor: LedgerCursor,
    /// Insertion-ordered dedupe keys (ring).
    pub processed_keys: VecDeque<String>,
    /// Fast membership for `processed_keys`.
    pub processed_set: BTreeSet<String>,
    /// Pending/accepted considerations (bounded).
    pub considerations: VecDeque<Consideration>,
    /// Recent process records for debugging (bounded to dedupe capacity).
    pub recent: VecDeque<ProcessRecord>,
}

impl MentionIngestState {
    pub fn new(mut config: MentionIngestConfig) -> Result<Self> {
        config.normalize_and_validate()?;
        let cursor = LedgerCursor::fresh(config.instance.clone());
        Ok(Self {
            config,
            cursor,
            processed_keys: VecDeque::new(),
            processed_set: BTreeSet::new(),
            considerations: VecDeque::new(),
            recent: VecDeque::new(),
        })
    }

    pub fn with_defaults() -> Result<Self> {
        Self::new(MentionIngestConfig::default())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read mention ingest state {}", path.display()))?;
        let mut state: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse mention ingest state {}", path.display()))?;
        state.normalize_and_validate()?;
        Ok(state)
    }

    /// Load and refuse state whose identity does not match the deployment
    /// pin. Prefer this in integrations: a hand-edited or swapped state file
    /// cannot silently retarget the bridge at another instance or handle.
    pub fn load_pinned(
        path: impl AsRef<Path>,
        expected_instance: &str,
        expected_handle: &str,
    ) -> Result<Self> {
        let state = Self::load(path)?;
        let instance = normalize_instance(expected_instance)?;
        let handle = normalize_handle(expected_handle)?;
        if state.config.instance != instance {
            bail!(
                "state instance {} does not match deployment pin {instance}",
                state.config.instance
            );
        }
        if state.config.steve_handle != handle {
            bail!(
                "state handle {} does not match deployment pin {handle}",
                state.config.steve_handle
            );
        }
        Ok(state)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(&tmp, bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
    }

    /// Restore internal invariants after deserialization. The membership set
    /// is rebuilt from the insertion-ordered ring (never trusted from disk),
    /// rings are re-trimmed to capacity, and cursor identity must match the
    /// config. A state file edit can still change values, but it cannot make
    /// the in-memory structures inconsistent with each other.
    fn normalize_and_validate(&mut self) -> Result<()> {
        self.config.normalize_and_validate()?;
        self.cursor.instance = normalize_instance(&self.cursor.instance)?;
        if self.cursor.instance != self.config.instance {
            bail!(
                "cursor instance {} does not match config instance {}",
                self.cursor.instance,
                self.config.instance
            );
        }
        let mut rebuilt = BTreeSet::new();
        let mut keys = VecDeque::new();
        for key in self.processed_keys.drain(..) {
            if rebuilt.insert(key.clone()) {
                keys.push_back(key);
            }
        }
        while keys.len() > self.config.dedupe_capacity {
            if let Some(old) = keys.pop_front() {
                rebuilt.remove(&old);
            }
        }
        self.processed_keys = keys;
        self.processed_set = rebuilt;
        while self.considerations.len() > self.config.consideration_capacity {
            self.considerations.pop_front();
        }
        while self.recent.len() > self.config.dedupe_capacity {
            self.recent.pop_front();
        }
        Ok(())
    }

    fn remember_processed(&mut self, key: &EventKey) {
        let id = key.dedupe_id();
        if self.processed_set.insert(id.clone()) {
            self.processed_keys.push_back(id);
            while self.processed_keys.len() > self.config.dedupe_capacity {
                if let Some(old) = self.processed_keys.pop_front() {
                    self.processed_set.remove(&old);
                }
            }
        }
    }

    fn already_processed(&self, key: &EventKey) -> bool {
        self.processed_set.contains(&key.dedupe_id())
    }

    fn push_recent(&mut self, record: ProcessRecord) {
        self.recent.push_back(record);
        while self.recent.len() > self.config.dedupe_capacity {
            self.recent.pop_front();
        }
    }

    fn pending_for_actor(&self, actor_norm: &str) -> usize {
        self.considerations
            .iter()
            .filter(|c| !c.enqueued && normalize_ident(&c.actor) == actor_norm)
            .count()
    }

    fn has_consideration_for(&self, key: &EventKey) -> bool {
        self.considerations.iter().any(|c| &c.event_key == key)
    }

    /// Admit a consideration under capacity discipline. When full, the oldest
    /// *enqueued* (already handed off) record is evicted first; if every slot
    /// is still pending, admission is refused and the caller reports it —
    /// pending mentions are never silently displaced by newer ones.
    fn admit_consideration(&mut self, consideration: Consideration) -> bool {
        if self.considerations.len() >= self.config.consideration_capacity {
            let evictable = self.considerations.iter().position(|c| c.enqueued);
            match evictable {
                Some(index) => {
                    self.considerations.remove(index);
                }
                None => return false,
            }
        }
        self.considerations.push_back(consideration);
        true
    }
}

/// Summary of applying one event batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestReport {
    pub applied: usize,
    pub considered: usize,
    pub skipped: usize,
    pub gap: bool,
    pub cursor_after: u64,
    pub outcomes: Vec<ProcessRecord>,
}

/// Pure ingest engine. No network, no model calls, no forced replies.
#[derive(Debug)]
pub struct MentionIngest {
    state: MentionIngestState,
}

impl MentionIngest {
    pub fn new(config: MentionIngestConfig) -> Result<Self> {
        Ok(Self {
            state: MentionIngestState::new(config)?,
        })
    }

    pub fn with_defaults() -> Result<Self> {
        Ok(Self {
            state: MentionIngestState::with_defaults()?,
        })
    }

    pub fn from_state(mut state: MentionIngestState) -> Result<Self> {
        state.normalize_and_validate()?;
        Ok(Self { state })
    }

    pub fn state(&self) -> &MentionIngestState {
        &self.state
    }

    pub fn into_state(self) -> MentionIngestState {
        self.state
    }

    pub fn cursor(&self) -> &LedgerCursor {
        &self.state.cursor
    }

    /// Pending considerations that have not been marked enqueued yet.
    pub fn pending_considerations(&self) -> impl Iterator<Item = &Consideration> {
        self.state.considerations.iter().filter(|c| !c.enqueued)
    }

    /// Apply a provider-neutral event batch. Idempotent for re-delivered seqs.
    ///
    /// Sequence discipline: events above the cursor must be contiguous
    /// (`last_seq+1, last_seq+2, …`; a fresh cursor bootstraps at the first
    /// new sequence). The first hole marks an implicit gap — the cursor
    /// freezes at the last contiguous event, the remaining events are
    /// reported `GapRecoveryRequired` and **not** remembered, so an honest
    /// redelivery that fills the hole resumes cleanly. Never sets
    /// `response_required` on any consideration.
    pub fn apply_batch(&mut self, batch: EventBatch, now: u64) -> Result<IngestReport> {
        batch.validate()?;
        let instance = normalize_instance(&batch.instance)?;
        if instance != self.state.config.instance {
            bail!(
                "batch instance {} does not match configured {}",
                instance,
                self.state.config.instance
            );
        }

        self.state.cursor.observed_height =
            self.state.cursor.observed_height.max(batch.ledger_height);
        self.state.cursor.updated_at = now;

        if batch.gap {
            // Do not invent history; operator/reconciler must reset cursor.
            self.state.cursor.gap = true;
            return Ok(self.frozen_report(true));
        }

        // Floor discipline: if the provider pruned past our cursor without
        // raising the gap bit, history is unreachable — that IS a gap (T3).
        if self.state.cursor.last_seq > 0 && batch.ledger_floor > self.state.cursor.last_seq + 1 {
            self.state.cursor.gap = true;
            return Ok(self.frozen_report(true));
        }

        let mut outcomes = Vec::new();
        let mut applied = 0usize;
        let mut considered = 0usize;
        let mut skipped = 0usize;

        // Process in sequence order for deterministic cursor advancement.
        let mut events = batch.events;
        events.sort_by_key(|e| e.seq);

        let bootstrap = self.state.cursor.last_seq == 0;
        let mut expected_next = self.state.cursor.last_seq.saturating_add(1);
        let mut hole = false;
        let mut anchored = false;
        let mut advanced = false;

        for event in events {
            let key = event.key(&instance);

            if event.seq > self.state.cursor.last_seq {
                if hole {
                    // Everything after a detected hole is deferred, not
                    // remembered: honest redelivery must be able to replay it.
                    let record = skip_record(key, SkipReason::GapRecoveryRequired, now);
                    outcomes.push(record.clone());
                    self.state.push_recent(record);
                    skipped += 1;
                    continue;
                }
                if applied >= self.state.config.max_apply_per_batch {
                    let record = skip_record(key, SkipReason::BatchLimit, now);
                    outcomes.push(record.clone());
                    self.state.push_recent(record);
                    skipped += 1;
                    continue;
                }
                if bootstrap && !anchored {
                    // Fresh cursor: the ledger floor may exceed 1; accept the
                    // first new sequence as the starting point.
                    expected_next = event.seq;
                }
                anchored = true;
                if event.seq != expected_next {
                    hole = true;
                    self.state.cursor.gap = true;
                    let record = skip_record(key, SkipReason::GapRecoveryRequired, now);
                    outcomes.push(record.clone());
                    self.state.push_recent(record);
                    skipped += 1;
                    continue;
                }
                expected_next = expected_next.saturating_add(1);
            }

            let record = self.apply_one(&instance, event, now);
            match &record.outcome {
                ProcessOutcome::Considered { .. } => {
                    applied += 1;
                    considered += 1;
                    advanced = true;
                    self.state.cursor.last_seq =
                        self.state.cursor.last_seq.max(record.event_key.event_seq);
                }
                ProcessOutcome::Skipped {
                    reason: SkipReason::AlreadyProcessed | SkipReason::BelowCursor,
                } => {
                    skipped += 1;
                }
                ProcessOutcome::Skipped { .. } => {
                    // First-time observation of non-mention/noise/refused
                    // admission: explicit outcome, cursor still advances.
                    applied += 1;
                    skipped += 1;
                    advanced = true;
                    self.state.cursor.last_seq =
                        self.state.cursor.last_seq.max(record.event_key.event_seq);
                }
            }

            outcomes.push(record.clone());
            self.state.push_recent(record);
        }

        if !hole && advanced {
            // Contiguity held (or was restored by a hole-filling redelivery).
            self.state.cursor.gap = false;
        }

        self.state.cursor.updated_at = now;
        Ok(IngestReport {
            applied,
            considered,
            skipped,
            gap: self.state.cursor.gap,
            cursor_after: self.state.cursor.last_seq,
            outcomes,
        })
    }

    fn frozen_report(&self, gap: bool) -> IngestReport {
        IngestReport {
            applied: 0,
            considered: 0,
            skipped: 0,
            gap,
            cursor_after: self.state.cursor.last_seq,
            outcomes: Vec::new(),
        }
    }

    fn apply_one(&mut self, instance: &str, event: InboundEvent, now: u64) -> ProcessRecord {
        let key = event.key(instance);

        if event.seq <= self.state.cursor.last_seq && self.state.already_processed(&key) {
            return skip_record(key, SkipReason::AlreadyProcessed, now);
        }
        if event.seq <= self.state.cursor.last_seq && !self.state.already_processed(&key) {
            // Late/out-of-order behind cursor: still dedupe, do not re-consider.
            self.state.remember_processed(&key);
            return skip_record(key, SkipReason::BelowCursor, now);
        }
        if self.state.already_processed(&key) {
            return skip_record(key, SkipReason::AlreadyProcessed, now);
        }

        self.state.remember_processed(&key);

        if event.oversized_identity() {
            return skip_record(key, SkipReason::Oversized, now);
        }

        if event.kind.is_ignorable_noise() {
            return skip_record(key, SkipReason::NoiseKind, now);
        }

        let handle = self.state.config.handle_norm();
        let actor_norm = normalize_ident(&event.actor);
        if actor_norm == handle {
            return skip_record(key, SkipReason::SelfAuthored, now);
        }

        if event.text.trim().is_empty() && event.kind == EventKind::Comment {
            return skip_record(key, SkipReason::EmptyText, now);
        }

        // Bound the text BEFORE classification and storage: matching work,
        // durable size, and later model-context size all share one cap. A
        // mention placed beyond the cap is missed by design (prefer an
        // explicit miss over unbounded processing of attacker-sized input).
        let (text, text_truncated) = truncate_utf8(&event.text, self.state.config.max_text_bytes);

        let bounded_event = InboundEvent {
            text: text.clone(),
            ..event
        };
        let signal = classify_mention(&bounded_event, &handle);
        if signal == MentionSignal::None {
            return skip_record(key, SkipReason::NotAMention, now);
        }

        // Rewind guard: if a consideration for this event key is still
        // retained (pending or enqueued), never create a duplicate.
        if self.state.has_consideration_for(&key) {
            return skip_record(key, SkipReason::AlreadyProcessed, now);
        }

        // Per-actor admission bound: one actor cannot monopolize the queue.
        if self.state.pending_for_actor(&actor_norm) >= self.state.config.max_pending_per_actor {
            return skip_record(key, SkipReason::ActorSaturated, now);
        }

        let consideration = Consideration {
            consideration_id: stable_uuid("consideration", &key),
            event_key: key.clone(),
            item_id: bounded_event.item_id,
            comment_id: bounded_event.comment_id,
            actor: bounded_event.actor,
            text,
            text_truncated,
            signal: signal.clone(),
            // Hard invariant: never compel a response.
            response_required: false,
            request_id: stable_uuid("request", &key),
            created_at: now,
            enqueued: false,
        };
        let consideration_id = consideration.consideration_id;
        if !self.state.admit_consideration(consideration) {
            return skip_record(key, SkipReason::ConsiderationCapacity, now);
        }

        ProcessRecord {
            event_key: key,
            outcome: ProcessOutcome::Considered {
                consideration_id,
                signal,
            },
            processed_at: now,
        }
    }

    /// Mark a consideration as handed to the runtime queue. Idempotent.
    ///
    /// Returns `true` if this call transitioned `enqueued` from false to true.
    /// Does not imply any public response will (or should) occur.
    pub fn mark_enqueued(&mut self, consideration_id: Uuid) -> bool {
        for c in &mut self.state.considerations {
            if c.consideration_id == consideration_id {
                if c.enqueued {
                    return false;
                }
                c.enqueued = true;
                return true;
            }
        }
        false
    }

    /// Drain pending considerations into a list for the runtime to enqueue as
    /// *tasks for deliberation only*. Never fabricates a reply.
    pub fn take_pending_for_enqueue(&mut self) -> Vec<Consideration> {
        let mut out = Vec::new();
        for c in &mut self.state.considerations {
            if !c.enqueued {
                c.enqueued = true;
                out.push(c.clone());
            }
        }
        out
    }

    /// Operator/reconciler path: move the cursor **forward** after gap
    /// recovery. Rewinds are refused here — a rewind replays history and can
    /// re-create considerations once dedupe keys have been evicted, so it
    /// requires the explicitly named [`MentionIngest::force_cursor_rewind`].
    pub fn force_cursor(&mut self, last_seq: u64, now: u64) -> Result<()> {
        if last_seq < self.state.cursor.last_seq {
            bail!(
                "refusing cursor rewind {} -> {last_seq}; use force_cursor_rewind for deliberate replay",
                self.state.cursor.last_seq
            );
        }
        self.state.cursor.last_seq = last_seq;
        self.state.cursor.gap = false;
        self.state.cursor.updated_at = now;
        Ok(())
    }

    /// Deliberate history replay. Dedupe keys and retained considerations are
    /// kept, so events whose keys survive are still suppressed; anything
    /// evicted from the dedupe ring MAY be re-considered — its deterministic
    /// consideration/request ids let downstream collapse the duplicates.
    pub fn force_cursor_rewind(&mut self, last_seq: u64, now: u64) {
        self.state.cursor.last_seq = last_seq;
        self.state.cursor.gap = false;
        self.state.cursor.updated_at = now;
    }
}

fn skip_record(event_key: EventKey, reason: SkipReason, now: u64) -> ProcessRecord {
    ProcessRecord {
        event_key,
        outcome: ProcessOutcome::Skipped { reason },
        processed_at: now,
    }
}

/// Canonical instance form: trimmed, ASCII-lowercased, host-shaped. Rejects
/// anything that could smuggle scheme/path/credentials into durable keys.
pub fn normalize_instance(instance: &str) -> Result<String> {
    let normalized = instance.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        bail!("instance must be non-empty");
    }
    if normalized.len() > MAX_INSTANCE_BYTES {
        bail!("instance exceeds {MAX_INSTANCE_BYTES} bytes");
    }
    if !normalized
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
    {
        bail!("instance must contain only [a-z0-9.-] (host form, no scheme or path)");
    }
    if normalized.starts_with(['.', '-']) || normalized.ends_with(['.', '-']) {
        bail!("instance must not begin or end with '.' or '-'");
    }
    Ok(normalized)
}

/// Canonical handle form: trimmed, zero-width-stripped, ASCII-lowercased,
/// restricted to `[a-z0-9_-]`. Configured handles must be unambiguous ASCII;
/// inbound actor strings are merely *normalized* (never rejected) because
/// they are untrusted evidence, not authority.
pub fn normalize_handle(handle: &str) -> Result<String> {
    let normalized = normalize_ident(handle);
    if normalized.is_empty() {
        bail!("handle must be non-empty");
    }
    if normalized.len() > MAX_IDENT_BYTES {
        bail!("handle exceeds {MAX_IDENT_BYTES} bytes");
    }
    if !normalized
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        bail!("handle must contain only [a-z0-9_-]");
    }
    Ok(normalized)
}

/// Identity normalization for comparisons: trim, strip zero-width characters
/// (ZWSP/ZWNJ/ZWJ/WJ/BOM), ASCII-lowercase. Deliberately NOT full Unicode
/// confusable folding — homoglyph actors remain distinct (and untrusted).
fn normalize_ident(value: &str) -> String {
    strip_zero_width(value.trim()).to_ascii_lowercase()
}

fn strip_zero_width(value: &str) -> String {
    value
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
            )
        })
        .collect()
}

/// Matching normalization for text: zero-width characters removed and the
/// fullwidth at-sign (U+FF20) mapped to '@', so `＠steve` and `@st​eve`
/// (ZWSP) still register as mentions. Stored text stays as delivered
/// (truncated only) — normalization is for matching, not evidence rewriting.
fn normalize_text_for_match(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' => None,
            '\u{FF20}' => Some('@'),
            other => Some(other),
        })
        .collect()
}

/// Truncate to a byte cap on a char boundary. Returns (text, was_truncated).
fn truncate_utf8(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_string(), true)
}

/// Deterministic UUID (RFC 9562 v8 layout) from a tag and event key: the
/// same logical event always yields the same consideration/request ids, so
/// re-created considerations can be collapsed by downstream idempotency.
fn stable_uuid(tag: &str, key: &EventKey) -> Uuid {
    let high = fnv1a64(tag, key, 0xcbf2_9ce4_8422_2325);
    let low = fnv1a64(tag, key, 0x8422_2325_cbf2_9ce4);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&high.to_be_bytes());
    bytes[8..].copy_from_slice(&low.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn fnv1a64(tag: &str, key: &EventKey, basis: u64) -> u64 {
    let mut hash = basis;
    for byte in tag.bytes().chain([0u8]).chain(key.dedupe_id().bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Detect whether an event is a direct mention/reply to Steve.
pub fn classify_mention(event: &InboundEvent, steve_handle: &str) -> MentionSignal {
    let handle = normalize_ident(steve_handle);
    if handle.is_empty() {
        return MentionSignal::None;
    }
    if let Some(reply_to) = &event.reply_to
        && normalize_ident(reply_to) == handle
    {
        return MentionSignal::ExplicitReply;
    }
    if text_has_at_mention(&normalize_text_for_match(&event.text), &handle) {
        return MentionSignal::DirectAtMention;
    }
    MentionSignal::None
}

fn text_has_at_mention(text: &str, handle: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let needle = format!("@{handle}");
    let bytes = lower.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return false;
    }
    let mut i = 0;
    while i + n.len() <= bytes.len() {
        if &bytes[i..i + n.len()] == n {
            let after = i + n.len();
            let boundary_after = bytes
                .get(after)
                .map(|c| !c.is_ascii_alphanumeric() && *c != b'_' && *c != b'-')
                .unwrap_or(true);
            let boundary_before = if i == 0 {
                true
            } else {
                let prev = bytes[i - 1];
                !prev.is_ascii_alphanumeric() && prev != b'_'
            };
            if boundary_before && boundary_after {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Map a consideration into a neutral runtime task payload (no reply body).
pub fn consideration_task_payload(c: &Consideration) -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    map.insert(
        "consideration_id".into(),
        serde_json::json!(c.consideration_id.to_string()),
    );
    map.insert(
        "event_key".into(),
        serde_json::json!(c.event_key.dedupe_id()),
    );
    map.insert("item_id".into(), serde_json::json!(c.item_id));
    map.insert("comment_id".into(), serde_json::json!(c.comment_id));
    map.insert("by".into(), serde_json::json!(c.actor));
    map.insert("text".into(), serde_json::json!(c.text));
    map.insert("text_truncated".into(), serde_json::json!(c.text_truncated));
    map.insert("signal".into(), serde_json::json!(c.signal));
    map.insert(
        "response_required".into(),
        serde_json::json!(c.response_required),
    );
    map.insert(
        "request_id".into(),
        serde_json::json!(c.request_id.to_string()),
    );
    map.insert(
        "policy".into(),
        serde_json::json!("mention_may_be_considered_never_compelled"),
    );
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(events: Vec<InboundEvent>) -> EventBatch {
        EventBatch {
            instance: DEFAULT_INSTANCE.into(),
            ledger_floor: 1,
            ledger_height: events.iter().map(|e| e.seq).max().unwrap_or(0),
            gap: false,
            events,
        }
    }

    fn comment(seq: u64, actor: &str, text: &str) -> InboundEvent {
        InboundEvent {
            seq,
            kind: EventKind::Comment,
            item_id: 100,
            comment_id: Some(seq + 1000),
            parent_comment_id: None,
            actor: actor.into(),
            text: text.into(),
            reply_to: None,
            content_hash: None,
        }
    }

    #[test]
    fn event_key_dedupe_format() {
        let key = EventKey::new("news.nuts.services", 42);
        assert_eq!(key.dedupe_id(), "nutnews:news.nuts.services:42");
    }

    #[test]
    fn classifies_direct_at_mention() {
        let event = comment(1, "alice", "hey @steve what do you think?");
        assert_eq!(
            classify_mention(&event, "steve"),
            MentionSignal::DirectAtMention
        );
    }

    #[test]
    fn classifies_explicit_reply() {
        let mut event = comment(2, "bob", "following up");
        event.reply_to = Some("Steve".into());
        assert_eq!(
            classify_mention(&event, "steve"),
            MentionSignal::ExplicitReply
        );
    }

    #[test]
    fn ignores_partial_handle_prefix() {
        let event = comment(3, "alice", "email steve@example.com and @steven hi");
        // @steven is not @steve as a token boundary
        assert_eq!(classify_mention(&event, "steve"), MentionSignal::None);
        let event2 = comment(4, "alice", "hi @steve!");
        assert_eq!(
            classify_mention(&event2, "steve"),
            MentionSignal::DirectAtMention
        );
    }

    #[test]
    fn zero_width_and_fullwidth_evasion_still_match() {
        // ZWSP inside the handle and a fullwidth at-sign both normalize away.
        let zwsp = comment(1, "alice", "hi @st\u{200B}eve, thoughts?");
        assert_eq!(
            classify_mention(&zwsp, "steve"),
            MentionSignal::DirectAtMention
        );
        let fullwidth = comment(2, "alice", "hi \u{FF20}steve!");
        assert_eq!(
            classify_mention(&fullwidth, "steve"),
            MentionSignal::DirectAtMention
        );
        // Homoglyph handles stay distinct: Cyrillic 's' is NOT a match (and
        // a homoglyph actor is not self-authored) — documented residual.
        let homoglyph = comment(3, "alice", "hi @\u{0455}teve");
        assert_eq!(classify_mention(&homoglyph, "steve"), MentionSignal::None);
    }

    #[test]
    fn reply_to_with_zero_width_still_matches() {
        let mut event = comment(2, "bob", "following up");
        event.reply_to = Some("Ste\u{200C}ve".into());
        assert_eq!(
            classify_mention(&event, "steve"),
            MentionSignal::ExplicitReply
        );
    }

    #[test]
    fn mention_enqueues_consideration_never_requires_response() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        let report = ingest
            .apply_batch(batch(vec![comment(1, "alice", "@steve hello")]), 10)
            .unwrap();
        assert_eq!(report.considered, 1);
        assert_eq!(report.cursor_after, 1);
        let pending: Vec<_> = ingest.pending_considerations().cloned().collect();
        assert_eq!(pending.len(), 1);
        assert!(!pending[0].response_required);
        assert!(pending[0].may_enqueue_consideration());
        // Policy invariant across all stored considerations.
        assert!(
            ingest
                .state()
                .considerations
                .iter()
                .all(|c| !c.response_required)
        );
    }

    #[test]
    fn non_mentions_advance_cursor_without_consideration() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        let report = ingest
            .apply_batch(
                batch(vec![comment(5, "alice", "nice post, no address")]),
                11,
            )
            .unwrap();
        assert_eq!(report.considered, 0);
        assert_eq!(report.applied, 1);
        assert_eq!(ingest.cursor().last_seq, 5);
        assert_eq!(ingest.pending_considerations().count(), 0);
    }

    #[test]
    fn ignores_votes_and_self_authored() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        let events = vec![
            InboundEvent {
                seq: 1,
                kind: EventKind::Vote,
                item_id: 9,
                comment_id: None,
                parent_comment_id: None,
                actor: "carol".into(),
                text: String::new(),
                reply_to: None,
                content_hash: None,
            },
            comment(2, "steve", "@steve talking to myself"),
            comment(3, "dave", "@steve real one"),
        ];
        let report = ingest.apply_batch(batch(events), 12).unwrap();
        assert_eq!(report.considered, 1);
        assert_eq!(report.cursor_after, 3);
        let actors: Vec<_> = ingest
            .pending_considerations()
            .map(|c| c.actor.as_str())
            .collect();
        assert_eq!(actors, ["dave"]);
    }

    #[test]
    fn idempotent_redelivery_does_not_duplicate_consideration() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        let b = batch(vec![comment(7, "erin", "@steve ping")]);
        let first = ingest.apply_batch(b.clone(), 20).unwrap();
        assert_eq!(first.considered, 1);
        let second = ingest.apply_batch(b, 21).unwrap();
        assert_eq!(second.considered, 0);
        assert!(second.outcomes.iter().any(|o| matches!(
            o.outcome,
            ProcessOutcome::Skipped {
                reason: SkipReason::AlreadyProcessed
            }
        )));
        assert_eq!(ingest.state().considerations.len(), 1);
        assert_eq!(ingest.cursor().last_seq, 7);
    }

    #[test]
    fn mark_enqueued_is_idempotent() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest
            .apply_batch(batch(vec![comment(1, "alice", "@steve x")]), 1)
            .unwrap();
        let id = ingest
            .pending_considerations()
            .next()
            .unwrap()
            .consideration_id;
        assert!(ingest.mark_enqueued(id));
        assert!(!ingest.mark_enqueued(id));
        assert_eq!(ingest.pending_considerations().count(), 0);
    }

    #[test]
    fn take_pending_for_enqueue_marks_all() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest
            .apply_batch(
                batch(vec![
                    comment(1, "a", "@steve one"),
                    comment(2, "b", "@steve two"),
                ]),
                1,
            )
            .unwrap();
        let taken = ingest.take_pending_for_enqueue();
        assert_eq!(taken.len(), 2);
        assert!(taken.iter().all(|c| !c.response_required));
        assert_eq!(ingest.pending_considerations().count(), 0);
        // Second drain is empty (idempotent handoff).
        assert!(ingest.take_pending_for_enqueue().is_empty());
    }

    #[test]
    fn gap_batch_does_not_apply_or_advance() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest.force_cursor(10, 1).unwrap();
        let mut b = batch(vec![comment(50, "alice", "@steve after gap")]);
        b.gap = true;
        b.ledger_floor = 40;
        let report = ingest.apply_batch(b, 2).unwrap();
        assert!(report.gap);
        assert_eq!(report.applied, 0);
        assert_eq!(ingest.cursor().last_seq, 10);
        assert!(ingest.cursor().gap);
    }

    #[test]
    fn floor_beyond_cursor_without_gap_bit_is_treated_as_gap() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest.force_cursor(10, 1).unwrap();
        let mut b = batch(vec![comment(50, "alice", "@steve floor lied")]);
        b.ledger_floor = 40; // history 11..40 unreachable, but gap=false
        let report = ingest.apply_batch(b, 2).unwrap();
        assert!(report.gap);
        assert_eq!(report.applied, 0);
        assert_eq!(ingest.cursor().last_seq, 10);
        assert!(ingest.cursor().gap);
    }

    #[test]
    fn silent_hole_freezes_cursor_at_last_contiguous_event() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest.force_cursor(5, 1).unwrap();
        let report = ingest
            .apply_batch(
                batch(vec![
                    comment(6, "a", "six"),
                    comment(7, "b", "@steve seven"),
                    comment(9, "c", "@steve nine, but eight is missing"),
                ]),
                2,
            )
            .unwrap();
        assert_eq!(ingest.cursor().last_seq, 7);
        assert!(ingest.cursor().gap);
        assert!(report.gap);
        assert_eq!(report.considered, 1);
        assert!(report.outcomes.iter().any(|o| matches!(
            o.outcome,
            ProcessOutcome::Skipped {
                reason: SkipReason::GapRecoveryRequired
            }
        )));
        // The deferred event was NOT remembered: an honest redelivery that
        // fills the hole processes both and clears the gap flag.
        let report = ingest
            .apply_batch(
                batch(vec![
                    comment(8, "d", "eight arrives"),
                    comment(9, "c", "@steve nine, but eight is missing"),
                ]),
                3,
            )
            .unwrap();
        assert_eq!(ingest.cursor().last_seq, 9);
        assert!(!ingest.cursor().gap);
        assert!(!report.gap);
        assert_eq!(report.considered, 1);
    }

    #[test]
    fn bootstrap_accepts_arbitrary_start_but_requires_contiguity() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        let report = ingest
            .apply_batch(
                batch(vec![
                    comment(41, "a", "@steve first ever"),
                    comment(42, "b", "next"),
                    comment(44, "c", "@steve hole at 43"),
                ]),
                1,
            )
            .unwrap();
        assert_eq!(ingest.cursor().last_seq, 42);
        assert!(ingest.cursor().gap);
        assert_eq!(report.considered, 1);
    }

    #[test]
    fn batch_size_limit_rejected() {
        let events: Vec<_> = (1..=MAX_BATCH_EVENTS + 1)
            .map(|seq| comment(seq as u64, "a", "x"))
            .collect();
        let b = batch(events);
        assert!(b.validate().is_err());
    }

    #[test]
    fn max_apply_per_batch_bounds_work() {
        let mut config = MentionIngestConfig::default();
        config.max_apply_per_batch = 2;
        let mut ingest = MentionIngest::new(config).unwrap();
        let report = ingest
            .apply_batch(
                batch(vec![
                    comment(1, "a", "nope"),
                    comment(2, "b", "@steve hi"),
                    comment(3, "c", "@steve later"),
                ]),
                1,
            )
            .unwrap();
        assert_eq!(report.applied, 2);
        assert_eq!(report.considered, 1);
        assert!(report.outcomes.iter().any(|o| matches!(
            o.outcome,
            ProcessOutcome::Skipped {
                reason: SkipReason::BatchLimit
            }
        )));
        // Cursor only advanced through applied events (seq 2).
        assert_eq!(ingest.cursor().last_seq, 2);
    }

    #[test]
    fn oversized_text_is_truncated_before_storage_and_classification() {
        let mut config = MentionIngestConfig::default();
        config.max_text_bytes = 64;
        let mut ingest = MentionIngest::new(config).unwrap();
        // Mention early: survives truncation, stored text is capped.
        let long_tail = "x".repeat(10_000);
        let report = ingest
            .apply_batch(
                batch(vec![comment(1, "a", &format!("@steve {long_tail}"))]),
                1,
            )
            .unwrap();
        assert_eq!(report.considered, 1);
        let c = ingest.pending_considerations().next().unwrap();
        assert!(c.text.len() <= 64);
        assert!(c.text_truncated);
        // Mention hidden beyond the cap: explicit miss, cursor still advances.
        let padded = format!("{} @steve", "y".repeat(10_000));
        let report = ingest
            .apply_batch(batch(vec![comment(2, "b", &padded)]), 2)
            .unwrap();
        assert_eq!(report.considered, 0);
        assert_eq!(ingest.cursor().last_seq, 2);
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let (text, truncated) = truncate_utf8("héllo wörld", 6);
        assert!(truncated);
        assert!(text.len() <= 6);
        assert!(text.is_char_boundary(text.len()));
        let (text, truncated) = truncate_utf8("short", 64);
        assert_eq!(text, "short");
        assert!(!truncated);
    }

    #[test]
    fn oversized_identity_fields_skip_without_freezing() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        let mut event = comment(1, &"a".repeat(MAX_IDENT_BYTES + 1), "@steve hi");
        event.reply_to = None;
        let report = ingest.apply_batch(batch(vec![event]), 1).unwrap();
        assert_eq!(report.considered, 0);
        assert!(report.outcomes.iter().any(|o| matches!(
            o.outcome,
            ProcessOutcome::Skipped {
                reason: SkipReason::Oversized
            }
        )));
        // Malformed event is remembered and the cursor moves past it.
        assert_eq!(ingest.cursor().last_seq, 1);
    }

    #[test]
    fn per_actor_pending_cap_prevents_queue_monopoly() {
        let mut config = MentionIngestConfig::default();
        config.max_pending_per_actor = 2;
        let mut ingest = MentionIngest::new(config).unwrap();
        let events = (1..=4)
            .map(|seq| comment(seq, "spammer", &format!("@steve n{seq}")))
            .collect();
        let report = ingest.apply_batch(batch(events), 1).unwrap();
        assert_eq!(report.considered, 2);
        assert_eq!(
            report
                .outcomes
                .iter()
                .filter(|o| matches!(
                    o.outcome,
                    ProcessOutcome::Skipped {
                        reason: SkipReason::ActorSaturated
                    }
                ))
                .count(),
            2
        );
        // Saturation is explicit loss: cursor advanced past the whole batch.
        assert_eq!(ingest.cursor().last_seq, 4);
        // A different actor is unaffected.
        let report = ingest
            .apply_batch(batch(vec![comment(5, "other", "@steve too")]), 2)
            .unwrap();
        assert_eq!(report.considered, 1);
    }

    #[test]
    fn full_queue_evicts_enqueued_first_then_refuses_explicitly() {
        let mut config = MentionIngestConfig::default();
        config.consideration_capacity = 2;
        config.max_pending_per_actor = 2;
        let mut ingest = MentionIngest::new(config).unwrap();
        ingest
            .apply_batch(
                batch(vec![
                    comment(1, "a", "@steve one"),
                    comment(2, "b", "@steve two"),
                ]),
                1,
            )
            .unwrap();
        // Queue full of PENDING items: a third mention is refused, never
        // displacing a pending mention, and the loss is an explicit outcome.
        let report = ingest
            .apply_batch(batch(vec![comment(3, "c", "@steve three")]), 2)
            .unwrap();
        assert_eq!(report.considered, 0);
        assert!(report.outcomes.iter().any(|o| matches!(
            o.outcome,
            ProcessOutcome::Skipped {
                reason: SkipReason::ConsiderationCapacity
            }
        )));
        assert_eq!(ingest.pending_considerations().count(), 2);
        // Once handed off, enqueued slots become evictable and admission resumes.
        let _ = ingest.take_pending_for_enqueue();
        let report = ingest
            .apply_batch(batch(vec![comment(4, "d", "@steve four")]), 3)
            .unwrap();
        assert_eq!(report.considered, 1);
        assert_eq!(ingest.state().considerations.len(), 2);
    }

    #[test]
    fn consideration_and_request_ids_are_deterministic() {
        let mut first = MentionIngest::with_defaults().unwrap();
        let mut second = MentionIngest::with_defaults().unwrap();
        let b = batch(vec![comment(7, "erin", "@steve ping")]);
        first.apply_batch(b.clone(), 1).unwrap();
        second.apply_batch(b, 99).unwrap();
        let c1 = first.pending_considerations().next().unwrap();
        let c2 = second.pending_considerations().next().unwrap();
        assert_eq!(c1.consideration_id, c2.consideration_id);
        assert_eq!(c1.request_id, c2.request_id);
        // Different events get different ids.
        let other_key = EventKey::new(DEFAULT_INSTANCE, 8);
        assert_ne!(c1.request_id, stable_uuid("request", &other_key));
        // Tag domains are separated.
        assert_ne!(c1.consideration_id, c1.request_id);
    }

    #[test]
    fn forward_force_cursor_ok_rewind_requires_explicit_api() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest.force_cursor(10, 1).unwrap();
        assert!(ingest.force_cursor(5, 2).is_err());
        assert_eq!(ingest.cursor().last_seq, 10);
        ingest.force_cursor_rewind(5, 3);
        assert_eq!(ingest.cursor().last_seq, 5);
    }

    #[test]
    fn rewind_with_retained_state_does_not_duplicate_considerations() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        let b = batch(vec![comment(7, "erin", "@steve ping")]);
        ingest.apply_batch(b.clone(), 1).unwrap();
        ingest.force_cursor_rewind(0, 2);
        // Dedupe key retained → AlreadyProcessed.
        let report = ingest.apply_batch(b.clone(), 3).unwrap();
        assert_eq!(report.considered, 0);
        assert_eq!(ingest.state().considerations.len(), 1);
        // Even if the dedupe ring had evicted the key, the retained
        // consideration for the same event key blocks a duplicate.
        ingest.state.processed_keys.clear();
        ingest.state.processed_set.clear();
        ingest.force_cursor_rewind(0, 4);
        let report = ingest.apply_batch(b, 5).unwrap();
        assert_eq!(report.considered, 0);
        assert_eq!(ingest.state().considerations.len(), 1);
    }

    #[test]
    fn serde_roundtrip_state() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest
            .apply_batch(batch(vec![comment(3, "zoe", "cc @steve")]), 99)
            .unwrap();
        let json = serde_json::to_string_pretty(ingest.state()).unwrap();
        let restored: MentionIngestState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.cursor.last_seq, 3);
        assert_eq!(restored.considerations.len(), 1);
        assert!(!restored.considerations[0].response_required);
    }

    #[test]
    fn durable_save_load_roundtrip_and_pinning() {
        let dir = std::env::temp_dir().join(format!("mention-ingest-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mention-ingest.json");
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest
            .apply_batch(batch(vec![comment(9, "ada", "@steve durable")]), 5)
            .unwrap();
        ingest.state().save(&path).unwrap();
        let loaded = MentionIngestState::load(&path).unwrap();
        assert_eq!(loaded.cursor.last_seq, 9);
        assert_eq!(loaded.processed_set.len(), 1);
        // Pinned load accepts matching identity, refuses a different pin.
        assert!(MentionIngestState::load_pinned(&path, DEFAULT_INSTANCE, "steve").is_ok());
        assert!(MentionIngestState::load_pinned(&path, "other.example", "steve").is_err());
        assert!(MentionIngestState::load_pinned(&path, DEFAULT_INSTANCE, "notsteve").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rebuilds_membership_set_from_key_ring() {
        let dir = std::env::temp_dir().join(format!("mention-ingest-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mention-ingest.json");
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest
            .apply_batch(batch(vec![comment(1, "ada", "@steve x")]), 5)
            .unwrap();
        let mut state = ingest.into_state();
        // Simulate a hand-edited file whose set disagrees with the ring.
        state.processed_set.clear();
        state.save(&path).unwrap();
        let loaded = MentionIngestState::load(&path).unwrap();
        assert_eq!(loaded.processed_set.len(), loaded.processed_keys.len());
        assert!(
            loaded
                .processed_set
                .contains(&EventKey::new(DEFAULT_INSTANCE, 1).dedupe_id())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn instance_and_handle_normalization() {
        assert_eq!(
            normalize_instance("  News.Nuts.Services ").unwrap(),
            "news.nuts.services"
        );
        assert!(normalize_instance("https://news.nuts.services").is_err());
        assert!(normalize_instance("evil.example/../news").is_err());
        assert!(normalize_instance(&"n".repeat(MAX_INSTANCE_BYTES + 1)).is_err());
        assert!(normalize_instance(".starts-with-dot").is_err());
        assert_eq!(normalize_handle(" Steve ").unwrap(), "steve");
        assert!(normalize_handle("st eve").is_err());
        assert!(normalize_handle("\u{0455}teve").is_err()); // homoglyph config refused
        // Batches with equivalent-but-uncanonical instance still match.
        let mut ingest = MentionIngest::with_defaults().unwrap();
        let mut b = batch(vec![comment(1, "a", "@steve hi")]);
        b.instance = "News.Nuts.Services".into();
        assert_eq!(ingest.apply_batch(b, 1).unwrap().considered, 1);
    }

    #[test]
    fn consideration_payload_never_marks_response_required() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        ingest
            .apply_batch(batch(vec![comment(1, "a", "@steve p")]), 1)
            .unwrap();
        let c = ingest.pending_considerations().next().unwrap();
        let payload = consideration_task_payload(c);
        assert_eq!(payload["response_required"], serde_json::json!(false));
        assert_eq!(
            payload["policy"],
            serde_json::json!("mention_may_be_considered_never_compelled")
        );
    }

    #[test]
    fn instance_mismatch_is_error() {
        let mut ingest = MentionIngest::with_defaults().unwrap();
        let mut b = batch(vec![comment(1, "a", "@steve")]);
        b.instance = "other.example".into();
        assert!(ingest.apply_batch(b, 1).is_err());
    }

    #[test]
    fn config_validation() {
        let mut cfg = MentionIngestConfig::default();
        cfg.dedupe_capacity = 0;
        assert!(MentionIngest::new(cfg).is_err());
        let mut cfg = MentionIngestConfig::default();
        cfg.max_text_bytes = MAX_TEXT_BYTES_CEILING + 1;
        assert!(MentionIngest::new(cfg).is_err());
        let mut cfg = MentionIngestConfig::default();
        cfg.max_pending_per_actor = 0;
        assert!(MentionIngest::new(cfg).is_err());
        let mut cfg = MentionIngestConfig::default();
        cfg.consideration_capacity = 2;
        cfg.max_pending_per_actor = 3;
        assert!(MentionIngest::new(cfg).is_err());
        let mut cfg = MentionIngestConfig::default();
        cfg.steve_handle = "https://steve".into();
        assert!(MentionIngest::new(cfg).is_err());
    }

    #[test]
    fn no_api_compels_response() {
        // Structural: Consideration always ships with response_required=false
        // and there is no constructor that can set it true via public apply path.
        let mut ingest = MentionIngest::with_defaults().unwrap();
        for seq in 1..20 {
            let _ = ingest.apply_batch(
                batch(vec![comment(seq, "user", &format!("@steve n{seq}"))]),
                seq,
            );
        }
        assert!(!ingest.state().considerations.is_empty());
        assert!(
            ingest
                .state()
                .considerations
                .iter()
                .all(|c| !c.response_required && c.may_enqueue_consideration())
        );
    }
}
