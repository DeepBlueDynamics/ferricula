use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use ferricula_core::engine::Engine;
use ferricula_core::graph::{EdgeKind, MemoryGraph};
use ferricula_core::memory::{LifecycleState, MemoryStore, now_epoch};
use ferricula_core::prime_tree::PrimeTree;
use ferricula_core::skg::{SkgState, SkgUpdateSummary};

/// A dream-imagery candidate: a prompt seeded from a high-positive-Weber-bracket
/// term pair in Phase 3.6. The core dream cycle identifies WHAT to dream about;
/// materialization (calling ComfyUI / Stable Diffusion / etc. to render the
/// image, then ingesting it as a `seeing` channel memory) is a runtime concern
/// outside the pure thermodynamic dream cycle. This decoupling keeps the core
/// external-dependency-free while making the dream-imagery integration point
/// concrete.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DreamImageryCandidate {
    /// The prompt as a single string, derived from the term pair.
    pub prompt: String,
    /// The source term pair (canonical order: a <= b).
    pub source_term_pair: (String, String),
    /// The positive Weber bracket value that surfaced this pair as emerging.
    pub weber_bracket: f32,
}

/// Report from a single cycle. Since Addendum A the cycle is
/// [`crate::bhavana::bhavana_cycle`]; this alias keeps old readers compiling.
/// `forgiven`, `archived` and `pruned` are always 0.
pub type DreamReport = crate::bhavana::BhavanaReport;

/// Cosine similarity threshold for consolidation grouping (parity value;
/// the policy knob is `BhavanaPolicy::consolidation_threshold`).
pub(crate) const CONSOLIDATION_THRESHOLD: f32 = 0.85;

// ── Semantic edge discovery knobs (env-tunable) ──────────────────────────────
//
// EDGE_ANCHOR_COUNT     — top-N memories by fidelity included every dream
// EDGE_EXPLORER_COUNT   — additional memories sampled via radio entropy
// EDGE_MAX_PER_DREAM    — cap on new edges created in one dream cycle
//
// Defaults chosen so the candidate pool covers ~8% of typical active set
// with a deterministic anchor core that lets repeat-recalls reinforce, plus
// an entropy-driven explorer ring that lets the long tail get connected.

pub(crate) fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

pub(crate) fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Run one cycle at full intensity.
///
/// Non-durable convenience wrapper: runs [`crate::bhavana::bhavana_cycle`]
/// with the default policy, no merge gate (every cluster goes to review),
/// a fresh [`crate::bhavana::BhavanaState`] and an in-memory karmic sink
/// whose entries are dropped. Nothing is forgiven, archived or pruned.
/// Integrators must call `bhavana_cycle` with a durable sink and state.
#[deprecated(note = "use bhavana::bhavana_cycle with a durable KarmicSink and BhavanaState")]
pub fn dream_cycle(
    store: &mut MemoryStore,
    engine: &Engine,
    graph: &mut MemoryGraph,
    skg: &mut SkgState,
    prime_tree: &PrimeTree,
    shivvr_url: Option<&str>,
) -> DreamReport {
    #[allow(deprecated)]
    dream_cycle_with_intensity(store, engine, graph, skg, prime_tree, 1.0, &[], shivvr_url)
}

/// Entropy-modulated variant of [`dream_cycle`]; same non-durable caveats.
/// `shivvr_url` is ignored: ghost-echo extraction ran only at prune time and
/// there is no prune.
#[deprecated(note = "use bhavana::bhavana_cycle with a durable KarmicSink and BhavanaState")]
#[allow(clippy::too_many_arguments)]
pub fn dream_cycle_with_intensity(
    store: &mut MemoryStore,
    engine: &Engine,
    graph: &mut MemoryGraph,
    skg: &mut SkgState,
    prime_tree: &PrimeTree,
    intensity: f32,
    entropy_seed: &[u8],
    _shivvr_url: Option<&str>,
) -> DreamReport {
    use crate::bhavana::{BhavanaPolicy, BhavanaState, bhavana_cycle};
    use crate::karmic::VecSink;
    use crate::scope::AgentId;

    let agent = AgentId::new("legacy-dream").expect("non-empty");
    let mut state = BhavanaState::default();
    let mut sink = VecSink::default();
    bhavana_cycle(
        store,
        engine,
        graph,
        skg,
        prime_tree,
        &mut state,
        &BhavanaPolicy::default(),
        None,
        &agent,
        intensity,
        entropy_seed,
        &mut sink,
    )
    .expect("in-memory sink cannot fail and the cycle never shrinks the store")
}

/// Intensity → active role-set classifier (inlined from the removed archetype
/// harness). Deterministic, no personas: `< 0.25` → none; `0.25..0.75` →
/// `["Intuition", "Fortune"]`; `>= 0.75` → all five role labels. Only the
/// `"Intuition"` and `"Ethics"` labels gate dream phases (3.5/5.5 and 5
/// respectively); the others are carried in the report for parity with the
/// prior `active_archetypes` field. At intensity 1.0 (the manual `dream`
/// command) all phases fire, matching pre-removal Full-tier behavior.
pub(crate) fn activation_roles(intensity: f32) -> Vec<String> {
    if intensity < 0.25 {
        vec![]
    } else if intensity < 0.75 {
        vec!["Intuition".to_string(), "Fortune".to_string()]
    } else {
        vec![
            "Intuition".to_string(),
            "Fortune".to_string(),
            "Craft".to_string(),
            "Ethics".to_string(),
            "Advocate".to_string(),
        ]
    }
}

/// Select a subset of IDs using entropy bits to determine inclusion.
/// At intensity 1.0 all are selected. At 0.0 none are.
/// When entropy_seed is empty, falls back to deterministic threshold selection.
pub(crate) fn select_by_entropy(ids: &[u32], intensity: f32, entropy_seed: &[u8]) -> Vec<u32> {
    if intensity >= 1.0 || ids.is_empty() {
        return ids.to_vec();
    }
    if intensity <= 0.0 {
        return vec![];
    }

    if entropy_seed.is_empty() {
        // Deterministic fallback: take the first `intensity` fraction
        let count = ((ids.len() as f32) * intensity).ceil() as usize;
        return ids[..count.min(ids.len())].to_vec();
    }

    // Use entropy bits: each bit decides include/exclude,
    // biased by intensity threshold
    let threshold = (intensity * 255.0) as u8;
    ids.iter()
        .enumerate()
        .filter(|&(i, _)| {
            let byte = entropy_seed[i % entropy_seed.len()];
            byte < threshold
        })
        .map(|(_, &id)| id)
        .collect()
}

/// Group memories by vector similarity. Greedy single-linkage from each
/// unassigned seed; a group is kept when it has at least `min_size` members.
pub(crate) fn find_similarity_groups(engine: &Engine, ids: &[u32], threshold: f32, min_size: usize) -> Vec<Vec<u32>> {
    let mut groups: Vec<Vec<u32>> = Vec::new();
    let mut assigned: HashSet<u32> = HashSet::new();

    for &id in ids {
        if assigned.contains(&id) {
            continue;
        }
        let Some(row) = engine.get(id) else { continue };
        if row.vector.is_empty() {
            continue;
        }

        let mut group = vec![id];
        for &other_id in ids {
            if assigned.contains(&other_id) || other_id == id || group.contains(&other_id) {
                continue;
            }
            let Some(other_row) = engine.get(other_id) else {
                continue;
            };
            if other_row.vector.len() != row.vector.len() {
                continue;
            }
            if cosine_sim(&row.vector, &other_row.vector) >= threshold {
                group.push(other_id);
            }
        }

        if group.len() >= min_size.max(2) {
            for &m in &group {
                assigned.insert(m);
            }
            groups.push(group);
        } else {
            assigned.insert(id);
        }
    }

    groups
}

/// Phase 5.5: Build dream-imagery candidates from the SKG summary's top
/// emerging term pairs. Each candidate's prompt is the pair concatenated; a
/// downstream consumer (Steve runtime, arena tools) materializes the image
/// via an external backend and writes the resulting `seeing` channel memory.
///
/// We take at most `top_n` candidates, requiring a positive Weber bracket.
pub(crate) fn select_dream_imagery_candidates(
    summary: &SkgUpdateSummary,
    top_n: usize,
) -> Vec<DreamImageryCandidate> {
    summary
        .top_emerging
        .iter()
        .filter_map(|edge| {
            let bracket = edge.weber_bracket?;
            if bracket <= 0.0 {
                return None;
            }
            Some(DreamImageryCandidate {
                prompt: format!("{} {}", edge.term_a, edge.term_b),
                source_term_pair: (edge.term_a.clone(), edge.term_b.clone()),
                weber_bracket: bracket,
            })
        })
        .take(top_n)
        .collect()
}

/// Phase 3.5: Discover semantic edges between high-fidelity active memories.
/// Finds pairs with cosine similarity in [0.7, CONSOLIDATION_THRESHOLD) that
/// aren't already connected. Capped at `max_edges` new edges per dream.
///
/// The candidate pool is **anchors + explorers**:
///   - top `EDGE_ANCHOR_COUNT` memories by fidelity (deterministic core)
///   - `EDGE_EXPLORER_COUNT` additional memories sampled via radio entropy
///
/// Without the explorer ring the same top-N gets re-scanned forever and edge
/// growth flatlines once that core's pairs are exhausted. Radio entropy gives
/// the long tail a chance to surface and connect.
pub(crate) fn discover_semantic_edges(
    store: &MemoryStore,
    engine: &Engine,
    graph: &mut MemoryGraph,
    max_edges: u32,
    entropy_seed: &[u8],
) -> u32 {
    let anchor_count = env_usize("EDGE_ANCHOR_COUNT", 10);
    let explorer_count = env_usize("EDGE_EXPLORER_COUNT", 30);

    let mut all: Vec<(u32, f32)> = store
        .in_state(LifecycleState::Active)
        .into_iter()
        .map(|r| (r.id, r.fidelity))
        .collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1));

    let anchors: Vec<(u32, f32)> = all.iter().take(anchor_count).cloned().collect();
    let rest: Vec<(u32, f32)> = all.into_iter().skip(anchor_count).collect();

    // Radio-driven sample of `rest`. Walk entropy bytes pairwise to build
    // a 16-bit index, modulo rest.len(). Skip duplicates. Empty entropy or
    // exhausted bytes → deterministic top-up from the head of `rest`.
    let mut explorers: Vec<(u32, f32)> = Vec::with_capacity(explorer_count);
    if !entropy_seed.is_empty() && !rest.is_empty() {
        let mut taken: HashSet<usize> = HashSet::new();
        let cap = entropy_seed.len() * 4;
        let mut i = 0usize;
        while explorers.len() < explorer_count && taken.len() < rest.len() && i < cap {
            let b1 = entropy_seed[i % entropy_seed.len()] as usize;
            let b2 = entropy_seed[(i + 1) % entropy_seed.len()] as usize;
            let idx = ((b1 << 8) | b2) % rest.len();
            if taken.insert(idx) {
                explorers.push(rest[idx].clone());
            }
            i += 1;
        }
    }
    if explorers.len() < explorer_count {
        let already: HashSet<u32> = explorers.iter().map(|e| e.0).collect();
        for r in &rest {
            if explorers.len() >= explorer_count {
                break;
            }
            if !already.contains(&r.0) {
                explorers.push(r.clone());
            }
        }
    }

    let mut candidates = anchors;
    candidates.extend(explorers);

    let mut edges_created = 0u32;

    for i in 0..candidates.len() {
        if edges_created >= max_edges {
            break;
        }
        let (id_a, _) = candidates[i];
        let Some(row_a) = engine.get(id_a) else {
            continue;
        };
        if row_a.vector.is_empty() {
            continue;
        }

        for j in (i + 1)..candidates.len() {
            if edges_created >= max_edges {
                break;
            }
            let (id_b, _) = candidates[j];
            let Some(row_b) = engine.get(id_b) else {
                continue;
            };
            if row_b.vector.len() != row_a.vector.len() {
                continue;
            }

            let sim = cosine_sim(&row_a.vector, &row_b.vector);
            // Phase 3.6 per-edge dynamics: record similarity observation
            // for every pair we evaluate, including those that don't cross
            // the edge-creation threshold. This builds Weber-bracket history
            // for "almost-edges" alongside established ones — the pakatūpanissaya
            // gate at recall time can consult bracket sign when expanding.
            let now = now_epoch();
            graph.push_edge_observation(id_a, id_b, now, sim);
            if sim >= 0.7 && sim < CONSOLIDATION_THRESHOLD {
                if graph.edge(id_a, id_b).is_none() {
                    graph.connect(id_a, id_b, "semantic".to_string(), sim, EdgeKind::Semantic);
                    edges_created += 1;
                }
            }
        }
    }

    edges_created
}

/// Phase 5 (Ethics): Promote heavily-recalled, high-fidelity memories to keystone.
/// Returns the number of promotions (capped at 1 per dream cycle).
pub(crate) fn review_keystones(store: &mut MemoryStore) -> u32 {
    let mut promote_id = None;
    for (&_id, record) in store.iter() {
        if !record.keystone
            && record.state == LifecycleState::Active
            && record.recall_count >= 5
            && record.fidelity > 0.95
        {
            promote_id = Some(record.id);
            break; // Cap: promote at most 1
        }
    }

    if let Some(id) = promote_id {
        if let Some(record) = store.get_mut(id) {
            record.keystone = true;
            return 1;
        }
    }
    0
}

pub(crate) fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= f32::EPSILON || nb <= f32::EPSILON {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use ferricula_core::memory::{FIDELITY_GATE, MemoryRecord};
    use ferricula_core::model::Row;

    fn make_row(id: u32, vector: Vec<f32>) -> Row {
        Row {
            id,
            tags: BTreeMap::new(),
            vector,
            refs: None,
        }
    }

    fn make_record(id: u32) -> MemoryRecord {
        MemoryRecord::new_at(id, 0)
    }

    #[test]
    fn dream_decays_active_records() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        store.insert(make_record(1));

        let report = dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);
        assert_eq!(report.decayed, 1);
        assert!(store.get(1).unwrap().fidelity < 1.0);
    }

    #[test]
    fn dream_never_forgives_below_gate_it_proposes() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        let mut r = make_record(1);
        r.fidelity = FIDELITY_GATE - 0.01; // just below gate
        store.insert(r);

        let report = dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);
        assert_eq!(report.forgiven, 0);
        assert_eq!(report.release_proposals.len(), 1);
        assert_eq!(store.get(1).unwrap().state, LifecycleState::Active);
    }

    #[test]
    fn dream_routes_similar_vectors_to_review_without_a_gate() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        engine.upsert(make_row(1, vec![1.0, 0.0, 0.0])).unwrap();
        engine.upsert(make_row(2, vec![0.99, 0.01, 0.0])).unwrap();
        engine.upsert(make_row(3, vec![0.98, 0.02, 0.0])).unwrap();
        engine.upsert(make_row(4, vec![0.0, 0.0, 1.0])).unwrap();
        for id in 1..=4 {
            store.insert(make_record(id));
        }

        let report = dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);
        assert_eq!(report.consolidated, 0);
        assert_eq!(report.review_clusters.len(), 1);
        assert_eq!(report.review_clusters[0].members, vec![1, 2, 3]);
        for id in 1..=4 {
            assert_eq!(store.get(id).unwrap().state, LifecycleState::Active);
        }
        assert_eq!(store.len(), 4);
    }

    #[test]
    fn dream_skips_keystones() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        let mut r = make_record(1);
        r.keystone = true;
        store.insert(r);

        dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);
        // Keystone should remain at full fidelity
        assert_eq!(store.get(1).unwrap().fidelity, 1.0);
    }

    #[test]
    fn dream_preserves_every_graph_edge_and_node() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        engine.upsert(make_row(1, vec![1.0, 0.0, 0.0])).unwrap();
        engine.upsert(make_row(2, vec![0.99, 0.01, 0.0])).unwrap();
        engine.upsert(make_row(3, vec![0.0, 0.0, 1.0])).unwrap();
        for id in 1..=3 {
            store.insert(make_record(id));
        }
        graph.connect(2, 3, "link".into(), 1.0, EdgeKind::Semantic);
        let edges_before = graph.edge_count();

        dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);

        assert!(graph.neighbors(2).contains(3));
        assert_eq!(graph.edge(2, 3).unwrap().label, "link");
        assert!(graph.edge_count() >= edges_before);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn select_by_entropy_full_intensity() {
        let ids = vec![1, 2, 3, 4, 5];
        let selected = select_by_entropy(&ids, 1.0, &[]);
        assert_eq!(selected, ids);
    }

    #[test]
    fn select_by_entropy_zero_intensity() {
        let ids = vec![1, 2, 3, 4, 5];
        let selected = select_by_entropy(&ids, 0.0, &[]);
        assert!(selected.is_empty());
    }

    #[test]
    fn select_by_entropy_partial_deterministic() {
        let ids = vec![1, 2, 3, 4, 5];
        let selected = select_by_entropy(&ids, 0.5, &[]);
        // ceil(5 * 0.5) = 3
        assert_eq!(selected.len(), 3);
        assert_eq!(selected, vec![1, 2, 3]);
    }

    #[test]
    fn select_by_entropy_with_seed() {
        let ids = vec![1, 2, 3, 4];
        // At intensity 0.5, threshold = 127. Bytes < 127 are selected.
        let seed = vec![0, 200, 50, 255]; // 0<127, 200>=127, 50<127, 255>=127
        let selected = select_by_entropy(&ids, 0.5, &seed);
        assert_eq!(selected, vec![1, 3]); // indices 0 and 2
    }

    #[test]
    fn dream_with_intensity_half() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        for i in 1..=4 {
            engine.upsert(make_row(i, vec![i as f32, 0.0])).unwrap();
            store.insert(make_record(i));
        }

        let report = dream_cycle_with_intensity(
            &mut store,
            &engine,
            &mut graph,
            &mut skg,
            &prime_tree,
            0.5,
            &[],
            None,
        );
        // ceil(4 * 0.5) = 2 should be decayed
        assert_eq!(report.decayed, 2);
    }

    /// Fresh-timestamp record so Phase 4 neglect doesn't fire during halo
    /// tests (NEGLECT_SECONDS is 86400, so stale-at-epoch-0 records always
    /// trip it). These halo tests want to isolate Phase 0 behavior.
    fn fresh_record(id: u32) -> MemoryRecord {
        MemoryRecord::new(id)
    }

    #[test]
    fn dream_halo_shrinks_keystone_neighbor_alpha() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        // Keystoned center memory
        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        let mut ks = fresh_record(1);
        ks.keystone = true;
        store.insert(ks);

        // Non-keystone neighbor — should receive the halo
        engine.upsert(make_row(2, vec![0.0, 1.0])).unwrap();
        store.insert(fresh_record(2));

        // Isolated non-keystone — no halo
        engine.upsert(make_row(3, vec![0.0, 0.0])).unwrap();
        store.insert(fresh_record(3));

        graph.connect(1, 2, "adjacent".into(), 1.0, EdgeKind::Semantic);

        let alpha_before_neighbor = store.get(2).unwrap().decay_alpha;
        let alpha_before_isolated = store.get(3).unwrap().decay_alpha;

        let report = dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);

        assert_eq!(
            report.halo_touched, 1,
            "exactly one neighbor should be halo'd"
        );

        let alpha_after_neighbor = store.get(2).unwrap().decay_alpha;
        let alpha_after_isolated = store.get(3).unwrap().decay_alpha;

        assert!(
            alpha_after_neighbor < alpha_before_neighbor,
            "halo should shrink the neighbor's alpha ({alpha_after_neighbor} >= {alpha_before_neighbor})"
        );
        assert_eq!(
            alpha_after_isolated, alpha_before_isolated,
            "isolated memory should not be halo'd"
        );
        assert!(
            alpha_after_neighbor < alpha_after_isolated,
            "halo'd neighbor must end with smaller alpha than isolated control"
        );
    }

    #[test]
    fn dream_halo_skips_keystone_neighbors_that_are_also_keystones() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        // Two keystones, connected
        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        engine.upsert(make_row(2, vec![0.0, 1.0])).unwrap();
        let mut a = fresh_record(1);
        a.keystone = true;
        let mut b = fresh_record(2);
        b.keystone = true;
        store.insert(a);
        store.insert(b);

        graph.connect(1, 2, "adjacent".into(), 1.0, EdgeKind::Semantic);

        let report = dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);

        // Both ends of the edge are keystones, so neither should be in the halo set.
        assert_eq!(report.halo_touched, 0);
    }

    #[test]
    fn dream_halo_preserves_neighbor_over_many_cycles() {
        // Verifies the halo is strong enough to keep a neighbor at higher
        // fidelity than an un-halo'd control over many cycles. Uses fresh
        // timestamps so neglect doesn't fire and contaminate the measurement.
        let mut engine_haloed = Engine::new();
        let mut store_haloed = MemoryStore::new();
        let mut graph_haloed = MemoryGraph::new();
        let mut skg_haloed = SkgState::new();
        let pt = PrimeTree::new();

        engine_haloed.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        let mut ks = fresh_record(1);
        ks.keystone = true;
        store_haloed.insert(ks);

        engine_haloed.upsert(make_row(2, vec![0.0, 1.0])).unwrap();
        store_haloed.insert(fresh_record(2));
        graph_haloed.connect(1, 2, "adjacent".into(), 1.0, EdgeKind::Semantic);

        // Control: same setup but no edge, so no halo.
        let mut engine_ctrl = Engine::new();
        let mut store_ctrl = MemoryStore::new();
        let mut graph_ctrl = MemoryGraph::new();
        let mut skg_ctrl = SkgState::new();

        engine_ctrl.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        let mut ks_ctrl = fresh_record(1);
        ks_ctrl.keystone = true;
        store_ctrl.insert(ks_ctrl);

        engine_ctrl.upsert(make_row(2, vec![0.0, 1.0])).unwrap();
        store_ctrl.insert(fresh_record(2));

        // Run 30 dream cycles — roughly the number of decay ticks 944/946
        // experienced over ~14 days before they crossed the gate.
        for _ in 0..30 {
            dream_cycle(
                &mut store_haloed,
                &engine_haloed,
                &mut graph_haloed,
                &mut skg_haloed,
                &pt,
                None,
            );
            dream_cycle(
                &mut store_ctrl,
                &engine_ctrl,
                &mut graph_ctrl,
                &mut skg_ctrl,
                &pt,
                None,
            );
        }

        let haloed_fidelity = store_haloed.get(2).map(|r| r.fidelity).unwrap_or(0.0);
        let ctrl_fidelity = store_ctrl.get(2).map(|r| r.fidelity).unwrap_or(0.0);

        assert!(
            haloed_fidelity > ctrl_fidelity,
            "halo'd neighbor should decay slower than control (haloed={haloed_fidelity}, ctrl={ctrl_fidelity})"
        );
    }

    #[test]
    fn dream_full_intensity_matches_original() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        store.insert(make_record(1));

        // dream_cycle wraps dream_cycle_with_intensity at 1.0
        let report = dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);
        assert_eq!(report.decayed, 1);
        assert!(store.get(1).unwrap().fidelity < 1.0);
    }
}
