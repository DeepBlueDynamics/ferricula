use std::collections::{BTreeMap, BTreeSet};
use anyhow::{bail, Result};
use crate::overlay::{OverlayEvent, OverlayPayload};

use crate::causal::{self, Walk};
use crate::model::*;

/// A stored observation folded from an overlay event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObservation {
    pub event_id: String,
    pub recorded_at: u64,
    pub report: ObservationReport,
}

/// A stored goal folded from an overlay event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGoal {
    pub event_id: String,
    pub recorded_at: u64,
    pub goal: GoalReport,
}

/// A stored hypothesis folded from an overlay event, with its active status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredHypothesis {
    pub event_id: String,
    pub recorded_at: u64,
    pub proposal: HypothesisProposal,
    pub current_status: HypothesisStatus,
    pub current_support_refs: BTreeSet<String>,
    pub current_against_refs: BTreeSet<String>,
    pub superseding_id: Option<String>,
}

/// Queryable in-memory projection folded from the immutable overlay log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EpisodeProjection {
    /// Observations keyed by overlay event ID (ov1-...).
    pub observations: BTreeMap<String, StoredObservation>,
    /// Index mapping user report_id to overlay event_id.
    pub report_id_to_event: BTreeMap<String, String>,

    /// Goals keyed by overlay event ID.
    pub goals: BTreeMap<String, StoredGoal>,
    pub goal_id_to_event: BTreeMap<String, String>,

    /// Hypotheses keyed by overlay event ID.
    pub hypotheses: BTreeMap<String, StoredHypothesis>,
    /// Index mapping user hypothesis_id to overlay event_id.
    pub hypothesis_id_to_event: BTreeMap<String, String>,

    /// Hypotheses targeting an observation episode ID (B3).
    pub episode_hypotheses: BTreeMap<String, Vec<String>>,
    /// Transition history per hypothesis ID.
    pub status_history: BTreeMap<String, Vec<StatusTransitionEvent>>,

    /// Bidirectional graph: event_id -> Vec<(neighbor_event_id, relation)>
    pub links: BTreeMap<String, Vec<(String, String)>>,

    /// Registry of unresolved observation event IDs.
    pub unresolved_episodes: BTreeSet<String>,

    /// Inverted index from lowercase token/tag to observation event IDs.
    pub tag_index: BTreeMap<String, BTreeSet<String>>,
}

impl EpisodeProjection {
    /// Fold a sequence of overlay events into the queryable projection.
    ///
    /// Fail-closed: returns Err if any episode_v1 event contains malformed JSON
    /// or violates integrity invariants during replay.
    pub fn fold_from_events(events: &[OverlayEvent]) -> Result<Self> {
        let mut proj = Self::default();
        for event in events {
            proj.apply_event(event)?;
        }
        Ok(proj)
    }

    /// Apply a single overlay event to the projection (fail-closed).
    pub fn apply_event(&mut self, event: &OverlayEvent) -> Result<()> {
        let OverlayPayload::ProposeMemory {
            ref text,
            ref channel,
            ..
        } = event.payload
        else {
            return Ok(());
        };

        if channel != EPISODE_CHANNEL_V1 {
            return Ok(());
        }

        let item: EpisodeItem = serde_json::from_str(text)
            .map_err(|e| anyhow::anyhow!("failed to deserialize EpisodeItem JSON in event {}: {e}", event.event_id))?;
        let event_id = event.event_id.clone();
        let recorded_at = event.recorded_at;

        match item {
            EpisodeItem::Observation(obs) => {
                if self.report_id_to_event.contains_key(&obs.report_id) {
                    bail!("duplicate report_id {:?} in event {}", obs.report_id, event_id);
                }

                self.index_text(&event_id, &obs.content);
                self.index_text(&event_id, &obs.location);
                self.index_text(&event_id, &obs.task_context);
                for tag in &obs.tags {
                    self.index_tag(&event_id, tag);
                }

                // Initial unresolved status:
                // An observation enters the unresolved pool only if explicitly flagged unresolved (honors explicit flag).
                if obs.is_unresolved {
                    self.unresolved_episodes.insert(event_id.clone());
                }

                self.report_id_to_event.insert(obs.report_id.clone(), event_id.clone());
                self.observations.insert(
                    event_id.clone(),
                    StoredObservation {
                        event_id,
                        recorded_at,
                        report: obs,
                    },
                );
            }
            EpisodeItem::Goal(goal) => {
                if self.goal_id_to_event.contains_key(&goal.goal_id) {
                    bail!("duplicate goal_id {:?} in event {}", goal.goal_id, event_id);
                }

                self.index_text(&event_id, &goal.target_entity);
                self.index_text(&event_id, &goal.description);
                for tag in &goal.tags {
                    self.index_tag(&event_id, tag);
                }

                self.goal_id_to_event.insert(goal.goal_id.clone(), event_id.clone());
                self.goals.insert(
                    event_id.clone(),
                    StoredGoal {
                        event_id,
                        recorded_at,
                        goal,
                    },
                );
            }
            EpisodeItem::Hypothesis(hyp) => {
                if self.hypothesis_id_to_event.contains_key(&hyp.hypothesis_id) {
                    bail!(
                        "duplicate hypothesis_id {:?} in event {}",
                        hyp.hypothesis_id,
                        event_id
                    );
                }

                self.index_text(&event_id, &hyp.claim);
                let target_obs_id = hyp.target_episode_id.clone();

                // Add bidirectional graph links between overlay event IDs
                self.add_link(&event_id, &target_obs_id, "explains");
                self.add_link(&target_obs_id, &event_id, "explained_by");

                // As long as a hypothesis is Candidate, mark the target episode unresolved
                self.unresolved_episodes.insert(target_obs_id.clone());

                for r in &hyp.support_refs {
                    if !self.observations.contains_key(r) {
                        bail!("evidential typing violation: support_ref {r:?} does not resolve to an ObservationReport");
                    }
                }
                for r in &hyp.against_refs {
                    if !self.observations.contains_key(r) {
                        bail!("evidential typing violation: against_ref {r:?} does not resolve to an ObservationReport");
                    }
                }

                self.episode_hypotheses
                    .entry(target_obs_id.clone())
                    .or_default()
                    .push(event_id.clone());

                let support = hyp.support_refs.clone();
                let against = hyp.against_refs.clone();
                self.hypothesis_id_to_event
                    .insert(hyp.hypothesis_id.clone(), event_id.clone());

                self.hypotheses.insert(
                    event_id.clone(),
                    StoredHypothesis {
                        event_id,
                        recorded_at,
                        proposal: hyp,
                        current_status: HypothesisStatus::Candidate,
                        current_support_refs: support,
                        current_against_refs: against,
                        superseding_id: None,
                    },
                );
            }
            EpisodeItem::StatusTransition(st) => {
                let Some(hyp_event_id) = self.hypothesis_id_to_event.get(&st.hypothesis_id).cloned()
                else {
                    bail!(
                        "status transition references unknown hypothesis_id {:?}",
                        st.hypothesis_id
                    );
                };

                // Collect links to add after mutating hypothesis (fixes borrow checker conflict)
                let mut links_to_add = Vec::new();
                let mut target_ep = String::new();

                if let Some(hyp) = self.hypotheses.get_mut(&hyp_event_id) {
                    target_ep = hyp.proposal.target_episode_id.clone();
                    let mut independent_evidence = false;
                    for r in &st.evidence_refs {
                        let Some(obs) = self.observations.get(r) else {
                            bail!("evidential typing violation: evidence_ref {r:?} does not resolve to an ObservationReport");
                        };
                        if r != &target_ep
                            && r != &hyp_event_id
                            && crate::causal::observation_can_change_hypothesis_status(&obs.report)
                            && crate::causal::evidence_has_authorizing_link(
                                &self.links,
                                r,
                                &hyp_event_id,
                                &target_ep,
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
                    match st.transition {
                        HypothesisTransition::Support => {
                            hyp.current_status = HypothesisStatus::Supported;
                            for r in &st.evidence_refs {
                                hyp.current_support_refs.insert(r.clone());
                                links_to_add.push((hyp_event_id.clone(), r.clone(), "supported_by".to_string()));
                                links_to_add.push((r.clone(), hyp_event_id.clone(), "supports".to_string()));
                            }
                        }
                        HypothesisTransition::Disconfirm => {
                            hyp.current_status = HypothesisStatus::Disconfirmed;
                            for r in &st.evidence_refs {
                                hyp.current_against_refs.insert(r.clone());
                                links_to_add.push((hyp_event_id.clone(), r.clone(), "disconfirmed_by".to_string()));
                                links_to_add.push((r.clone(), hyp_event_id.clone(), "disconfirms".to_string()));
                            }
                        }
                        HypothesisTransition::Supersede {
                            ref superseding_id,
                        } => {
                            hyp.current_status = HypothesisStatus::Superseded;
                            hyp.superseding_id = Some(superseding_id.clone());
                            if let Some(sup_ev) = self.hypothesis_id_to_event.get(superseding_id) {
                                links_to_add.push((hyp_event_id.clone(), sup_ev.clone(), "superseded_by".to_string()));
                            }
                        }
                    }
                }

                for (from, to, rel) in links_to_add {
                    self.add_link(&from, &to, &rel);
                }

                if !target_ep.is_empty() {
                    self.recompute_episode_resolution(&target_ep);
                }

                self.status_history
                    .entry(st.hypothesis_id.clone())
                    .or_default()
                    .push(st);
            }
            EpisodeItem::Link(link) => {
                let valid_from = self.observations.contains_key(&link.from_id)
                    || self.goals.contains_key(&link.from_id)
                    || self.hypotheses.contains_key(&link.from_id);
                let valid_to = self.observations.contains_key(&link.to_id)
                    || self.goals.contains_key(&link.to_id)
                    || self.hypotheses.contains_key(&link.to_id);
                if !valid_from || !valid_to {
                    bail!(
                        "dangling typed link: from_id {:?} (valid={}) to_id {:?} (valid={})",
                        link.from_id,
                        valid_from,
                        link.to_id,
                        valid_to
                    );
                }
                // Typed Paṭṭhāna relations keep their arrow. Legacy free-string
                // labels stay bidirectional so existing documents do not change.
                match causal::parse_relation(&link.relation).map(|relation| relation.walk()) {
                    Some(Walk::Directed) => {
                        self.add_link(&link.from_id, &link.to_id, &link.relation);
                    }
                    Some(Walk::Qualifier) => {}
                    Some(Walk::Mutual) | None => {
                        self.add_link(&link.from_id, &link.to_id, &link.relation);
                        self.add_link(&link.to_id, &link.from_id, &link.relation);
                    }
                }
            }
        }

        Ok(())
    }

    fn recompute_episode_resolution(&mut self, episode_id: &str) {
        // An episode remains unresolved if:
        // 1. Any linked hypothesis is still Candidate
        // 2. OR all proposed hypotheses were disconfirmed (disconfirming explanations does not resolve original uncertainty!)
        // 3. OR it was explicitly flagged unresolved and no hypothesis has reached Supported.
        let hyp_ids = self.episode_hypotheses.get(episode_id).cloned().unwrap_or_default();
        let hyps: Vec<&StoredHypothesis> = hyp_ids
            .iter()
            .filter_map(|id| self.hypotheses.get(id))
            .collect();

        let has_candidate_hypothesis = hyps
            .iter()
            .any(|h| h.current_status == HypothesisStatus::Candidate);

        let has_supported_hypothesis = hyps
            .iter()
            .any(|h| h.current_status == HypothesisStatus::Supported);

        let obs_is_unresolved = self
            .observations
            .get(episode_id)
            .map(|obs| obs.report.is_unresolved)
            .unwrap_or(false);

        if has_candidate_hypothesis {
            self.unresolved_episodes.insert(episode_id.to_string());
        } else if obs_is_unresolved && !has_supported_hypothesis {
            // Unresolved observation whose candidate explanations were disconfirmed
            // remains unresolved!
            self.unresolved_episodes.insert(episode_id.to_string());
        } else if has_supported_hypothesis || !obs_is_unresolved {
            // Positively explained and corroborated, or observation was not explicitly unresolved
            self.unresolved_episodes.remove(episode_id);
        }
    }

    fn add_link(&mut self, from: &str, to: &str, relation: &str) {
        let entry = self.links.entry(from.to_string()).or_default();
        if !entry.iter().any(|(t, r)| t == to && r == relation) {
            entry.push((to.to_string(), relation.to_string()));
        }
    }

    fn index_text(&mut self, event_id: &str, text: &str) {
        for token in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|p| p.chars().count() >= 3)
            .map(str::to_lowercase)
        {
            self.tag_index
                .entry(token)
                .or_default()
                .insert(event_id.to_string());
        }
    }

    fn index_tag(&mut self, event_id: &str, tag: &str) {
        self.tag_index
            .entry(tag.to_lowercase())
            .or_default()
            .insert(event_id.to_string());
    }
}
