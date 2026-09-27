//! Benchmarks §6.8 shape: thirty simulated nights of bhāvanā. Every night the
//! store and graph must not shrink, no text byte may change, no lifecycle
//! state may change without a deliberate decision, cluster membership must
//! not compound `consolidation_depth`, and every event must reach the sink.

use std::collections::BTreeMap;

use ferricula_cognition::bhavana::{BhavanaPolicy, BhavanaState, ReleaseKind, bhavana_cycle};
use ferricula_cognition::gates::{MergeGate, StaticMergeGate};
use ferricula_cognition::karmic::{KarmicEvent, VecSink};
use ferricula_cognition::scope::AgentId;
use ferricula_core::engine::Engine;
use ferricula_core::graph::{EdgeKind, MemoryGraph};
use ferricula_core::memory::{LifecycleState, MemoryRecord, MemoryStore};
use ferricula_core::model::Row;
use ferricula_core::prime_tree::PrimeTree;
use ferricula_core::skg::SkgState;

const DIM: usize = 8;
const CLUSTERS: u32 = 6;
const PER_CLUSTER: u32 = 8;
const UNRELATED: u32 = 12;
const NIGHTS: usize = 30;

struct World {
    engine: Engine,
    store: MemoryStore,
    graph: MemoryGraph,
    skg: SkgState,
    prime_tree: PrimeTree,
    state: BhavanaState,
    sink: VecSink,
    agent: AgentId,
    cluster_members: Vec<Vec<u32>>,
    keystones: Vec<u32>,
    released: Vec<u32>,
}

fn vector(cluster: u32, i: u32) -> Vec<f32> {
    let mut v = vec![0f32; DIM];
    v[cluster as usize % DIM] = 1.0;
    // small deterministic jitter on a different axis keeps cosine ≈ 0.99
    v[(cluster as usize + 1) % DIM] = 0.05 * (i % 3) as f32;
    v
}

fn unrelated_vector(i: u32) -> Vec<f32> {
    // pairwise cosine below 0.85: mix two axes per record
    let mut v = vec![0f32; DIM];
    v[i as usize % DIM] = 1.0;
    v[(i as usize + 3) % DIM] = 0.9;
    v[(i as usize + 5) % DIM] = 0.4 * (i % 2) as f32;
    v
}

fn build() -> World {
    let mut engine = Engine::new();
    let mut store = MemoryStore::new();
    let mut graph = MemoryGraph::new();
    let mut cluster_members = Vec::new();
    let mut next_id = 1u32;
    let mut add = |id: u32, vector: Vec<f32>, engine: &mut Engine, store: &mut MemoryStore| {
        let mut tags = BTreeMap::new();
        tags.insert("text".to_string(), format!("memory {id}: original bytes, never edited"));
        engine.upsert(Row { id, tags, vector, refs: None }).unwrap();
        store.insert(MemoryRecord::new(id));
    };
    for c in 0..CLUSTERS {
        let mut members = Vec::new();
        for i in 0..PER_CLUSTER {
            add(next_id, vector(c, i), &mut engine, &mut store);
            members.push(next_id);
            next_id += 1;
        }
        cluster_members.push(members);
    }
    let mut unrelated = Vec::new();
    for i in 0..UNRELATED {
        add(next_id, unrelated_vector(i), &mut engine, &mut store);
        unrelated.push(next_id);
        next_id += 1;
    }
    // two anchors among the unrelated records, connected to neighbours
    let keystones = vec![unrelated[0], unrelated[1]];
    for &k in &keystones {
        store.get_mut(k).unwrap().keystone = true;
        graph.connect(k, unrelated[2], "adjacent".into(), 1.0, EdgeKind::Semantic);
    }
    // two already-released records with zero fidelity: old prune targets
    let released = vec![unrelated[3], unrelated[4]];
    {
        let r = store.get_mut(released[0]).unwrap();
        r.fidelity = 0.0;
        r.forgive();
        // forgiven two hours ago: stale enough for a nirodha proposal
        r.last_recalled = ferricula_core::memory::now_epoch().saturating_sub(7200);
        let r = store.get_mut(released[1]).unwrap();
        r.fidelity = 0.0;
        r.forgive();
        r.archive();
    }
    // a structural edge inside a cluster that must never be overwritten
    graph.connect(cluster_members[0][0], cluster_members[0][1], "document-order".into(), 1.0, EdgeKind::Structural);
    World {
        engine,
        store,
        graph,
        skg: SkgState::new(),
        prime_tree: PrimeTree::new(),
        state: BhavanaState::default(),
        sink: VecSink::default(),
        agent: AgentId::new("agent_thirty_nights").unwrap(),
        cluster_members,
        keystones,
        released,
    }
}

fn texts(engine: &Engine) -> BTreeMap<u32, String> {
    engine
        .rows_iter()
        .map(|r| (r.id, r.tags.get("text").cloned().unwrap_or_default()))
        .collect()
}

fn states(store: &MemoryStore) -> BTreeMap<u32, LifecycleState> {
    store.iter().map(|(&id, r)| (id, r.state)).collect()
}

fn run_nights(w: &mut World, gate: Option<&dyn MergeGate>) -> Vec<ferricula_cognition::bhavana::BhavanaReport> {
    let policy = BhavanaPolicy::default();
    let texts_before = texts(&w.engine);
    let states_before = states(&w.store);
    let nodes_before = w.graph.node_count();
    let edges_before = w.graph.edge_count();
    let len_before = w.store.len();
    let mut reports = Vec::new();
    let mut prev_proposals = 0usize;
    for night in 0..NIGHTS {
        let seed = [(night * 37 % 251) as u8, (night * 11 % 251) as u8, 7, 199];
        let report = bhavana_cycle(
            &mut w.store,
            &w.engine,
            &mut w.graph,
            &mut w.skg,
            &w.prime_tree,
            &mut w.state,
            &policy,
            gate,
            &w.agent,
            0.9,
            &seed,
            &mut w.sink,
        )
        .unwrap_or_else(|e| panic!("night {night}: {e}"));

        assert_eq!(w.store.len(), len_before, "night {night}: store shrank or grew");
        assert!(w.graph.node_count() >= nodes_before, "night {night}: graph lost nodes");
        assert!(w.graph.edge_count() >= edges_before, "night {night}: graph lost edges");
        assert_eq!(texts(&w.engine), texts_before, "night {night}: a text byte changed");
        assert_eq!(states(&w.store), states_before, "night {night}: a lifecycle state changed without a decision");
        assert_eq!(report.forgiven + report.archived + report.pruned, 0, "night {night}: legacy destructive counters must stay 0");
        for &k in &w.keystones {
            assert_eq!(w.store.get(k).unwrap().fidelity, 1.0, "night {night}: anchor decayed");
        }
        for &r in &w.released {
            assert!(w.store.contains(r), "night {night}: released record pruned");
        }
        for (&id, r) in w.store.iter() {
            assert!(r.consolidation_depth <= 1, "night {night}: depth compounded on {id} to {}", r.consolidation_depth);
        }
        assert_eq!(
            w.graph.edge(w.cluster_members[0][0], w.cluster_members[0][1]).unwrap().label,
            "document-order",
            "night {night}: structural edge overwritten"
        );
        // ojā only falls, so upekkhā proposals never disappear across nights
        let upekkha = report.release_proposals.iter().filter(|p| p.kind == ReleaseKind::Upekkha).count();
        assert!(upekkha >= prev_proposals, "night {night}: a below-gate record stopped being proposed");
        prev_proposals = upekkha;
        reports.push(report);
    }
    reports
}

#[test]
fn thirty_nights_with_calibrated_gate_index_once_and_preserve_everything() {
    let mut w = build();
    let gate = StaticMergeGate {
        same_truth: 0.92,
        contradiction: 0.03,
        calibrated: true,
    };
    let reports = run_nights(&mut w, Some(&gate));

    // every cluster indexed exactly once, on night 1, then unchanged
    assert_eq!(reports[0].clusters.len(), CLUSTERS as usize, "night 1 should build every cluster");
    for (n, r) in reports.iter().enumerate().skip(1) {
        assert!(r.clusters.is_empty(), "night {}: cluster rebuilt", n + 1);
        assert_eq!(r.clusters_unchanged, CLUSTERS, "night {}: idempotent skip count", n + 1);
    }
    assert_eq!(w.state.clusters.len(), CLUSTERS as usize);
    for members in &w.cluster_members {
        for &m in members {
            assert_eq!(w.store.get(m).unwrap().consolidation_depth, 1, "member {m} depth");
            assert!(w.state.membership.contains_key(&m));
        }
        // member ↔ member links exist and originals are untouched
        for i in 0..members.len() {
            for j in (i + 1)..members.len() {
                assert!(w.graph.edge(members[i], members[j]).is_some());
            }
        }
    }
    for (&id, r) in w.store.iter() {
        let in_cluster = w.cluster_members.iter().any(|c| c.contains(&id));
        if !in_cluster {
            assert_eq!(r.consolidation_depth, 0, "unrelated {id} must not deepen");
        }
    }

    // by night 30 the α=0.01 decay has taken ordinary records below the gate:
    // they are proposed, not forgiven
    let last = reports.last().unwrap();
    let below_gate = w
        .store
        .iter()
        .filter(|(_, r)| r.state == LifecycleState::Active && !r.keystone && !r.above_gate())
        .count();
    assert!(below_gate > 0, "the simulation should reach the gate within 30 nights");
    let proposed = last.release_proposals.iter().filter(|p| p.kind == ReleaseKind::Upekkha).count();
    assert_eq!(proposed, below_gate);
    // the stale Forgiven record is proposed for nirodha every night, never archived
    assert!(last.release_proposals.iter().any(|p| p.id == w.released[0] && p.kind == ReleaseKind::Nirodha));
    assert_eq!(w.store.get(w.released[0]).unwrap().state, LifecycleState::Forgiven);

    // karmic log: one ClusterBuilt per cluster, every report entry landed
    let built = w.sink.entries.iter().filter(|e| matches!(e.event, KarmicEvent::ClusterBuilt { .. })).count();
    assert_eq!(built, CLUSTERS as usize);
    let total: u32 = reports.iter().map(|r| r.karmic_entries).sum();
    assert_eq!(total as usize, w.sink.entries.len());
    assert!(w.sink.entries.iter().all(|e| e.sati_enabled && e.agent.as_ref() == Some(&w.agent)));
}

#[test]
fn thirty_nights_without_gate_never_deepen_and_review_once() {
    let mut w = build();
    let reports = run_nights(&mut w, None);
    for r in &reports {
        assert!(r.clusters.is_empty());
        assert_eq!(r.review_clusters.len(), CLUSTERS as usize);
        assert_eq!(r.consolidated, 0);
    }
    for (_, r) in w.store.iter() {
        assert_eq!(r.consolidation_depth, 0);
    }
    assert!(w.state.clusters.is_empty());
    let reviews = w.sink.entries.iter().filter(|e| matches!(e.event, KarmicEvent::ClusterReview { .. })).count();
    assert_eq!(reviews, CLUSTERS as usize, "each review cluster logged exactly once across 30 nights");
}
