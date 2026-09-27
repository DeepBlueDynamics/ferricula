//! Bounded, append-only overlay for the recovered (read-only) memory base.
//!
//! Implements Stage 2 of `research/original-ferricula/MEMORY_DREAM_SPEC.md`:
//! the recovered base volume is **immutable**; every write intention Steve
//! forms — a new memory, a reinforcement, a lifecycle transition, a graph
//! edge, a dream-consolidation candidate, an advocate note — is recorded as
//! an overlay *event*. A union engine (integration, not this file) folds the
//! base and the overlay at read time; the offline `promote` tool (Stage 3)
//! compacts them later.
//!
//! Guarantees, by construction:
//!
//! 1. **Base immutability.** This module holds no reference to
//!    `DurableEngine`, opens no recovered path, and exposes no API that
//!    could reach one. Its only I/O is the overlay document itself
//!    (atomic tmp+rename, the crate's persistence idiom).
//! 2. **Append-only, tamper-evident.** Events form a hash chain: each
//!    deterministic `event_id` commits to the schema version, sequence,
//!    parent id, timestamp, and canonical payload. `load` re-derives the
//!    chain and refuses documents whose history was edited, reordered,
//!    or truncated mid-chain.
//! 3. **Destruction and promotion need an operator.** Pruning, base-edge
//!    removal, dream merges (which tombstone members), keystone proposals,
//!    and archive-revival are recorded with `requires_approval = true` and
//!    stay *ineffective* until a matching `Approval` event — itself
//!    append-only — decides them. A rejection is permanent.
//! 4. **Bounded.** Event count, text/note bytes, tags, and dream-candidate
//!    membership all have hard caps; the spec's uncapped-ingestion and
//!    noisy-advocate failure modes are structurally excluded (advocate
//!    notes live in a quarantined side-channel, never the semantic index).
//!
//! Callers supply `now` (unix seconds); the module reads no clock. Not
//! wired into `lib.rs`; the integrator adds `pub mod memory_overlay;`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Version of the overlay document schema. `load` accepts exactly this
/// version; older documents need an explicit migration (none exist yet).
pub const OVERLAY_SCHEMA_VERSION: u32 = 1;

/// Parent id of the first event in a chain.
pub const GENESIS_PARENT: &str = "genesis";

/// Hard cap on retained overlay events; promotion compacts, append refuses.
pub const DEFAULT_MAX_EVENTS: usize = 65_536;

/// Byte cap for proposed memory text.
pub const DEFAULT_MAX_TEXT_BYTES: usize = 16 * 1024;

/// Byte cap for advocate/approval notes and dream rationales.
pub const DEFAULT_MAX_NOTE_BYTES: usize = 4 * 1024;

/// Cap on tags per proposed memory.
pub const DEFAULT_MAX_TAGS: usize = 16;

/// Cap on members in one dream-consolidation candidate.
pub const DEFAULT_MAX_DREAM_MEMBERS: usize = 64;

/// Byte cap on base memory ids, tags, channels, and relations.
pub const MAX_IDENT_BYTES: usize = 64;

/// Filenames that belong to the recovered base volume. The overlay must
/// never open these for write — doing so would violate the read-only core
/// invariant (VISION_PARITY_ROADMAP §2, MEMORY_DREAM_SPEC §3 Stage 2).
pub const BASE_ARTIFACT_NAMES: &[&str] = &[
    "snapshot_v4.bin",
    "snapshot_v5.bin",
    "wal.log",
    "identity.json",
    "agent.toml",
];

// Spec constants (§1B, §1D of MEMORY_DREAM_SPEC.md). The overlay records
// *intentions*; the union engine applies this math. Kept here so both sides
// share one source of truth once integrated.
pub const FIDELITY_GATE: f64 = 0.75;
pub const ALPHA_MIN: f64 = 0.001;
pub const ALPHA_MAX: f64 = 0.02;
pub const RECALL_ALPHA_SHRINK: f64 = 0.95;
pub const NEGLECT_ALPHA_GROWTH: f64 = 1.005;
pub const HALO_ALPHA_SHRINK: f64 = 0.99;
pub const CONSOLIDATION_SIMILARITY_FLOOR: f64 = 0.85;

/// A memory identity visible to the overlay: either a record in the
/// immutable recovered base, or a memory proposed by an overlay event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRef {
    /// Id of a record in the recovered base (opaque; never dereferenced here).
    Base(String),
    /// `event_id` of an effective `ProposeMemory` overlay event.
    Overlay(String),
}

impl MemoryRef {
    /// Stable map key: `base:<id>` / `overlay:<event_id>`.
    pub fn key(&self) -> String {
        match self {
            MemoryRef::Base(id) => format!("base:{id}"),
            MemoryRef::Overlay(id) => format!("overlay:{id}"),
        }
    }

    fn validate_shape(&self) -> Result<()> {
        match self {
            MemoryRef::Base(id) => validate_ident(id, "base memory id"),
            MemoryRef::Overlay(id) => {
                if !looks_like_event_id(id) {
                    bail!("overlay ref {id:?} is not a well-formed event id");
                }
                Ok(())
            }
        }
    }
}

/// Lifecycle states from the spec (§1C). `Pruned` is terminal and only ever
/// reached through an approved destructive transition (tombstone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Active,
    Forgiven,
    Archived,
    Pruned,
}

/// Classification of a lifecycle edge; drives the approval requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionClass {
    /// Spec-forward decay path (Active→Forgiven, Forgiven→Archived).
    Forward,
    /// Recall-driven recovery (Forgiven→Active).
    Restore,
    /// Archive revival — promotion, operator-gated.
    Promotion,
    /// Any →Pruned — destructive tombstone, operator-gated.
    Destructive,
}

fn transition_class(from: LifecycleState, to: LifecycleState) -> Option<TransitionClass> {
    use LifecycleState::*;
    match (from, to) {
        (Active, Forgiven) | (Forgiven, Archived) => Some(TransitionClass::Forward),
        (Forgiven, Active) => Some(TransitionClass::Restore),
        (Archived, Active) => Some(TransitionClass::Promotion),
        (Active, Pruned) | (Forgiven, Pruned) | (Archived, Pruned) => {
            Some(TransitionClass::Destructive)
        }
        _ => None,
    }
}

/// Reinforcement kinds from the spec (§1B); the union engine applies the
/// corresponding alpha math within `[ALPHA_MIN, ALPHA_MAX]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReinforcementKind {
    /// Recall: alpha ×= RECALL_ALPHA_SHRINK.
    Recall,
    /// Keystone halo touch: alpha ×= HALO_ALPHA_SHRINK.
    HaloTouch,
    /// Neglect (>24h stale): alpha ×= NEGLECT_ALPHA_GROWTH.
    Neglect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeAction {
    Add,
    Remove,
}

/// Values-alignment verdict recorded by the internal advocate cycle. Notes
/// are quarantined in the overlay side-channel; they never enter the
/// semantic index unless a later promotion deliberately moves them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvocateVerdict {
    Aligned,
    Tension,
    Violation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

/// Approval status of an event, folded from later `Approval` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    NotRequired,
    Pending,
    Approved,
    Rejected,
}

/// The append-only overlay event payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayPayload {
    /// A new memory Steve proposes. Its identity, once effective, is the
    /// event id (`MemoryRef::Overlay`). Keystone proposals are promotion
    /// (keystones are decay-immune per spec §1C) and need approval.
    ProposeMemory {
        text: String,
        channel: String,
        importance: f64,
        keystone_proposed: bool,
        tags: Vec<String>,
    },
    /// Bounded alpha nudge on an existing memory.
    Reinforce {
        target: MemoryRef,
        kind: ReinforcementKind,
    },
    /// Lifecycle transition intention. `from` asserts the current state
    /// (base state for first-touch refs; must match overlay history after).
    Lifecycle {
        target: MemoryRef,
        from: LifecycleState,
        to: LifecycleState,
    },
    /// Graph edge intention. Removal tombstones base structure → approval.
    GraphEdge {
        from: MemoryRef,
        to: MemoryRef,
        relation: String,
        action: EdgeAction,
    },
    /// Dream-consolidation candidate (spec §1D): merging members into a
    /// survivor tombstones the non-survivors, so applying it needs approval.
    DreamCandidate {
        members: Vec<MemoryRef>,
        similarity: f64,
        rationale: String,
    },
    /// Quarantined advocate reflection; never enters the semantic index.
    AdvocateNote {
        verdict: AdvocateVerdict,
        note: String,
    },
    /// Operator decision on an earlier approval-gated event.
    Approval {
        target_event_id: String,
        decision: ApprovalDecision,
        note: String,
    },
}

impl OverlayPayload {
    /// Which payloads are destructive or promotional and therefore gated.
    fn requires_approval(&self) -> bool {
        match self {
            OverlayPayload::ProposeMemory {
                keystone_proposed, ..
            } => *keystone_proposed,
            OverlayPayload::Reinforce { .. } => false,
            OverlayPayload::Lifecycle { from, to, .. } => matches!(
                transition_class(*from, *to),
                Some(TransitionClass::Destructive) | Some(TransitionClass::Promotion)
            ),
            OverlayPayload::GraphEdge { action, .. } => *action == EdgeAction::Remove,
            OverlayPayload::DreamCandidate { .. } => true,
            OverlayPayload::AdvocateNote { .. } => false,
            OverlayPayload::Approval { .. } => false,
        }
    }

    fn kind_name(&self) -> &'static str {
        match self {
            OverlayPayload::ProposeMemory { .. } => "propose_memory",
            OverlayPayload::Reinforce { .. } => "reinforce",
            OverlayPayload::Lifecycle { .. } => "lifecycle",
            OverlayPayload::GraphEdge { .. } => "graph_edge",
            OverlayPayload::DreamCandidate { .. } => "dream_candidate",
            OverlayPayload::AdvocateNote { .. } => "advocate_note",
            OverlayPayload::Approval { .. } => "approval",
        }
    }
}

/// One link in the overlay chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayEvent {
    pub schema_version: u32,
    /// 1-based position in the chain.
    pub seq: u64,
    /// Deterministic id committing to everything above plus the payload.
    pub event_id: String,
    /// `event_id` of the previous event, or [`GENESIS_PARENT`].
    pub parent_id: String,
    pub recorded_at: u64,
    /// Derived from the payload class; re-derived (never trusted) on load.
    pub requires_approval: bool,
    pub payload: OverlayPayload,
}

/// Bounds for the overlay log. All defaults are safe; validation follows
/// the crate's `config.rs` style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    pub max_events: usize,
    pub max_text_bytes: usize,
    pub max_note_bytes: usize,
    pub max_tags: usize,
    pub max_dream_members: usize,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            max_events: DEFAULT_MAX_EVENTS,
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
            max_note_bytes: DEFAULT_MAX_NOTE_BYTES,
            max_tags: DEFAULT_MAX_TAGS,
            max_dream_members: DEFAULT_MAX_DREAM_MEMBERS,
        }
    }
}

impl OverlayConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_events == 0 {
            bail!("overlay.max_events must be positive");
        }
        if self.max_text_bytes == 0 || self.max_text_bytes > 1024 * 1024 {
            bail!("overlay.max_text_bytes must be in 1..=1048576");
        }
        if self.max_note_bytes == 0 || self.max_note_bytes > 64 * 1024 {
            bail!("overlay.max_note_bytes must be in 1..=65536");
        }
        if self.max_tags == 0 || self.max_tags > 64 {
            bail!("overlay.max_tags must be in 1..=64");
        }
        if self.max_dream_members < 2 || self.max_dream_members > 1024 {
            bail!("overlay.max_dream_members must be in 2..=1024");
        }
        Ok(())
    }
}

/// Serialized document shape (what `save` writes and `load` verifies).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OverlayDocument {
    schema_version: u32,
    config: OverlayConfig,
    events: Vec<OverlayEvent>,
}

/// A pending operator decision, surfaced for review UIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub event_id: String,
    pub kind: String,
    pub recorded_at: u64,
}

/// Fold of the overlay for the union engine: only *effective* events
/// contribute (unapproved gated events are visible solely as pending).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OverlayProjection {
    /// Effective proposed memories, keyed by their overlay event id.
    pub proposed_memories: BTreeMap<String, ProposedMemoryView>,
    /// Latest effective lifecycle state per memory key (see `MemoryRef::key`).
    pub lifecycle_overrides: BTreeMap<String, LifecycleState>,
    /// Memory keys effectively pruned (tombstones for promotion).
    pub tombstones: BTreeSet<String>,
    /// Effective edge additions / removals: (from key, to key, relation).
    pub edge_adds: BTreeSet<(String, String, String)>,
    pub edge_removes: BTreeSet<(String, String, String)>,
    /// Reinforcement counts per memory key: (recalls, halo touches, neglects).
    pub reinforcements: BTreeMap<String, (u64, u64, u64)>,
    /// Approved dream candidates awaiting offline application, by event id.
    pub approved_dream_candidates: Vec<String>,
    /// Quarantined advocate notes, by event id (side-channel, not memories).
    pub advocate_note_ids: Vec<String>,
    /// Gated events with no decision yet.
    pub pending_approvals: Vec<PendingApproval>,
}

/// Effective proposed-memory content for the union engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposedMemoryView {
    pub text: String,
    pub channel: String,
    pub importance: f64,
    pub keystone: bool,
    pub tags: Vec<String>,
    pub recorded_at: u64,
}

/// Explicit separation of the immutable base root from the overlay document.
/// Construction fails if the overlay path would write into a base artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPaths {
    /// Recovered base directory (read-only by policy; never written here).
    pub base_root: PathBuf,
    /// Overlay JSON document path (must live outside base artifacts).
    pub overlay_file: PathBuf,
}

impl OverlayPaths {
    pub fn new(base_root: impl Into<PathBuf>, overlay_file: impl Into<PathBuf>) -> Result<Self> {
        let base_root = base_root.into();
        let overlay_file = overlay_file.into();
        assert_safe_overlay_write_path(&overlay_file)?;
        if overlay_file == base_root {
            bail!("overlay path must not be the base root directory itself");
        }
        if paths_collide(&base_root, &overlay_file) {
            bail!(
                "overlay file {} collides with immutable base root {}; \
                 overlay must be a separate writable path",
                overlay_file.display(),
                base_root.display()
            );
        }
        Ok(Self {
            base_root,
            overlay_file,
        })
    }

    /// The only path this module will write.
    pub fn overlay_file(&self) -> &Path {
        &self.overlay_file
    }

    /// Base root is exposed for the union engine to open **read-only**.
    /// This module never writes there.
    pub fn base_root(&self) -> &Path {
        &self.base_root
    }

    pub fn save(&self, log: &OverlayLog) -> Result<()> {
        assert_safe_overlay_write_path(&self.overlay_file)?;
        log.save(&self.overlay_file)
    }

    pub fn load(&self) -> Result<OverlayLog> {
        OverlayLog::load(&self.overlay_file)
    }
}

/// Refuse paths whose leaf is a known base-volume artifact.
pub fn assert_safe_overlay_write_path(path: &Path) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.is_empty() {
        bail!("overlay path must include a file name");
    }
    for artifact in BASE_ARTIFACT_NAMES {
        if name == *artifact {
            bail!(
                "refusing to write overlay to base artifact {name:?}; \
                 legacy volume must remain immutable"
            );
        }
    }
    // Explicit markers operators use for the read-only mount.
    let path_str = path.to_string_lossy().to_ascii_lowercase();
    if path_str.contains("snapshot_v4.bin")
        || path_str.contains("snapshot_v5.bin")
        || path_str.ends_with("/wal.log")
        || path_str.ends_with("\\wal.log")
    {
        bail!("refusing overlay path that embeds a base artifact name");
    }
    Ok(())
}

fn paths_collide(base_root: &Path, overlay_file: &Path) -> bool {
    if base_root == overlay_file {
        return true;
    }
    // Overlay file path equals a direct child artifact of the base.
    if let Some(name) = overlay_file.file_name() {
        if base_root.join(name) == *overlay_file
            && BASE_ARTIFACT_NAMES
                .iter()
                .any(|a| name == std::ffi::OsStr::new(a))
        {
            return true;
        }
    }
    false
}

/// The append-only overlay log.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayLog {
    config: OverlayConfig,
    events: Vec<OverlayEvent>,
}

impl OverlayLog {
    pub fn new(config: OverlayConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            events: Vec::new(),
        })
    }

    pub fn with_defaults() -> Result<Self> {
        Self::new(OverlayConfig::default())
    }

    pub fn config(&self) -> &OverlayConfig {
        &self.config
    }

    pub fn events(&self) -> &[OverlayEvent] {
        &self.events
    }

    pub fn get(&self, event_id: &str) -> Option<&OverlayEvent> {
        self.events.iter().find(|e| e.event_id == event_id)
    }

    /// Validate and append one event. Returns the recorded event.
    ///
    /// `now` must be ≥ the previous event's `recorded_at` so the chain is
    /// temporally ordered as well as hash-linked.
    pub fn append(&mut self, payload: OverlayPayload, now: u64) -> Result<&OverlayEvent> {
        if self.events.len() >= self.config.max_events {
            bail!(
                "overlay is full ({} events); run offline promotion to compact",
                self.config.max_events
            );
        }
        if let Some(prev) = self.events.last() {
            if now < prev.recorded_at {
                bail!(
                    "recorded_at {now} is earlier than previous event at {}; \
                     append order must be non-decreasing",
                    prev.recorded_at
                );
            }
        }
        self.validate_payload(&payload)?;
        let seq = self.events.len() as u64 + 1;
        let parent_id = self
            .events
            .last()
            .map(|e| e.event_id.clone())
            .unwrap_or_else(|| GENESIS_PARENT.to_string());
        if seq == 1 && parent_id != GENESIS_PARENT {
            bail!("first event parent must be {GENESIS_PARENT}");
        }
        let requires_approval = payload.requires_approval();
        // Approvals never gate themselves and never rewrite history.
        if matches!(payload, OverlayPayload::Approval { .. }) && requires_approval {
            bail!("internal error: Approval payload must not require approval");
        }
        let event_id = derive_event_id(OVERLAY_SCHEMA_VERSION, seq, &parent_id, now, &payload)?;
        self.events.push(OverlayEvent {
            schema_version: OVERLAY_SCHEMA_VERSION,
            seq,
            event_id,
            parent_id,
            recorded_at: now,
            requires_approval,
            payload,
        });
        Ok(self.events.last().expect("event just pushed"))
    }

    /// Approval status of an event in this log.
    pub fn approval_status(&self, event_id: &str) -> Option<ApprovalStatus> {
        let event = self.get(event_id)?;
        if !event.requires_approval {
            return Some(ApprovalStatus::NotRequired);
        }
        Some(match self.decision_for(event_id) {
            Some(ApprovalDecision::Approved) => ApprovalStatus::Approved,
            Some(ApprovalDecision::Rejected) => ApprovalStatus::Rejected,
            None => ApprovalStatus::Pending,
        })
    }

    /// An event contributes to the projection only when effective.
    pub fn is_effective(&self, event: &OverlayEvent) -> bool {
        !event.requires_approval
            || self.decision_for(&event.event_id) == Some(ApprovalDecision::Approved)
    }

    fn decision_for(&self, event_id: &str) -> Option<ApprovalDecision> {
        // At most one decision is legal; `verify` enforces uniqueness. First
        // match wins for projection stability if a corrupt log is inspected
        // before verify (load always verifies).
        self.events.iter().find_map(|e| match &e.payload {
            OverlayPayload::Approval {
                target_event_id,
                decision,
                ..
            } if target_event_id == event_id => Some(*decision),
            _ => None,
        })
    }

    /// Fold the chain into the union-engine view.
    pub fn projection(&self) -> OverlayProjection {
        let mut p = OverlayProjection::default();
        for event in &self.events {
            let effective = self.is_effective(event);
            if event.requires_approval && self.decision_for(&event.event_id).is_none() {
                p.pending_approvals.push(PendingApproval {
                    event_id: event.event_id.clone(),
                    kind: event.payload.kind_name().to_string(),
                    recorded_at: event.recorded_at,
                });
            }
            if !effective {
                continue;
            }
            match &event.payload {
                OverlayPayload::ProposeMemory {
                    text,
                    channel,
                    importance,
                    keystone_proposed,
                    tags,
                } => {
                    p.proposed_memories.insert(
                        event.event_id.clone(),
                        ProposedMemoryView {
                            text: text.clone(),
                            channel: channel.clone(),
                            importance: *importance,
                            keystone: *keystone_proposed,
                            tags: tags.clone(),
                            recorded_at: event.recorded_at,
                        },
                    );
                }
                OverlayPayload::Reinforce { target, kind } => {
                    let entry = p.reinforcements.entry(target.key()).or_insert((0, 0, 0));
                    match kind {
                        ReinforcementKind::Recall => entry.0 += 1,
                        ReinforcementKind::HaloTouch => entry.1 += 1,
                        ReinforcementKind::Neglect => entry.2 += 1,
                    }
                }
                OverlayPayload::Lifecycle { target, to, .. } => {
                    let key = target.key();
                    p.lifecycle_overrides.insert(key.clone(), *to);
                    if *to == LifecycleState::Pruned {
                        p.tombstones.insert(key);
                    }
                }
                OverlayPayload::GraphEdge {
                    from,
                    to,
                    relation,
                    action,
                } => {
                    let edge = (from.key(), to.key(), relation.clone());
                    match action {
                        EdgeAction::Add => {
                            p.edge_removes.remove(&edge);
                            p.edge_adds.insert(edge);
                        }
                        EdgeAction::Remove => {
                            p.edge_adds.remove(&edge);
                            p.edge_removes.insert(edge);
                        }
                    }
                }
                OverlayPayload::DreamCandidate { .. } => {
                    p.approved_dream_candidates.push(event.event_id.clone());
                }
                OverlayPayload::AdvocateNote { .. } => {
                    p.advocate_note_ids.push(event.event_id.clone());
                }
                OverlayPayload::Approval { .. } => {}
            }
        }
        p
    }

    /// Verify the full hash chain and per-event invariants. `load`/`save`
    /// call this; it is public so operators can audit a document on demand.
    pub fn verify(&self) -> Result<()> {
        let mut parent = GENESIS_PARENT.to_string();
        let mut last_ts: Option<u64> = None;
        let mut seen_ids = BTreeSet::new();
        let mut approval_targets: BTreeMap<String, u64> = BTreeMap::new();
        for (index, event) in self.events.iter().enumerate() {
            let seq = index as u64 + 1;
            if event.schema_version != OVERLAY_SCHEMA_VERSION {
                bail!(
                    "event {} has schema version {}, expected {OVERLAY_SCHEMA_VERSION}",
                    event.event_id,
                    event.schema_version
                );
            }
            if event.seq != seq {
                bail!(
                    "event {} has seq {}, expected {seq}",
                    event.event_id,
                    event.seq
                );
            }
            if event.parent_id != parent {
                bail!(
                    "event {} parent {} breaks the chain (expected {parent})",
                    event.event_id,
                    event.parent_id
                );
            }
            if seq == 1 && event.parent_id != GENESIS_PARENT {
                bail!("first event must parent to {GENESIS_PARENT}");
            }
            if !looks_like_event_id(&event.event_id) {
                bail!("event id {} is malformed", event.event_id);
            }
            if !seen_ids.insert(event.event_id.clone()) {
                bail!("duplicate event id {} in chain", event.event_id);
            }
            if let Some(prev_ts) = last_ts {
                if event.recorded_at < prev_ts {
                    bail!(
                        "event {} recorded_at {} is before previous {}; \
                         chain timestamps must be non-decreasing",
                        event.event_id,
                        event.recorded_at,
                        prev_ts
                    );
                }
            }
            last_ts = Some(event.recorded_at);
            let expected = derive_event_id(
                event.schema_version,
                seq,
                &parent,
                event.recorded_at,
                &event.payload,
            )?;
            if event.event_id != expected {
                bail!(
                    "event {} fails hash verification: payload or metadata was altered",
                    event.event_id
                );
            }
            if event.requires_approval != event.payload.requires_approval() {
                bail!(
                    "event {} approval flag does not match its payload class",
                    event.event_id
                );
            }
            self.validate_bounds(&event.payload)?;
            if let OverlayPayload::Approval {
                target_event_id, ..
            } = &event.payload
            {
                if approval_targets.contains_key(target_event_id) {
                    bail!(
                        "event {} is a second decision for {target_event_id}; \
                         approvals must be unique and final",
                        event.event_id
                    );
                }
                // Target must appear earlier in the chain (not self, not future).
                if target_event_id == &event.event_id {
                    bail!("approval event cannot target itself");
                }
                if !seen_ids.contains(target_event_id) {
                    // Target not yet seen — invalid ordering / corruption.
                    // (Normal append also requires target to exist already.)
                    bail!(
                        "approval event {} targets {target_event_id} which is \
                         not a prior event in the chain",
                        event.event_id
                    );
                }
                approval_targets.insert(target_event_id.clone(), event.seq);
            }
            parent = event.event_id.clone();
        }
        // Cross-check approval targets required approval and exist.
        for (target_id, _seq) in &approval_targets {
            let Some(target) = self.get(target_id) else {
                bail!("approval targets missing event {target_id}");
            };
            if !target.requires_approval {
                bail!("approval targets event {target_id} which does not require approval");
            }
        }
        Ok(())
    }

    /// Atomic persistence: verify, then whole-document write via tmp+rename.
    /// Refuses base-artifact paths so the recovered volume stays immutable.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        assert_safe_overlay_write_path(path)?;
        self.verify()
            .with_context(|| "refusing to save overlay that fails verification")?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
        }
        let document = OverlayDocument {
            schema_version: OVERLAY_SCHEMA_VERSION,
            config: self.config.clone(),
            events: self.events.clone(),
        };
        // Sibling temp file (same directory) so rename is atomic on one FS.
        let tmp = sibling_temp_path(path);
        let bytes = serde_json::to_vec_pretty(&document)?;
        fs::write(&tmp, bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| {
            let _ = fs::remove_file(&tmp);
            format!("failed to replace {}", path.display())
        })?;
        Ok(())
    }

    /// Reload a document, verifying schema version, config bounds, and the
    /// entire hash chain before accepting any of it. On failure the on-disk
    /// file is left untouched and no partial state is returned.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes =
            fs::read(path).with_context(|| format!("failed to read overlay {}", path.display()))?;
        if bytes.is_empty() {
            bail!(
                "overlay {} is empty (corrupt or truncated write)",
                path.display()
            );
        }
        let document: OverlayDocument = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse overlay {}", path.display()))?;
        if document.schema_version != OVERLAY_SCHEMA_VERSION {
            bail!(
                "overlay {} has schema version {}, this build supports {OVERLAY_SCHEMA_VERSION}; \
                 run the matching migration before loading",
                path.display(),
                document.schema_version
            );
        }
        document.config.validate()?;
        if document.events.len() > document.config.max_events {
            bail!(
                "overlay {} holds {} events, above its own cap {}",
                path.display(),
                document.events.len(),
                document.config.max_events
            );
        }
        let log = Self {
            config: document.config,
            events: document.events,
        };
        log.verify()
            .with_context(|| format!("overlay {} failed verification", path.display()))?;
        Ok(log)
    }

    // ---- validation -----------------------------------------------------

    /// Structural bounds only (no cross-event state); reused by `verify`.
    fn validate_bounds(&self, payload: &OverlayPayload) -> Result<()> {
        match payload {
            OverlayPayload::ProposeMemory {
                text,
                channel,
                importance,
                tags,
                ..
            } => {
                if text.trim().is_empty() {
                    bail!("proposed memory text must be non-empty");
                }
                if text.len() > self.config.max_text_bytes {
                    bail!(
                        "proposed memory text exceeds {} bytes",
                        self.config.max_text_bytes
                    );
                }
                validate_ident(channel, "channel")?;
                if !importance.is_finite() || !(0.0..=1.0).contains(importance) {
                    bail!("importance must be finite and in 0..=1");
                }
                if tags.len() > self.config.max_tags {
                    bail!("too many tags (max {})", self.config.max_tags);
                }
                let mut seen = BTreeSet::new();
                for tag in tags {
                    validate_ident(tag, "tag")?;
                    if !seen.insert(tag) {
                        bail!("duplicate tag {tag:?}");
                    }
                }
                Ok(())
            }
            OverlayPayload::Reinforce { target, .. } => target.validate_shape(),
            OverlayPayload::Lifecycle { target, from, to } => {
                target.validate_shape()?;
                if transition_class(*from, *to).is_none() {
                    bail!("lifecycle transition {from:?} -> {to:?} is not a legal edge");
                }
                Ok(())
            }
            OverlayPayload::GraphEdge {
                from, to, relation, ..
            } => {
                from.validate_shape()?;
                to.validate_shape()?;
                if from == to {
                    bail!("graph edge cannot connect a memory to itself");
                }
                validate_ident(relation, "relation")
            }
            OverlayPayload::DreamCandidate {
                members,
                similarity,
                rationale,
            } => {
                if members.len() < 2 {
                    bail!("dream candidate needs at least 2 members");
                }
                if members.len() > self.config.max_dream_members {
                    bail!(
                        "dream candidate exceeds {} members",
                        self.config.max_dream_members
                    );
                }
                let mut seen = BTreeSet::new();
                for member in members {
                    member.validate_shape()?;
                    if !seen.insert(member.key()) {
                        bail!("duplicate dream member {}", member.key());
                    }
                }
                if !similarity.is_finite()
                    || !(CONSOLIDATION_SIMILARITY_FLOOR..=1.0).contains(similarity)
                {
                    bail!(
                        "dream similarity must be finite and in {CONSOLIDATION_SIMILARITY_FLOOR}..=1"
                    );
                }
                if rationale.len() > self.config.max_note_bytes {
                    bail!("rationale exceeds {} bytes", self.config.max_note_bytes);
                }
                Ok(())
            }
            OverlayPayload::AdvocateNote { note, .. } => {
                if note.trim().is_empty() {
                    bail!("advocate note must be non-empty");
                }
                if note.len() > self.config.max_note_bytes {
                    bail!("advocate note exceeds {} bytes", self.config.max_note_bytes);
                }
                Ok(())
            }
            OverlayPayload::Approval {
                target_event_id,
                note,
                ..
            } => {
                if !looks_like_event_id(target_event_id) {
                    bail!("approval target {target_event_id:?} is not a well-formed event id");
                }
                if note.len() > self.config.max_note_bytes {
                    bail!("approval note exceeds {} bytes", self.config.max_note_bytes);
                }
                Ok(())
            }
        }
    }

    /// Full validation for a *new* append: bounds plus cross-event state.
    fn validate_payload(&self, payload: &OverlayPayload) -> Result<()> {
        self.validate_bounds(payload)?;
        match payload {
            OverlayPayload::ProposeMemory { .. } | OverlayPayload::AdvocateNote { .. } => Ok(()),
            OverlayPayload::Reinforce { target, .. } => {
                self.require_live_target(target, "reinforce")
            }
            OverlayPayload::Lifecycle { target, from, .. } => {
                self.require_live_target(target, "transition")?;
                let key = target.key();
                if self.pending_lifecycle_exists(&key) {
                    bail!(
                        "a lifecycle transition for {key} is already awaiting approval; \
                         decide it before recording another"
                    );
                }
                if let Some(current) = self.effective_lifecycle(&key) {
                    if current != *from {
                        bail!(
                            "lifecycle from-state {from:?} does not match overlay history \
                             {current:?} for {key}"
                        );
                    }
                }
                Ok(())
            }
            OverlayPayload::GraphEdge {
                from,
                to,
                relation,
                action,
            } => {
                self.require_live_target(from, "connect")?;
                self.require_live_target(to, "connect")?;
                let edge = (from.key(), to.key(), relation.clone());
                match action {
                    EdgeAction::Add => {
                        if self.edge_state(&edge) == Some(EdgeAction::Add) {
                            bail!("edge already added by the overlay");
                        }
                    }
                    EdgeAction::Remove => {
                        if self.edge_state(&edge) == Some(EdgeAction::Remove) {
                            bail!("edge already removed by the overlay");
                        }
                        if self.pending_edge_exists(&edge) {
                            bail!("an edge event for this edge is already awaiting approval");
                        }
                    }
                }
                Ok(())
            }
            OverlayPayload::DreamCandidate { members, .. } => {
                for member in members {
                    self.require_live_target(member, "consolidate")?;
                }
                Ok(())
            }
            OverlayPayload::Approval {
                target_event_id, ..
            } => {
                let Some(target) = self.get(target_event_id) else {
                    bail!("approval target {target_event_id} does not exist in this overlay");
                };
                if matches!(target.payload, OverlayPayload::Approval { .. }) {
                    bail!("cannot approve an Approval event");
                }
                if !target.requires_approval {
                    bail!(
                        "event {target_event_id} ({}) does not require approval",
                        target.payload.kind_name()
                    );
                }
                if self.decision_for(target_event_id).is_some() {
                    bail!("event {target_event_id} was already decided; decisions are final");
                }
                Ok(())
            }
        }
    }

    /// A target must be addressable and not tombstoned. Overlay refs must
    /// point at an *effective* `ProposeMemory` event; base refs are asserted
    /// by the caller (the overlay cannot see the base, by design).
    fn require_live_target(&self, target: &MemoryRef, verb: &str) -> Result<()> {
        if let MemoryRef::Overlay(id) = target {
            let Some(event) = self.get(id) else {
                bail!("cannot {verb} {}: no such overlay event", target.key());
            };
            if !matches!(event.payload, OverlayPayload::ProposeMemory { .. }) {
                bail!(
                    "cannot {verb} {}: event is a {}, not a proposed memory",
                    target.key(),
                    event.payload.kind_name()
                );
            }
            if !self.is_effective(event) {
                bail!(
                    "cannot {verb} {}: the proposal is not effective (pending or rejected)",
                    target.key()
                );
            }
        }
        let key = target.key();
        if self.effective_lifecycle(&key) == Some(LifecycleState::Pruned) {
            bail!("cannot {verb} {key}: it is tombstoned (pruned)");
        }
        Ok(())
    }

    fn effective_lifecycle(&self, key: &str) -> Option<LifecycleState> {
        let mut current = None;
        for event in &self.events {
            if let OverlayPayload::Lifecycle { target, to, .. } = &event.payload {
                if target.key() == key && self.is_effective(event) {
                    current = Some(*to);
                }
            }
        }
        current
    }

    fn pending_lifecycle_exists(&self, key: &str) -> bool {
        self.events.iter().any(|event| {
            matches!(&event.payload, OverlayPayload::Lifecycle { target, .. } if target.key() == key)
                && event.requires_approval
                && self.decision_for(&event.event_id).is_none()
        })
    }

    fn pending_edge_exists(&self, edge: &(String, String, String)) -> bool {
        self.events.iter().any(|event| {
            matches!(
                &event.payload,
                OverlayPayload::GraphEdge { from, to, relation, .. }
                    if &(from.key(), to.key(), relation.clone()) == edge
            ) && event.requires_approval
                && self.decision_for(&event.event_id).is_none()
        })
    }

    /// Last effective action recorded for an edge, if any.
    fn edge_state(&self, edge: &(String, String, String)) -> Option<EdgeAction> {
        let mut state = None;
        for event in &self.events {
            if let OverlayPayload::GraphEdge {
                from,
                to,
                relation,
                action,
            } = &event.payload
            {
                if &(from.key(), to.key(), relation.clone()) == edge && self.is_effective(event) {
                    state = Some(*action);
                }
            }
        }
        state
    }
}

/// Identifier fields (base ids, channels, relations, tags): printable,
/// compact, and safe inside durable keys.
fn validate_ident(value: &str, what: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{what} must be non-empty");
    }
    if value.len() > MAX_IDENT_BYTES {
        bail!("{what} exceeds {MAX_IDENT_BYTES} bytes");
    }
    if value != value.trim() {
        bail!("{what} must not have surrounding whitespace");
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    {
        bail!("{what} must contain only [A-Za-z0-9._:-]");
    }
    Ok(())
}

fn looks_like_event_id(id: &str) -> bool {
    id.strip_prefix("ov1-")
        .is_some_and(|hex| hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Deterministic event id: two FNV-1a streams over the canonical encoding
/// of (schema, seq, parent, recorded_at, payload JSON). Serde struct/enum
/// field order is fixed at compile time, so the encoding is canonical.
fn derive_event_id(
    schema_version: u32,
    seq: u64,
    parent_id: &str,
    recorded_at: u64,
    payload: &OverlayPayload,
) -> Result<String> {
    let payload_json = serde_json::to_string(payload)?;
    let mut material = Vec::new();
    material.extend_from_slice(&schema_version.to_be_bytes());
    material.extend_from_slice(&seq.to_be_bytes());
    material.extend_from_slice(parent_id.as_bytes());
    material.push(0);
    material.extend_from_slice(&recorded_at.to_be_bytes());
    material.extend_from_slice(payload_json.as_bytes());
    let high = fnv1a64(&material, 0xcbf2_9ce4_8422_2325);
    let low = fnv1a64(&material, 0x8422_2325_cbf2_9ce4);
    Ok(format!("ov1-{high:016x}{low:016x}"))
}

fn fnv1a64(bytes: &[u8], basis: u64) -> u64 {
    let mut hash = basis;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Temp file beside `path` so `rename` stays atomic on one filesystem.
fn sibling_temp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn propose(text: &str) -> OverlayPayload {
        OverlayPayload::ProposeMemory {
            text: text.into(),
            channel: "thinking".into(),
            importance: 0.5,
            keystone_proposed: false,
            tags: vec!["design".into()],
        }
    }

    fn base(id: &str) -> MemoryRef {
        MemoryRef::Base(id.into())
    }

    #[test]
    fn config_defaults_validate_and_bad_bounds_reject() {
        OverlayConfig::default().validate().unwrap();
        let mut config = OverlayConfig::default();
        config.max_events = 0;
        assert!(config.validate().is_err());
        let mut config = OverlayConfig::default();
        config.max_dream_members = 1;
        assert!(config.validate().is_err());
        let mut config = OverlayConfig::default();
        config.max_text_bytes = 2 * 1024 * 1024;
        assert!(config.validate().is_err());
    }

    #[test]
    fn event_ids_are_deterministic_and_chain_links() {
        let mut a = OverlayLog::with_defaults().unwrap();
        let mut b = OverlayLog::with_defaults().unwrap();
        let id_a = a.append(propose("one"), 100).unwrap().event_id.clone();
        let id_b = b.append(propose("one"), 100).unwrap().event_id.clone();
        assert_eq!(id_a, id_b);
        assert!(looks_like_event_id(&id_a));
        // Different position/parent ⇒ different id for the same payload.
        let id_a2 = a.append(propose("one"), 100).unwrap().event_id.clone();
        assert_ne!(id_a, id_a2);
        assert_eq!(a.events()[0].parent_id, GENESIS_PARENT);
        assert_eq!(a.events()[1].parent_id, id_a);
        a.verify().unwrap();
    }

    #[test]
    fn plain_proposal_is_effective_keystone_needs_approval() {
        let mut log = OverlayLog::with_defaults().unwrap();
        let plain = log.append(propose("plain"), 1).unwrap().event_id.clone();
        let keystone = log
            .append(
                OverlayPayload::ProposeMemory {
                    text: "keystone-worthy".into(),
                    channel: "thinking".into(),
                    importance: 0.9,
                    keystone_proposed: true,
                    tags: vec![],
                },
                2,
            )
            .unwrap()
            .event_id
            .clone();
        assert_eq!(
            log.approval_status(&plain),
            Some(ApprovalStatus::NotRequired)
        );
        assert_eq!(
            log.approval_status(&keystone),
            Some(ApprovalStatus::Pending)
        );
        let p = log.projection();
        assert!(p.proposed_memories.contains_key(&plain));
        assert!(!p.proposed_memories.contains_key(&keystone));
        assert_eq!(p.pending_approvals.len(), 1);
        // Approve: the keystone proposal becomes effective.
        log.append(
            OverlayPayload::Approval {
                target_event_id: keystone.clone(),
                decision: ApprovalDecision::Approved,
                note: "operator reviewed".into(),
            },
            3,
        )
        .unwrap();
        let p = log.projection();
        assert!(p.proposed_memories.contains_key(&keystone));
        assert!(p.proposed_memories[&keystone].keystone);
        assert!(p.pending_approvals.is_empty());
    }

    #[test]
    fn proposal_bounds_are_enforced() {
        let mut log = OverlayLog::with_defaults().unwrap();
        let long = "x".repeat(DEFAULT_MAX_TEXT_BYTES + 1);
        assert!(log.append(propose(&long), 1).is_err());
        assert!(log.append(propose("   "), 1).is_err());
        assert!(log
            .append(
                OverlayPayload::ProposeMemory {
                    text: "ok".into(),
                    channel: "bad channel!".into(),
                    importance: 0.5,
                    keystone_proposed: false,
                    tags: vec![],
                },
                1,
            )
            .is_err());
        assert!(log
            .append(
                OverlayPayload::ProposeMemory {
                    text: "ok".into(),
                    channel: "thinking".into(),
                    importance: f64::NAN,
                    keystone_proposed: false,
                    tags: vec![],
                },
                1,
            )
            .is_err());
        assert!(log
            .append(
                OverlayPayload::ProposeMemory {
                    text: "ok".into(),
                    channel: "thinking".into(),
                    importance: 0.5,
                    keystone_proposed: false,
                    tags: vec!["dup".into(), "dup".into()],
                },
                1,
            )
            .is_err());
    }

    #[test]
    fn reinforcement_targets_must_be_live() {
        let mut log = OverlayLog::with_defaults().unwrap();
        // Base refs are caller-asserted (the overlay cannot see the base).
        log.append(
            OverlayPayload::Reinforce {
                target: base("mem-42"),
                kind: ReinforcementKind::Recall,
            },
            1,
        )
        .unwrap();
        // Unknown overlay ref refused.
        assert!(log
            .append(
                OverlayPayload::Reinforce {
                    target: MemoryRef::Overlay(format!("ov1-{}", "0".repeat(32))),
                    kind: ReinforcementKind::Recall,
                },
                2,
            )
            .is_err());
        // Pending (keystone) proposal is not yet reinforceable.
        let keystone = log
            .append(
                OverlayPayload::ProposeMemory {
                    text: "gated".into(),
                    channel: "thinking".into(),
                    importance: 0.9,
                    keystone_proposed: true,
                    tags: vec![],
                },
                3,
            )
            .unwrap()
            .event_id
            .clone();
        assert!(log
            .append(
                OverlayPayload::Reinforce {
                    target: MemoryRef::Overlay(keystone.clone()),
                    kind: ReinforcementKind::HaloTouch,
                },
                4,
            )
            .is_err());
        let p = log.projection();
        assert_eq!(p.reinforcements["base:mem-42"], (1, 0, 0));
    }

    #[test]
    fn lifecycle_state_machine_and_overlay_consistency() {
        let mut log = OverlayLog::with_defaults().unwrap();
        // Illegal jump refused.
        assert!(log
            .append(
                OverlayPayload::Lifecycle {
                    target: base("m1"),
                    from: LifecycleState::Active,
                    to: LifecycleState::Archived,
                },
                1,
            )
            .is_err());
        // Legal forward chain, no approval needed.
        log.append(
            OverlayPayload::Lifecycle {
                target: base("m1"),
                from: LifecycleState::Active,
                to: LifecycleState::Forgiven,
            },
            2,
        )
        .unwrap();
        // From-state must now match overlay history.
        assert!(log
            .append(
                OverlayPayload::Lifecycle {
                    target: base("m1"),
                    from: LifecycleState::Active,
                    to: LifecycleState::Forgiven,
                },
                3,
            )
            .is_err());
        log.append(
            OverlayPayload::Lifecycle {
                target: base("m1"),
                from: LifecycleState::Forgiven,
                to: LifecycleState::Archived,
            },
            4,
        )
        .unwrap();
        assert_eq!(
            log.projection().lifecycle_overrides["base:m1"],
            LifecycleState::Archived
        );
    }

    #[test]
    fn prune_is_gated_and_terminal() {
        let mut log = OverlayLog::with_defaults().unwrap();
        let prune = log
            .append(
                OverlayPayload::Lifecycle {
                    target: base("m2"),
                    from: LifecycleState::Archived,
                    to: LifecycleState::Pruned,
                },
                1,
            )
            .unwrap()
            .event_id
            .clone();
        assert_eq!(log.approval_status(&prune), Some(ApprovalStatus::Pending));
        assert!(log.projection().tombstones.is_empty());
        // Second transition for the same ref while one is pending: refused.
        assert!(log
            .append(
                OverlayPayload::Lifecycle {
                    target: base("m2"),
                    from: LifecycleState::Archived,
                    to: LifecycleState::Active,
                },
                2,
            )
            .is_err());
        log.append(
            OverlayPayload::Approval {
                target_event_id: prune.clone(),
                decision: ApprovalDecision::Approved,
                note: String::new(),
            },
            3,
        )
        .unwrap();
        let p = log.projection();
        assert!(p.tombstones.contains("base:m2"));
        // Tombstoned refs are terminal: nothing further may touch them.
        assert!(log
            .append(
                OverlayPayload::Reinforce {
                    target: base("m2"),
                    kind: ReinforcementKind::Recall,
                },
                4,
            )
            .is_err());
        assert!(log
            .append(
                OverlayPayload::Lifecycle {
                    target: base("m2"),
                    from: LifecycleState::Pruned,
                    to: LifecycleState::Active,
                },
                5,
            )
            .is_err());
    }

    #[test]
    fn archive_revival_is_promotion_and_gated() {
        let mut log = OverlayLog::with_defaults().unwrap();
        let revive = log
            .append(
                OverlayPayload::Lifecycle {
                    target: base("m3"),
                    from: LifecycleState::Archived,
                    to: LifecycleState::Active,
                },
                1,
            )
            .unwrap()
            .event_id
            .clone();
        assert_eq!(log.approval_status(&revive), Some(ApprovalStatus::Pending));
        // Forgiven→Active restore is NOT gated (recall-driven recovery).
        let restore = log
            .append(
                OverlayPayload::Lifecycle {
                    target: base("m4"),
                    from: LifecycleState::Forgiven,
                    to: LifecycleState::Active,
                },
                2,
            )
            .unwrap()
            .event_id
            .clone();
        assert_eq!(
            log.approval_status(&restore),
            Some(ApprovalStatus::NotRequired)
        );
    }

    #[test]
    fn edge_add_is_free_edge_remove_is_gated() {
        let mut log = OverlayLog::with_defaults().unwrap();
        let add = log
            .append(
                OverlayPayload::GraphEdge {
                    from: base("m1"),
                    to: base("m2"),
                    relation: "context".into(),
                    action: EdgeAction::Add,
                },
                1,
            )
            .unwrap()
            .event_id
            .clone();
        assert_eq!(log.approval_status(&add), Some(ApprovalStatus::NotRequired));
        // Duplicate add refused.
        assert!(log
            .append(
                OverlayPayload::GraphEdge {
                    from: base("m1"),
                    to: base("m2"),
                    relation: "context".into(),
                    action: EdgeAction::Add,
                },
                2,
            )
            .is_err());
        // Self-edge refused.
        assert!(log
            .append(
                OverlayPayload::GraphEdge {
                    from: base("m1"),
                    to: base("m1"),
                    relation: "context".into(),
                    action: EdgeAction::Add,
                },
                3,
            )
            .is_err());
        let remove = log
            .append(
                OverlayPayload::GraphEdge {
                    from: base("m1"),
                    to: base("m2"),
                    relation: "context".into(),
                    action: EdgeAction::Remove,
                },
                4,
            )
            .unwrap()
            .event_id
            .clone();
        assert_eq!(log.approval_status(&remove), Some(ApprovalStatus::Pending));
        // Pending removal blocks a duplicate removal request.
        assert!(log
            .append(
                OverlayPayload::GraphEdge {
                    from: base("m1"),
                    to: base("m2"),
                    relation: "context".into(),
                    action: EdgeAction::Remove,
                },
                5,
            )
            .is_err());
        log.append(
            OverlayPayload::Approval {
                target_event_id: remove,
                decision: ApprovalDecision::Approved,
                note: String::new(),
            },
            6,
        )
        .unwrap();
        let p = log.projection();
        assert!(p.edge_adds.is_empty());
        assert!(p.edge_removes.contains(&(
            "base:m1".to_string(),
            "base:m2".to_string(),
            "context".to_string()
        )));
    }

    #[test]
    fn dream_candidates_validate_and_are_gated() {
        let mut log = OverlayLog::with_defaults().unwrap();
        // Too few members.
        assert!(log
            .append(
                OverlayPayload::DreamCandidate {
                    members: vec![base("m1")],
                    similarity: 0.9,
                    rationale: "solo".into(),
                },
                1,
            )
            .is_err());
        // Below the spec's consolidation floor.
        assert!(log
            .append(
                OverlayPayload::DreamCandidate {
                    members: vec![base("m1"), base("m2")],
                    similarity: 0.5,
                    rationale: "weak".into(),
                },
                2,
            )
            .is_err());
        // NaN refused.
        assert!(log
            .append(
                OverlayPayload::DreamCandidate {
                    members: vec![base("m1"), base("m2")],
                    similarity: f64::NAN,
                    rationale: "nan".into(),
                },
                3,
            )
            .is_err());
        let candidate = log
            .append(
                OverlayPayload::DreamCandidate {
                    members: vec![base("m1"), base("m2")],
                    similarity: 0.91,
                    rationale: "cosine 0.91 on shared design terms".into(),
                },
                4,
            )
            .unwrap()
            .event_id
            .clone();
        assert_eq!(
            log.approval_status(&candidate),
            Some(ApprovalStatus::Pending)
        );
        assert!(log.projection().approved_dream_candidates.is_empty());
        log.append(
            OverlayPayload::Approval {
                target_event_id: candidate.clone(),
                decision: ApprovalDecision::Approved,
                note: "merge reviewed".into(),
            },
            5,
        )
        .unwrap();
        assert_eq!(log.projection().approved_dream_candidates, vec![candidate]);
    }

    #[test]
    fn advocate_notes_are_quarantined_and_bounded() {
        let mut log = OverlayLog::with_defaults().unwrap();
        let note = log
            .append(
                OverlayPayload::AdvocateNote {
                    verdict: AdvocateVerdict::Tension,
                    note: "action drifted from stated values".into(),
                },
                1,
            )
            .unwrap()
            .event_id
            .clone();
        assert_eq!(
            log.approval_status(&note),
            Some(ApprovalStatus::NotRequired)
        );
        let p = log.projection();
        // Side-channel only: never a proposed memory.
        assert_eq!(p.advocate_note_ids, vec![note]);
        assert!(p.proposed_memories.is_empty());
        let long = "y".repeat(DEFAULT_MAX_NOTE_BYTES + 1);
        assert!(log
            .append(
                OverlayPayload::AdvocateNote {
                    verdict: AdvocateVerdict::Aligned,
                    note: long,
                },
                2,
            )
            .is_err());
    }

    #[test]
    fn approval_rules_are_strict_and_final() {
        let mut log = OverlayLog::with_defaults().unwrap();
        let plain = log.append(propose("plain"), 1).unwrap().event_id.clone();
        // Approving a non-gated event is refused.
        assert!(log
            .append(
                OverlayPayload::Approval {
                    target_event_id: plain,
                    decision: ApprovalDecision::Approved,
                    note: String::new(),
                },
                2,
            )
            .is_err());
        // Unknown target refused.
        assert!(log
            .append(
                OverlayPayload::Approval {
                    target_event_id: format!("ov1-{}", "a".repeat(32)),
                    decision: ApprovalDecision::Approved,
                    note: String::new(),
                },
                3,
            )
            .is_err());
        let prune = log
            .append(
                OverlayPayload::Lifecycle {
                    target: base("m9"),
                    from: LifecycleState::Archived,
                    to: LifecycleState::Pruned,
                },
                4,
            )
            .unwrap()
            .event_id
            .clone();
        log.append(
            OverlayPayload::Approval {
                target_event_id: prune.clone(),
                decision: ApprovalDecision::Rejected,
                note: "keep it".into(),
            },
            5,
        )
        .unwrap();
        assert_eq!(log.approval_status(&prune), Some(ApprovalStatus::Rejected));
        // Decisions are final — no second decision, no flip.
        assert!(log
            .append(
                OverlayPayload::Approval {
                    target_event_id: prune.clone(),
                    decision: ApprovalDecision::Approved,
                    note: String::new(),
                },
                6,
            )
            .is_err());
        // A rejected prune never tombstones.
        assert!(log.projection().tombstones.is_empty());
    }

    #[test]
    fn capacity_is_a_hard_ceiling() {
        let mut config = OverlayConfig::default();
        config.max_events = 2;
        let mut log = OverlayLog::new(config).unwrap();
        log.append(propose("one"), 1).unwrap();
        log.append(propose("two"), 2).unwrap();
        let err = log.append(propose("three"), 3).unwrap_err();
        assert!(err.to_string().contains("promotion"));
        assert_eq!(log.events().len(), 2);
    }

    #[test]
    fn save_load_roundtrip_and_tamper_detection() {
        let dir = std::env::temp_dir().join(format!(
            "memory-overlay-{}",
            derive_event_id(1, 1, "seed", 0, &propose("tmpdir")).unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlay.json");

        let mut log = OverlayLog::with_defaults().unwrap();
        log.append(propose("durable"), 10).unwrap();
        log.append(
            OverlayPayload::Reinforce {
                target: base("m1"),
                kind: ReinforcementKind::Recall,
            },
            11,
        )
        .unwrap();
        log.save(&path).unwrap();

        let loaded = OverlayLog::load(&path).unwrap();
        assert_eq!(loaded, log);
        assert!(!path.with_extension("json.tmp").exists());

        // Tamper with recorded content: load must refuse.
        let text = fs::read_to_string(&path).unwrap();
        let tampered = text.replace("durable", "rewritten history");
        fs::write(&path, tampered).unwrap();
        let err = OverlayLog::load(&path).unwrap_err();
        assert!(format!("{err:#}").contains("verification"));

        // Truncating the chain tail also fails (parent linkage breaks when
        // an interior event is removed; removing the last event alone is
        // detected only by comparing against external expectations, so we
        // drop an interior one).
        let mut document: serde_json::Value = serde_json::from_str(&text).unwrap();
        let events = document["events"].as_array_mut().unwrap();
        events.remove(0);
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(OverlayLog::load(&path).is_err());

        // Wrong schema version refused with a migration message.
        let mut document: serde_json::Value = serde_json::from_str(&text).unwrap();
        document["schema_version"] = serde_json::json!(999);
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let err = OverlayLog::load(&path).unwrap_err();
        assert!(format!("{err:#}").contains("migration"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn approval_flag_in_file_is_rederived_not_trusted() {
        let dir = std::env::temp_dir().join(format!(
            "memory-overlay-{}",
            derive_event_id(1, 2, "seed", 0, &propose("tmpdir2")).unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlay.json");
        let mut log = OverlayLog::with_defaults().unwrap();
        log.append(
            OverlayPayload::Lifecycle {
                target: base("m1"),
                from: LifecycleState::Archived,
                to: LifecycleState::Pruned,
            },
            1,
        )
        .unwrap();
        log.save(&path).unwrap();
        // Flip the gate off in the file: verification refuses the document.
        let text = fs::read_to_string(&path).unwrap();
        let flipped = text.replace(
            "\"requires_approval\": true",
            "\"requires_approval\": false",
        );
        assert_ne!(text, flipped);
        fs::write(&path, flipped).unwrap();
        assert!(OverlayLog::load(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn serde_projection_roundtrip() {
        let mut log = OverlayLog::with_defaults().unwrap();
        log.append(propose("p"), 1).unwrap();
        let projection = log.projection();
        let json = serde_json::to_string(&projection).unwrap();
        assert_eq!(projection, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn ident_validation_rules() {
        assert!(validate_ident("mem-42", "id").is_ok());
        assert!(validate_ident("a.b:c_d-e", "id").is_ok());
        assert!(validate_ident("", "id").is_err());
        assert!(validate_ident(" padded ", "id").is_err());
        assert!(validate_ident("has space", "id").is_err());
        assert!(validate_ident(&"x".repeat(MAX_IDENT_BYTES + 1), "id").is_err());
        assert!(validate_ident("emoji🐉", "id").is_err());
    }

    #[test]
    fn refuses_base_artifact_write_paths() {
        for name in BASE_ARTIFACT_NAMES {
            let p = PathBuf::from(format!("/data/steve-memory/readonly/{name}"));
            let err = assert_safe_overlay_write_path(&p).unwrap_err().to_string();
            assert!(
                err.contains("immutable") || err.contains("base artifact"),
                "{err}"
            );
        }
        assert!(assert_safe_overlay_write_path(Path::new("/data/overlay/events.json")).is_ok());
        assert!(assert_safe_overlay_write_path(Path::new("")).is_err());
    }

    #[test]
    fn overlay_paths_reject_base_collision() {
        let base = PathBuf::from("/data/steve-memory/readonly");
        let bad = base.join("wal.log");
        assert!(OverlayPaths::new(&base, &bad).is_err());
        let ok =
            OverlayPaths::new(&base, PathBuf::from("/data/steve-memory/overlay/log.json")).unwrap();
        assert_eq!(ok.base_root(), base.as_path());
        assert!(ok.overlay_file().ends_with("log.json"));
    }

    #[test]
    fn append_requires_non_decreasing_timestamps() {
        let mut log = OverlayLog::with_defaults().unwrap();
        log.append(propose("first"), 100).unwrap();
        let err = log.append(propose("second"), 50).unwrap_err().to_string();
        assert!(err.contains("non-decreasing") || err.contains("earlier"));
        log.append(propose("second"), 100).unwrap(); // equal is ok
        log.append(propose("third"), 101).unwrap();
    }

    #[test]
    fn save_refuses_base_artifact_and_verify_before_write() {
        let mut log = OverlayLog::with_defaults().unwrap();
        log.append(propose("x"), 1).unwrap();
        assert!(log.save(Path::new("/tmp/wal.log")).is_err());
        // Corrupt in-memory seq and ensure save refuses.
        log.events[0].seq = 99;
        let dir = std::env::temp_dir().join(format!(
            "memory-overlay-bad-{}",
            derive_event_id(1, 9, "seed", 0, &propose("t")).unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlay.json");
        assert!(log.save(&path).is_err());
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_file_load_is_corruption() {
        let dir = std::env::temp_dir().join(format!(
            "memory-overlay-empty-{}",
            derive_event_id(1, 8, "seed", 0, &propose("t")).unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlay.json");
        fs::write(&path, b"").unwrap();
        let err = OverlayLog::load(&path).unwrap_err().to_string();
        assert!(err.contains("empty") || err.contains("corrupt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reordered_recorded_at_fails_verify() {
        let mut log = OverlayLog::with_defaults().unwrap();
        log.append(propose("a"), 10).unwrap();
        log.append(propose("b"), 20).unwrap();
        // Break temporal order while keeping hash (recompute id for tamper).
        log.events[1].recorded_at = 5;
        log.events[1].event_id = derive_event_id(
            OVERLAY_SCHEMA_VERSION,
            2,
            &log.events[0].event_id,
            5,
            &log.events[1].payload,
        )
        .unwrap();
        let err = log.verify().unwrap_err().to_string();
        assert!(err.contains("non-decreasing") || err.contains("before"));
    }

    #[test]
    fn duplicate_event_id_fails_verify() {
        let mut log = OverlayLog::with_defaults().unwrap();
        log.append(propose("a"), 1).unwrap();
        log.append(propose("b"), 2).unwrap();
        log.events[1].event_id = log.events[0].event_id.clone();
        assert!(log.verify().is_err());
    }

    #[test]
    fn cannot_approve_approval_event() {
        let mut log = OverlayLog::with_defaults().unwrap();
        let prune = log
            .append(
                OverlayPayload::Lifecycle {
                    target: base("mx"),
                    from: LifecycleState::Archived,
                    to: LifecycleState::Pruned,
                },
                1,
            )
            .unwrap()
            .event_id
            .clone();
        let approval = log
            .append(
                OverlayPayload::Approval {
                    target_event_id: prune,
                    decision: ApprovalDecision::Approved,
                    note: String::new(),
                },
                2,
            )
            .unwrap()
            .event_id
            .clone();
        assert!(log
            .append(
                OverlayPayload::Approval {
                    target_event_id: approval,
                    decision: ApprovalDecision::Approved,
                    note: String::new(),
                },
                3,
            )
            .is_err());
    }

    #[test]
    fn overlay_paths_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "memory-overlay-paths-{}",
            derive_event_id(1, 7, "seed", 0, &propose("t")).unwrap()
        ));
        let base = dir.join("readonly");
        let overlay = dir.join("overlay").join("events.json");
        fs::create_dir_all(&base).unwrap();
        // Place a decoy base artifact — overlay must not touch it.
        fs::write(base.join("wal.log"), b"do-not-touch").unwrap();
        let paths = OverlayPaths::new(&base, &overlay).unwrap();
        let mut log = OverlayLog::with_defaults().unwrap();
        log.append(propose("via-paths"), 42).unwrap();
        paths.save(&log).unwrap();
        let loaded = paths.load().unwrap();
        assert_eq!(loaded.events().len(), 1);
        assert_eq!(fs::read(base.join("wal.log")).unwrap(), b"do-not-touch");
        let _ = fs::remove_dir_all(&dir);
    }
}
