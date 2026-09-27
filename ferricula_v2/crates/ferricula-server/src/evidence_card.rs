//! Pure typed evidence cards derived from [`EpisodeQueryResponse`].
//!
//! Serializes independently of chat prose. Does not infer causes, rewrite
//! generated text, or expose `excluded_causes` as if it enforced replies.
//! `cause_established` stays false even when a hypothesis is `supported`
//! (support is not proof). `unsupported_exclusion_forbidden` stays true
//! unless the bundle carries an explicit warranted exclusion — this module
//! does not mint one from `not_seen_in_scope` or from support.

use ferricula_episode::query::{EpisodeBundle, EpisodeQueryResponse};
use ferricula_episode::{HypothesisStatus, ScopedSearchResult};
use serde::{Deserialize, Serialize};

/// One stored observation's factual card. Not generated suggestion text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceCard {
    pub event_id: String,
    pub original_report: String,
    pub location: String,
    pub inspected_scope: Option<String>,
    pub search_result: Option<ScopedSearchResult>,
    pub interpretation_status: Option<HypothesisStatus>,
    /// Always false here: a supported hypothesis is not proof of cause.
    pub cause_established: bool,
    /// True when the record has no explicit warranted exclusion.
    pub unsupported_exclusion_forbidden: bool,
}

impl EvidenceCard {
    pub fn from_query(
        response: &ferricula_episode::query::EpisodeQueryResponse,
    ) -> Vec<EvidenceCard> {
        cards(response)
    }

    pub fn from_bundle(bundle: &EpisodeBundle) -> Self {
        let obs = &bundle.original_observation;
        Self {
            event_id: bundle.event_id.clone(),
            original_report: obs.content.clone(),
            location: obs.location.clone(),
            inspected_scope: obs.scoped_search.as_ref().map(|search| search.scope.clone()),
            search_result: obs.scoped_search.as_ref().map(|search| search.result),
            interpretation_status: bundle.current_interpretation.as_ref().map(|interp| interp.status),
            cause_established: false,
            unsupported_exclusion_forbidden: !explicit_warranted_exclusion(bundle),
        }
    }
}

/// Bundles do not currently carry a typed warranted-exclusion of unnamed causes.
/// `NotSeenInScope` and `Supported` are not treated as one.
fn explicit_warranted_exclusion(_bundle: &EpisodeBundle) -> bool {
    false
}

fn bundles_in_wire_order(response: &EpisodeQueryResponse) -> Vec<&EpisodeBundle> {
    let mut ordered = Vec::new();
    ordered.extend(response.explored_hits.iter());
    ordered.extend(response.linked_hits.iter());
    ordered.extend(response.lexical_hits.iter());
    ordered
}

/// Independent serialize entry for wiring. Canonical cards only; first `event_id` wins.
pub fn cards(response: &ferricula_episode::query::EpisodeQueryResponse) -> Vec<EvidenceCard> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for bundle in bundles_in_wire_order(response) {
        if !seen.insert(bundle.event_id.clone()) {
            continue;
        }
        out.push(EvidenceCard::from_bundle(bundle));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferricula_episode::query::{
        EpisodeBundle, EpisodeQueryResponse, InterpretationView, ObservationView, RetrievalMode,
    };
    use ferricula_episode::{
        CandidateOrigin, HypothesisStatus, ScopedSearchReport, ScopedSearchResult, SensoryModality,
    };
    use std::collections::BTreeSet;

    fn observation(
        content: &str,
        location: &str,
        unresolved: bool,
        scope: Option<(&str, ScopedSearchResult)>,
    ) -> ObservationView {
        ObservationView {
            report_id: "rep".into(),
            content: content.into(),
            modality: SensoryModality::Sound,
            location: location.into(),
            task_context: "operator-report".into(),
            event_time: None,
            ingested_at: 1,
            is_unresolved: unresolved,
            scoped_search: scope.map(|(scope, result)| ScopedSearchReport {
                scope: scope.into(),
                result,
            }),
            source_actor: "operator".into(),
        }
    }

    fn bundle(
        event_id: &str,
        origin: CandidateOrigin,
        obs: ObservationView,
        interpretation: Option<InterpretationView>,
    ) -> EpisodeBundle {
        EpisodeBundle {
            event_id: event_id.into(),
            score: 1.0,
            origin,
            via_seed_id: None,
            relation: None,
            original_observation: obs,
            current_interpretation: interpretation,
            alternative_interpretations: Vec::new(),
            evidential_delta: None,
            linked_context: Vec::new(),
        }
    }

    fn empty_query() -> EpisodeQueryResponse {
        EpisodeQueryResponse {
            hits: Vec::new(),
            lexical_hits: Vec::new(),
            linked_hits: Vec::new(),
            explored_hits: Vec::new(),
            mode_applied: RetrievalMode::Explore,
            evaluated_candidates_count: 0,
            exploration_budget_exhausted: false,
            exclusions: Vec::new(),
            eligible_unresolved_count: 0,
            sampled_unresolved_count: 0,
            actual_seed: None,
        }
    }

    fn near_bench_inside_drawer() -> EpisodeBundle {
        bundle(
            "evt-clatter",
            CandidateOrigin::Explored,
            observation(
                "plastic clatter heard near the bench",
                "near bench",
                true,
                Some(("inside drawer", ScopedSearchResult::NotSeenInScope)),
            ),
            None,
        )
    }

    fn supported(claim: &str) -> InterpretationView {
        InterpretationView {
            hypothesis_id: "hyp-1".into(),
            claim: claim.into(),
            status: HypothesisStatus::Supported,
            proposed_at: 2,
            support_refs: BTreeSet::new(),
            against_refs: BTreeSet::new(),
        }
    }

    #[test]
    fn sound_near_bench_stays_distinct_from_search_inside_drawer() {
        let mut response = empty_query();
        response.explored_hits = vec![near_bench_inside_drawer()];
        let cards = cards(&response);
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(card.location, "near bench");
        assert_eq!(card.inspected_scope.as_deref(), Some("inside drawer"));
        assert_eq!(card.search_result, Some(ScopedSearchResult::NotSeenInScope));
        assert_eq!(card.original_report, "plastic clatter heard near the bench");
        assert_ne!(card.location, card.inspected_scope.clone().unwrap());
        assert!(!card.location.contains("drawer"));
        assert!(!card.inspected_scope.as_ref().unwrap().contains("bench"));
        assert!(!card.original_report.contains("inside drawer"));
    }

    #[test]
    fn serialize_keeps_location_and_scope_independent() {
        let mut response = empty_query();
        response.explored_hits = vec![near_bench_inside_drawer()];
        let value = serde_json::to_value(cards(&response)).unwrap();
        assert!(value.is_array());
        assert_eq!(value[0]["location"], "near bench");
        assert_eq!(value[0]["inspected_scope"], "inside drawer");
        assert_eq!(value[0]["original_report"], "plastic clatter heard near the bench");
        assert!(value[0].get("excluded_causes").is_none());
        assert_eq!(value[0]["cause_established"], false);
        assert_eq!(value[0]["unsupported_exclusion_forbidden"], true);
    }

    #[test]
    fn supported_hypothesis_does_not_establish_cause() {
        let item = bundle(
            "evt-supported",
            CandidateOrigin::Lexical,
            observation(
                "plastic clatter heard near the bench",
                "near bench",
                true,
                Some(("inside drawer", ScopedSearchResult::NotSeenInScope)),
            ),
            Some(supported("it is in the drawer")),
        );
        let card = EvidenceCard::from_bundle(&item);
        assert_eq!(card.interpretation_status, Some(HypothesisStatus::Supported));
        assert!(!card.cause_established);
        assert!(card.unsupported_exclusion_forbidden);
        assert_eq!(card.location, "near bench");
        assert_eq!(card.inspected_scope.as_deref(), Some("inside drawer"));
        assert!(!card.location.contains("drawer"));
    }

    #[test]
    fn cards_signature_returns_vec_of_evidence_card() {
        assert!(cards(&empty_query()).is_empty());
        let mut response = empty_query();
        response.explored_hits = vec![near_bench_inside_drawer()];
        let rows: Vec<EvidenceCard> = cards(&response);
        let _cloned: Vec<EvidenceCard> = rows.clone();
        assert_eq!(rows[0].event_id, "evt-clatter");
        assert!(!rows[0].cause_established);
        assert!(rows[0].unsupported_exclusion_forbidden);
    }

    #[test]
    fn walks_arms_and_dedups_event_id() {
        let a = near_bench_inside_drawer();
        let mut lexical = a.clone();
        lexical.origin = CandidateOrigin::Lexical;
        let other = bundle(
            "evt-other",
            CandidateOrigin::Linked,
            observation("unrelated beep", "hallway", false, None),
            None,
        );
        let mut response = empty_query();
        response.explored_hits = vec![a];
        response.linked_hits = vec![other];
        response.lexical_hits = vec![lexical];
        let rows = cards(&response);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].event_id, "evt-clatter");
        assert_eq!(rows[1].event_id, "evt-other");
        assert_eq!(rows[1].inspected_scope, None);
        assert_eq!(rows[1].search_result, None);
        assert!(!rows[1].cause_established);
        assert!(rows[1].unsupported_exclusion_forbidden);
    }
}
