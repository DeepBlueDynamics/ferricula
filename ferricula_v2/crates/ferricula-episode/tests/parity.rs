//! Independent Parity test suite for the `episode-component` prototype.
//!
//! Read-only vs the prototype's implementation: this file tests the **public
//! contract** enumerated by the clatter contract review (research/
//! clatter-contract-review.md) and the Coordinator's test list. It must be
//! **independently falsifiable** — each case asserts a concrete invariant
//! that can FAIL if the implementation is wrong; none is a fixture constant.
//!
//! No production claims: these tests validate mechanics (identity dedup,
//! evidential typing, fail-closed reopen, transition legality without
//! mutation, hard caps, mode invariants, read/reopen parity). They do NOT
//! assert that an interpretation is "correct"/"true" — interpretation
//! selection is only checked structurally (status policy, coexistence,
//! no retroactive edit). No hardcoded "vape" answer is asserted as a result.
//!
//! Posture vs in-flight Engine fixes: the review (clatter-contract-review.md
//! Retrieval addendum B1–B4) names defects the Engine is fixing (origin
//! attribution, global budget bounds, keying indices, lexical-domain
//! coverage). Cases below assert the CONTRACT; where the current revision is
//! known mid-fix, the assertion documents the intended invariant and may be
//! red until the fix lands — that red is the point (falsifiable).

use std::collections::BTreeSet;

use ferricula_episode::*;
use ferricula_episode::memory_overlay::{OverlayConfig, OverlayLog, OverlayPayload};
use tempfile::tempdir;

fn obs(report_id: &str, location: &str, content: &str, unresolved: bool) -> ObservationReport {
    ObservationReport {
        report_id: report_id.to_string(),
        event_time: None,
        ingested_at: 1000,
        modality: SensoryModality::Text,
        location: location.to_string(),
        task_context: "test".to_string(),
        content: content.to_string(),
        scoped_search: None,
        source_actor: "parity".to_string(),
        is_unresolved: unresolved,
        tags: vec![],
    }
}

fn goal(goal_id: &str, entity: &str) -> GoalReport {
    GoalReport {
        goal_id: goal_id.to_string(),
        created_at: 2000,
        target_entity: entity.to_string(),
        description: format!("find {entity}"),
        tags: vec![],
    }
}

fn hyp(hyp_id: &str, target: &str, claim: &str) -> HypothesisProposal {
    HypothesisProposal {
        hypothesis_id: hyp_id.to_string(),
        target_episode_id: target.to_string(),
        claim: claim.to_string(),
        status: HypothesisStatus::Candidate,
        support_refs: BTreeSet::new(),
        against_refs: BTreeSet::new(),
        proposed_at: 3000,
    }
}

fn query(
    projection: &EpisodeProjection,
    q: &str,
    mode: RetrievalMode,
    limit: Option<usize>,
    max_links: Option<usize>,
    max_explore: Option<usize>,
    seed: Option<u64>,
) -> EpisodeQueryResponse {
    // New API fields default to None (Engine's B1/B2 defaults apply).
    query_episodes(
        &EpisodeQueryRequest {
            query: q.to_string(),
            mode,
            limit,
            seed,
            max_links_per_hit: max_links,
            max_explore_candidates: max_explore,
            max_expansion_seeds: None,
            candidate_cap: None,
        },
        projection,
    )
}

/// Full-option query for the new budget/seed/cap fields.
fn query_full(
    projection: &EpisodeProjection,
    q: &str,
    mode: RetrievalMode,
    limit: Option<usize>,
    max_links: Option<usize>,
    max_explore: Option<usize>,
    max_seeds: Option<usize>,
    candidate_cap: Option<usize>,
    seed: Option<u64>,
) -> EpisodeQueryResponse {
    query_episodes(
        &EpisodeQueryRequest {
            query: q.to_string(),
            mode,
            limit,
            seed,
            max_links_per_hit: max_links,
            max_explore_candidates: max_explore,
            max_expansion_seeds: max_seeds,
            candidate_cap: candidate_cap,
        },
        projection,
    )
}

// ---------------------------------------------------------------------------
// 1. Duplicate IDs cannot overwrite
// ---------------------------------------------------------------------------

#[test]
fn duplicate_report_id_cannot_overwrite_observation() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    a.commit(EpisodeItem::Observation(obs("R1", "couch", "clatter", true)), 1000).unwrap();

    // Same report_id with DIFFERENT content: must be rejected, not overwrite.
    let err = a
        .commit(
            EpisodeItem::Observation(obs("R1", "kitchen", "entirely different", true)),
            2000,
        )
        .unwrap_err();
    assert!(err.to_string().contains("duplicate report_id"), "got: {err}");

    // The original is still present with its original content via projection.
    let proj = a.projection();
    assert_eq!(proj.observations.len(), 1, "no second observation added");
    let stored = proj.observations.values().next().unwrap();
    assert_eq!(stored.report.report_id, "R1");
    assert_eq!(stored.report.content, "clatter", "original content preserved, not overwritten");
}

#[test]
fn duplicate_goal_id_and_hypothesis_id_rejected() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();

    a.commit(EpisodeItem::Goal(goal("G1", "vape")), 1000).unwrap();
    let err_g = a
        .commit(EpisodeItem::Goal(goal("G1", "other")), 2000)
        .unwrap_err();
    assert!(err_g.to_string().contains("duplicate goal_id"), "got: {err_g}");

    let clatter_id = a
        .commit(EpisodeItem::Observation(obs("R2", "couch", "clatter", true)), 1000)
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H1", &clatter_id, "clatter is the vape")), 1000)
        .unwrap();
    let err_h = a
        .commit(EpisodeItem::Hypothesis(hyp("H1", &clatter_id, "different claim")), 2000)
        .unwrap_err();
    assert!(err_h.to_string().contains("duplicate hypothesis_id"), "got: {err_h}");
}

// ---------------------------------------------------------------------------
// 2. Support evidence must be an observation — never self or a hypothesis
// ---------------------------------------------------------------------------

#[test]
fn support_ref_must_be_observation_not_hypothesis_or_self() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();

    let clatter_id = a
        .commit(EpisodeItem::Observation(obs("R3", "couch", "clatter sound", true)), 1000)
        .unwrap();

    // A first hypothesis to have a hypothesis event id.
    let h1_id = a
        .commit(EpisodeItem::Hypothesis(hyp("H-A", &clatter_id, "cat did it")), 1000)
        .unwrap();

    // Support ref pointing at a HYPOTHESIS event id is an evidential-typing
    // violation: support must be an observation report, not a hypothesis.
    let mut bad = hyp("H-B", &clatter_id, "vape did it");
    bad.support_refs = BTreeSet::from([h1_id.clone()]);
    let err = a.commit(EpisodeItem::Hypothesis(bad), 2000).unwrap_err();
    assert!(
        err.to_string().contains("ObservationReport"),
        "hypothesis-as-support must be rejected; got: {err}"
    );

    // Self reference in support_refs is rejected.
    let mut self_ref = hyp("H-C", &clatter_id, "self-asserted");
    self_ref.support_refs = BTreeSet::from(["H-C".to_string()]);
    let err_s = a.commit(EpisodeItem::Hypothesis(self_ref), 2000).unwrap_err();
    assert!(
        err_s.to_string().contains("cannot point to hypothesis itself"),
        "self-support must be rejected; got: {err_s}"
    );

    // Same for against_refs: must be observation, not hypothesis/self.
    let mut bad_against = hyp("H-D", &clatter_id, "countered");
    bad_against.against_refs = BTreeSet::from([h1_id.clone()]);
    let err_a = a.commit(EpisodeItem::Hypothesis(bad_against), 2000).unwrap_err();
    assert!(
        err_a.to_string().contains("ObservationReport"),
        "hypothesis-as-against must be rejected; got: {err_a}"
    );
}

#[test]
fn support_and_against_deduplicate_by_identity_not_encounter() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let id = a
        .commit(EpisodeItem::Observation(obs("R4", "couch", "clatter", true)), 1000)
        .unwrap();
    let support_obs = a
        .commit(
            EpisodeItem::Observation(obs("R5", "corner", "found object", false)),
            1000,
        )
        .unwrap();

    let mut h = hyp("H-E", &id, "explained");
    // Same observation referenced (as a set) collapses to one distinct ref.
    h.support_refs = BTreeSet::from([support_obs.clone(), support_obs.clone()]);
    a.commit(EpisodeItem::Hypothesis(h), 1000).unwrap();

    let stored = a.projection().hypotheses.values().find(|h| h.proposal.hypothesis_id == "H-E").unwrap();
    assert_eq!(stored.current_support_refs.len(), 1, "dedup by identity, not count");
    assert!(stored.current_support_refs.contains(&support_obs));
}

// ---------------------------------------------------------------------------
// 3. Malformed episode_v1 re-open fails (fail-closed)
// ---------------------------------------------------------------------------

#[test]
fn malformed_episode_v1_reopen_fails_closed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("m.json");

    // Build an overlay log directly with an episode_v1 event whose payload
    // JSON is NOT a valid EpisodeItem. Append allows free-form text; the
    // episode fold must reject it on reopen.
    let mut log = OverlayLog::new(OverlayConfig::default()).unwrap();
    log.append(
        OverlayPayload::ProposeMemory {
            text: "{ this is not valid episode json".to_string(),
            channel: EPISODE_CHANNEL_V1.to_string(),
            importance: 0.5,
            keystone_proposed: false,
            tags: vec![],
        },
        1000,
    )
    .unwrap();
    log.save(&path).unwrap();

    let err = EpisodeAdapter::open(&path).unwrap_err();
    assert!(
        err.to_string().contains("failed to deserialize")
            || err.to_string().contains("EpisodeItem"),
        "malformed episode_v1 must fail fold-closed; got: {err}"
    );
}

// ---------------------------------------------------------------------------
// 4. Unresolved clatter without any initial explanation is preserved
// ---------------------------------------------------------------------------

#[test]
fn unresolved_without_explanation_preserved_and_queried() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let id = a
        .commit(
            EpisodeItem::Observation(obs("R6", "couch", "unexplained plastic clatter", true)),
            1000,
        )
        .unwrap();

    assert!(a.projection().unresolved_episodes.contains(&id), "flagged unresolved");

    // Explore mode must surface it (only eligible unresolved episode).
    let resp = query(a.projection(), "plastic", RetrievalMode::Explore, Some(5), None, Some(16), Some(1));
    assert!(
        resp.hits.iter().any(|b| b.event_id == id),
        "unresolved episode must be sampled by Explore"
    );
    // Original observation content intact, no interpretation invented.
    let b = resp.hits.iter().find(|b| b.event_id == id).unwrap();
    assert_eq!(b.original_observation.content, "unexplained plastic clatter");
    assert!(b.current_interpretation.is_none(), "no hypothesis => none, not a fabricated answer");
}

// ---------------------------------------------------------------------------
// 5. All hypotheses disconfirmed => episode remains unexplained
// ---------------------------------------------------------------------------

#[test]
fn all_disconfirmed_remains_unexplained() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let id = a
        .commit(
            EpisodeItem::Observation(obs("R7", "couch", "mystery clatter", true)),
            1000,
        )
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H-F", &id, "cat")), 1000).unwrap();

    // Disconfirming evidence must be an observation.
    let counter = a
        .commit(
            EpisodeItem::Observation(obs("R8", "bedroom", "cat asleep elsewhere", false)),
            1000,
        )
        .unwrap();
    authorize_evidence(&mut a, &counter, "H-F", 2000);
    a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H-F".to_string(),
            transition: HypothesisTransition::Disconfirm,
            evidence_refs: BTreeSet::from([counter]),
            rationale: "cat elsewhere during clatter".to_string(),
            transition_time: 2000,
        }),
        2000,
    )
    .unwrap();

    // Sole hypothesis disconfirmed => episode stays unresolved (contract 3,
    // projection.rs `recompute_episode_resolution`: disconfirming an
    // explanation does not resolve the original uncertainty).
    assert!(
        a.projection().unresolved_episodes.contains(&id),
        "all-disconfirmed must remain unexplained"
    );
}

// ---------------------------------------------------------------------------
// 6. Plain mode has NO linked context
// ---------------------------------------------------------------------------

#[test]
fn plain_mode_has_no_linked_context() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let id = a
        .commit(EpisodeItem::Observation(obs("R9", "couch", "clatter sound", true)), 1000)
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H-G", &id, "explanation")), 1000).unwrap();

    let resp = query(a.projection(), "clatter sound", RetrievalMode::Plain, Some(5), None, None, None);
    assert_eq!(resp.hits.len(), 1);
    assert!(
        resp.hits[0].linked_context.is_empty(),
        "Plain mode invariant: linked_context must be strictly empty"
    );
    // Linked mode exposes it; Plain never does.
    let linked = query(a.projection(), "clatter sound", RetrievalMode::Linked, Some(5), None, None, None);
    assert!(
        linked.hits[0].linked_context.len() >= 1,
        "Linked mode must expose 1-hop context"
    );
}

// ---------------------------------------------------------------------------
// 7. Hard caps survive hostile usize inputs
// ---------------------------------------------------------------------------

#[test]
fn hard_caps_survive_usize_max_inputs() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    for i in 0..12u32 {
        a.commit(
            EpisodeItem::Observation(obs(&format!("RB{i}"), "room", "shared token noise", false)),
            1000,
        )
        .unwrap();
    }

    let resp = query(
        a.projection(),
        "shared token",
        RetrievalMode::Plain,
        Some(usize::MAX),
        Some(usize::MAX),
        Some(usize::MAX),
        None,
    );
    assert!(
        resp.hits.len() <= MAX_QUERY_LIMIT,
        "limit clamp: got {} (max {MAX_QUERY_LIMIT})",
        resp.hits.len()
    );

    let explore = query(
        a.projection(),
        "zzz-nonexistent",
        RetrievalMode::Explore,
        Some(usize::MAX),
        Some(usize::MAX),
        Some(usize::MAX),
        Some(7),
    );
    assert!(
        explore.hits.len() <= MAX_QUERY_LIMIT,
        "explore limit clamp: got {}",
        explore.hits.len()
    );
    // No panic is the other half: usize::MAX must not overflow length math.
}

// ---------------------------------------------------------------------------
// 8. Multiple hypotheses coexist; selection is structural, not truth
// ---------------------------------------------------------------------------

#[test]
fn multiple_hypotheses_coexist_with_primary_and_alternatives() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let id = a
        .commit(
            EpisodeItem::Observation(obs("R10", "couch", "clatter sound", true)),
            1000,
        )
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H1", &id, "cat")), 1000).unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H2", &id, "book")), 1000).unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H3", &id, "vape")), 1000).unwrap();

    let resp = query(a.projection(), "clatter sound", RetrievalMode::Linked, Some(5), None, None, None);
    assert_eq!(resp.hits.len(), 1);
    let b = &resp.hits[0];
    // One primary + two alternatives = all three coexisting hypotheses.
    assert!(b.current_interpretation.is_some(), "one primary");
    assert_eq!(b.alternative_interpretations.len(), 2, "two alternatives preserved");
    // No hypothesis is asserted TRUE; coexistence is the check.
}

#[test]
fn invalid_transition_does_not_mutate_status() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let id = a
        .commit(EpisodeItem::Observation(obs("R11", "couch", "clatter", true)), 1000)
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H-X", &id, "cause")), 1000).unwrap();
    let ev = a
        .commit(
            EpisodeItem::Observation(obs("R12", "corner", "found thing", false)),
            1000,
        )
        .unwrap();
    // Support first (valid: Candidate -> Supported).
    authorize_evidence(&mut a, &ev, "H-X", 2000);
    a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H-X".to_string(),
            transition: HypothesisTransition::Support,
            evidence_refs: BTreeSet::from([ev.clone()]),
            rationale: "found".to_string(),
            transition_time: 2000,
        }),
        2000,
    )
    .unwrap();
    assert_eq!(
        a.projection().hypotheses.get(&id_event(&a, "H-X")).unwrap().current_status,
        HypothesisStatus::Supported
    );

    // Supporting an already-Supported hypothesis is invalid: reject, no mutation.
    let err = a
        .commit(
            EpisodeItem::StatusTransition(StatusTransitionEvent {
                hypothesis_id: "H-X".to_string(),
                transition: HypothesisTransition::Support,
                evidence_refs: BTreeSet::from([ev.clone()]),
                rationale: "redundant".to_string(),
                transition_time: 3000,
            }),
            3000,
        )
        .unwrap_err();
    assert!(err.to_string().contains("invalid transition"), "got: {err}");
    assert_eq!(
        a.projection().hypotheses.get(&id_event(&a, "H-X")).unwrap().current_status,
        HypothesisStatus::Supported,
        "invalid transition must not change status"
    );
}

// ---------------------------------------------------------------------------
// 9. Read / reopen parity (durable, deterministic)
// ---------------------------------------------------------------------------

#[test]
fn read_reopen_parity_projection_and_query_identical() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("m.json");

    let first_projection;
    {
        let mut a = EpisodeAdapter::open(&path).unwrap();
        let id = a
            .commit(EpisodeItem::Observation(obs("R13", "couch", "clatter sound", true)), 1000)
            .unwrap();
        a.commit(EpisodeItem::Hypothesis(hyp("H-P", &id, "cause")), 1000).unwrap();
        first_projection = a.projection().clone();
    }

    let reopened = EpisodeAdapter::open(&path).unwrap();
    assert_eq!(
        reopened.projection().observations,
        first_projection.observations,
        "observations identical across reopen"
    );
    assert_eq!(
        reopened.projection().hypotheses,
        first_projection.hypotheses,
        "hypotheses identical across reopen"
    );
    assert_eq!(
        reopened.projection().unresolved_episodes,
        first_projection.unresolved_episodes,
        "unresolved registry identical across reopen"
    );

    // Query results byte-identical across reopen (deterministic).
    let q1 = query(&first_projection, "clatter sound", RetrievalMode::Linked, Some(5), None, None, None);
    let q2 = query(reopened.projection(), "clatter sound", RetrievalMode::Linked, Some(5), None, None, None);
    assert_eq!(q1.hits.len(), q2.hits.len());
    for (a_b, b_b) in q1.hits.iter().zip(q2.hits.iter()) {
        assert_eq!(a_b.event_id, b_b.event_id);
        assert_eq!(a_b.score, b_b.score);
        assert_eq!(a_b.current_interpretation.as_ref().map(|i| i.status), b_b.current_interpretation.as_ref().map(|i| i.status));
    }
}

// Small helper: resolve a user hypothesis id to its overlay event id from the
// projection's index (independent of the internal keying the review flagged B3).
fn id_event(a: &EpisodeAdapter, user_hyp_id: &str) -> String {
    a.projection()
        .hypothesis_id_to_event
        .get(user_hyp_id)
        .expect("hypothesis user id resolves to event id")
        .clone()
}

fn authorize_evidence(a: &mut EpisodeAdapter, evidence_id: &str, hyp_user_id: &str, now: u64) {
    let hyp_event = id_event(a, hyp_user_id);
    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: evidence_id.to_string(),
            to_id: hyp_event,
            relation: "vipaka".to_string(),
            weight: 1.0,
        }),
        now,
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// B1/B2 — FAILING against current code; contract-pinned until Engine lands.
// ---------------------------------------------------------------------------
// These assert the CONTRACT (clatter-contract-review B1/B2, episode-review-gates
// 5/7). They are intentionally red while the current `query.rs` backfill +
// merged-score behavior stands; they document the defect and flip green when
// Engine separates origin groups and makes exploration independent of the
// lexical-full limit.

#[test]
fn explore_must_sample_unresolved_even_when_lexical_fills_limit() {
    // Current code only explores when `expanded_candidates.len() < limit`
    // (query.rs backfill guard). If lexical candidates already fill the limit,
    // Explore silently degrades to Linked and never touches the unresolved
    // episode. Contract (B2 proposal): Explore ALWAYS samples eligible
    // unresolved episodes, independent of the limit.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();

    // Lexically-matched observation (will fill the limit as a lexical hit).
    let matched = a
        .commit(
            EpisodeItem::Observation(obs("R-X1", "couch", "plastic clatter noise", true)),
            1000,
        )
        .unwrap();
    // Unresolved observation that does NOT match the query terms, so it can
    // only be reached by exploration.
    let unresolved = a
        .commit(
            EpisodeItem::Observation(obs("R-X2", "garage", "unrelated mystery sound", true)),
            1000,
        )
        .unwrap();
    assert!(a.projection().unresolved_episodes.contains(&unresolved));

    let resp = query(
        a.projection(),
        "plastic clatter noise",
        RetrievalMode::Explore,
        Some(1), // limit 1 already saturated by the lexical hit
        None,
        Some(16),
        Some(1),
    );
    // Contract: Explore must surface the unresolved episode too, even though
    // the lexical list already filled the limit.
    assert!(
        resp.hits.iter().any(|b| b.event_id == unresolved),
        "Explore must sample unresolved even when lexical fills the limit (B2)"
    );
    assert!(
        resp.hits.iter().any(|b| b.event_id == matched),
        "lexical hit preserved"
    );
}

#[test]
fn exploration_budget_exhausted_reflects_eligible_minus_sampled() {
    // Current code sets `exploration_budget_exhausted = unresolved.len() > max_explore`
    // (pool comparison, query.rs). Contract (B2): the flag is true when
    // (eligible − sampled) > 0, where eligible = unresolved not already admitted.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();

    // 4 unresolved episodes, no lexical matches for the query.
    for i in 0..4u32 {
        a.commit(
            EpisodeItem::Observation(obs(&format!("RBX{i}"), "room", "odd sound", true)),
            1000,
        )
        .unwrap();
    }

    // max_explore = 2 < 4 eligible: two remain unsampled -> exhausted = true.
    let resp = query(
        a.projection(),
        "zzz-nonexistent",
        RetrievalMode::Explore,
        Some(10),
        None,
        Some(2),
        Some(5),
    );
    assert!(
        resp.exploration_budget_exhausted,
        "eligible(4) - sampled(2) > 0 must set exhaustion (B2)"
    );
    // Exact sampling count is also part of the contract: report, don't drop.
    assert!(resp.evaluated_candidates_count >= 4);
}

// ---------------------------------------------------------------------------
// B4 — FAILING against current code; goal-to-observation must exercise retrieval
// ---------------------------------------------------------------------------

#[test]
fn goal_token_does_not_admit_via_two_hop_chain() {
    // The missing-vape goal is the demo's cue. A goal-only token must NOT
    // pull the observation through goal -> hypothesis -> observation: that is
    // a TWO-hop chain (goal→hypothesis, hypothesis→observation), which is
    // beyond the one-hop budget. Only a DIRECT explicit goal->observation edge
    // is a genuine one-hop admission. Gate 7 / B4 with strict one-hop.
    //
    // Isolation: the hypothesis claim must NOT contain the query token, or it
    // becomes a legitimate direct lexical seed (B4) and reaches the
    // observation in one hop via its `explains` edge — that would not test the
    // goal chain.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();

    let clatter_id = a
        .commit(
            EpisodeItem::Observation(obs("R-Y1", "couch", "plastic clatter", true)),
            1000,
        )
        .unwrap();
    // Claim deliberately avoids "vape" so only the GOAL is a lexical seed.
    let hyp_id = a
        .commit(
            EpisodeItem::Hypothesis(hyp("H-Y1", &clatter_id, "a displaced object made the noise")),
            1000,
        )
        .unwrap();
    let goal_id = a
        .commit(EpisodeItem::Goal(goal("G-Y1", "vape")), 1000)
        .unwrap();
    // Explicit link: goal -> hypothesis (goal drives the search cue).
    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: goal_id.clone(),
            to_id: hyp_id.clone(),
            relation: "investigates".to_string(),
            weight: 1.0,
        }),
        1000,
    )
    .unwrap();

    // Query the GOAL cue ("vape"), which appears in NO observation content and
    // NO hypothesis claim — the goal is the ONLY lexical seed. The clatter
    // observation is 2 hops from the goal (goal→hyp→obs), so at a strict
    // one-hop budget it must NOT be admitted as a direct linked hit.
    let resp = query(a.projection(), "vape", RetrievalMode::Linked, Some(5), None, None, None);
    assert!(
        !resp.hits.iter().any(|b| b.event_id == clatter_id),
        "goal→hypothesis→observation is TWO hops; must not be admitted as one-hop (B4/one-hop)"
    );
}

#[test]
fn direct_goal_observation_edge_succeeds_one_hop() {
    // The legitimate one-hop goal case: an EXPLICIT goal -> observation link.
    // A goal-only token must admit the directly-linked observation.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let clatter_id = a
        .commit(
            EpisodeItem::Observation(obs("R-Y2", "couch", "plastic clatter", true)),
            1000,
        )
        .unwrap();
    let goal_id = a
        .commit(EpisodeItem::Goal(goal("G-Y2", "vape")), 1000)
        .unwrap();
    // DIRECT edge goal -> observation (not through a hypothesis).
    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: goal_id.clone(),
            to_id: clatter_id.clone(),
            relation: "pertains_to".to_string(),
            weight: 1.0,
        }),
        1000,
    )
    .unwrap();

    let resp = query_full(
        a.projection(),
        "vape",
        RetrievalMode::Linked,
        Some(5),
        Some(8),
        None,
        Some(200),
        Some(64),
        None,
    );
    let hit = resp
        .linked_hits
        .iter()
        .find(|b| b.event_id == clatter_id)
        .expect("direct goal→observation one-hop must be admitted");
    assert_eq!(hit.origin, CandidateOrigin::Linked);
}

#[test]
fn repeated_queries_do_not_mutate_projection_read_invariance() {
    // change the projection (no reinforcement, no status mutation, no growth
    // of the unresolved pool from a query).
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let id = a
        .commit(EpisodeItem::Observation(obs("R-Z1", "couch", "clatter", true)), 1000)
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H-Z1", &id, "cause")), 1000).unwrap();

    let before = a.projection().clone();
    for _ in 0..5 {
        query(a.projection(), "clatter", RetrievalMode::Explore, Some(5), None, Some(16), Some(1));
    }
    let after = a.projection().clone();
    assert_eq!(before.observations, after.observations, "observations unchanged by queries");
    assert_eq!(before.hypotheses, after.hypotheses, "hypotheses unchanged by queries");
    assert_eq!(before.unresolved_episodes, after.unresolved_episodes, "unresolved pool unchanged by queries");
}

// ---------------------------------------------------------------------------
// F6 — corrected: scoped NotSeenInScope alone must NOT mark unresolved.
// F7 — staged earlier query capture unchanged (no as-of-time API implied).
// F3 — failed commit leaves projection + log bytes unchanged.
// F4 — prior transition history retained (not overwritten).
// These use only the existing report/commit/history APIs; no new query fields.
// ---------------------------------------------------------------------------

fn obs_scoped(report_id: &str, is_unresolved: bool, result: ScopedSearchResult) -> ObservationReport {
    ObservationReport {
        report_id: report_id.to_string(),
        event_time: None,
        ingested_at: 1000,
        modality: SensoryModality::Visual,
        location: "couch".to_string(),
        task_context: "searching".to_string(),
        content: "scoped search observation".to_string(),
        scoped_search: Some(ScopedSearchReport {
            scope: "under-couch-only".to_string(),
            result,
        }),
        source_actor: "parity".to_string(),
        is_unresolved,
        tags: vec![],
    }
}

#[test]
fn scoped_notseen_not_unresolved_unless_explicit_flag() {
    // F6 corrected contract: `NotSeenInScope` on its own is a BOUNDED observation
    // and must NOT force the episode into the unresolved pool. The explicit
    // is_unresolved flag controls unresolved retention. A failed search can be
    // fully understood ("checked under couch, saw nothing — done").
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();

    // (a) is_unresolved=false + NotSeenInScope => stays OUT of unresolved pool.
    let bounded_id = a
        .commit(
            EpisodeItem::Observation(obs_scoped("R-F6a", false, ScopedSearchResult::NotSeenInScope)),
            1000,
        )
        .unwrap();
    assert!(
        !a.projection().unresolved_episodes.contains(&bounded_id),
        "NotSeenInScope with is_unresolved=false must remain a bounded, understood observation"
    );

    // (b) same scoped result but explicit is_unresolved=true => IS unresolved.
    let unresolved_id = a
        .commit(
            EpisodeItem::Observation(obs_scoped("R-F6b", true, ScopedSearchResult::NotSeenInScope)),
            1000,
        )
        .unwrap();
    assert!(
        a.projection().unresolved_episodes.contains(&unresolved_id),
        "explicit is_unresolved=true must mark the episode unresolved"
    );

    // (c) a Found elsewhere does not retroactively "resolve" the earlier bounded
    // scoped-negative — the earlier observation record is untouched.
    let _found_id = a
        .commit(
            EpisodeItem::Observation(obs_scoped("R-F6c", false, ScopedSearchResult::Found)),
            1000,
        )
        .unwrap();
    let stored = a.projection().observations.get(&bounded_id).unwrap();
    assert_eq!(
        stored.report.scoped_search.as_ref().unwrap().result,
        ScopedSearchResult::NotSeenInScope,
        "later Found elsewhere must not rewrite the earlier scoped-negative"
    );
}

#[test]
fn scoped_miss_cannot_support_or_disconfirm_a_cause() {
    // Live failure: "not under the couch" was treated as proof the vape was
    // not the clatter. A bounded miss is not disconfirming evidence, and the
    // unexplained report is not corroborating evidence.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let clatter = ObservationReport {
        report_id: "clatter".to_string(),
        event_time: None,
        ingested_at: 1000,
        modality: SensoryModality::Sound,
        location: "near couch".to_string(),
        task_context: "leaving".to_string(),
        content: "plastic clatter near the couch".to_string(),
        scoped_search: Some(ScopedSearchReport {
            scope: "under-couch-only".to_string(),
            result: ScopedSearchResult::NotSeenInScope,
        }),
        source_actor: "user".to_string(),
        is_unresolved: true,
        tags: vec![],
    };
    let clatter_id = a.commit(EpisodeItem::Observation(clatter), 1000).unwrap();
    a.commit(
        EpisodeItem::Hypothesis(hyp("H-vape", &clatter_id, "the kicked vape caused the clatter")),
        1000,
    )
    .unwrap();

    let before = a.projection().clone();
    let disconfirm = a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H-vape".to_string(),
            transition: HypothesisTransition::Disconfirm,
            evidence_refs: BTreeSet::from([clatter_id.clone()]),
            rationale: "not seen under the couch, so it was not the vape".to_string(),
            transition_time: 2000,
        }),
        2000,
    );
    assert!(disconfirm.is_err(), "scoped miss must not disconfirm");
    assert!(
        disconfirm.unwrap_err().to_string().contains("not evidence"),
        "rejection must name the evidence rule"
    );

    let support_from_target = a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H-vape".to_string(),
            transition: HypothesisTransition::Support,
            evidence_refs: BTreeSet::from([clatter_id.clone()]),
            rationale: "the clatter itself proves the vape".to_string(),
            transition_time: 2000,
        }),
        2000,
    );
    assert!(support_from_target.is_err(), "the unexplained report is not corroboration");

    let stored = a.projection().observations.get(&clatter_id).unwrap();
    assert_eq!(stored.report.content, "plastic clatter near the couch");
    assert_eq!(
        stored.report.scoped_search.as_ref().unwrap().result,
        ScopedSearchResult::NotSeenInScope
    );
    assert!(a.projection().unresolved_episodes.contains(&clatter_id));
    let hyp_event = a.projection().hypothesis_id_to_event.get("H-vape").unwrap();
    assert_eq!(
        a.projection().hypotheses.get(hyp_event).unwrap().current_status,
        HypothesisStatus::Candidate
    );
    assert_eq!(before.observations, a.projection().observations);

    let found_id = a
        .commit(
            EpisodeItem::Observation(obs("found", "far corner", "found the vape on the floor", false)),
            3000,
        )
        .unwrap();
    authorize_evidence(&mut a, &found_id, "H-vape", 3000);
    a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H-vape".to_string(),
            transition: HypothesisTransition::Support,
            evidence_refs: BTreeSet::from([found_id]),
            rationale: "a separate finding, not the miss under the couch".to_string(),
            transition_time: 3000,
        }),
        3000,
    )
    .unwrap();
    let stored = a.projection().observations.get(&clatter_id).unwrap();
    assert_eq!(stored.report.content, "plastic clatter near the couch");
    assert_eq!(
        stored.report.scoped_search.as_ref().unwrap().result,
        ScopedSearchResult::NotSeenInScope
    );
}

#[test]
fn typed_kamma_link_is_one_way_and_legacy_labels_stay_both_ways() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let from_id = a
        .commit(
            EpisodeItem::Observation(obs("src", "couch", "alpha source clatter", false)),
            1000,
        )
        .unwrap();
    let to_id = a
        .commit(
            EpisodeItem::Observation(obs("dst", "corner", "beta sink finding", false)),
            1000,
        )
        .unwrap();

    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: from_id.clone(),
            to_id: to_id.clone(),
            relation: "kamma".to_string(),
            weight: 1.0,
        }),
        1000,
    )
    .unwrap();
    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: from_id.clone(),
            to_id: to_id.clone(),
            relation: "followed_by".to_string(),
            weight: 1.0,
        }),
        1000,
    )
    .unwrap();
    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: from_id.clone(),
            to_id: to_id.clone(),
            relation: "vigata".to_string(),
            weight: 1.0,
        }),
        1000,
    )
    .unwrap();

    let links = &a.projection().links;
    let forward = links.get(&from_id).expect("source has outgoing links");
    assert!(forward.iter().any(|(id, rel)| id == &to_id && rel == "kamma"));
    assert!(forward.iter().any(|(id, rel)| id == &to_id && rel == "followed_by"));
    assert!(!forward.iter().any(|(_, rel)| rel == "vigata"));

    let back = links.get(&to_id).cloned().unwrap_or_default();
    assert!(!back.iter().any(|(id, rel)| id == &from_id && rel == "kamma"));
    assert!(back.iter().any(|(id, rel)| id == &from_id && rel == "followed_by"));
    assert!(!back.iter().any(|(_, rel)| rel == "vigata"));
}

#[test]
fn unlinked_observation_cannot_authorize_and_replay_cannot_bypass() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("m.json");
    let mut a = EpisodeAdapter::open(&path).unwrap();
    let clatter = a
        .commit(
            EpisodeItem::Observation(obs("clatter", "couch", "plastic clatter near the couch", true)),
            1000,
        )
        .unwrap();
    a.commit(
        EpisodeItem::Hypothesis(hyp("H", &clatter, "the vape caused the clatter")),
        1000,
    )
    .unwrap();
    let unrelated = a
        .commit(
            EpisodeItem::Observation(obs("weather", "window", "the weather is fine", false)),
            1000,
        )
        .unwrap();
    let miss = a
        .commit(
            EpisodeItem::Observation(obs_scoped("miss", true, ScopedSearchResult::NotSeenInScope)),
            1000,
        )
        .unwrap();

    let report_before = serde_json::to_vec(&a.projection().observations.get(&clatter).unwrap().report).unwrap();
    let file_before = std::fs::read(&path).unwrap();

    let rejected = a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H".to_string(),
            transition: HypothesisTransition::Support,
            evidence_refs: BTreeSet::from([unrelated.clone()]),
            rationale: "some other report exists".to_string(),
            transition_time: 2000,
        }),
        2000,
    );
    assert!(rejected.is_err(), "an unlinked observation must not establish a cause");
    assert!(rejected.unwrap_err().to_string().contains("not evidence"));
    assert_eq!(std::fs::read(&path).unwrap(), file_before);

    let same_report = a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H".to_string(),
            transition: HypothesisTransition::Disconfirm,
            evidence_refs: BTreeSet::from([clatter.clone()]),
            rationale: "the report disproves itself".to_string(),
            transition_time: 2000,
        }),
        2000,
    );
    assert!(same_report.is_err(), "the unexplained report cannot establish or rule out its own cause");

    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: unrelated.clone(),
            to_id: id_event(&a, "H"),
            relation: "followed_by".to_string(),
            weight: 1.0,
        }),
        2000,
    )
    .unwrap();
    let legacy = a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H".to_string(),
            transition: HypothesisTransition::Support,
            evidence_refs: BTreeSet::from([unrelated.clone()]),
            rationale: "a non-evidence label was attached".to_string(),
            transition_time: 2000,
        }),
        2000,
    );
    assert!(legacy.is_err(), "followed_by must not authorize a status change");

    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: miss.clone(),
            to_id: id_event(&a, "H"),
            relation: "vipaka".to_string(),
            weight: 1.0,
        }),
        2000,
    )
    .unwrap();
    let laundered = a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H".to_string(),
            transition: HypothesisTransition::Disconfirm,
            evidence_refs: BTreeSet::from([miss]),
            rationale: "not seen, therefore not the cause".to_string(),
            transition_time: 2000,
        }),
        2000,
    );
    assert!(laundered.is_err(), "a vipaka link must not launder a scoped miss");

    let sneaky_path = dir.path().join("sneaky.json");
    std::fs::copy(&path, &sneaky_path).unwrap();
    let mut raw = OverlayLog::load(&sneaky_path).unwrap();
    let forged = EpisodeItem::StatusTransition(StatusTransitionEvent {
        hypothesis_id: "H".to_string(),
        transition: HypothesisTransition::Support,
        evidence_refs: BTreeSet::from([unrelated.clone()]),
        rationale: "appended under the adapter".to_string(),
        transition_time: 3000,
    });
    raw.append(
        OverlayPayload::ProposeMemory {
            text: serde_json::to_string(&forged).unwrap(),
            channel: EPISODE_CHANNEL_V1.to_string(),
            importance: 0.5,
            keystone_proposed: false,
            tags: forged.summary_tags(),
        },
        3000,
    )
    .unwrap();
    raw.save(&sneaky_path).unwrap();
    let reopened = EpisodeAdapter::open(&sneaky_path);
    assert!(reopened.is_err(), "reload must reject a transition the adapter would reject");

    let kept = EpisodeAdapter::open(&path).unwrap();
    let report_after = serde_json::to_vec(&kept.projection().observations.get(&clatter).unwrap().report).unwrap();
    assert_eq!(report_before, report_after, "original report bytes survive the rejected transitions");
    let hyp_event = id_event(&kept, "H");
    assert_eq!(
        kept.projection().hypotheses.get(&hyp_event).unwrap().current_status,
        HypothesisStatus::Candidate
    );
}

#[test]
fn staged_earlier_query_capture_is_unchanged_by_later_commits() {
    // F7 — no as-of-time API implied: capture an earlier query RESPONSE, then
    // commit later events, and assert the earlier capture is byte-identical
    // (no Support delta, no later observation leaks into it).
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let id = a
        .commit(
            EpisodeItem::Observation(obs("R-F7a", "couch", "plastic clatter", true)),
            1000,
        )
        .unwrap();

    // Stage 1 capture: only the observation exists; no interpretation.
    let earlier = query(a.projection(), "clatter", RetrievalMode::Linked, Some(5), None, None, None);
    // Snapshot bytes of the earlier response for later comparison.
    let earlier_bytes = serde_json::to_vec(&earlier).unwrap();

    // Later commits: hypothesis + finding + Support transition.
    a.commit(EpisodeItem::Hypothesis(hyp("H-F7", &id, "may be the vape")), 1000).unwrap();
    let find_id = a
        .commit(
            EpisodeItem::Observation(obs("R-F7b", "corner", "found the vape", false)),
            1000,
        )
        .unwrap();
    authorize_evidence(&mut a, &find_id, "H-F7", 2000);
    a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H-F7".to_string(),
            transition: HypothesisTransition::Support,
            evidence_refs: BTreeSet::from([find_id]),
            rationale: "found".to_string(),
            transition_time: 2000,
        }),
        2000,
    )
    .unwrap();

    // Stage 1 capture is unchanged: no Support delta, no later finding leaked.
    let earlier_now = serde_json::to_vec(&earlier).unwrap();
    assert_eq!(earlier_bytes, earlier_now, "earlier staged query capture must be immutable");
    assert!(
        earlier.hits.iter().all(|b| b.evidential_delta.is_none()),
        "stage-1 capture must have no evidential delta from a future commit"
    );
}

#[test]
fn failed_commit_leaves_projection_and_log_bytes_unchanged() {
    // F3 — failed commit must leave BOTH in-memory projection and on-disk log
    // unchanged (gate 4 save semantics: no power-loss claim, but the promised
    // atomic save must not publish a partial side effect).
    let dir = tempdir().unwrap();
    let path = dir.path().join("m.json");
    let mut a = EpisodeAdapter::open(&path).unwrap();
    let id = a
        .commit(EpisodeItem::Observation(obs("R-F3a", "couch", "clatter", true)), 1000)
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H-F3", &id, "cause")), 1000).unwrap();

    // A valid Support first (so the FAILED second Support is the only delta).
    let ev = a
        .commit(
            EpisodeItem::Observation(obs("R-F3b", "corner", "found thing", false)),
            1000,
        )
        .unwrap();
    authorize_evidence(&mut a, &ev, "H-F3", 2000);
    a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H-F3".to_string(),
            transition: HypothesisTransition::Support,
            evidence_refs: BTreeSet::from([ev.clone()]),
            rationale: "found".to_string(),
            transition_time: 2000,
        }),
        2000,
    )
    .unwrap();

    // Establish the pre-failure projection + log bytes AFTER the valid commit.
    let proj_before = a.projection().clone();
    let log_bytes_before = std::fs::read(&path).unwrap();

    // Deliberately invalid commit (Support from already-Supported).
    let err = a
        .commit(
            EpisodeItem::StatusTransition(StatusTransitionEvent {
                hypothesis_id: "H-F3".to_string(),
                transition: HypothesisTransition::Support, // invalid: already Supported
                evidence_refs: BTreeSet::from([ev]),
                rationale: "redundant".to_string(),
                transition_time: 3000,
            }),
            3000,
        )
        .unwrap_err();
    assert!(err.to_string().contains("invalid transition"), "got: {err}");

    // In-memory projection unchanged by the FAILED commit.
    let proj_after = a.projection().clone();
    assert_eq!(proj_before.hypotheses, proj_after.hypotheses, "failed commit must not mutate projection");
    assert_eq!(proj_before.observations, proj_after.observations, "failed commit must not mutate observations");

    // On-disk log bytes unchanged by the failed commit.
    let log_bytes_after = std::fs::read(&path).unwrap();
    assert_eq!(log_bytes_before, log_bytes_after, "failed commit must not persist a partial event");

    // Reopen from disk reproduces the state (no leaked event).
    let reopened = EpisodeAdapter::open(&path).unwrap();
    assert_eq!(reopened.projection().hypotheses, proj_after.hypotheses, "reopen matches in-memory after failed commit");
}

#[test]
fn prior_transition_history_retained_after_supersede() {
    // F4 — a later StatusTransition adds history; it must NOT overwrite the
    // earlier transition entry (gate 6: preserve transition history; selection
    // of a primary hypothesis is explicit, not destructive re-write).
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let id = a
        .commit(EpisodeItem::Observation(obs("R-F4a", "couch", "clatter", true)), 1000)
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H-F4-1", &id, "cat did it")), 1000).unwrap();
    let ev = a
        .commit(
            EpisodeItem::Observation(obs("R-F4b", "corner", "found object", false)),
            1000,
        )
        .unwrap();
    authorize_evidence(&mut a, &ev, "H-F4-1", 2000);
    a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H-F4-1".to_string(),
            transition: HypothesisTransition::Support,
            evidence_refs: BTreeSet::from([ev.clone()]),
            rationale: "corroborated".to_string(),
            transition_time: 2000,
        }),
        2000,
    )
    .unwrap();

    // A superseding hypothesis + Supersede transition (evidence ref is
    // required by validation, so reuse the finding observation). Commit time
    // must be monotonic: H-F4-2 lands AFTER the Support event (2000).
    a.commit(EpisodeItem::Hypothesis(hyp("H-F4-2", &id, "book fell")), 2500).unwrap();
    a.commit(
        EpisodeItem::StatusTransition(StatusTransitionEvent {
            hypothesis_id: "H-F4-1".to_string(),
            transition: HypothesisTransition::Supersede {
                superseding_id: "H-F4-2".to_string(),
            },
            evidence_refs: BTreeSet::from([ev.clone()]),
            rationale: "more precise".to_string(),
            transition_time: 3000,
        }),
        3000,
    )
    .unwrap();

    // History has BOTH entries: Support (2000) then Supersede (3000), in order.
    let history = a.projection().status_history.get("H-F4-1").expect("history present");
    assert_eq!(history.len(), 2, "prior transition retained, not overwritten");
    assert_eq!(history[0].transition, HypothesisTransition::Support);
    assert_eq!(history[0].transition_time, 2000);
    assert_eq!(
        history[1].transition,
        HypothesisTransition::Supersede { superseding_id: "H-F4-2".to_string() }
    );
    assert_eq!(history[1].transition_time, 3000);
}

// ---------------------------------------------------------------------------
// New-API (B1/B2) cases — origin attribution, counters, zero-budget.
// These assert parity over the Engine's NEW response fields. Zero-budget
// cases are pinned red: current `clamp(1,..)` violates a Some(0) budget.
// ---------------------------------------------------------------------------

#[test]
fn per_origin_attribution_lexical_linked_explored() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();

    // One lexical observation, one linked-only observation, one unresolved
    // exploration target.
    let lexical = a
        .commit(EpisodeItem::Observation(obs("R-O1", "kitchen", "coffee cup on counter", false)), 1000)
        .unwrap();
    let linked_obs = a
        .commit(EpisodeItem::Observation(obs("R-O2", "desk", "notebook missing", true)), 1000)
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H-O1", &linked_obs, "linked cause")), 1000).unwrap();
    // An additional unresolved observation reachable only by exploration.
    let explored_obs = a
        .commit(EpisodeItem::Observation(obs("R-O3", "porch", "unrelated odd noise", true)), 1000)
        .unwrap();
    assert!(a.projection().unresolved_episodes.contains(&explored_obs));

    // Explore query for the coffee term: lexical hit; linked via hypothesis
    // seed on "notebook"; explored from unresolved pool.
    let resp = query_full(
        a.projection(),
        "coffee counter notebook",
        RetrievalMode::Explore,
        Some(10),
        Some(8),
        Some(16),
        Some(200),
        Some(64),
        Some(11),
    );

    // Separated arms: each bundle carries its origin.
    assert!(
        resp.lexical_hits.iter().all(|b| b.origin == CandidateOrigin::Lexical),
        "lexical_hits must all be Lexical"
    );
    assert!(
        resp.linked_hits.iter().all(|b| b.origin == CandidateOrigin::Linked),
        "linked_hits must all be Linked"
    );
    assert!(
        resp.explored_hits.iter().all(|b| b.origin == CandidateOrigin::Explored),
        "explored_hits must all be Explored"
    );

    // The lexical coffee observation is Lexical, not misattributed.
    assert!(
        resp.lexical_hits.iter().any(|b| b.event_id == lexical && b.origin == CandidateOrigin::Lexical),
        "lexical observation must be Lexical"
    );
    // Combined `hits` preserves the same origin tags (compat surface).
    for b in &resp.hits {
        assert!(
            matches!(b.origin, CandidateOrigin::Lexical | CandidateOrigin::Linked | CandidateOrigin::Explored),
            "combined hits must carry an origin"
        );
    }
}

#[test]
fn counters_eligible_and_sampled_unresolved_reflect_reality() {
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    for i in 0..5u32 {
        a.commit(
            EpisodeItem::Observation(obs(&format!("R-C{i}"), "room", "mystery noise", true)),
            1000,
        )
        .unwrap();
    }

    // max_explore=2 over 5 eligible unresolved: sampled=2, eligible=5,
    // exhausted true (eligible - sampled = 3 > 0).
    let resp = query_full(
        a.projection(),
        "zzz-nonexistent",
        RetrievalMode::Explore,
        Some(10),
        None,
        Some(2),
        Some(200),
        Some(64),
        Some(1),
    );
    assert_eq!(resp.eligible_unresolved_count, 5);
    assert_eq!(resp.sampled_unresolved_count, 2);
    assert!(resp.exploration_budget_exhausted);
}

#[test]
fn zero_budget_disables_respective_work() {
    // Pinned red against current code: `clamp(1,..)` promotes a Some(0)
    // budget to 1, so a zero budget does NOT disable the work. Contract (B2):
    // Some(0) for explore/links/seeds/candidate-cap must disable that arm.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let obs_id = a
        .commit(EpisodeItem::Observation(obs("R-Z0", "couch", "clatter", true)), 1000)
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H-Z0", &obs_id, "cause")), 1000).unwrap();

    // max_links=Some(0) must disable linked-context expansion.
    let resp = query_full(
        a.projection(),
        "clatter",
        RetrievalMode::Linked,
        Some(10),
        Some(0), // zero link budget
        None,
        Some(200),
        Some(0), // zero candidate cap
        None,
    );
    assert!(
        resp.linked_hits.is_empty(),
        "Some(0) max_links must disable linked work; current clamp(1,..) violates zero budget"
    );

    // max_explore_candidates=Some(0) must disable exploration sampling.
    let explore = query_full(
        a.projection(),
        "zzz-nonexistent",
        RetrievalMode::Explore,
        Some(10),
        None,
        Some(0), // zero explore budget
        Some(200),
        Some(64),
        Some(1),
    );
    assert!(
        explore.explored_hits.is_empty(),
        "Some(0) explore budget must disable sampling; current clamp(1,..) violates zero budget"
    );
    assert_eq!(explore.sampled_unresolved_count, 0);
}

#[test]
fn goal_cue_provenance_does_not_masquerade_direct_one_hop() {
    // A goal -> hypothesis -> observation path must NOT be reported as a
    // direct one-hop link goal->observation. The goal seed reaches the
    // observation through the hypothesis; the observation's linked_context
    // must show the HYPOTHESIS neighbor (LinkTargetKind::Hypothesis), not
    // claim the goal as a direct observation neighbor.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let clatter_id = a
        .commit(EpisodeItem::Observation(obs("R-P1", "couch", "plastic clatter", true)), 1000)
        .unwrap();
    let hyp_id = a
        .commit(EpisodeItem::Hypothesis(hyp("H-P1", &clatter_id, "a kicked object caused the noise")), 1000)
        .unwrap();
    let goal_id = a
        .commit(EpisodeItem::Goal(goal("G-P1", "vape")), 1000)
        .unwrap();
    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: goal_id.clone(),
            to_id: hyp_id.clone(),
            relation: "investigates".to_string(),
            weight: 1.0,
        }),
        1000,
    )
    .unwrap();

    // Query by the goal cue "vape" in Linked mode.
    let resp = query_full(
        a.projection(),
        "vape",
        RetrievalMode::Linked,
        Some(5),
        Some(8),
        None,
        Some(200),
        Some(64),
        None,
    );

    // A goal-only token must NOT admit the observation through the
    // goal→hypothesis→observation chain: that is TWO hops (goal→hyp,
    // hyp→obs), beyond the one-hop budget. Checking rendered neighbors alone
    // is insufficient — the candidate itself must not be admitted.
    assert!(
        !resp.linked_hits.iter().any(|b| b.event_id == clatter_id),
        "goal→hypothesis→observation is 2 hops; must not be admitted as one-hop linked"
    );

    // If the observation is somehow returned (lexical or otherwise), it must
    // be attributed as Linked, never masquerade as Lexical, and its rendered
    // linked context must show the hypothesis neighbor — never the goal as a
    // direct one-hop observation neighbor.
    if let Some(hit) = resp.hits.iter().find(|b| b.event_id == clatter_id) {
        assert!(
            !hit.linked_context.iter().any(|l| l.target_id == goal_id),
            "goal must not masquerade as a direct one-hop observation neighbor"
        );
        assert!(
            hit.linked_context.iter().any(|l| l.target_id == hyp_id && l.target_kind == LinkTargetKind::Hypothesis),
            "provenance must show the hypothesis neighbor, not a direct goal link"
        );
    }
}

// ---------------------------------------------------------------------------
// New-API provenance (Q-A/Q-B confirmed by Coordinator):
// - linked hits carry Some(via_seed_id) + Some(relation); lexical/explored None
// - actual_seed present in Explore response; default 42, byte-stable replay
// - UnresolvableTarget exclusion recorded for non-observation link endpoints
// ---------------------------------------------------------------------------

#[test]
fn provenance_via_seed_and_relation_per_origin() {
    // Q-A/Q-B confirmation: linked hits have Some(via_seed_id) + Some(relation)
    // while lexical/explored hits carry None for both. This is the parity case
    // I flagged in audit/magpie-tests.md §2 — asserted against the real fields.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();

    let lexical = a
        .commit(EpisodeItem::Observation(obs("R-V1", "kitchen", "coffee cup", false)), 1000)
        .unwrap();
    let linked_obs = a
        .commit(EpisodeItem::Observation(obs("R-V2", "desk", "spiral pad missing", true)), 1000)
        .unwrap();
    a.commit(EpisodeItem::Hypothesis(hyp("H-V1", &linked_obs, "linked cause")), 1000).unwrap();
    // An explicit direct goal->observation edge gives a genuine 1-hop linked hit.
    let goal_v = a
        .commit(EpisodeItem::Goal(goal("G-V1", "notebook")), 1000)
        .unwrap();
    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: goal_v.clone(),
            to_id: linked_obs.clone(),
            relation: "pertains_to".to_string(),
            weight: 1.0,
        }),
        1000,
    )
    .unwrap();

    let resp = query_full(
        a.projection(),
        "notebook coffee",
        RetrievalMode::Explore,
        Some(10),
        Some(8),
        Some(16),
        Some(200),
        Some(64),
        Some(42),
    );

    // Lexical hits: via_seed_id None, relation None.
    for b in &resp.lexical_hits {
        assert!(b.via_seed_id.is_none(), "lexical hit must have no via_seed_id");
        assert!(b.relation.is_none(), "lexical hit must have no relation");
    }
    // Linked hits (genuine one-hop): via_seed_id Some (the goal seed), relation Some.
    let linked = resp
        .linked_hits
        .iter()
        .find(|b| b.event_id == linked_obs)
        .expect("direct goal->observation must be a linked hit");
    assert!(linked.via_seed_id.is_some(), "linked hit must carry via_seed_id");
    assert!(linked.relation.is_some(), "linked hit must carry relation");
    assert_eq!(linked.origin, CandidateOrigin::Linked);
    // Explored hits: via_seed_id None, relation None.
    for b in &resp.explored_hits {
        assert!(b.via_seed_id.is_none(), "explored hit must have no via_seed_id");
        assert!(b.relation.is_none(), "explored hit must have no relation");
    }
}

#[test]
fn actual_seed_present_and_replay_is_byte_stable() {
    // Q-A: Explore sets Some(seed) (default 42); BTreeSet pool order + SimplePrng
    // is byte-stable across runs. Two identical requests with seed 42 give
    // identical explored hit ordering.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    for i in 0..6u32 {
        a.commit(
            EpisodeItem::Observation(obs(&format!("R-S{i}"), "room", "mystery noise", true)),
            1000,
        )
        .unwrap();
    }

    let r1 = query_full(
        a.projection(),
        "zzz-nonexistent",
        RetrievalMode::Explore,
        Some(10),
        None,
        Some(4),
        Some(200),
        Some(64),
        Some(42),
    );
    let r2 = query_full(
        a.projection(),
        "zzz-nonexistent",
        RetrievalMode::Explore,
        Some(10),
        None,
        Some(4),
        Some(200),
        Some(64),
        Some(42),
    );
    assert_eq!(r1.actual_seed, Some(42), "actual_seed records the seed used");
    let ids1: Vec<&str> = r1.explored_hits.iter().map(|b| b.event_id.as_str()).collect();
    let ids2: Vec<&str> = r2.explored_hits.iter().map(|b| b.event_id.as_str()).collect();
    assert_eq!(ids1, ids2, "seed=42 exploration is byte-stable (same order)");
}

#[test]
fn unresolvable_target_recorded_as_exclusion() {
    // Q-B: exclusions explicitly records UnresolvableTarget when a link points
    // at a non-observation endpoint (a genuine entity that is not an
    // observation, e.g. another Goal), rather than silently dropping it.
    let dir = tempdir().unwrap();
    let mut a = EpisodeAdapter::open(dir.path().join("m.json")).unwrap();
    let obs_x = a
        .commit(EpisodeItem::Observation(obs("R-U1", "couch", "clatter", true)), 1000)
        .unwrap();
    let goal_x = a
        .commit(EpisodeItem::Goal(goal("G-U1", "vape")), 1000)
        .unwrap();
    let goal_y = a
        .commit(EpisodeItem::Goal(goal("G-U2", "spare_goal")), 1000)
        .unwrap();

    // Valid 1-hop: goal -> observation.
    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: goal_x.clone(),
            to_id: obs_x.clone(),
            relation: "pertains_to".to_string(),
            weight: 1.0,
        }),
        1000,
    )
    .unwrap();
    // Valid but unresolvable-at-query: goal -> ANOTHER GOAL (a real non-
    // observation endpoint that exists in the log). At expansion, its
    // neighbor is not an observation, so it must be recorded as an exclusion,
    // not silently dropped (strict one-hop; goals do not hop a second edge).
    a.commit(
        EpisodeItem::Link(ExplicitLink {
            from_id: goal_x.clone(),
            to_id: goal_y.clone(),
            relation: "relates_to".to_string(),
            weight: 1.0,
        }),
        1000,
    )
    .unwrap();

    let resp = query_full(
        a.projection(),
        "vape",
        RetrievalMode::Linked,
        Some(5),
        Some(8),
        None,
        Some(200),
        Some(64),
        None,
    );
    // The observation is admitted (genuine goal->observation one-hop).
    assert!(
        resp.linked_hits.iter().any(|b| b.event_id == obs_x),
        "direct goal->observation must be admitted"
    );
    // The goal->goal neighbor is a real link endpoint but not an observation:
    // it must surface as an UnresolvableTarget exclusion, not vanish.
    assert!(
        resp.exclusions.iter().any(|e| {
            e.candidate_id == goal_y
                && e.reason == ExclusionReason::UnresolvableTarget
        }),
        "goal->goal neighbor must be excluded as UnresolvableTarget (not dropped)"
    );
}
