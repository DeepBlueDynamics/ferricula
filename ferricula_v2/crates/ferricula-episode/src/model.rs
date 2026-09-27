use std::collections::BTreeSet;
use serde::{Deserialize, Serialize};

/// Channel constant used in OverlayPayload::ProposeMemory to identify structured episode items.
pub const EPISODE_CHANNEL_V1: &str = "episode_v1";

/// Hard safety bounds for queries.
pub const MAX_QUERY_LIMIT: usize = 50;
pub const MAX_LINKS_PER_HIT: usize = 16;
pub const MAX_EXPLORE_CANDIDATES: usize = 32;
pub const DEFAULT_MAX_EXPANSION_SEEDS: usize = 200;
pub const DEFAULT_LINKED_CANDIDATE_CAP: usize = 64;

/// Attribution origin of a retrieved candidate in the context bundle (B1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateOrigin {
    Lexical,
    Linked,
    Explored,
}

/// Category of an entity referenced as a linked context neighbor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTargetKind {
    Observation,
    Hypothesis,
    Goal,
    Other,
}

/// Reason why a potential candidate was excluded from admission (B2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    CapOverflow,
    VisitedBlocked,
    UnresolvableTarget,
    MissingEvidence,
}

/// Record of an excluded candidate during bounded expansion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExclusionRecord {
    pub candidate_id: String,
    pub reason: ExclusionReason,
}

/// Physical occurrence time in the world, distinct from ingestion and commit times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimeSpec {
    /// Exact known physical timestamp (unix seconds).
    Exact { timestamp: u64 },
    /// Bounded interval (e.g. during a 20-minute chore).
    Interval { start: u64, end: u64 },
    /// Approximated or relative context marker (e.g. "during morning cleaning").
    Uncertain { description: String },
}

/// Sensory channel of an observation report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensoryModality {
    Sound,
    Visual,
    Proprioceptive,
    Tactile,
    Action,
    Text,
}

/// Explicitly bounded search outcome preventing negative searches from
/// masquerading as universal absence claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedSearchReport {
    /// Physical or conceptual boundary inspected (e.g. "under-couch-only").
    pub scope: String,
    /// Result strictly bounded by scope.
    pub result: ScopedSearchResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopedSearchResult {
    /// Target was not observed within the inspected boundary.
    NotSeenInScope,
    /// Target was positively observed within scope.
    Found,
    /// Inspection was obscured or inconclusive.
    Inconclusive,
}

/// An attributed sensory report from an observer, sensor, or user.
///
/// Note: This is an attributed report, not an infallible physical fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationReport {
    pub report_id: String,
    pub event_time: Option<TimeSpec>,
    pub ingested_at: u64,
    pub modality: SensoryModality,
    pub location: String,
    pub task_context: String,
    pub content: String,
    pub scoped_search: Option<ScopedSearchReport>,
    pub source_actor: String,
    /// Explicit designation of whether this observation represents an unexplained
    /// or unresolved experience requiring explanation.
    pub is_unresolved: bool,
    pub tags: Vec<String>,
}

/// A registered user or agent goal that can act as a retrieval cue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalReport {
    pub goal_id: String,
    pub created_at: u64,
    pub target_entity: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// Hypothesis verification lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatus {
    /// All proposals MUST begin in this state.
    Candidate,
    /// Corroborated by positive evidence reports.
    Supported,
    /// Contradicted by disconfirming evidence reports.
    Disconfirmed,
    /// Replaced by a more comprehensive or precise hypothesis.
    Superseded,
}

/// An explanatory interpretation linking a sensory observation to a candidate cause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HypothesisProposal {
    pub hypothesis_id: String,
    /// Stable event ID of the observation report being explained.
    pub target_episode_id: String,
    pub claim: String,
    /// Initial status: invariant requires this to be Candidate on proposal.
    pub status: HypothesisStatus,
    /// Overlay event IDs of supporting observation reports (must not be empty for Supported).
    pub support_refs: BTreeSet<String>,
    /// Overlay event IDs of disconfirming observation reports.
    pub against_refs: BTreeSet<String>,
    pub proposed_at: u64,
}

/// State transition for an existing hypothesis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusTransitionEvent {
    pub hypothesis_id: String,
    pub transition: HypothesisTransition,
    /// Event IDs of observation reports providing evidence for this transition.
    pub evidence_refs: BTreeSet<String>,
    pub rationale: String,
    pub transition_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HypothesisTransition {
    /// Transitions Candidate -> Supported.
    Support,
    /// Transitions Candidate or Supported -> Disconfirmed.
    Disconfirm,
    /// Transitions Candidate or Supported -> Superseded.
    Supersede { superseding_id: String },
}

/// Explicit relation between two entities in the overlay graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExplicitLink {
    pub from_id: String,
    pub to_id: String,
    pub relation: String,
    pub weight: f32,
}

/// Versioned envelope for structured episode data stored in `ProposeMemory.text`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum EpisodeItem {
    Observation(ObservationReport),
    Goal(GoalReport),
    Hypothesis(HypothesisProposal),
    StatusTransition(StatusTransitionEvent),
    Link(ExplicitLink),
}

/// Derives a valid overlay index tag from an optional prefix and raw string value.
///
/// Ensures compliance with `OverlayLog::validate_ident`:
/// - Length strictly <= 64 bytes (`MAX_IDENT_BYTES`)
/// - Characters restricted to ASCII `[A-Za-z0-9._:-]`
/// - Non-empty and no surrounding whitespace/punctuation
/// - Omits characters/strings that cannot form a valid identifier
pub fn derive_safe_tag(prefix: Option<&str>, raw: &str) -> Option<String> {
    let raw_trimmed = raw.trim();
    if raw_trimmed.is_empty() {
        return None;
    }

    let mut buf = String::with_capacity(64);
    if let Some(p) = prefix {
        let p_clean: String = p
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            .collect();
        if !p_clean.is_empty() {
            buf.push_str(&p_clean);
            buf.push(':');
        }
    }

    let mut last_was_delim = buf.ends_with(':') || buf.ends_with('_') || buf.ends_with('-');
    for c in raw_trimmed.chars() {
        if buf.len() >= 64 {
            break;
        }
        if c.is_ascii_alphanumeric() || matches!(c, '.' | ':') {
            buf.push(c.to_ascii_lowercase());
            last_was_delim = false;
        } else if c.is_whitespace() || c == '_' {
            if !last_was_delim && buf.len() < 64 {
                buf.push('_');
                last_was_delim = true;
            }
        } else if c == '-' {
            if !last_was_delim && buf.len() < 64 {
                buf.push('-');
                last_was_delim = true;
            }
        }
    }

    let clean = buf.trim_end_matches(|c| matches!(c, ':' | '_' | '-' | '.'));
    if clean.is_empty() || clean.ends_with(':') {
        None
    } else if clean.len() <= 64 {
        Some(clean.to_string())
    } else {
        let slice = &clean[..64];
        let trimmed = slice.trim_end_matches(|c| matches!(c, ':' | '_' | '-' | '.'));
        if trimmed.is_empty() || trimmed.ends_with(':') {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

impl EpisodeItem {
    pub fn summary_tags(&self) -> Vec<String> {
        let mut tags = Vec::new();
        tags.push("channel:episode_v1".to_string());
        match self {
            EpisodeItem::Observation(obs) => {
                tags.push("kind:observation".to_string());
                tags.push(format!("modality:{:?}", obs.modality).to_lowercase());
                if let Some(t) = derive_safe_tag(Some("location"), &obs.location) {
                    tags.push(t);
                }
                if obs.is_unresolved {
                    tags.push("status:unresolved".to_string());
                }
                if let Some(ref s) = obs.scoped_search {
                    if let Some(t) = derive_safe_tag(Some("search_scope"), &s.scope) {
                        tags.push(t);
                    }
                    tags.push(format!("search_result:{:?}", s.result).to_lowercase());
                }
                for t in &obs.tags {
                    if let Some(st) = derive_safe_tag(None, t) {
                        tags.push(st);
                    }
                }
            }
            EpisodeItem::Goal(goal) => {
                tags.push("kind:goal".to_string());
                if let Some(t) = derive_safe_tag(Some("entity"), &goal.target_entity) {
                    tags.push(t);
                }
                for t in &goal.tags {
                    if let Some(st) = derive_safe_tag(None, t) {
                        tags.push(st);
                    }
                }
            }
            EpisodeItem::Hypothesis(hyp) => {
                tags.push("kind:hypothesis".to_string());
                tags.push(format!("status:{:?}", hyp.status).to_lowercase());
                if let Some(t) = derive_safe_tag(Some("target"), &hyp.target_episode_id) {
                    tags.push(t);
                }
            }
            EpisodeItem::StatusTransition(st) => {
                tags.push("kind:status_transition".to_string());
                if let Some(t) = derive_safe_tag(Some("hyp"), &st.hypothesis_id) {
                    tags.push(t);
                }
            }
            EpisodeItem::Link(link) => {
                tags.push("kind:link".to_string());
                if let Some(t) = derive_safe_tag(Some("rel"), &link.relation) {
                    tags.push(t);
                }
            }
        }
        tags.sort();
        tags.dedup();
        // Extra safeguard: retain only valid idents <= 64 bytes and clamp to max 16 tags
        tags.retain(|t| {
            !t.is_empty()
                && t.len() <= 64
                && t == t.trim()
                && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
        });
        tags.truncate(16);
        tags
    }

    pub fn item_id(&self) -> &str {
        match self {
            EpisodeItem::Observation(obs) => &obs.report_id,
            EpisodeItem::Goal(goal) => &goal.goal_id,
            EpisodeItem::Hypothesis(hyp) => &hyp.hypothesis_id,
            EpisodeItem::StatusTransition(st) => &st.hypothesis_id,
            EpisodeItem::Link(link) => &link.relation,
        }
    }
}
