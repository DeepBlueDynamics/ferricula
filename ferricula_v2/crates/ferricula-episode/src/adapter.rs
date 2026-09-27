use std::path::{Path, PathBuf};

use anyhow::{Context, bail, Result};
use crate::overlay::{OverlayConfig, OverlayEvent, OverlayLog, OverlayPayload};

use crate::causal::{evidence_has_authorizing_link, observation_can_change_hypothesis_status};
use crate::model::*;
use crate::projection::EpisodeProjection;

/// Canonical default filename for the dedicated episode overlay log.
/// Prevents competing writes with the runtime overlay (`memory-overlay.json`).
pub const DEFAULT_EPISODES_FILENAME: &str = "episodes.json";

/// Transactional adapter managing the persistent `OverlayLog` with strict validation.
///
/// Implements the 4-stage pipeline:
/// `Clone -> Validate -> Pre-Fold -> Save -> Publish`
#[derive(Debug)]
pub struct EpisodeAdapter {
    path: PathBuf,
    log: OverlayLog,
    projection: EpisodeProjection,
}

impl EpisodeAdapter {
    /// Open the episode adapter in the specified writable state directory using `episodes.json`.
    pub fn open_in_state_dir(state_dir: impl AsRef<Path>) -> Result<Self> {
        let path = state_dir.as_ref().join(DEFAULT_EPISODES_FILENAME);
        Self::open(path)
    }

    /// Open an existing overlay log or create a new empty log at `path`.
    ///
    /// Fail-closed: returns Err if hash verification fails or if any
    /// episode_v1 event contains malformed data.
    ///
    /// Enforces isolation: refuses to open `memory-overlay.json` (owned by runtime mutex)
    /// or base recovery volume artifacts.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        let log = if path.exists() {
            OverlayLog::load(&path).context("failed to load overlay log")?
        } else {
            OverlayLog::new(OverlayConfig::default()).context("failed to initialize overlay log")?
        };

        log.verify().context("overlay log integrity verification failed")?;
        let projection = EpisodeProjection::fold_from_events(log.events())?;
        Ok(Self {
            path,
            log,
            projection,
        })
    }

    /// Path to persisted overlay file on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Access the queryable projection (read-only).
    pub fn projection(&self) -> &EpisodeProjection {
        &self.projection
    }

    /// Access the underlying overlay log (read-only).
    pub fn log(&self) -> &OverlayLog {
        &self.log
    }

    /// Access raw overlay events.
    pub fn events(&self) -> &[OverlayEvent] {
        self.log.events()
    }

    /// Commit an observation report. Returns the deterministic overlay event ID.
    pub fn commit_observation(&mut self, report: ObservationReport, now: u64) -> Result<String> {
        self.commit(EpisodeItem::Observation(report), now)
    }

    /// Commit a hypothesis proposal. Returns the deterministic overlay event ID.
    pub fn commit_hypothesis(&mut self, proposal: HypothesisProposal, now: u64) -> Result<String> {
        self.commit(EpisodeItem::Hypothesis(proposal), now)
    }

    /// Commit a status transition. Returns the deterministic overlay event ID.
    pub fn commit_transition(&mut self, transition: StatusTransitionEvent, now: u64) -> Result<String> {
        self.commit(EpisodeItem::StatusTransition(transition), now)
    }

    /// Commit a goal report. Returns the deterministic overlay event ID.
    pub fn commit_goal(&mut self, goal: GoalReport, now: u64) -> Result<String> {
        self.commit(EpisodeItem::Goal(goal), now)
    }

    /// Commit an explicit link. Returns the deterministic overlay event ID.
    pub fn commit_link(&mut self, link: ExplicitLink, now: u64) -> Result<String> {
        self.commit(EpisodeItem::Link(link), now)
    }

    /// Commit a new structured episode item to the log via the 4-stage pipeline:
    /// `Clone -> Validate -> Save -> Publish`.
    ///
    /// Returns the deterministic, full overlay event ID on success.
    pub fn commit(&mut self, item: EpisodeItem, now: u64) -> Result<String> {
        // Stage 1: Clone state (both log and in-memory projection)
        let mut cloned_log = self.log.clone();
        let mut cloned_projection = self.projection.clone();

        // Stage 2: Validate preconditions and invariants
        self.validate_item(&item, now)?;

        // Prepare payload as an un-gated ProposeMemory on channel `episode_v1`
        let text = serde_json::to_string(&item).context("failed to serialize EpisodeItem")?;
        let payload = OverlayPayload::ProposeMemory {
            text,
            channel: EPISODE_CHANNEL_V1.to_string(),
            importance: 0.5,
            keystone_proposed: false,
            tags: item.summary_tags(),
        };

        // Append to cloned log (checks hash chaining and sequence)
        let appended_event = cloned_log
            .append(payload, now)
            .context("failed to append event to overlay log")?;
        let event_id = appended_event.event_id.clone();

        // Verify cloned log invariant
        cloned_log
            .verify()
            .context("cloned log failed tamper-evident verification")?;

        // Pre-validate & pre-apply event to cloned projection BEFORE writing to disk.
        // If fold validation rejects (e.g. invalid evidence or typed links), disk is untouched!
        let published_event = cloned_log
            .get(&event_id)
            .cloned()
            .expect("event must exist in cloned log");
        cloned_projection
            .apply_event(&published_event)
            .context("failed to apply event to cloned projection")?;

        // Stage 3: Persist atomically (tmp + rename via OverlayLog::save)
        // If save fails (e.g. disk full / read-only path), self.log and self.projection remain completely unchanged.
        cloned_log
            .save(&self.path)
            .context("failed to persist overlay log to disk")?;

        // Stage 4: Infallibly publish prepared log and projection to state
        self.log = cloned_log;
        self.projection = cloned_projection;

        Ok(event_id)
    }

    fn validate_item(&self, item: &EpisodeItem, now: u64) -> Result<()> {
        // Enforce non-decreasing commit timestamps
        if let Some(last) = self.log.events().last() {
            if now < last.recorded_at {
                bail!(
                    "clock skew: commit timestamp {now} < last event timestamp {}",
                    last.recorded_at
                );
            }
        }

        match item {
            EpisodeItem::Observation(obs) => {
                if obs.report_id.trim().is_empty() {
                    bail!("observation report_id must not be empty");
                }
                if self.projection.report_id_to_event.contains_key(&obs.report_id) {
                    bail!("duplicate report_id: observation {:?} already exists", obs.report_id);
                }
                if obs.content.trim().is_empty() {
                    bail!("observation content must not be empty");
                }
            }
            EpisodeItem::Goal(goal) => {
                if goal.goal_id.trim().is_empty() {
                    bail!("goal_id must not be empty");
                }
                if self.projection.goal_id_to_event.contains_key(&goal.goal_id) {
                    bail!("duplicate goal_id: goal {:?} already exists", goal.goal_id);
                }
                if goal.target_entity.trim().is_empty() {
                    bail!("goal target_entity must not be empty");
                }
            }
            EpisodeItem::Hypothesis(hyp) => {
                if hyp.hypothesis_id.trim().is_empty() {
                    bail!("hypothesis_id must not be empty");
                }
                if self.projection.hypothesis_id_to_event.contains_key(&hyp.hypothesis_id) {
                    bail!(
                        "duplicate hypothesis_id: hypothesis {:?} already exists",
                        hyp.hypothesis_id
                    );
                }

                // Invariant 1: Caller cannot self-assert Supported; must start Candidate
                if hyp.status != HypothesisStatus::Candidate {
                    bail!(
                        "invariant violation: hypothesis proposal must have status Candidate, found {:?}",
                        hyp.status
                    );
                }

                // Invariant 2: No self-reference
                if hyp.target_episode_id == hyp.hypothesis_id {
                    bail!("invariant violation: hypothesis cannot target itself");
                }

                // Invariant 3: Target episode must exist in the log as an attributed observation report
                if !self.projection.observations.contains_key(&hyp.target_episode_id) {
                    bail!(
                        "dangling reference: target_episode_id {:?} does not exist in observation registry",
                        hyp.target_episode_id
                    );
                }

                // Invariant 4: Validate support refs (must resolve to observation reports, not future, not self)
                for ref_id in &hyp.support_refs {
                    if ref_id == &hyp.hypothesis_id {
                        bail!("invariant violation: support ref cannot point to hypothesis itself");
                    }
                    let Some(event) = self.log.get(ref_id) else {
                        bail!("dangling reference: support_ref {ref_id:?} does not exist in log");
                    };
                    if event.recorded_at > now {
                        bail!("future reference: support_ref {ref_id:?} recorded in the future");
                    }
                    if !self.projection.observations.contains_key(ref_id) {
                        bail!(
                            "evidential typing violation: support_ref {ref_id:?} must resolve to an ObservationReport"
                        );
                    }
                }

                // Invariant 5: Validate against refs (must resolve to observation reports, not future, not self)
                for ref_id in &hyp.against_refs {
                    if ref_id == &hyp.hypothesis_id {
                        bail!("invariant violation: against ref cannot point to hypothesis itself");
                    }
                    let Some(event) = self.log.get(ref_id) else {
                        bail!("dangling reference: against_ref {ref_id:?} does not exist in log");
                    };
                    if event.recorded_at > now {
                        bail!("future reference: against_ref {ref_id:?} recorded in the future");
                    }
                    if !self.projection.observations.contains_key(ref_id) {
                        bail!(
                            "evidential typing violation: against_ref {ref_id:?} must resolve to an ObservationReport"
                        );
                    }
                }
            }
            EpisodeItem::StatusTransition(st) => {
                // Invariant 1: Target hypothesis must exist
                let Some(hyp_ev_id) = self
                    .projection
                    .hypothesis_id_to_event
                    .get(&st.hypothesis_id)
                    .cloned()
                else {
                    bail!("dangling reference: hypothesis {:?} not found", st.hypothesis_id);
                };
                let current_hyp = self.projection.hypotheses.get(&hyp_ev_id).expect("must exist");
                let target_episode = current_hyp.proposal.target_episode_id.clone();

                // Invariant 2: Evidence refs must be non-empty
                if st.evidence_refs.is_empty() {
                    bail!("status transition requires at least one corroborating evidence reference");
                }

                // Invariant 3: Validate all evidence refs (must resolve to observation reports, not self, not future).
                // A scoped miss, an inconclusive search, or the unexplained target alone cannot move status.
                let mut independent_evidence = false;
                for ref_id in &st.evidence_refs {
                    if ref_id == &st.hypothesis_id || ref_id == &hyp_ev_id {
                        bail!("invariant violation: evidence ref cannot point to hypothesis itself");
                    }
                    let Some(event) = self.log.get(ref_id) else {
                        bail!("dangling reference: evidence_ref {ref_id:?} does not exist in log");
                    };
                    if event.recorded_at > now {
                        bail!("future reference: evidence_ref {ref_id:?} recorded in the future");
                    }
                    let Some(obs) = self.projection.observations.get(ref_id) else {
                        bail!(
                            "evidential typing violation: evidence_ref {ref_id:?} must resolve to an ObservationReport"
                        );
                    };
                    if ref_id != &target_episode
                        && ref_id != &hyp_ev_id
                        && observation_can_change_hypothesis_status(&obs.report)
                        && evidence_has_authorizing_link(
                            &self.projection.links,
                            ref_id,
                            &hyp_ev_id,
                            &target_episode,
                        )
                    {
                        independent_evidence = true;
                    }
                }
                if !independent_evidence {
                    bail!(
                        "hypothesis status requires an authorizing evidence link to an independent observation; a scoped miss, an inconclusive search, the unexplained report, or an unlinked observation is not evidence"
                    );
                }

                // Invariant 4: Transition validity
                match st.transition {
                    HypothesisTransition::Support => {
                        if current_hyp.current_status != HypothesisStatus::Candidate {
                            bail!(
                                "invalid transition: Support transition only valid from Candidate, current is {:?}",
                                current_hyp.current_status
                            );
                        }
                    }
                    HypothesisTransition::Disconfirm => {
                        if current_hyp.current_status == HypothesisStatus::Disconfirmed {
                            bail!("invalid transition: hypothesis is already Disconfirmed");
                        }
                    }
                    HypothesisTransition::Supersede { ref superseding_id } => {
                        if superseding_id == &st.hypothesis_id {
                            bail!("invalid transition: hypothesis cannot supersede itself");
                        }
                        if !self.projection.hypothesis_id_to_event.contains_key(superseding_id) {
                            bail!(
                                "dangling reference: superseding_id {:?} does not exist",
                                superseding_id
                            );
                        }
                    }
                }
            }
            EpisodeItem::Link(link) => {
                let valid_from = self.projection.observations.contains_key(&link.from_id)
                    || self.projection.goals.contains_key(&link.from_id)
                    || self.projection.hypotheses.contains_key(&link.from_id);
                let valid_to = self.projection.observations.contains_key(&link.to_id)
                    || self.projection.goals.contains_key(&link.to_id)
                    || self.projection.hypotheses.contains_key(&link.to_id);
                if !valid_from || !valid_to {
                    bail!(
                        "dangling typed link: from_id {:?} (valid={}) to_id {:?} (valid={})",
                        link.from_id,
                        valid_from,
                        link.to_id,
                        valid_to
                    );
                }
            }
        }

        Ok(())
    }
}
