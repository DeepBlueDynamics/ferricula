//! Paṭṭhāna relation types and admission consensus for episode links.
//!
//! This module types causal edges. It does not walk them and it does not
//! write the recovered base volume. Projection still mirrors legacy string
//! links in both directions; callers that need arrow-of-time behavior use
//! [`Walk`] from here and must not clone a directed relation backwards.
//!
//! Sources, each used only for what it actually states:
//! - `research/08_CAUSAL_DYNAMICS_APOHA_AND_COGNITIVE_CONTROL.md` §2.1–§2.2
//!   (24 relations, four root conditions, transition fails if any root is empty).
//! - `research/10_PATTHANA_24_CAUSAL_CONDITIONS.md` §2 (the same 24 names).
//! - `research/18_SHENTONG_RANGTONG_AND_THE_TWO_TRUTHS_OUTLIER.md` §7.2
//!   (a goal–observation edge must trace to a verified sensory record;
//!   base and overlay do not share a mutability contract).
//! - `research/lume-ollaya-abhidhamma-addendum-A.md.pdf` §A.1.1 and Rule 10
//!   (decay changes retrieval priority; text is removed only by a deliberate
//!   release; contradictory clusters are not merged).
//! - `ferricula-core` `EdgeKind` (directed causal vs bidirectional semantic).
//!
//! The commentary claim that all 24 fold into the four roots is stated in
//! doc 08 and is not tabulated there. This module does not invent that table.
//! Only the four named roots participate in [`transition_consensus`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{ObservationReport, ScopedSearchResult};

/// One of the 24 Paṭṭhāna conditions. Serde names are ASCII snake_case so
/// they survive `derive_safe_tag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PatthanaRelation {
    Hetu,
    Arammana,
    Adhipati,
    Anantara,
    Samanantara,
    Sahajata,
    Annamanna,
    Nissaya,
    Upanissaya,
    Purejata,
    Pacchajata,
    Asevana,
    Kamma,
    Vipaka,
    Ahara,
    Indriya,
    Jhana,
    Magga,
    Sampayutta,
    Vippayutta,
    Atthi,
    Natthi,
    Vigata,
    Avigata,
}

impl PatthanaRelation {
    pub const ALL: [PatthanaRelation; 24] = [
        Self::Hetu,
        Self::Arammana,
        Self::Adhipati,
        Self::Anantara,
        Self::Samanantara,
        Self::Sahajata,
        Self::Annamanna,
        Self::Nissaya,
        Self::Upanissaya,
        Self::Purejata,
        Self::Pacchajata,
        Self::Asevana,
        Self::Kamma,
        Self::Vipaka,
        Self::Ahara,
        Self::Indriya,
        Self::Jhana,
        Self::Magga,
        Self::Sampayutta,
        Self::Vippayutta,
        Self::Atthi,
        Self::Natthi,
        Self::Vigata,
        Self::Avigata,
    ];

    /// How a walker may use this relation.
    ///
    /// Directed relations follow doc 08's operators that have a before/after
    /// or actor/result arrow. Mutual relations are the ones doc 08 describes
    /// as simultaneous and symmetric. Everything else qualifies a state
    /// (budget, faculty, presence, absence, repetition) and is not a neighbor
    /// walk. This split is an engineering reading of §2.1, not a quoted
    /// commentary table.
    pub fn walk(self) -> Walk {
        match self {
            Self::Hetu
            | Self::Arammana
            | Self::Adhipati
            | Self::Anantara
            | Self::Samanantara
            | Self::Upanissaya
            | Self::Purejata
            | Self::Pacchajata
            | Self::Kamma
            | Self::Vipaka
            | Self::Magga => Walk::Directed,
            Self::Sahajata | Self::Annamanna | Self::Sampayutta => Walk::Mutual,
            Self::Nissaya
            | Self::Asevana
            | Self::Ahara
            | Self::Indriya
            | Self::Jhana
            | Self::Vippayutta
            | Self::Atthi
            | Self::Natthi
            | Self::Vigata
            | Self::Avigata => Walk::Qualifier,
        }
    }

    /// The four root conditions named in doc 08 §2.2. Other relations return
    /// `None` because the reduction table is not in the source.
    pub fn root(self) -> Option<RootCondition> {
        match self {
            Self::Arammana => Some(RootCondition::Object),
            Self::Upanissaya => Some(RootCondition::DecisiveSupport),
            Self::Kamma => Some(RootCondition::Kamma),
            Self::Atthi => Some(RootCondition::Presence),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hetu => "hetu",
            Self::Arammana => "arammana",
            Self::Adhipati => "adhipati",
            Self::Anantara => "anantara",
            Self::Samanantara => "samanantara",
            Self::Sahajata => "sahajata",
            Self::Annamanna => "annamanna",
            Self::Nissaya => "nissaya",
            Self::Upanissaya => "upanissaya",
            Self::Purejata => "purejata",
            Self::Pacchajata => "pacchajata",
            Self::Asevana => "asevana",
            Self::Kamma => "kamma",
            Self::Vipaka => "vipaka",
            Self::Ahara => "ahara",
            Self::Indriya => "indriya",
            Self::Jhana => "jhana",
            Self::Magga => "magga",
            Self::Sampayutta => "sampayutta",
            Self::Vippayutta => "vippayutta",
            Self::Atthi => "atthi",
            Self::Natthi => "natthi",
            Self::Vigata => "vigata",
            Self::Avigata => "avigata",
        }
    }
}

/// Neighbor-walk permission for a typed relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Walk {
    /// `from` conditions `to`. The reverse arrow is a different relation
    /// (kamma / vipāka, explains / explained_by), never a copy of this label.
    Directed,
    /// Both endpoints condition each other under the same relation.
    Mutual,
    /// Recorded on a state or an existing edge. Not expanded as a neighbor.
    Qualifier,
}

/// The four root conditions of doc 08 §2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootCondition {
    /// Ārammaṇa. Current object or query.
    Object,
    /// Upanissaya. Long-term prior or attractor.
    DecisiveSupport,
    /// Kamma. A volitional action or tool call.
    Kamma,
    /// Atthi. Present in the working set.
    Presence,
}

/// Witnesses for one state transition. A missing root blocks the transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootWitnesses {
    pub object: bool,
    pub decisive_support: bool,
    pub kamma: bool,
    pub presence: bool,
}

impl RootWitnesses {
    pub fn missing(self) -> Option<RootCondition> {
        if !self.object {
            Some(RootCondition::Object)
        } else if !self.decisive_support {
            Some(RootCondition::DecisiveSupport)
        } else if !self.kamma {
            Some(RootCondition::Kamma)
        } else if !self.presence {
            Some(RootCondition::Presence)
        } else {
            None
        }
    }
}

/// Why a proposed causal commit is held instead of applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// Doc 08 §2.2: a transition with any empty root does not fire.
    MissingRoot(RootCondition),
    /// Doc 18 §7.2.3: a goal–observation edge with no sensory record.
    UnanchoredGoalObservation,
    /// Plan §4.3 / Addendum: contradictory clusters stay apart.
    ContradictionHeldApart,
    /// Addendum §A.1.1: decay does not delete text.
    DecayWouldEraseText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consensus {
    Admit,
    Hold(HoldReason),
}

/// Admit a cognitive state transition only when all four roots are present.
pub fn transition_consensus(witnesses: RootWitnesses) -> Consensus {
    match witnesses.missing() {
        Some(root) => Consensus::Hold(HoldReason::MissingRoot(root)),
        None => Consensus::Admit,
    }
}

/// A goal–observation proposal. Contradiction is held apart even when anchored.
pub fn goal_observation_consensus(anchored_to_observation: bool, contradiction: bool) -> Consensus {
    if contradiction {
        Consensus::Hold(HoldReason::ContradictionHeldApart)
    } else if !anchored_to_observation {
        Consensus::Hold(HoldReason::UnanchoredGoalObservation)
    } else {
        Consensus::Admit
    }
}

/// Addendum §A.1.1. Time, jarā, and vigata change rank. They do not delete text.
/// Text leaves only through a deliberate upekkhā or nirodha, which the overlay
/// already records as an operator approval. This function does not grant that.
pub fn decay_may_erase_text() -> bool {
    false
}

/// A bounded miss or an inconclusive inspection records what was checked.
/// It does not corroborate a cause and it does not rule one out.
/// `Found` inside the named scope can. An observation with no scoped search
/// can. The caller still excludes the unexplained target itself: citing the
/// report a hypothesis is trying to explain is not independent evidence.
pub fn observation_can_change_hypothesis_status(report: &ObservationReport) -> bool {
    match report.scoped_search.as_ref().map(|search| search.result) {
        Some(ScopedSearchResult::NotSeenInScope) | Some(ScopedSearchResult::Inconclusive) => false,
        Some(ScopedSearchResult::Found) | None => true,
    }
}

/// Relations that may connect an observation to a hypothesis as evidence.
///
/// Presence in `evidence_refs` is not entailment. One of these relations must
/// already link the observation to the hypothesis or to its target episode.
/// Legacy labels and the links a transition itself writes (`supported_by`,
/// `disconfirmed_by`) are not in this set.
pub fn relation_authorizes_evidence(relation: &str) -> bool {
    matches!(
        parse_relation(relation),
        Some(
            PatthanaRelation::Vipaka
                | PatthanaRelation::Kamma
                | PatthanaRelation::Purejata
                | PatthanaRelation::Pacchajata
                | PatthanaRelation::Upanissaya
        )
    )
}

/// True when an authorizing relation already joins `evidence_id` to the
/// hypothesis event or the target episode.
pub fn evidence_has_authorizing_link(
    links: &BTreeMap<String, Vec<(String, String)>>,
    evidence_id: &str,
    hypothesis_event_id: &str,
    target_episode_id: &str,
) -> bool {
    let anchors = [evidence_id, hypothesis_event_id, target_episode_id];
    for from in anchors {
        let Some(outgoing) = links.get(from) else {
            continue;
        };
        for (to, relation) in outgoing {
            if !relation_authorizes_evidence(relation) {
                continue;
            }
            let touches_evidence = from == evidence_id || to == evidence_id;
            let touches_claim = from == hypothesis_event_id
                || to == hypothesis_event_id
                || from == target_episode_id
                || to == target_episode_id;
            if touches_evidence && touches_claim && from != to {
                return true;
            }
        }
    }
    false
}

/// Parse a stored `ExplicitLink.relation` string. Legacy labels
/// (`investigates`, `followed_by`, `pertains_to`, `relates_to`) return `None`
/// so existing episode documents keep their current projection.
pub fn parse_relation(raw: &str) -> Option<PatthanaRelation> {
    let key = raw.trim().to_ascii_lowercase();
    PatthanaRelation::ALL
        .into_iter()
        .find(|relation| relation.as_str() == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twenty_four_distinct_relations_round_trip() {
        let mut names = std::collections::BTreeSet::new();
        for relation in PatthanaRelation::ALL {
            assert!(names.insert(relation.as_str()));
            let json = serde_json::to_string(&relation).unwrap();
            let back: PatthanaRelation = serde_json::from_str(&json).unwrap();
            assert_eq!(back, relation);
            assert_eq!(parse_relation(relation.as_str()), Some(relation));
        }
        assert_eq!(names.len(), 24);
    }

    #[test]
    fn only_the_four_named_roots_are_roots() {
        let roots: Vec<_> = PatthanaRelation::ALL.iter().filter_map(|r| r.root()).collect();
        assert_eq!(
            roots,
            vec![
                RootCondition::Object,
                RootCondition::DecisiveSupport,
                RootCondition::Kamma,
                RootCondition::Presence,
            ]
        );
    }

    #[test]
    fn directed_relations_are_not_mutual() {
        for relation in PatthanaRelation::ALL {
            match relation.walk() {
                Walk::Directed => {
                    assert_ne!(relation, PatthanaRelation::Annamanna);
                    assert_ne!(relation, PatthanaRelation::Sahajata);
                    assert_ne!(relation, PatthanaRelation::Sampayutta);
                }
                Walk::Mutual | Walk::Qualifier => {}
            }
        }
        assert_eq!(PatthanaRelation::Kamma.walk(), Walk::Directed);
        assert_eq!(PatthanaRelation::Vipaka.walk(), Walk::Directed);
        assert_eq!(PatthanaRelation::Pacchajata.walk(), Walk::Directed);
        assert_eq!(PatthanaRelation::Annamanna.walk(), Walk::Mutual);
        assert_eq!(PatthanaRelation::Vigata.walk(), Walk::Qualifier);
    }

    #[test]
    fn legacy_link_labels_stay_untyped() {
        for label in ["investigates", "followed_by", "pertains_to", "relates_to", ""] {
            assert_eq!(parse_relation(label), None);
        }
    }

    #[test]
    fn transition_fails_closed_when_any_root_is_missing() {
        let full = RootWitnesses {
            object: true,
            decisive_support: true,
            kamma: true,
            presence: true,
        };
        assert_eq!(transition_consensus(full), Consensus::Admit);

        let mut missing_object = full;
        missing_object.object = false;
        assert_eq!(
            transition_consensus(missing_object),
            Consensus::Hold(HoldReason::MissingRoot(RootCondition::Object))
        );

        let mut missing_presence = full;
        missing_presence.presence = false;
        assert_eq!(
            transition_consensus(missing_presence),
            Consensus::Hold(HoldReason::MissingRoot(RootCondition::Presence))
        );
    }

    #[test]
    fn goal_observation_holds_contradiction_and_unanchored_edges() {
        assert_eq!(
            goal_observation_consensus(true, false),
            Consensus::Admit
        );
        assert_eq!(
            goal_observation_consensus(false, false),
            Consensus::Hold(HoldReason::UnanchoredGoalObservation)
        );
        assert_eq!(
            goal_observation_consensus(true, true),
            Consensus::Hold(HoldReason::ContradictionHeldApart)
        );
    }

    #[test]
    fn scoped_miss_cannot_change_hypothesis_status() {
        let mut report = ObservationReport {
            report_id: "r".to_string(),
            event_time: None,
            ingested_at: 1,
            modality: crate::model::SensoryModality::Visual,
            location: "under couch".to_string(),
            task_context: "search".to_string(),
            content: "nothing there".to_string(),
            scoped_search: Some(crate::model::ScopedSearchReport {
                scope: "under-couch-only".to_string(),
                result: ScopedSearchResult::NotSeenInScope,
            }),
            source_actor: "test".to_string(),
            is_unresolved: true,
            tags: vec![],
        };
        assert!(!observation_can_change_hypothesis_status(&report));
        report.scoped_search.as_mut().unwrap().result = ScopedSearchResult::Inconclusive;
        assert!(!observation_can_change_hypothesis_status(&report));
        report.scoped_search.as_mut().unwrap().result = ScopedSearchResult::Found;
        assert!(observation_can_change_hypothesis_status(&report));
        report.scoped_search = None;
        assert!(observation_can_change_hypothesis_status(&report));
    }

    #[test]
    fn unlinked_observation_does_not_authorize_and_vipaka_does() {
        let mut links = BTreeMap::new();
        assert!(!evidence_has_authorizing_link(
            &links, "obs", "hyp", "target"
        ));
        links.insert(
            "obs".to_string(),
            vec![("hyp".to_string(), "followed_by".to_string())],
        );
        assert!(
            !evidence_has_authorizing_link(&links, "obs", "hyp", "target"),
            "a legacy label is not an evidence link"
        );
        links.insert(
            "obs".to_string(),
            vec![("hyp".to_string(), "vipaka".to_string())],
        );
        assert!(evidence_has_authorizing_link(
            &links, "obs", "hyp", "target"
        ));
        assert!(!relation_authorizes_evidence("supported_by"));
        assert!(!relation_authorizes_evidence("vigata"));
    }

    #[test]
    fn decay_does_not_erase_text() {
        assert!(!decay_may_erase_text());
        assert_eq!(PatthanaRelation::Vigata.walk(), Walk::Qualifier);
    }
}
