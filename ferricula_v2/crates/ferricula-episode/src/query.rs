use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};

use crate::model::*;
use crate::projection::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    Plain,
    Linked,
    Explore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeQueryRequest {
    pub query: String,
    pub mode: RetrievalMode,
    pub limit: Option<usize>,
    pub seed: Option<u64>,
    pub max_links_per_hit: Option<usize>,
    pub max_explore_candidates: Option<usize>,
    pub max_expansion_seeds: Option<usize>,
    pub candidate_cap: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeQueryResponse {
    pub hits: Vec<EpisodeBundle>,
    pub lexical_hits: Vec<EpisodeBundle>,
    pub linked_hits: Vec<EpisodeBundle>,
    pub explored_hits: Vec<EpisodeBundle>,
    pub mode_applied: RetrievalMode,
    pub evaluated_candidates_count: usize,
    pub exploration_budget_exhausted: bool,
    pub exclusions: Vec<ExclusionRecord>,
    pub eligible_unresolved_count: usize,
    pub sampled_unresolved_count: usize,
    pub actual_seed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeBundle {
    pub event_id: String,
    pub score: f32,
    pub origin: CandidateOrigin,
    pub via_seed_id: Option<String>,
    pub relation: Option<String>,
    /// 1. Original observation report (immutable sensory report)
    pub original_observation: ObservationView,
    /// 2a. Primary interpretation (selected by status policy: Supported > Candidate > Disconfirmed)
    pub current_interpretation: Option<InterpretationView>,
    /// 2b. All coexisting alternative interpretations targeting this observation
    pub alternative_interpretations: Vec<InterpretationView>,
    /// 3. Evidential delta explaining why interpretation changed
    pub evidential_delta: Option<EvidentialDeltaView>,
    /// 1-hop connected context items (strictly empty in Plain mode!)
    pub linked_context: Vec<LinkedContextItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationView {
    pub report_id: String,
    pub content: String,
    pub modality: SensoryModality,
    pub location: String,
    pub task_context: String,
    pub event_time: Option<TimeSpec>,
    pub ingested_at: u64,
    pub is_unresolved: bool,
    pub scoped_search: Option<ScopedSearchReport>,
    pub source_actor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretationView {
    pub hypothesis_id: String,
    pub claim: String,
    pub status: HypothesisStatus,
    pub proposed_at: u64,
    pub support_refs: BTreeSet<String>,
    pub against_refs: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidentialDeltaView {
    pub latest_transition: HypothesisTransition,
    pub evidence_refs: BTreeSet<String>,
    pub rationale: String,
    pub transition_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedContextItem {
    pub target_id: String,
    pub target_kind: LinkTargetKind,
    pub relation: String,
    pub summary: String,
}

/// Query the episode projection without any read-side state mutation.
///
/// Enforces hard safety bounds and separability across arms (B1, B2, B4):
/// - Plain: purely lexical observation retrieval; linked_context strictly empty
/// - Linked: Plain lexical hits + 1-hop expansion from observation/goal/hypothesis seeds
/// - Explore: Linked hits + sampling from eligible unresolved episodes with independent budget
pub fn query_episodes(
    request: &EpisodeQueryRequest,
    projection: &EpisodeProjection,
) -> EpisodeQueryResponse {
    let limit = request.limit.unwrap_or(10).clamp(1, MAX_QUERY_LIMIT);
    let max_links = request.max_links_per_hit.map(|v| v.min(MAX_LINKS_PER_HIT)).unwrap_or(8);
    let max_explore = request.max_explore_candidates.map(|v| v.min(MAX_EXPLORE_CANDIDATES)).unwrap_or(16);
    let max_seeds = request.max_expansion_seeds.map(|v| v.min(DEFAULT_MAX_EXPANSION_SEEDS)).unwrap_or(DEFAULT_MAX_EXPANSION_SEEDS);
    let candidate_cap = request.candidate_cap.map(|v| v.min(DEFAULT_LINKED_CANDIDATE_CAP)).unwrap_or(DEFAULT_LINKED_CANDIDATE_CAP);

    let query_tokens = tokenize(&request.query);
    let mut evaluated_count = 0;

    let mut lexical_obs_scores: BTreeMap<String, f32> = BTreeMap::new();
    let mut expansion_seeds: Vec<String> = Vec::new();

    if !query_tokens.is_empty() {
        // 1. Observations lexical scan
        for (event_id, obs) in &projection.observations {
            evaluated_count += 1;
            let haystack = format!(
                "{} {} {} {}",
                obs.report.content,
                obs.report.location,
                obs.report.task_context,
                obs.report.tags.join(" ")
            )
            .to_lowercase();

            let matched = query_tokens
                .iter()
                .filter(|t| haystack.contains(t.as_str()))
                .count();

            if matched > 0 {
                let score = matched as f32 / query_tokens.len() as f32;
                lexical_obs_scores.insert(event_id.clone(), score);
                expansion_seeds.push(event_id.clone());
            }
        }

        // Contextual seeds for Linked and Explore modes: Goals and Hypotheses (B4)
        if matches!(request.mode, RetrievalMode::Linked | RetrievalMode::Explore) {
            for (goal_id, goal) in &projection.goals {
                evaluated_count += 1;
                let haystack = format!(
                    "{} {} {}",
                    goal.goal.target_entity,
                    goal.goal.description,
                    goal.goal.tags.join(" ")
                )
                .to_lowercase();

                let matched = query_tokens
                    .iter()
                    .filter(|t| haystack.contains(t.as_str()))
                    .count();

                if matched > 0 {
                    expansion_seeds.push(goal_id.clone());
                }
            }

            for (hyp_id, hyp) in &projection.hypotheses {
                evaluated_count += 1;
                let haystack = hyp.proposal.claim.to_lowercase();
                let matched = query_tokens
                    .iter()
                    .filter(|t| haystack.contains(t.as_str()))
                    .count();

                if matched > 0 {
                    expansion_seeds.push(hyp_id.clone());
                }
            }
        }
    }

    // Rank lexical observation hits
    let mut ranked_lexical: Vec<(String, f32)> = lexical_obs_scores.into_iter().collect();
    ranked_lexical.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked_lexical.truncate(limit);

    let mut visited_obs: BTreeSet<String> = BTreeSet::new();
    let mut lexical_hits: Vec<EpisodeBundle> = Vec::new();
    for (event_id, score) in ranked_lexical {
        visited_obs.insert(event_id.clone());
        if let Some(bundle) = build_bundle(
            &event_id,
            score,
            CandidateOrigin::Lexical,
            None,
            None,
            request.mode,
            max_links,
            projection,
        ) {
            lexical_hits.push(bundle);
        }
    }

    // Plain mode returns immediately with pure lexical results (C2 purity)
    if request.mode == RetrievalMode::Plain {
        let hits = lexical_hits.clone();
        return EpisodeQueryResponse {
            hits,
            lexical_hits,
            linked_hits: Vec::new(),
            explored_hits: Vec::new(),
            mode_applied: RetrievalMode::Plain,
            evaluated_candidates_count: evaluated_count,
            exploration_budget_exhausted: false,
            exclusions: Vec::new(),
            eligible_unresolved_count: 0,
            sampled_unresolved_count: 0,
            actual_seed: None,
        };
    }

    // 2. Linked mode expansion (strict 1-hop)
    let mut linked_hits: Vec<EpisodeBundle> = Vec::new();
    let mut exclusions: Vec<ExclusionRecord> = Vec::new();

    if max_links > 0 && max_seeds > 0 && candidate_cap > 0 {
        let seeds_to_process: Vec<String> = expansion_seeds.into_iter().take(max_seeds).collect();
        for seed_id in seeds_to_process {
            // Case 1: Direct hypothesis seed connects directly to its target observation (1-hop)
            if let Some(hyp) = projection.hypotheses.get(&seed_id) {
                let target_obs_id = &hyp.proposal.target_episode_id;
                if projection.observations.contains_key(target_obs_id) {
                    if visited_obs.contains(target_obs_id) {
                        exclusions.push(ExclusionRecord {
                            candidate_id: target_obs_id.clone(),
                            reason: ExclusionReason::VisitedBlocked,
                        });
                    } else if linked_hits.len() >= candidate_cap {
                        exclusions.push(ExclusionRecord {
                            candidate_id: target_obs_id.clone(),
                            reason: ExclusionReason::CapOverflow,
                        });
                    } else {
                        visited_obs.insert(target_obs_id.clone());
                        if let Some(bundle) = build_bundle(
                            target_obs_id,
                            1.0,
                            CandidateOrigin::Linked,
                            Some(seed_id.clone()),
                            Some("explains".to_string()),
                            request.mode,
                            max_links,
                            projection,
                        ) {
                            linked_hits.push(bundle);
                        }
                    }
                } else {
                    exclusions.push(ExclusionRecord {
                        candidate_id: target_obs_id.clone(),
                        reason: ExclusionReason::UnresolvableTarget,
                    });
                }
            }

            // Case 2: Neighbors in graph - STRICT 1-HOP ONLY!
            // Direct graph neighbors must resolve directly to an Observation.
            // Hypotheses and Goals do not hop a second edge.
            if let Some(neighbors) = projection.links.get(&seed_id) {
                for (neighbor_id, relation) in neighbors.iter().take(max_links) {
                    if projection.observations.contains_key(neighbor_id) {
                        if visited_obs.contains(neighbor_id) {
                            exclusions.push(ExclusionRecord {
                                candidate_id: neighbor_id.clone(),
                                reason: ExclusionReason::VisitedBlocked,
                            });
                        } else if linked_hits.len() >= candidate_cap {
                            exclusions.push(ExclusionRecord {
                                candidate_id: neighbor_id.clone(),
                                reason: ExclusionReason::CapOverflow,
                            });
                        } else {
                            visited_obs.insert(neighbor_id.clone());
                            if let Some(bundle) = build_bundle(
                                neighbor_id,
                                1.0,
                                CandidateOrigin::Linked,
                                Some(seed_id.clone()),
                                Some(relation.clone()),
                                request.mode,
                                max_links,
                                projection,
                            ) {
                                linked_hits.push(bundle);
                            }
                        }
                    } else {
                        exclusions.push(ExclusionRecord {
                            candidate_id: neighbor_id.clone(),
                            reason: ExclusionReason::UnresolvableTarget,
                        });
                    }
                }
            }
        }
    }

    // 3. Exploration mode (sampling from eligible unresolved episodes with independent budget)
    let mut explored_hits: Vec<EpisodeBundle> = Vec::new();
    let mut budget_exhausted = false;
    let mut eligible_unresolved_count = 0;
    let mut sampled_unresolved_count = 0;
    let mut actual_seed = None;

    if request.mode == RetrievalMode::Explore {
        let mut eligible: Vec<String> = projection
            .unresolved_episodes
            .iter()
            .filter(|id| !visited_obs.contains(*id))
            .cloned()
            .collect();

        eligible_unresolved_count = eligible.len();

        let to_sample = eligible_unresolved_count.min(max_explore);
        sampled_unresolved_count = to_sample;
        budget_exhausted = (eligible_unresolved_count - sampled_unresolved_count) > 0;

        let seed_val = request.seed.unwrap_or(42);
        actual_seed = Some(seed_val);

        if to_sample > 0 {
            let mut prng = SimplePrng::new(seed_val);
            prng.shuffle(&mut eligible);

            for id in eligible.into_iter().take(to_sample) {
                evaluated_count += 1;
                visited_obs.insert(id.clone());
                if let Some(bundle) = build_bundle(
                    &id,
                    1.0,
                    CandidateOrigin::Explored,
                    None,
                    None,
                    request.mode,
                    max_links,
                    projection,
                ) {
                    explored_hits.push(bundle);
                }
            }
        }
    }

    // Combined hits list preserving backward compatibility and hard cap
    let mut all_hits = Vec::new();
    all_hits.extend(lexical_hits.clone());
    all_hits.extend(linked_hits.clone());
    all_hits.extend(explored_hits.clone());
    all_hits.truncate(MAX_QUERY_LIMIT);

    EpisodeQueryResponse {
        hits: all_hits,
        lexical_hits,
        linked_hits,
        explored_hits,
        mode_applied: request.mode,
        evaluated_candidates_count: evaluated_count,
        exploration_budget_exhausted: budget_exhausted,
        exclusions,
        eligible_unresolved_count,
        sampled_unresolved_count,
        actual_seed,
    }
}

fn build_bundle(
    event_id: &str,
    score: f32,
    origin: CandidateOrigin,
    via_seed_id: Option<String>,
    relation: Option<String>,
    mode: RetrievalMode,
    max_links: usize,
    projection: &EpisodeProjection,
) -> Option<EpisodeBundle> {
    let obs = projection.observations.get(event_id)?;

    let original_observation = ObservationView {
        report_id: obs.report.report_id.clone(),
        content: obs.report.content.clone(),
        modality: obs.report.modality,
        location: obs.report.location.clone(),
        task_context: obs.report.task_context.clone(),
        event_time: obs.report.event_time.clone(),
        ingested_at: obs.report.ingested_at,
        is_unresolved: obs.report.is_unresolved,
        scoped_search: obs.report.scoped_search.clone(),
        source_actor: obs.report.source_actor.clone(),
    };

    // Fast O(1) indexed lookup of hypotheses targeting this observation (B3)
    let matching_hyps: Vec<&StoredHypothesis> = if let Some(hyp_ids) = projection.episode_hypotheses.get(event_id) {
        let mut list: Vec<&StoredHypothesis> = hyp_ids
            .iter()
            .filter_map(|id| projection.hypotheses.get(id))
            .collect();
        list.sort_by(|a, b| {
            status_priority(b.current_status)
                .cmp(&status_priority(a.current_status))
                .then_with(|| b.proposal.proposed_at.cmp(&a.proposal.proposed_at))
        });
        list
    } else {
        Vec::new()
    };

    let mut primary_view = None;
    let mut alternatives = Vec::new();

    for (idx, h) in matching_hyps.into_iter().enumerate() {
        let view = InterpretationView {
            hypothesis_id: h.proposal.hypothesis_id.clone(),
            claim: h.proposal.claim.clone(),
            status: h.current_status,
            proposed_at: h.proposal.proposed_at,
            support_refs: h.current_support_refs.clone(),
            against_refs: h.current_against_refs.clone(),
        };
        if idx == 0 {
            primary_view = Some((h, view));
        } else {
            alternatives.push(view);
        }
    }

    let (primary_h, current_interpretation) = match primary_view {
        Some((h, view)) => (Some(h), Some(view)),
        None => (None, None),
    };

    let evidential_delta = primary_h.and_then(|h| {
        projection
            .status_history
            .get(&h.proposal.hypothesis_id)
            .and_then(|history| history.last())
            .map(|st| EvidentialDeltaView {
                latest_transition: st.transition.clone(),
                evidence_refs: st.evidence_refs.clone(),
                rationale: st.rationale.clone(),
                transition_time: st.transition_time,
            })
    });

    // Construct Linked Context:
    // INVARIANT: Plain mode MUST NOT expose linked_context! Zero link budget also produces empty context.
    let mut linked_context = Vec::new();
    if mode != RetrievalMode::Plain && max_links > 0 {
        if let Some(neighbors) = projection.links.get(event_id) {
            let mut visited = BTreeSet::new();
            for (target_id, relation) in neighbors.iter().take(max_links) {
                if visited.insert(target_id.clone()) {
                    let (kind, summary) = if let Some(target_obs) = projection.observations.get(target_id) {
                        (LinkTargetKind::Observation, target_obs.report.content.clone())
                    } else if let Some(target_hyp) = projection.hypotheses.get(target_id) {
                        (LinkTargetKind::Hypothesis, target_hyp.proposal.claim.clone())
                    } else if let Some(target_goal) = projection.goals.get(target_id) {
                        (LinkTargetKind::Goal, target_goal.goal.description.clone())
                    } else {
                        (LinkTargetKind::Other, target_id.clone())
                    };

                    linked_context.push(LinkedContextItem {
                        target_id: target_id.clone(),
                        target_kind: kind,
                        relation: relation.clone(),
                        summary,
                    });
                }
            }
        }
    }

    Some(EpisodeBundle {
        event_id: event_id.to_string(),
        score,
        origin,
        via_seed_id,
        relation,
        original_observation,
        current_interpretation,
        alternative_interpretations: alternatives,
        evidential_delta,
        linked_context,
    })
}

fn status_priority(status: HypothesisStatus) -> u8 {
    match status {
        HypothesisStatus::Supported => 3,
        HypothesisStatus::Candidate => 2,
        HypothesisStatus::Disconfirmed => 1,
        HypothesisStatus::Superseded => 0,
    }
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|p| p.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}

/// Simple, deterministic pseudo-random number generator for reproducible sampling.
struct SimplePrng {
    state: u64,
}

impl SimplePrng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0xdead_beef_cafe_babe } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.state
    }

    fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = (self.next_u64() as usize) % (i + 1);
            slice.swap(i, j);
        }
    }
}
