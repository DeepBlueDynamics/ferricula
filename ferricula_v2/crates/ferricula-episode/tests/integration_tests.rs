use std::collections::BTreeSet;
use ferricula_episode::*;
use tempfile::tempdir;

#[test]
fn test_clatter_scenario_lifecycle_and_modes() {
    let dir = tempdir().unwrap();
    let log_path = dir.path().join("memory-overlay.json");
    let mut adapter = EpisodeAdapter::open(&log_path).unwrap();

    // -------------------------------------------------------------------------
    // Step 1: Stage initial acoustic observation with scoped negative search
    // -------------------------------------------------------------------------
    let clatter_obs = ObservationReport {
        report_id: "rep-clatter-001".to_string(),
        event_time: Some(TimeSpec::Interval {
            start: 1789794000,
            end: 1789794100,
        }),
        ingested_at: 1789794120,
        modality: SensoryModality::Sound,
        location: "couch".to_string(),
        task_context: "dump_run_prep".to_string(),
        content: "heard sharp plastic clatter near couch after kicking something".to_string(),
        scoped_search: Some(ScopedSearchReport {
            scope: "under-couch-only".to_string(),
            result: ScopedSearchResult::NotSeenInScope,
        }),
        source_actor: "human_report".to_string(),
        is_unresolved: true,
        tags: vec!["noise".to_string(), "plastic".to_string()],
    };

    let clatter_event_id = adapter
        .commit(EpisodeItem::Observation(clatter_obs.clone()), 1789794120)
        .unwrap();

    // Verify unresolved registry caught the observation
    assert!(
        adapter
            .projection()
            .unresolved_episodes
            .contains(&clatter_event_id),
        "scoped search with NotSeenInScope or is_unresolved=true must be registered as unresolved"
    );

    // -------------------------------------------------------------------------
    // Step 2: Interleaved distractor task (cleaning kitchen counter)
    // -------------------------------------------------------------------------
    let distractor_obs = ObservationReport {
        report_id: "rep-distractor-002".to_string(),
        event_time: Some(TimeSpec::Exact { timestamp: 1789794500 }),
        ingested_at: 1789794510,
        modality: SensoryModality::Action,
        location: "kitchen".to_string(),
        task_context: "cleaning_counter".to_string(),
        content: "wiped kitchen counters and sorted recycling".to_string(),
        scoped_search: None,
        source_actor: "sensor".to_string(),
        is_unresolved: false,
        tags: vec!["kitchen".to_string(), "cleaning".to_string()],
    };

    let _distractor_event_id = adapter
        .commit(EpisodeItem::Observation(distractor_obs), 1789794510)
        .unwrap();

    // -------------------------------------------------------------------------
    // Step 3: User registers missing-object goal
    // -------------------------------------------------------------------------
    let vape_goal = GoalReport {
        goal_id: "goal-vape-003".to_string(),
        created_at: 1789795000,
        target_entity: "vape".to_string(),
        description: "locate missing vape before departure".to_string(),
        tags: vec!["lost_item".to_string(), "vape".to_string()],
    };

    let goal_event_id = adapter
        .commit(EpisodeItem::Goal(vape_goal), 1789795000)
        .unwrap();

    // -------------------------------------------------------------------------
    // Step 4: Compare Retrieval Modes Before Any Hypothesis Exists
    // -------------------------------------------------------------------------

    // A. Plain Mode: Query "vape"
    let plain_resp = query_episodes(
        &EpisodeQueryRequest {
            query: "vape".to_string(),
            mode: RetrievalMode::Plain,
            limit: Some(5),
            seed: None,
            max_links_per_hit: None,
            max_explore_candidates: None,
            max_expansion_seeds: None,
            candidate_cap: None,
        },
        adapter.projection(),
    );
    assert_eq!(
        plain_resp.hits.len(),
        0,
        "Plain mode cannot bridge distinct lexical domains without links"
    );

    // B. Linked Mode: Query "vape"
    let linked_resp = query_episodes(
        &EpisodeQueryRequest {
            query: "vape".to_string(),
            mode: RetrievalMode::Linked,
            limit: Some(5),
            seed: None,
            max_links_per_hit: None,
            max_explore_candidates: None,
            max_expansion_seeds: None,
            candidate_cap: None,
        },
        adapter.projection(),
    );
    assert_eq!(linked_resp.hits.len(), 0);

    // C. Explore Mode: Query "vape"
    let explore_resp = query_episodes(
        &EpisodeQueryRequest {
            query: "vape".to_string(),
            mode: RetrievalMode::Explore,
            limit: Some(5),
            seed: Some(42),
            max_links_per_hit: None,
            max_explore_candidates: Some(16),
            max_expansion_seeds: None,
            candidate_cap: None,
        },
        adapter.projection(),
    );
    assert_eq!(
        explore_resp.hits.len(),
        1,
        "Explore mode must sample the unresolved clatter episode"
    );
    assert_eq!(explore_resp.hits[0].event_id, clatter_event_id);
    assert_eq!(
        explore_resp.hits[0].original_observation.location, "couch"
    );

    // -------------------------------------------------------------------------
    // Step 5: Propose Candidate Hypothesis (Linking Clatter to Vape)
    // -------------------------------------------------------------------------
    let hypothesis = HypothesisProposal {
        hypothesis_id: "hyp-clatter-vape-004".to_string(),
        target_episode_id: clatter_event_id.clone(),
        claim: "plastic clatter near couch may have been the kicked vape".to_string(),
        status: HypothesisStatus::Candidate,
        support_refs: BTreeSet::from([clatter_event_id.clone()]),
        against_refs: BTreeSet::new(),
        proposed_at: 1789795100,
    };

    let hyp_event_id = adapter
        .commit(EpisodeItem::Hypothesis(hypothesis), 1789795100)
        .unwrap();

    // Link the goal to the hypothesis explicitly
    adapter
        .commit(
            EpisodeItem::Link(ExplicitLink {
                from_id: goal_event_id.clone(),
                to_id: hyp_event_id.clone(),
                relation: "investigates".to_string(),
                weight: 1.0,
            }),
            1789795105,
        )
        .unwrap();

    // -------------------------------------------------------------------------
    // Step 6: Query After Hypothesis: Plain vs Linked Mode
    // -------------------------------------------------------------------------

    // A. Plain query for "couch clatter"
    let plain_resp_after_hyp = query_episodes(
        &EpisodeQueryRequest {
            query: "couch clatter".to_string(),
            mode: RetrievalMode::Plain,
            limit: Some(5),
            seed: None,
            max_links_per_hit: None,
            max_explore_candidates: None,
            max_expansion_seeds: None,
            candidate_cap: None,
        },
        adapter.projection(),
    );
    assert_eq!(plain_resp_after_hyp.hits.len(), 1);
    // Plain mode MUST NOT expose linked context!
    assert!(
        plain_resp_after_hyp.hits[0].linked_context.is_empty(),
        "Plain mode invariant: linked_context must be strictly empty"
    );

    // B. Linked query for "couch clatter"
    let linked_resp_after_hyp = query_episodes(
        &EpisodeQueryRequest {
            query: "couch clatter".to_string(),
            mode: RetrievalMode::Linked,
            limit: Some(5),
            seed: None,
            max_links_per_hit: None,
            max_explore_candidates: None,
            max_expansion_seeds: None,
            candidate_cap: None,
        },
        adapter.projection(),
    );

    assert_eq!(linked_resp_after_hyp.hits.len(), 1);
    let hit = &linked_resp_after_hyp.hits[0];
    assert_eq!(hit.event_id, clatter_event_id);

    // 1. Original Observation is intact
    assert_eq!(
        hit.original_observation.content,
        "heard sharp plastic clatter near couch after kicking something"
    );
    assert_eq!(
        hit.original_observation.scoped_search.as_ref().unwrap().result,
        ScopedSearchResult::NotSeenInScope
    );

    // 2. Current Interpretation shows Candidate status
    let interp = hit.current_interpretation.as_ref().expect("must have active interpretation");
    assert_eq!(interp.status, HypothesisStatus::Candidate);
    assert_eq!(interp.claim, "plastic clatter near couch may have been the kicked vape");

    // 3. Evidential Delta is None (no status change yet)
    assert!(hit.evidential_delta.is_none());

    // 4. Linked mode exposes 1-hop links
    assert!(
        !hit.linked_context.is_empty(),
        "Linked mode must expose 1-hop context"
    );

    // -------------------------------------------------------------------------
    // Step 7: Subsequent Finding (Finding Vape in Far Corner)
    // -------------------------------------------------------------------------
    let corner_find = ObservationReport {
        report_id: "rep-corner-005".to_string(),
        event_time: Some(TimeSpec::Exact { timestamp: 1789795500 }),
        ingested_at: 1789795520,
        modality: SensoryModality::Visual,
        location: "far_corner".to_string(),
        task_context: "searching".to_string(),
        content: "found missing vape lying under far corner bookshelf".to_string(),
        scoped_search: Some(ScopedSearchReport {
            scope: "far-corner-floor".to_string(),
            result: ScopedSearchResult::Found,
        }),
        source_actor: "visual_search".to_string(),
        is_unresolved: false,
        tags: vec!["found".to_string(), "vape".to_string(), "corner".to_string()],
    };

    let corner_event_id = adapter
        .commit(EpisodeItem::Observation(corner_find), 1789795520)
        .unwrap();

    adapter
        .commit(
            EpisodeItem::Link(ExplicitLink {
                from_id: corner_event_id.clone(),
                to_id: hyp_event_id.clone(),
                relation: "vipaka".to_string(),
                weight: 1.0,
            }),
            1789795530,
        )
        .unwrap();

    // -------------------------------------------------------------------------
    // Step 8: Status Transition: Hypothesis Becomes Supported
    // -------------------------------------------------------------------------
    let transition = StatusTransitionEvent {
        hypothesis_id: "hyp-clatter-vape-004".to_string(),
        transition: HypothesisTransition::Support,
        evidence_refs: BTreeSet::from([corner_event_id.clone()]),
        rationale: "vape discovered in trajectory alignment with couch kick".to_string(),
        transition_time: 1789795550,
    };

    adapter
        .commit(EpisodeItem::StatusTransition(transition), 1789795550)
        .unwrap();

    // -------------------------------------------------------------------------
    // Step 9: Final Linked Recall Verification of Three-Part Bundle
    // -------------------------------------------------------------------------
    let final_resp = query_episodes(
        &EpisodeQueryRequest {
            query: "couch clatter".to_string(),
            mode: RetrievalMode::Linked,
            limit: Some(5),
            seed: None,
            max_links_per_hit: None,
            max_explore_candidates: None,
            max_expansion_seeds: None,
            candidate_cap: None,
        },
        adapter.projection(),
    );

    let final_hit = final_resp
        .hits
        .iter()
        .find(|b| b.event_id == clatter_event_id)
        .expect("clatter observation must be present in response");

    // (1) Original Observation: STILL 100% IMMUTABLE
    assert_eq!(
        final_hit.original_observation.content,
        "heard sharp plastic clatter near couch after kicking something"
    );
    assert_eq!(
        final_hit.original_observation.scoped_search.as_ref().unwrap().result,
        ScopedSearchResult::NotSeenInScope
    );

    // (2) Current Interpretation: Status is now Supported
    let final_interp = final_hit.current_interpretation.as_ref().unwrap();
    assert_eq!(final_interp.status, HypothesisStatus::Supported);
    assert!(final_interp.support_refs.contains(&corner_event_id));

    // (3) Evidential Delta: Explains why interpretation changed
    let delta = final_hit.evidential_delta.as_ref().expect("must have evidential delta");
    assert_eq!(delta.latest_transition, HypothesisTransition::Support);
    assert!(delta.evidence_refs.contains(&corner_event_id));
    assert_eq!(
        delta.rationale,
        "vape discovered in trajectory alignment with couch kick"
    );

    // (4) Invariant Check: Episode is now resolved
    assert!(
        !adapter
            .projection()
            .unresolved_episodes
            .contains(&clatter_event_id),
        "clatter episode must now be resolved after corroborated finding"
    );
}

#[test]
fn test_coexisting_hypothesis_alternatives_and_disconfirmation() {
    let dir = tempdir().unwrap();
    let log_path = dir.path().join("memory-overlay.json");
    let mut adapter = EpisodeAdapter::open(&log_path).unwrap();

    // 1. Record unexplained clatter
    let clatter_id = adapter
        .commit(
            EpisodeItem::Observation(ObservationReport {
                report_id: "obs-sound".to_string(),
                event_time: Some(TimeSpec::Uncertain {
                    description: "morning".to_string(),
                }),
                ingested_at: 100,
                modality: SensoryModality::Sound,
                location: "living_room".to_string(),
                task_context: "reading".to_string(),
                content: "unexplained thump sound".to_string(),
                scoped_search: None,
                source_actor: "listener".to_string(),
                is_unresolved: true,
                tags: vec!["sound".to_string(), "thump".to_string()],
            }),
            100,
        )
        .unwrap();

    assert!(adapter.projection().unresolved_episodes.contains(&clatter_id));

    // 2. Propose Hypothesis A: "it was the cat"
    let hyp_cat = adapter
        .commit(
            EpisodeItem::Hypothesis(HypothesisProposal {
                hypothesis_id: "hyp-cat".to_string(),
                target_episode_id: clatter_id.clone(),
                claim: "cat knocked something off the shelf".to_string(),
                status: HypothesisStatus::Candidate,
                support_refs: BTreeSet::new(),
                against_refs: BTreeSet::new(),
                proposed_at: 110,
            }),
            110,
        )
        .unwrap();

    // 3. Propose Hypothesis B: "it was a book falling"
    let _hyp_book = adapter
        .commit(
            EpisodeItem::Hypothesis(HypothesisProposal {
                hypothesis_id: "hyp-book".to_string(),
                target_episode_id: clatter_id.clone(),
                claim: "a heavy book slipped from the stack".to_string(),
                status: HypothesisStatus::Candidate,
                support_refs: BTreeSet::new(),
                against_refs: BTreeSet::new(),
                proposed_at: 120,
            }),
            120,
        )
        .unwrap();

    // Query should return both hypotheses: one primary, one alternative
    let query_res = query_episodes(
        &EpisodeQueryRequest {
            query: "thump sound".to_string(),
            mode: RetrievalMode::Linked,
            limit: Some(5),
            seed: None,
            max_links_per_hit: None,
            max_explore_candidates: None,
            max_expansion_seeds: None,
            candidate_cap: None,
        },
        adapter.projection(),
    );

    assert_eq!(query_res.hits.len(), 1);
    let hit = &query_res.hits[0];
    assert!(hit.current_interpretation.is_some());
    assert_eq!(hit.alternative_interpretations.len(), 1);

    // 4. Record observation disconfirming the cat: "cat was asleep in bedroom"
    let cat_asleep_id = adapter
        .commit(
            EpisodeItem::Observation(ObservationReport {
                report_id: "obs-cat-asleep".to_string(),
                event_time: Some(TimeSpec::Exact { timestamp: 130 }),
                ingested_at: 130,
                modality: SensoryModality::Visual,
                location: "bedroom".to_string(),
                task_context: "inspecting".to_string(),
                content: "saw cat sleeping soundly in bedroom".to_string(),
                scoped_search: None,
                source_actor: "observer".to_string(),
                is_unresolved: false,
                tags: vec!["cat".to_string(), "sleeping".to_string()],
            }),
            130,
        )
        .unwrap();

    // Transition hyp-cat -> Disconfirmed. The asleep report authorizes only
    // because a vipaka link joins it to this hypothesis.
    adapter
        .commit(
            EpisodeItem::Link(ExplicitLink {
                from_id: cat_asleep_id.clone(),
                to_id: hyp_cat.clone(),
                relation: "vipaka".to_string(),
                weight: 1.0,
            }),
            135,
        )
        .unwrap();
    adapter
        .commit(
            EpisodeItem::StatusTransition(StatusTransitionEvent {
                hypothesis_id: "hyp-cat".to_string(),
                transition: HypothesisTransition::Disconfirm,
                evidence_refs: BTreeSet::from([cat_asleep_id]),
                rationale: "cat was asleep in another room".to_string(),
                transition_time: 140,
            }),
            140,
        )
        .unwrap();

    // CRITICAL INVARIANT CHECK:
    // Disconfirming hyp-cat does NOT resolve the original sound!
    // The clatter is STILL unresolved because hyp-book is still Candidate!
    assert!(
        adapter.projection().unresolved_episodes.contains(&clatter_id),
        "disconfirming an explanation does not resolve the unexplained observation"
    );
}

#[test]
fn test_validation_invariants_reject_invalid_states() {
    let dir = tempdir().unwrap();
    let log_path = dir.path().join("memory-overlay.json");
    let mut adapter = EpisodeAdapter::open(&log_path).unwrap();

    // 1. Reject duplicate report_id
    let obs = ObservationReport {
        report_id: "obs-dup".to_string(),
        event_time: None,
        ingested_at: 10,
        modality: SensoryModality::Text,
        location: "room".to_string(),
        task_context: "ctx".to_string(),
        content: "test".to_string(),
        scoped_search: None,
        source_actor: "me".to_string(),
        is_unresolved: false,
        tags: vec![],
    };
    adapter.commit(EpisodeItem::Observation(obs.clone()), 10).unwrap();
    let err_dup = adapter.commit(EpisodeItem::Observation(obs), 20).unwrap_err();
    assert!(err_dup.to_string().contains("duplicate report_id"));

    // 2. Invariant: Hypothesis proposal cannot self-assert Supported
    let invalid_hyp = HypothesisProposal {
        hypothesis_id: "hyp-invalid-001".to_string(),
        target_episode_id: "ov1-fake".to_string(),
        claim: "self-asserted truth".to_string(),
        status: HypothesisStatus::Supported, // INVALID
        support_refs: BTreeSet::new(),
        against_refs: BTreeSet::new(),
        proposed_at: 100,
    };
    let err = adapter.commit(EpisodeItem::Hypothesis(invalid_hyp), 100).unwrap_err();
    assert!(err.to_string().contains("Candidate"));

    // 3. Invariant: Dangling target episode reference is rejected
    let dangling_target = HypothesisProposal {
        hypothesis_id: "hyp-invalid-002".to_string(),
        target_episode_id: "ov1-nonexistent".to_string(), // INVALID
        claim: "points to void".to_string(),
        status: HypothesisStatus::Candidate,
        support_refs: BTreeSet::new(),
        against_refs: BTreeSet::new(),
        proposed_at: 100,
    };
    let err2 = adapter.commit(EpisodeItem::Hypothesis(dangling_target), 100).unwrap_err();
    assert!(err2.to_string().contains("dangling reference"));

    // 4. Invariant: Self-reference is rejected
    let self_ref_hyp = HypothesisProposal {
        hypothesis_id: "hyp-self".to_string(),
        target_episode_id: "hyp-self".to_string(), // INVALID
        claim: "points to self".to_string(),
        status: HypothesisStatus::Candidate,
        support_refs: BTreeSet::new(),
        against_refs: BTreeSet::new(),
        proposed_at: 100,
    };
    let err3 = adapter.commit(EpisodeItem::Hypothesis(self_ref_hyp), 100).unwrap_err();
    assert!(err3.to_string().contains("cannot target itself"));

    // 5. Invariant: Status transition requires existing hypothesis and non-empty evidence refs
    let empty_evidence_st = StatusTransitionEvent {
        hypothesis_id: "hyp-ghost".to_string(),
        transition: HypothesisTransition::Support,
        evidence_refs: BTreeSet::new(), // INVALID: empty
        rationale: "no evidence".to_string(),
        transition_time: 100,
    };
    let err4 = adapter.commit(EpisodeItem::StatusTransition(empty_evidence_st), 100).unwrap_err();
    assert!(err4.to_string().contains("not found"));
}

#[test]
fn test_persistence_tamper_evidence_and_reload() {
    let dir = tempdir().unwrap();
    let log_path = dir.path().join("memory-overlay.json");

    let event_id_1;
    let event_id_2;

    {
        let mut adapter = EpisodeAdapter::open(&log_path).unwrap();
        event_id_1 = adapter
            .commit(
                EpisodeItem::Observation(ObservationReport {
                    report_id: "obs-1".to_string(),
                    event_time: Some(TimeSpec::Exact { timestamp: 1000 }),
                    ingested_at: 1005,
                    modality: SensoryModality::Visual,
                    location: "hallway".to_string(),
                    task_context: "walking".to_string(),
                    content: "seen keys on table".to_string(),
                    scoped_search: None,
                    source_actor: "agent".to_string(),
                    is_unresolved: false,
                    tags: vec!["keys".to_string()],
                }),
                1005,
            )
            .unwrap();

        event_id_2 = adapter
            .commit(
                EpisodeItem::Goal(GoalReport {
                    goal_id: "goal-1".to_string(),
                    created_at: 1010,
                    target_entity: "keys".to_string(),
                    description: "retrieve keys".to_string(),
                    tags: vec!["keys".to_string()],
                }),
                1010,
            )
            .unwrap();

        assert_eq!(adapter.log().events().len(), 2);
    }

    // Reload from disk and verify tamper-evidence and projection reconstruction
    {
        let reloaded = EpisodeAdapter::open(&log_path).unwrap();
        assert_eq!(reloaded.log().events().len(), 2);
        assert_eq!(reloaded.log().events()[0].event_id, event_id_1);
        assert_eq!(reloaded.log().events()[1].event_id, event_id_2);

        // Verification passes bit-for-bit
        reloaded.log().verify().unwrap();

        // Projection restored
        assert!(reloaded.projection().observations.contains_key(&event_id_1));
        assert!(reloaded.projection().goals.contains_key(&event_id_2));
    }
}

#[test]
fn test_commit_save_failure_leaves_adapter_state_unchanged() {
    let dir = tempdir().unwrap();
    let log_path = dir.path().join("memory-overlay.json");
    let mut adapter = EpisodeAdapter::open(&log_path).unwrap();

    let ev1 = adapter
        .commit(
            EpisodeItem::Observation(ObservationReport {
                report_id: "obs-init".to_string(),
                event_time: None,
                ingested_at: 100,
                modality: SensoryModality::Visual,
                location: "desk".to_string(),
                task_context: "init".to_string(),
                content: "initial observation".to_string(),
                scoped_search: None,
                source_actor: "user".to_string(),
                is_unresolved: false,
                tags: vec![],
            }),
            100,
        )
        .unwrap();

    assert_eq!(adapter.log().events().len(), 1);
    let log_before = adapter.log().clone();
    let proj_before = adapter.projection().clone();

    // Induce a save failure: creating a directory where OverlayLog::save writes its temporary file
    // causes fs::write(&tmp, bytes) to fail with EISDIR regardless of running as root.
    let tmp_path = dir.path().join("memory-overlay.json.tmp");
    std::fs::create_dir(&tmp_path).unwrap();

    let res = adapter.commit(
        EpisodeItem::Observation(ObservationReport {
            report_id: "obs-fail".to_string(),
            event_time: None,
            ingested_at: 200,
            modality: SensoryModality::Visual,
            location: "desk".to_string(),
            task_context: "fail".to_string(),
            content: "should fail to save".to_string(),
            scoped_search: None,
            source_actor: "user".to_string(),
            is_unresolved: false,
            tags: vec![],
        }),
        200,
    );

    // Clean up temporary blocker directory
    let _ = std::fs::remove_dir(&tmp_path);

    assert!(res.is_err(), "commit must fail when disk write fails");
    assert_eq!(
        adapter.log().events().len(),
        1,
        "log must remain unchanged on save failure"
    );
    assert_eq!(
        adapter.log().events()[0].event_id,
        ev1,
        "existing event intact"
    );
    assert_eq!(
        adapter.projection().observations.len(),
        1,
        "projection must remain unchanged on save failure"
    );
    assert!(
        adapter.projection().observations.contains_key(&ev1),
        "initial observation remains in projection"
    );
    assert_eq!(adapter.log().clone(), log_before);
    assert_eq!(adapter.projection().clone(), proj_before);
}

#[test]
fn test_zero_budgets_disable_work_integration() {
    let dir = tempdir().unwrap();
    let log_path = dir.path().join("memory-overlay.json");
    let mut adapter = EpisodeAdapter::open(&log_path).unwrap();

    let obs_id = adapter
        .commit(
            EpisodeItem::Observation(ObservationReport {
                report_id: "obs-zero".to_string(),
                event_time: None,
                ingested_at: 100,
                modality: SensoryModality::Sound,
                location: "couch".to_string(),
                task_context: "test".to_string(),
                content: "plastic noise".to_string(),
                scoped_search: None,
                source_actor: "test".to_string(),
                is_unresolved: true,
                tags: vec![],
            }),
            100,
        )
        .unwrap();

    let _hyp_id = adapter
        .commit(
            EpisodeItem::Hypothesis(HypothesisProposal {
                hypothesis_id: "hyp-zero".to_string(),
                target_episode_id: obs_id.clone(),
                claim: "plastic noise explanation".to_string(),
                status: HypothesisStatus::Candidate,
                support_refs: BTreeSet::new(),
                against_refs: BTreeSet::new(),
                proposed_at: 110,
            }),
            110,
        )
        .unwrap();

    // max_links = Some(0): disables linked hits
    let resp = query_episodes(
        &EpisodeQueryRequest {
            query: "noise".to_string(),
            mode: RetrievalMode::Linked,
            limit: Some(10),
            seed: None,
            max_links_per_hit: Some(0),
            max_explore_candidates: None,
            max_expansion_seeds: Some(200),
            candidate_cap: Some(64),
        },
        adapter.projection(),
    );
    assert!(resp.linked_hits.is_empty(), "Some(0) max_links must disable linked work");

    // max_explore = Some(0): disables explored hits
    let resp_exp = query_episodes(
        &EpisodeQueryRequest {
            query: "zzz-nonexistent".to_string(),
            mode: RetrievalMode::Explore,
            limit: Some(10),
            seed: Some(42),
            max_links_per_hit: None,
            max_explore_candidates: Some(0),
            max_expansion_seeds: None,
            candidate_cap: None,
        },
        adapter.projection(),
    );
    assert!(resp_exp.explored_hits.is_empty(), "Some(0) max_explore must disable exploration");
    assert_eq!(resp_exp.sampled_unresolved_count, 0);
}

#[test]
fn test_separated_output_groups_and_provenance() {
    let dir = tempdir().unwrap();
    let log_path = dir.path().join("memory-overlay.json");
    let mut adapter = EpisodeAdapter::open(&log_path).unwrap();

    let obs1 = adapter
        .commit(
            EpisodeItem::Observation(ObservationReport {
                report_id: "obs-lex".to_string(),
                event_time: None,
                ingested_at: 100,
                modality: SensoryModality::Sound,
                location: "kitchen".to_string(),
                task_context: "cooking".to_string(),
                content: "sizzling pan on stove".to_string(),
                scoped_search: None,
                source_actor: "user".to_string(),
                is_unresolved: false,
                tags: vec![],
            }),
            100,
        )
        .unwrap();

    let obs2 = adapter
        .commit(
            EpisodeItem::Observation(ObservationReport {
                report_id: "obs-linked".to_string(),
                event_time: None,
                ingested_at: 110,
                modality: SensoryModality::Sound,
                location: "pantry".to_string(),
                task_context: "cooking".to_string(),
                content: "cabinet door creak".to_string(),
                scoped_search: None,
                source_actor: "user".to_string(),
                is_unresolved: false,
                tags: vec![],
            }),
            110,
        )
        .unwrap();

    adapter
        .commit(
            EpisodeItem::Link(ExplicitLink {
                from_id: obs1.clone(),
                to_id: obs2.clone(),
                relation: "followed_by".to_string(),
                weight: 1.0,
            }),
            115,
        )
        .unwrap();

    let obs3 = adapter
        .commit(
            EpisodeItem::Observation(ObservationReport {
                report_id: "obs-unresolved".to_string(),
                event_time: None,
                ingested_at: 120,
                modality: SensoryModality::Sound,
                location: "basement".to_string(),
                task_context: "idle".to_string(),
                content: "unidentified metallic sound".to_string(),
                scoped_search: None,
                source_actor: "user".to_string(),
                is_unresolved: true,
                tags: vec![],
            }),
            120,
        )
        .unwrap();

    let resp = query_episodes(
        &EpisodeQueryRequest {
            query: "sizzling".to_string(),
            mode: RetrievalMode::Explore,
            limit: Some(10),
            seed: Some(99),
            max_links_per_hit: Some(5),
            max_explore_candidates: Some(5),
            max_expansion_seeds: Some(200),
            candidate_cap: Some(64),
        },
        adapter.projection(),
    );

    assert_eq!(resp.lexical_hits.len(), 1);
    assert_eq!(resp.lexical_hits[0].event_id, obs1);
    assert_eq!(resp.lexical_hits[0].origin, CandidateOrigin::Lexical);
    assert_eq!(resp.lexical_hits[0].via_seed_id, None);

    assert_eq!(resp.linked_hits.len(), 1);
    assert_eq!(resp.linked_hits[0].event_id, obs2);
    assert_eq!(resp.linked_hits[0].origin, CandidateOrigin::Linked);
    assert_eq!(resp.linked_hits[0].via_seed_id, Some(obs1.clone()));
    assert_eq!(resp.linked_hits[0].relation, Some("followed_by".to_string()));

    assert_eq!(resp.explored_hits.len(), 1);
    assert_eq!(resp.explored_hits[0].event_id, obs3);
    assert_eq!(resp.explored_hits[0].origin, CandidateOrigin::Explored);

    assert_eq!(resp.actual_seed, Some(99));
}

#[test]
fn test_multiword_unicode_location_scope_and_tag_commit_reopen_exactreport() {
    let dir = tempdir().unwrap();
    let log_path = dir.path().join("episodes.json");
    let mut adapter = EpisodeAdapter::open(&log_path).unwrap();

    // Natural text with spaces, punctuation, and Unicode in location, scoped search, and tags
    let raw_location = "near couch 桌子 🛋️";
    let raw_scope = "initial visible area under couch only 检查";
    let raw_tags = vec![
        "living room".to_string(),
        "🛋️".to_string(),
        "plastic clatter sound".to_string(),
        "special:tag-123".to_string(),
    ];
    let raw_content = "heard a distinct plastic clatter near the couch 桌子";

    let obs_report = ObservationReport {
        report_id: "rep-natural-unicode-001".to_string(),
        event_time: Some(TimeSpec::Exact { timestamp: 1789794000 }),
        ingested_at: 1789794010,
        modality: SensoryModality::Sound,
        location: raw_location.to_string(),
        task_context: "living room 桌子".to_string(),
        content: raw_content.to_string(),
        scoped_search: Some(ScopedSearchReport {
            scope: raw_scope.to_string(),
            result: ScopedSearchResult::NotSeenInScope,
        }),
        source_actor: "auditory_monitor".to_string(),
        is_unresolved: true,
        tags: raw_tags.clone(),
    };

    // 1. Commit must succeed without failing OverlayLog validate_ident
    let event_id = adapter
        .commit_observation(obs_report.clone(), 1789794010)
        .expect("commit with natural text and Unicode must succeed");

    // 2. In-memory projection must preserve exact, unmodified natural text and Unicode
    {
        let proj = adapter.projection();
        let stored = proj
            .observations
            .get(&event_id)
            .expect("observation must be in projection");
        assert_eq!(stored.report.location, raw_location, "location must not be sanitized");
        assert_eq!(
            stored.report.scoped_search.as_ref().unwrap().scope,
            raw_scope,
            "scoped search must not be sanitized"
        );
        assert_eq!(stored.report.tags, raw_tags, "user tags must not be sanitized");
        assert_eq!(stored.report.content, raw_content, "content must not be sanitized");
        assert_eq!(
            stored.report.scoped_search.as_ref().unwrap().result,
            ScopedSearchResult::NotSeenInScope
        );
    }

    // 3. Re-open adapter from persisted disk file
    drop(adapter);
    let reopened = EpisodeAdapter::open(&log_path).expect("reopen must succeed");
    {
        let proj = reopened.projection();
        let stored = proj
            .observations
            .get(&event_id)
            .expect("observation must be in reopened projection");
        assert_eq!(stored.report.location, raw_location, "reopened location must match exact natural text");
        assert_eq!(
            stored.report.scoped_search.as_ref().unwrap().scope,
            raw_scope,
            "reopened scope must match exact natural text"
        );
        assert_eq!(stored.report.tags, raw_tags, "reopened tags must match exact natural text");
        assert_eq!(stored.report.content, raw_content, "reopened content must match exact natural text");
    }

    // 4. Verify that the derived summary_tags in the underlying overlay event satisfy all OverlayLog constraints
    let last_event = reopened.events().last().expect("must have committed event");
    if let ferricula_episode::memory_overlay::OverlayPayload::ProposeMemory { ref tags, .. } = last_event.payload {
        assert!(tags.len() <= 16, "tags count {} must be <= 16", tags.len());
        for tag in tags {
            assert!(!tag.is_empty(), "tag must not be empty");
            assert!(tag.len() <= 64, "tag len {} must be <= 64", tag.len());
            assert_eq!(tag, tag.trim(), "tag must not have surrounding whitespace");
            assert!(
                tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')),
                "tag {:?} must satisfy [A-Za-z0-9._:-]",
                tag
            );
        }
        // Location and scope should be safely derived into valid tags
        assert!(tags.contains(&"location:near_couch".to_string()));
        assert!(tags.contains(&"search_scope:initial_visible_area_under_couch_only".to_string()));
    } else {
        panic!("expected ProposeMemory overlay payload");
    }
}
