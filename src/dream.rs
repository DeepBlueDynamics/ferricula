use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::archetypes::ArchetypeRole;
use crate::engine::Engine;
use crate::graph::{EdgeKind, MemoryGraph};
use crate::inversion;
use crate::memory::{LifecycleState, MemoryStore, Provenance};
use crate::prime_tree::PrimeTree;
use crate::skg::{SkgState, SkgUpdateSummary};

/// Report from a single dream cycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DreamReport {
    pub ticks: u32,
    pub decayed: u32,
    pub forgiven: u32,
    pub archived: u32,
    pub consolidated: u32,
    pub pruned: u32,
    pub ghost_echoes: u32,
    pub keystones_reviewed: u32,
    pub active_archetypes: Vec<String>,
    pub edges_created: u32,
    pub keystones_promoted: u32,
    pub skg_summary: SkgUpdateSummary,
    /// IDs of memories that were decay-ticked this cycle.
    pub decayed_ids: Vec<u32>,
    /// IDs of memories that transitioned Active → Forgiven this cycle.
    pub forgiven_ids: Vec<u32>,
    /// IDs involved in consolidation groups this cycle.
    pub consolidated_ids: Vec<u32>,
    /// Count of memories that received a keystone halo touch this cycle.
    /// These are Active non-keystone neighbors of keystones whose decay_alpha
    /// was shrunk to preserve dialectical context around keystoned quotes.
    pub halo_touched: u32,
}

/// Cosine similarity threshold for consolidation grouping.
const CONSOLIDATION_THRESHOLD: f32 = 0.85;
/// Seconds without recall before a memory is considered neglected.
const NEGLECT_SECONDS: u64 = 86400;

/// Run one dream cycle at full intensity (manual `dream` command).
pub fn dream_cycle(
    store: &mut MemoryStore,
    engine: &Engine,
    graph: &mut MemoryGraph,
    skg: &mut SkgState,
    prime_tree: &PrimeTree,
    shivvr_url: Option<&str>,
) -> DreamReport {
    dream_cycle_with_intensity(store, engine, graph, skg, prime_tree, 1.0, &[], shivvr_url)
}

/// Run one dream cycle with entropy-modulated decay.
///
/// `intensity` (0.0..1.0) controls what fraction of active memories get decay-ticked.
/// `entropy_seed` provides random bits for selecting which memories are ticked.
/// At intensity 1.0 all are selected (equivalent to `dream_cycle`).
/// All other phases (forgive, consolidate, neglect, review, prune) run on the full set.
pub fn dream_cycle_with_intensity(
    store: &mut MemoryStore,
    engine: &Engine,
    graph: &mut MemoryGraph,
    skg: &mut SkgState,
    prime_tree: &PrimeTree,
    intensity: f32,
    entropy_seed: &[u8],
    shivvr_url: Option<&str>,
) -> DreamReport {
    let mut report = DreamReport::default();
    let intensity = intensity.clamp(0.0, 1.0);

    // Archetype activation — determines which behaviors fire this dream
    let tier = crate::archetypes::activation_tier(intensity);
    let active_roles = tier.active_roles();
    report.active_archetypes = active_roles.iter().map(|r| r.name().to_string()).collect();

    let ids: Vec<u32> = store.iter().map(|(&id, _)| id).collect();

    // Capture pre-existing forgiven IDs before any transitions this cycle.
    let pre_forgiven: Vec<u32> = store
        .in_state(LifecycleState::Forgiven)
        .into_iter()
        .map(|r| r.id)
        .collect();

    // Phase 0: Keystone halo — protect the dialectical context of keystones.
    //
    // A memory that is a direct graph neighbor of a keystone gets a weak
    // alpha shrink (on_halo_touch). Runs before decay tick so the reduced
    // alpha takes effect in this cycle's decay pass.
    //
    // Rationale: keystoning a single memory (e.g., a famous quote from a
    // longer passage) preserves the quote but lets its surrounding context
    // decay. The result is a "sharpened" memory that is literally correct
    // but meaningfully incomplete — the qualifications and nuance that
    // gave the quote its original sense are gone. The halo slows the
    // decay of direct neighbors so the surrounding chunks survive long
    // enough to be re-encountered when the keystone is recalled.
    //
    // Thermodynamically: proximity to something editorially important is
    // itself a weak form of attention. The memory next to a keystone
    // matters by association.
    let halo_set: HashSet<u32> = {
        let keystone_ids: Vec<u32> = store.keystones().iter().map(|r| r.id).collect();
        let mut set: HashSet<u32> = HashSet::new();
        for ks_id in keystone_ids {
            for neighbor_id in graph.neighbors(ks_id).iter() {
                if let Some(record) = store.get(neighbor_id) {
                    if record.state == LifecycleState::Active && !record.keystone {
                        set.insert(neighbor_id);
                    }
                }
            }
        }
        set
    };
    for &halo_id in &halo_set {
        if let Some(record) = store.get_mut(halo_id) {
            record.on_halo_touch();
        }
    }
    report.halo_touched = halo_set.len() as u32;

    // Phase 1: Decay tick — entropy-gated. Only selected memories are ticked.
    let decay_candidates: Vec<u32> = ids
        .iter()
        .copied()
        .filter(|&id| {
            store
                .get(id)
                .map_or(false, |r| r.state == LifecycleState::Active && !r.keystone)
        })
        .collect();
    let selected = select_by_entropy(&decay_candidates, intensity, entropy_seed);
    for &id in &selected {
        if let Some(record) = store.get_mut(id) {
            record.decay_tick();
            report.decayed += 1;
            report.decayed_ids.push(id);
            report.ticks += 1;
        }
    }

    // Phase 2: Lifecycle — Active below fidelity gate → Forgiven.
    for &id in &ids {
        if let Some(record) = store.get_mut(id) {
            if record.state == LifecycleState::Active && !record.above_gate() {
                record.forgive();
                report.forgiven += 1;
                report.forgiven_ids.push(id);
            }
        }
    }

    // Phase 3: Consolidation — merge similar active memories.
    let active_ids: Vec<u32> = store
        .in_state(LifecycleState::Active)
        .into_iter()
        .map(|r| r.id)
        .collect();
    let groups = find_consolidation_groups(engine, &active_ids);
    for group in groups {
        if group.len() >= 2 {
            let count = group.len() as u32;
            report.consolidated_ids.extend_from_slice(&group);
            let edges = consolidate_group(store, graph, &group);
            report.consolidated += count;
            report.edges_created += edges;
        }
    }

    // Phase 3.5: Semantic edge discovery (Intuition archetype — the Weaver)
    if active_roles.contains(&ArchetypeRole::Intuition) {
        let edges = discover_semantic_edges(store, engine, graph, 5);
        report.edges_created += edges;
    }

    // Phase 3.6: SKG Weber update — track co-occurrence velocity/acceleration
    let skg_summary = skg.update(prime_tree, store, engine, 20);
    report.skg_summary = skg_summary;

    // Phase 4: Neglect — grow alpha for stale memories.
    for &id in &ids {
        if let Some(record) = store.get_mut(id) {
            if record.state == LifecycleState::Active && record.staleness() > NEGLECT_SECONDS {
                record.on_neglect();
            }
        }
    }

    // Phase 5: Keystone review (Ethics archetype — the Guardian).
    report.keystones_reviewed = store.keystones().len() as u32;
    if active_roles.contains(&ArchetypeRole::Ethics) {
        report.keystones_promoted = review_keystones(store);
    }

    // Phase 6: Archive old forgiven records (only those forgiven BEFORE this cycle), prune zeroed archived.
    for id in pre_forgiven {
        if let Some(record) = store.get_mut(id) {
            // Forgiven for over an hour → archive
            if record.staleness() > 3600 {
                record.archive();
                report.archived += 1;
            }
        }
    }

    let archived_ids: Vec<u32> = store
        .in_state(LifecycleState::Archived)
        .into_iter()
        .map(|r| r.id)
        .collect();
    for id in archived_ids {
        let should_prune = store.get(id).map_or(false, |r| r.fidelity < f32::EPSILON);
        if should_prune {
            // Deathbed confession: extract what survives before deletion.
            if let Some(url) = shivvr_url {
                if let Some(row) = engine.get(id) {
                    if !row.vector.is_empty() {
                        if let Some(echo) = extract_ghost_echo(url, &row.vector) {
                            let neighbors = graph.neighbors(id);
                            if !neighbors.is_empty() {
                                // Attach the echo as labeled edges to surviving neighbors.
                                for neighbor in neighbors.iter() {
                                    graph.connect(
                                        neighbor,
                                        neighbor, // self-edge carries the label
                                        format!("echo:{}", echo),
                                        0.1, // low weight — it's a ghost
                                        EdgeKind::Semantic,
                                    );
                                    report.ghost_echoes += 1;
                                }
                            } else {
                                // Orphan ghost: no neighbors, but echo was extracted
                                report.ghost_echoes += 1;
                            }
                        }
                    }
                }
            }
            store.remove(id);
            graph.remove_node(id);
            report.pruned += 1;
        }
    }

    report
}

/// Select a subset of IDs using entropy bits to determine inclusion.
/// At intensity 1.0 all are selected. At 0.0 none are.
/// When entropy_seed is empty, falls back to deterministic threshold selection.
fn select_by_entropy(ids: &[u32], intensity: f32, entropy_seed: &[u8]) -> Vec<u32> {
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

/// Group active memories by vector similarity for consolidation.
fn find_consolidation_groups(engine: &Engine, active_ids: &[u32]) -> Vec<Vec<u32>> {
    let mut groups: Vec<Vec<u32>> = Vec::new();
    let mut assigned: HashSet<u32> = HashSet::new();

    for &id in active_ids {
        if assigned.contains(&id) {
            continue;
        }
        let Some(row) = engine.get(id) else { continue };
        if row.vector.is_empty() {
            continue;
        }

        let mut group = vec![id];
        assigned.insert(id);

        for &other_id in active_ids {
            if assigned.contains(&other_id) || other_id == id {
                continue;
            }
            let Some(other_row) = engine.get(other_id) else {
                continue;
            };
            if other_row.vector.len() != row.vector.len() {
                continue;
            }
            let sim = cosine_sim(&row.vector, &other_row.vector);
            if sim >= CONSOLIDATION_THRESHOLD {
                group.push(other_id);
                assigned.insert(other_id);
            }
        }

        if group.len() >= 2 {
            groups.push(group);
        }
    }

    groups
}

/// Merge a group: highest-fidelity member absorbs the rest.
/// Returns the number of new graph edges created.
fn consolidate_group(store: &mut MemoryStore, graph: &mut MemoryGraph, group: &[u32]) -> u32 {
    if group.is_empty() {
        return 0;
    }

    // Survivor = highest fidelity
    let survivor_id = group
        .iter()
        .filter_map(|&id| store.get(id).map(|r| (id, r.fidelity)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id);
    let Some(survivor_id) = survivor_id else {
        return 0;
    };

    let absorbed: Vec<u32> = group
        .iter()
        .copied()
        .filter(|&id| id != survivor_id)
        .collect();

    let mut edges_created = 0u32;

    // Update survivor metadata
    if let Some(survivor) = store.get_mut(survivor_id) {
        survivor.consolidation_depth += 1;
        survivor.importance += absorbed.len() as f32 * 0.1;
        survivor.provenance = Provenance::Consolidated {
            from: group.to_vec(),
        };
    }

    // Transfer absorbed graph edges to survivor, then archive absorbed records
    for &absorbed_id in &absorbed {
        let neighbors = graph.neighbors(absorbed_id);
        for neighbor in neighbors.iter() {
            if neighbor != survivor_id {
                graph.connect(survivor_id, neighbor, "consolidated".to_string(), 0.5, EdgeKind::Semantic);
                edges_created += 1;
            }
        }
        graph.remove_node(absorbed_id);

        // Audit edge: survivor absorbed this memory (created after remove_node
        // so it isn't cleaned up by the cascade).
        // CAUSAL: directed from survivor → absorbed. The absorbed memory
        // cannot traverse backward to re-enter the survivor's context.
        graph.connect(survivor_id, absorbed_id, "absorbed".to_string(), 1.0, EdgeKind::Causal);
        edges_created += 1;

        if let Some(record) = store.get_mut(absorbed_id) {
            record.forgive();
            record.archive();
        }
    }

    edges_created
}

/// Deathbed confession: invert a dying memory's vector to text via shivvr,
/// then re-embed the extracted text and check round-trip fidelity.
/// Returns the extracted text only if it passes a minimum quality threshold.
fn extract_ghost_echo(shivvr_url: &str, vector: &[f32]) -> Option<String> {
    // Step 1: Invert the vector to approximate text.
    let extracted = inversion::invert_vector(shivvr_url, vector)?;
    if extracted.trim().is_empty() {
        return None;
    }

    // Step 2: Re-embed the extracted text and check fidelity.
    // POST to shivvr /embed to get the round-trip vector.
    let body = serde_json::json!({ "text": extracted }).to_string();
    let response = inversion::http_request(shivvr_url, "POST", "/embed", Some(&body))?;
    let val: serde_json::Value = serde_json::from_str(&response).ok()?;
    let re_embedded: Vec<f32> = val.get("embedding")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())?;

    if re_embedded.len() != vector.len() {
        return None;
    }

    // Step 3: Cosine similarity between original vector and round-trip vector.
    let fidelity = cosine_sim(vector, &re_embedded);

    // Ghost threshold: the echo must retain at least 0.5 similarity to be worth keeping.
    // This is deliberately lower than the normal fidelity gate — it's a ghost, not a memory.
    if fidelity >= 0.5 {
        Some(extracted)
    } else {
        None
    }
}

/// Phase 3.5: Discover semantic edges between high-fidelity active memories.
/// Finds pairs with cosine similarity in [0.7, CONSOLIDATION_THRESHOLD) that
/// aren't already connected. Capped at `max_edges` new edges per dream.
fn discover_semantic_edges(
    store: &MemoryStore,
    engine: &Engine,
    graph: &mut MemoryGraph,
    max_edges: u32,
) -> u32 {
    // Top 20 active memories by fidelity
    let mut candidates: Vec<(u32, f32)> = store
        .in_state(LifecycleState::Active)
        .into_iter()
        .map(|r| (r.id, r.fidelity))
        .collect();
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    candidates.truncate(20);

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
fn review_keystones(store: &mut MemoryStore) -> u32 {
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

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
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
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::memory::{FIDELITY_GATE, MemoryRecord};
    use crate::model::Row;

    fn make_row(id: u32, vector: Vec<f32>) -> Row {
        Row {
            id,
            tags: BTreeMap::new(),
            vector,
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
    fn dream_forgives_below_gate() {
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
        // Decay ticks first (reduces fidelity further), then forgive
        assert!(report.forgiven >= 1);
        assert_eq!(store.get(1).unwrap().state, LifecycleState::Forgiven);
    }

    #[test]
    fn dream_consolidates_similar_vectors() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        // Two nearly identical vectors
        engine.upsert(make_row(1, vec![1.0, 0.0, 0.0])).unwrap();
        engine.upsert(make_row(2, vec![0.99, 0.01, 0.0])).unwrap();
        // One different vector
        engine.upsert(make_row(3, vec![0.0, 0.0, 1.0])).unwrap();

        store.insert(make_record(1));
        store.insert(make_record(2));
        store.insert(make_record(3));

        let report = dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);
        assert!(report.consolidated >= 2);
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
    fn consolidation_preserves_graph_edges() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut skg = SkgState::new();
        let prime_tree = PrimeTree::new();

        engine.upsert(make_row(1, vec![1.0, 0.0, 0.0])).unwrap();
        engine.upsert(make_row(2, vec![0.99, 0.01, 0.0])).unwrap();
        engine.upsert(make_row(3, vec![0.0, 0.0, 1.0])).unwrap();

        store.insert(make_record(1));
        store.insert(make_record(2));
        store.insert(make_record(3));

        // Memory 2 is connected to memory 3
        graph.connect(2, 3, "link".into(), 1.0, EdgeKind::Semantic);

        dream_cycle(&mut store, &engine, &mut graph, &mut skg, &prime_tree, None);

        // After consolidation, survivor (1 or 2) should be connected to 3
        let survivor = if store
            .get(1)
            .map_or(false, |r| r.state == LifecycleState::Active)
        {
            1
        } else {
            2
        };
        let neighbors = graph.neighbors(survivor);
        assert!(neighbors.contains(3));
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

        let report = dream_cycle_with_intensity(&mut store, &engine, &mut graph, &mut skg, &prime_tree, 0.5, &[], None);
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

        assert_eq!(report.halo_touched, 1, "exactly one neighbor should be halo'd");

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
            dream_cycle(&mut store_haloed, &engine_haloed, &mut graph_haloed, &mut skg_haloed, &pt, None);
            dream_cycle(&mut store_ctrl, &engine_ctrl, &mut graph_ctrl, &mut skg_ctrl, &pt, None);
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
