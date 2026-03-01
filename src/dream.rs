use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::graph::MemoryGraph;
use crate::inversion;
use crate::memory::{LifecycleState, MemoryStore, Provenance};

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
    chonk_url: Option<&str>,
) -> DreamReport {
    dream_cycle_with_intensity(store, engine, graph, 1.0, &[], chonk_url)
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
    intensity: f32,
    entropy_seed: &[u8],
    chonk_url: Option<&str>,
) -> DreamReport {
    let mut report = DreamReport::default();
    let intensity = intensity.clamp(0.0, 1.0);

    let ids: Vec<u32> = store.iter().map(|(&id, _)| id).collect();

    // Capture pre-existing forgiven IDs before any transitions this cycle.
    let pre_forgiven: Vec<u32> = store
        .in_state(LifecycleState::Forgiven)
        .into_iter()
        .map(|r| r.id)
        .collect();

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
            report.ticks += 1;
        }
    }

    // Phase 2: Lifecycle — Active below fidelity gate → Forgiven.
    for &id in &ids {
        if let Some(record) = store.get_mut(id) {
            if record.state == LifecycleState::Active && !record.above_gate() {
                record.forgive();
                report.forgiven += 1;
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
            consolidate_group(store, graph, &group);
            report.consolidated += count;
        }
    }

    // Phase 4: Neglect — grow alpha for stale memories.
    for &id in &ids {
        if let Some(record) = store.get_mut(id) {
            if record.state == LifecycleState::Active && record.staleness() > NEGLECT_SECONDS {
                record.on_neglect();
            }
        }
    }

    // Phase 5: Keystone review.
    report.keystones_reviewed = store.keystones().len() as u32;

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
            if let Some(url) = chonk_url {
                if let Some(row) = engine.get(id) {
                    if !row.vector.is_empty() {
                        let neighbors = graph.neighbors(id);
                        if !neighbors.is_empty() {
                            if let Some(echo) = extract_ghost_echo(url, &row.vector) {
                                // Attach the echo as labeled edges to surviving neighbors.
                                for neighbor in neighbors.iter() {
                                    graph.connect(
                                        neighbor,
                                        neighbor, // self-edge carries the label
                                        format!("echo:{}", echo),
                                        0.1, // low weight — it's a ghost
                                    );
                                    report.ghost_echoes += 1;
                                }
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
fn consolidate_group(store: &mut MemoryStore, graph: &mut MemoryGraph, group: &[u32]) {
    if group.is_empty() {
        return;
    }

    // Survivor = highest fidelity
    let survivor_id = group
        .iter()
        .filter_map(|&id| store.get(id).map(|r| (id, r.fidelity)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id);
    let Some(survivor_id) = survivor_id else {
        return;
    };

    let absorbed: Vec<u32> = group
        .iter()
        .copied()
        .filter(|&id| id != survivor_id)
        .collect();

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
                graph.connect(survivor_id, neighbor, "consolidated".to_string(), 0.5);
            }
        }
        graph.remove_node(absorbed_id);

        if let Some(record) = store.get_mut(absorbed_id) {
            record.forgive();
            record.archive();
        }
    }
}

/// Deathbed confession: invert a dying memory's vector to text via chonk,
/// then re-embed the extracted text and check round-trip fidelity.
/// Returns the extracted text only if it passes a minimum quality threshold.
fn extract_ghost_echo(chonk_url: &str, vector: &[f32]) -> Option<String> {
    // Step 1: Invert the vector to approximate text.
    let extracted = inversion::invert_vector(chonk_url, vector)?;
    if extracted.trim().is_empty() {
        return None;
    }

    // Step 2: Re-embed the extracted text and check fidelity.
    // POST to chonk /embed to get the round-trip vector.
    let body = serde_json::json!({ "text": extracted }).to_string();
    let response = inversion_post(chonk_url, "/embed", &body)?;
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

/// POST helper for chonk (re-embed during ghost extraction).
fn inversion_post(base_url: &str, path: &str, body: &str) -> Option<String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let stripped = base_url.strip_prefix("http://").unwrap_or(base_url);
    let (host, port) = if let Some(colon) = stripped.rfind(':') {
        let h = &stripped[..colon];
        let p = stripped[colon + 1..].trim_end_matches('/').parse::<u16>().ok()?;
        (h.to_string(), p)
    } else {
        (stripped.trim_end_matches('/').to_string(), 8080u16)
    };

    let addr = format!("{host}:{port}");
    let mut stream = TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(2000)).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(5000))).ok()?;

    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let body_start = response.find("\r\n\r\n").map(|i| i + 4)?;
    Some(response[body_start..].to_string())
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

        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        store.insert(make_record(1));

        let report = dream_cycle(&mut store, &engine, &mut graph, None);
        assert_eq!(report.decayed, 1);
        assert!(store.get(1).unwrap().fidelity < 1.0);
    }

    #[test]
    fn dream_forgives_below_gate() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();

        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        let mut r = make_record(1);
        r.fidelity = FIDELITY_GATE - 0.01; // just below gate
        store.insert(r);

        let report = dream_cycle(&mut store, &engine, &mut graph, None);
        // Decay ticks first (reduces fidelity further), then forgive
        assert!(report.forgiven >= 1);
        assert_eq!(store.get(1).unwrap().state, LifecycleState::Forgiven);
    }

    #[test]
    fn dream_consolidates_similar_vectors() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();

        // Two nearly identical vectors
        engine.upsert(make_row(1, vec![1.0, 0.0, 0.0])).unwrap();
        engine.upsert(make_row(2, vec![0.99, 0.01, 0.0])).unwrap();
        // One different vector
        engine.upsert(make_row(3, vec![0.0, 0.0, 1.0])).unwrap();

        store.insert(make_record(1));
        store.insert(make_record(2));
        store.insert(make_record(3));

        let report = dream_cycle(&mut store, &engine, &mut graph, None);
        assert!(report.consolidated >= 2);
    }

    #[test]
    fn dream_skips_keystones() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();

        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        let mut r = make_record(1);
        r.keystone = true;
        store.insert(r);

        dream_cycle(&mut store, &engine, &mut graph, None);
        // Keystone should remain at full fidelity
        assert_eq!(store.get(1).unwrap().fidelity, 1.0);
    }

    #[test]
    fn consolidation_preserves_graph_edges() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();

        engine.upsert(make_row(1, vec![1.0, 0.0, 0.0])).unwrap();
        engine.upsert(make_row(2, vec![0.99, 0.01, 0.0])).unwrap();
        engine.upsert(make_row(3, vec![0.0, 0.0, 1.0])).unwrap();

        store.insert(make_record(1));
        store.insert(make_record(2));
        store.insert(make_record(3));

        // Memory 2 is connected to memory 3
        graph.connect(2, 3, "link".into(), 1.0);

        dream_cycle(&mut store, &engine, &mut graph, None);

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

        for i in 1..=4 {
            engine.upsert(make_row(i, vec![i as f32, 0.0])).unwrap();
            store.insert(make_record(i));
        }

        let report = dream_cycle_with_intensity(&mut store, &engine, &mut graph, 0.5, &[], None);
        // ceil(4 * 0.5) = 2 should be decayed
        assert_eq!(report.decayed, 2);
    }

    #[test]
    fn dream_full_intensity_matches_original() {
        let mut engine = Engine::new();
        let mut store = MemoryStore::new();
        let mut graph = MemoryGraph::new();

        engine.upsert(make_row(1, vec![1.0, 0.0])).unwrap();
        store.insert(make_record(1));

        // dream_cycle wraps dream_cycle_with_intensity at 1.0
        let report = dream_cycle(&mut store, &engine, &mut graph, None);
        assert_eq!(report.decayed, 1);
        assert!(store.get(1).unwrap().fidelity < 1.0);
    }
}
