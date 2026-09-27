//! Bhāvanā — the nightly cycle, source-preserving (Addendum §A.1.1).
//!
//! * Decay lowers ojā (fidelity): retrieval priority only. Text stays.
//! * Records below the gate become [`ReleaseProposal`]s. State is untouched
//!   until a deliberate [`ReleaseDecision`] is applied.
//! * Consolidation builds a [`ClusterIndex`] that points at raw members.
//!   Members keep text, state, vector and every graph link; only their
//!   `consolidation_depth` rises (α falls: "becoming structure").
//! * Nothing is pruned. Ever. The cycle fails if the store or graph shrinks.
//!
//! Cognition changes lifecycle *state* only. Text removal on upekkhā and
//! nimitta removal on nirodha are Engine's commit, keyed by id; every
//! `ReleaseApplied` entry says `text_purge_pending: true` for that reason.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ferricula_core::engine::Engine;
use ferricula_core::graph::{EdgeKind, MemoryGraph};
use ferricula_core::memory::{LifecycleState, MemoryStore, now_epoch};
use ferricula_core::prime_tree::PrimeTree;
use ferricula_core::skg::{SkgState, SkgUpdateSummary};

use crate::dream::{
    DreamImageryCandidate, activation_roles, cosine_sim, discover_semantic_edges, env_u32, env_usize,
    find_similarity_groups, review_keystones, select_by_entropy, select_dream_imagery_candidates,
};
use crate::gates::MergeGate;
use crate::karmic::{KarmicEntry, KarmicEvent, KarmicSink};
use crate::scope::AgentId;

pub const BHAVANA_OPERATOR_VERSION: &str = "bhavana/2.0.0-alpha.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseMode {
    /// The only mode. There is no variant that releases automatically.
    ProposeOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BhavanaPolicy {
    /// Cosine threshold for cluster candidates (parity with dream.rs: 0.85).
    pub consolidation_threshold: f32,
    /// Plan §2: "Cosine clusters n ≥ 3". Coordinator: 2 only as explicit legacy/test policy.
    pub min_cluster: usize,
    pub neglect_seconds: u64,
    /// Clusters with `contradiction >= tau` go to review (plan §4.3).
    pub contradiction_review_tau: f32,
    /// Clusters with `same_truth < min` go to review.
    pub same_truth_min: f32,
    /// Forgiven records idle this long are proposed for nirodha.
    pub forgiven_stale_seconds: u64,
    pub release: ReleaseMode,
}

impl Default for BhavanaPolicy {
    fn default() -> Self {
        Self {
            consolidation_threshold: 0.85,
            min_cluster: 3,
            neglect_seconds: 86_400,
            contradiction_review_tau: 0.5,
            same_truth_min: 0.5,
            forgiven_stale_seconds: 3600,
            release: ReleaseMode::ProposeOnly,
        }
    }
}

impl BhavanaPolicy {
    /// Explicit legacy/test policy: pairs may form clusters.
    pub fn legacy_pairs() -> Self {
        Self {
            min_cluster: 2,
            ..Self::default()
        }
    }
}

/// A cluster node that POINTS at raw members.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterIndex {
    /// Stable: hash of the sorted member ids and the operator version.
    pub cluster_id: u64,
    pub members: Vec<u32>,
    /// ojā per member at build time.
    pub weights: Vec<f32>,
    /// Σ(ojā · nimitta) / Σ ojā over members; retrieval aid only.
    pub centroid: Vec<f32>,
    pub mean_cosine: f32,
    /// From a calibrated sankhāra-merge gate. `None` when routed to review.
    pub same_truth: Option<f32>,
    pub contradiction: Option<f32>,
    pub gate_version: Option<String>,
    pub operator_version: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseKind {
    /// Active → Forgiven. Text → ∅ (Engine), nimitta kept.
    Upekkha,
    /// Forgiven → Archived. Text and nimitta → ∅ (Engine), karmic log kept.
    Nirodha,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ReleaseReason {
    BelowGate { fidelity: f32 },
    ForgivenStale { seconds: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseProposal {
    pub id: u32,
    pub kind: ReleaseKind,
    pub reason: ReleaseReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseDecision {
    pub id: u32,
    pub kind: ReleaseKind,
    pub approved: bool,
    /// Who decided: "operator:<name>" | "overlay:<event id>" | ...
    pub decided_by: String,
}

/// Durable cognition state the integrator persists between cycles.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BhavanaState {
    pub clusters: BTreeMap<u64, ClusterIndex>,
    /// member id → cluster id
    pub membership: BTreeMap<u32, u64>,
    /// Clusters already routed to review (logged once).
    pub reviewed: BTreeSet<u64>,
}

/// Report from one cycle. Legacy `forgiven`, `archived` and `pruned` are
/// kept for readers of the old `DreamReport` and are always 0: the cycle
/// proposes releases and never prunes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BhavanaReport {
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
    pub decayed_ids: Vec<u32>,
    pub forgiven_ids: Vec<u32>,
    pub consolidated_ids: Vec<u32>,
    pub halo_touched: u32,
    #[serde(default)]
    pub dream_imagery_candidates: Vec<DreamImageryCandidate>,
    // --- Addendum A additions ---
    #[serde(default)]
    pub clusters: Vec<ClusterIndex>,
    #[serde(default)]
    pub review_clusters: Vec<ClusterIndex>,
    #[serde(default)]
    pub clusters_unchanged: u32,
    #[serde(default)]
    pub release_proposals: Vec<ReleaseProposal>,
    #[serde(default)]
    pub karmic_entries: u32,
}

/// Run one source-preserving cycle. Fails if the karmic sink fails or if
/// the store/graph would shrink.
#[allow(clippy::too_many_arguments)]
pub fn bhavana_cycle(
    store: &mut MemoryStore,
    engine: &Engine,
    graph: &mut MemoryGraph,
    skg: &mut SkgState,
    prime_tree: &PrimeTree,
    state: &mut BhavanaState,
    policy: &BhavanaPolicy,
    merge_gate: Option<&dyn MergeGate>,
    agent: &AgentId,
    intensity: f32,
    entropy_seed: &[u8],
    sink: &mut dyn KarmicSink,
) -> Result<BhavanaReport> {
    let mut report = BhavanaReport::default();
    let intensity = intensity.clamp(0.0, 1.0);
    let store_len_before = store.len();
    let graph_nodes_before = graph.node_count();
    let graph_edges_before = graph.edge_count();

    let active_roles = activation_roles(intensity);
    report.active_archetypes = active_roles.clone();
    let ids: Vec<u32> = store.iter().map(|(&id, _)| id).collect();

    // Phase 0: keystone halo.
    let halo_set: HashSet<u32> = {
        let keystone_ids: Vec<u32> = store.keystones().iter().map(|r| r.id).collect();
        let mut set = HashSet::new();
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

    // Phase 1: decay tick — priority only.
    let decay_candidates: Vec<u32> = ids
        .iter()
        .copied()
        .filter(|&id| store.get(id).is_some_and(|r| r.state == LifecycleState::Active && !r.keystone))
        .collect();
    for &id in &select_by_entropy(&decay_candidates, intensity, entropy_seed) {
        if let Some(record) = store.get_mut(id) {
            record.decay_tick();
            report.decayed += 1;
            report.decayed_ids.push(id);
            report.ticks += 1;
        }
    }

    // Phase 2: release proposals. State untouched.
    for &id in &ids {
        let Some(record) = store.get(id) else { continue };
        let proposal = if record.state == LifecycleState::Active && !record.keystone && !record.above_gate() {
            Some(ReleaseProposal {
                id,
                kind: ReleaseKind::Upekkha,
                reason: ReleaseReason::BelowGate {
                    fidelity: record.fidelity,
                },
            })
        } else if record.state == LifecycleState::Forgiven && record.staleness() > policy.forgiven_stale_seconds {
            Some(ReleaseProposal {
                id,
                kind: ReleaseKind::Nirodha,
                reason: ReleaseReason::ForgivenStale {
                    seconds: record.staleness(),
                },
            })
        } else {
            None
        };
        if let Some(p) = proposal {
            log(sink, &mut report, agent, None, KarmicEvent::ReleaseProposed { proposal: p.clone() })?;
            report.release_proposals.push(p);
        }
    }

    // Phase 3: consolidation as index.
    let active_ids: Vec<u32> = store.in_state(LifecycleState::Active).into_iter().map(|r| r.id).collect();
    let groups = find_similarity_groups(engine, &active_ids, policy.consolidation_threshold, policy.min_cluster);
    for mut group in groups {
        group.sort_unstable();
        let cluster_id = cluster_id_for(&group);
        if state.clusters.contains_key(&cluster_id) {
            report.clusters_unchanged += 1;
            continue;
        }
        let vectors: Vec<(u32, Vec<f32>, f32)> = group
            .iter()
            .filter_map(|&id| {
                let row = engine.get(id)?;
                let oja = store.get(id)?.fidelity;
                Some((id, row.vector.clone(), oja))
            })
            .collect();
        let mean_cosine = mean_pairwise_cosine(&vectors);
        let mut index = ClusterIndex {
            cluster_id,
            members: group.clone(),
            weights: vectors.iter().map(|v| v.2).collect(),
            centroid: weighted_centroid(&vectors),
            mean_cosine,
            same_truth: None,
            contradiction: None,
            gate_version: None,
            operator_version: BHAVANA_OPERATOR_VERSION.to_string(),
            created_at: now_epoch(),
        };

        let review_reason = judge_cluster(store, engine, merge_gate, agent, policy, &group, &mut index);
        match review_reason {
            Some(reason) => {
                if state.reviewed.insert(cluster_id) {
                    log(
                        sink,
                        &mut report,
                        agent,
                        index.gate_version.clone(),
                        KarmicEvent::ClusterReview {
                            cluster_id,
                            members: group.clone(),
                            mean_cosine,
                            reason,
                        },
                    )?;
                }
                report.review_clusters.push(index);
            }
            None => {
                // Idempotent membership: only members joining new structure deepen.
                let mut newly_joined = Vec::new();
                let mut superseded = BTreeSet::new();
                for &m in &group {
                    match state.membership.get(&m) {
                        Some(old) if state.clusters.get(old).is_some_and(|c| c.members.iter().all(|x| group.contains(x))) => {
                            superseded.insert(*old);
                        }
                        _ => newly_joined.push(m),
                    }
                }
                for old in superseded {
                    state.clusters.remove(&old);
                }
                for &m in &group {
                    state.membership.insert(m, cluster_id);
                    if newly_joined.contains(&m) {
                        if let Some(record) = store.get_mut(m) {
                            record.consolidation_depth += 1;
                        }
                    }
                }
                // Member ↔ member links; never overwrite an existing edge.
                for i in 0..group.len() {
                    for j in (i + 1)..group.len() {
                        let (a, b) = (group[i], group[j]);
                        if graph.edge(a, b).is_none() {
                            let sim = pair_cosine(&vectors, a, b);
                            graph.connect(a, b, format!("co-member:{cluster_id}"), sim, EdgeKind::Semantic);
                            report.edges_created += 1;
                        }
                    }
                }
                report.consolidated += group.len() as u32;
                report.consolidated_ids.extend_from_slice(&group);
                log(
                    sink,
                    &mut report,
                    agent,
                    index.gate_version.clone(),
                    KarmicEvent::ClusterBuilt {
                        cluster_id,
                        members: group.clone(),
                        same_truth: index.same_truth.unwrap_or(0.0),
                        contradiction: index.contradiction.unwrap_or(0.0),
                        newly_joined,
                    },
                )?;
                state.clusters.insert(cluster_id, index.clone());
                report.clusters.push(index);
            }
        }
    }

    // Phase 3.5: semantic edge discovery.
    if active_roles.iter().any(|r| r == "Intuition") {
        let max_edges = env_u32("EDGE_MAX_PER_DREAM", 12);
        report.edges_created += discover_semantic_edges(store, engine, graph, max_edges, entropy_seed);
    }

    // Phase 3.6: SKG Weber update.
    report.skg_summary = skg.update(prime_tree, store, engine, 20);

    // Phase 4: neglect.
    for &id in &ids {
        if let Some(record) = store.get_mut(id) {
            if record.state == LifecycleState::Active && record.staleness() > policy.neglect_seconds {
                record.on_neglect();
            }
        }
    }

    // Phase 5: keystone review.
    report.keystones_reviewed = store.keystones().len() as u32;
    if active_roles.iter().any(|r| r == "Ethics") {
        report.keystones_promoted = review_keystones(store);
    }

    // Phase 5.5: dream imagery candidates.
    if active_roles.iter().any(|r| r == "Intuition") {
        let top_n = env_usize("DREAM_IMAGERY_TOP_N", 2);
        report.dream_imagery_candidates = select_dream_imagery_candidates(&report.skg_summary, top_n);
    }

    // Phase 6 (old: archive + prune) does not exist. Invariants:
    ensure!(store.len() == store_len_before, "bhavana must not change the record count");
    ensure!(graph.node_count() >= graph_nodes_before, "bhavana must not remove graph nodes");
    ensure!(graph.edge_count() >= graph_edges_before, "bhavana must not remove graph edges");
    Ok(report)
}

/// Returns `Some(reason)` when the cluster must go to review.
fn judge_cluster(
    store: &MemoryStore,
    engine: &Engine,
    merge_gate: Option<&dyn MergeGate>,
    agent: &AgentId,
    policy: &BhavanaPolicy,
    group: &[u32],
    index: &mut ClusterIndex,
) -> Option<String> {
    if group.iter().any(|&id| store.get(id).is_some_and(|r| r.keystone)) {
        return Some("anchor member: anchors are never merged without review".into());
    }
    let Some(gate) = merge_gate else {
        return Some("no sankhāra-merge gate: cosine is a similarity heuristic, not a calibrated same_truth".into());
    };
    let texts: Option<Vec<String>> = group
        .iter()
        .map(|&id| engine.get(id).and_then(|row| row.tags.get("text").cloned()))
        .collect();
    let Some(texts) = texts else {
        return Some("member without text: cannot judge same_truth".into());
    };
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let judged = gate.judge(agent, &refs);
    index.gate_version = Some(judged.provenance.gate_version.clone());
    match judged.calibrated_answer() {
        Err(e) => Some(format!("invalid merge verdict: {e}")),
        Ok(None) => Some(if judged.provenance.calibrated {
            "merge gate abstained or state truncated".into()
        } else {
            "merge gate not calibrated: no lifecycle change before measured calibration".into()
        }),
        Ok(Some(v)) => {
            index.same_truth = Some(v.same_truth);
            index.contradiction = Some(v.contradiction);
            if v.contradiction >= policy.contradiction_review_tau {
                Some(format!("contradiction {:.2} >= {:.2}: contradictory clusters stay apart", v.contradiction, policy.contradiction_review_tau))
            } else if v.same_truth < policy.same_truth_min {
                Some(format!("same_truth {:.2} < {:.2}", v.same_truth, policy.same_truth_min))
            } else {
                None
            }
        }
    }
}

/// Outcome of applying deliberate release decisions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReleaseOutcome {
    pub applied: Vec<u32>,
    pub declined: Vec<u32>,
    /// (id, why) — e.g. nirodha requested on an Active record.
    pub invalid: Vec<(u32, String)>,
    /// Ids whose text/nimitta purge Engine must still commit. Always equals `applied`.
    pub text_purge_pending: Vec<u32>,
}

/// The only path that changes lifecycle state. Changes `MemoryRecord.state`
/// only; it does NOT purge text or vectors and never claims to.
pub fn apply_release(
    store: &mut MemoryStore,
    decisions: &[ReleaseDecision],
    agent: &AgentId,
    sink: &mut dyn KarmicSink,
) -> Result<ReleaseOutcome> {
    let mut out = ReleaseOutcome::default();
    for d in decisions {
        if !d.approved {
            sink.append(KarmicEntry::now(
                "bhavana",
                KarmicEvent::ReleaseDeclined { decision: d.clone() },
                None,
                true,
                Some(agent),
            ))?;
            out.declined.push(d.id);
            continue;
        }
        let Some(record) = store.get_mut(d.id) else {
            out.invalid.push((d.id, "unknown record".into()));
            continue;
        };
        let ok = match d.kind {
            ReleaseKind::Upekkha => record.forgive(),
            ReleaseKind::Nirodha => record.archive(),
        };
        if !ok {
            out.invalid.push((d.id, format!("{:?} not applicable in state {:?}", d.kind, record.state)));
            continue;
        }
        sink.append(KarmicEntry::now(
            "bhavana",
            KarmicEvent::ReleaseApplied {
                decision: d.clone(),
                text_purge_pending: true,
            },
            None,
            true,
            Some(agent),
        ))?;
        out.applied.push(d.id);
        out.text_purge_pending.push(d.id);
    }
    Ok(out)
}

fn log(
    sink: &mut dyn KarmicSink,
    report: &mut BhavanaReport,
    agent: &AgentId,
    gate_version: Option<String>,
    event: KarmicEvent,
) -> Result<()> {
    sink.append(KarmicEntry::now("bhavana", event, gate_version, true, Some(agent)))?;
    report.karmic_entries += 1;
    Ok(())
}

pub fn cluster_id_for(sorted_members: &[u32]) -> u64 {
    let mut h = Sha256::new();
    for m in sorted_members {
        h.update(m.to_le_bytes());
    }
    h.update(BHAVANA_OPERATOR_VERSION.as_bytes());
    let d = h.finalize();
    u64::from_le_bytes(d[..8].try_into().expect("8 bytes"))
}

fn weighted_centroid(vectors: &[(u32, Vec<f32>, f32)]) -> Vec<f32> {
    let Some(dim) = vectors.first().map(|v| v.1.len()) else {
        return Vec::new();
    };
    let mut acc = vec![0f32; dim];
    let mut total = 0f32;
    for (_, v, w) in vectors {
        if v.len() != dim {
            continue;
        }
        let w = w.max(0.0);
        for (a, x) in acc.iter_mut().zip(v) {
            *a += w * x;
        }
        total += w;
    }
    if total > f32::EPSILON {
        for a in &mut acc {
            *a /= total;
        }
    }
    acc
}

fn mean_pairwise_cosine(vectors: &[(u32, Vec<f32>, f32)]) -> f32 {
    let mut sum = 0f32;
    let mut n = 0f32;
    for i in 0..vectors.len() {
        for j in (i + 1)..vectors.len() {
            sum += cosine_sim(&vectors[i].1, &vectors[j].1);
            n += 1.0;
        }
    }
    if n > 0.0 { sum / n } else { 0.0 }
}

fn pair_cosine(vectors: &[(u32, Vec<f32>, f32)], a: u32, b: u32) -> f32 {
    let va = vectors.iter().find(|v| v.0 == a);
    let vb = vectors.iter().find(|v| v.0 == b);
    match (va, vb) {
        (Some(x), Some(y)) => cosine_sim(&x.1, &y.1),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::gates::StaticMergeGate;
    use crate::karmic::{FailingSink, VecSink};
    use ferricula_core::memory::{FIDELITY_GATE, MemoryRecord};
    use ferricula_core::model::Row;

    struct World {
        engine: Engine,
        store: MemoryStore,
        graph: MemoryGraph,
        skg: SkgState,
        prime_tree: PrimeTree,
        state: BhavanaState,
        sink: VecSink,
    }

    impl World {
        fn new() -> Self {
            Self {
                engine: Engine::new(),
                store: MemoryStore::new(),
                graph: MemoryGraph::new(),
                skg: SkgState::new(),
                prime_tree: PrimeTree::new(),
                state: BhavanaState::default(),
                sink: VecSink::default(),
            }
        }

        fn add(&mut self, id: u32, vector: Vec<f32>, text: &str) {
            let mut tags = BTreeMap::new();
            tags.insert("text".to_string(), text.to_string());
            self.engine
                .upsert(Row {
                    id,
                    tags,
                    vector,
                    refs: None,
                })
                .unwrap();
            self.store.insert(MemoryRecord::new(id));
        }

        fn run(&mut self, policy: &BhavanaPolicy, gate: Option<&dyn MergeGate>) -> BhavanaReport {
            bhavana_cycle(
                &mut self.store,
                &self.engine,
                &mut self.graph,
                &mut self.skg,
                &self.prime_tree,
                &mut self.state,
                policy,
                gate,
                &agent(),
                1.0,
                &[],
                &mut self.sink,
            )
            .unwrap()
        }

        fn texts(&self) -> BTreeMap<u32, String> {
            self.engine
                .rows_iter()
                .map(|r| (r.id, r.tags.get("text").cloned().unwrap_or_default()))
                .collect()
        }
    }

    fn agent() -> AgentId {
        AgentId::new("agent_test").unwrap()
    }

    fn calibrated_gate() -> StaticMergeGate {
        StaticMergeGate {
            same_truth: 0.9,
            contradiction: 0.05,
            calibrated: true,
        }
    }

    fn similar_triplet(w: &mut World) {
        w.add(1, vec![1.0, 0.0, 0.0], "alpha one");
        w.add(2, vec![0.99, 0.01, 0.0], "alpha two");
        w.add(3, vec![0.98, 0.02, 0.0], "alpha three");
        w.add(4, vec![0.0, 0.0, 1.0], "unrelated");
    }

    #[test]
    fn below_gate_record_stays_active_and_is_proposed() {
        let mut w = World::new();
        w.add(1, vec![1.0, 0.0], "fading");
        w.store.get_mut(1).unwrap().fidelity = FIDELITY_GATE - 0.01;
        let report = w.run(&BhavanaPolicy::default(), None);
        assert_eq!(w.store.get(1).unwrap().state, LifecycleState::Active);
        assert_eq!(report.forgiven, 0);
        assert_eq!(report.release_proposals.len(), 1);
        assert_eq!(report.release_proposals[0].kind, ReleaseKind::Upekkha);
        assert!(matches!(
            w.sink.entries[0].event,
            KarmicEvent::ReleaseProposed { .. }
        ));
    }

    #[test]
    fn zero_fidelity_archived_record_survives_and_store_never_shrinks() {
        let mut w = World::new();
        w.add(1, vec![1.0, 0.0], "ancient");
        {
            let r = w.store.get_mut(1).unwrap();
            r.fidelity = 0.0;
            r.forgive();
            r.archive();
        }
        w.graph.connect(1, 1, "self".into(), 1.0, EdgeKind::Semantic);
        let nodes = w.graph.node_count();
        let report = w.run(&BhavanaPolicy::default(), None);
        assert_eq!(report.pruned, 0);
        assert!(w.store.contains(1));
        assert_eq!(w.store.len(), 1);
        assert_eq!(w.graph.node_count(), nodes);
        assert_eq!(w.texts()[&1], "ancient");
    }

    #[test]
    fn calibrated_gate_builds_index_and_members_keep_everything() {
        let mut w = World::new();
        similar_triplet(&mut w);
        w.graph.connect(2, 4, "link".into(), 1.0, EdgeKind::Semantic);
        let texts_before = w.texts();
        let gate = calibrated_gate();
        let report = w.run(&BhavanaPolicy::default(), Some(&gate));
        assert_eq!(report.clusters.len(), 1);
        let c = &report.clusters[0];
        assert_eq!(c.members, vec![1, 2, 3]);
        assert_eq!(c.same_truth, Some(0.9));
        assert_eq!(c.centroid.len(), 3);
        for id in 1..=3 {
            let r = w.store.get(id).unwrap();
            assert_eq!(r.state, LifecycleState::Active);
            assert_eq!(r.consolidation_depth, 1);
        }
        assert_eq!(w.store.get(4).unwrap().consolidation_depth, 0);
        assert_eq!(w.texts(), texts_before);
        assert!(w.graph.neighbors(2).contains(4), "existing member links preserved");
        assert!(w.graph.neighbors(1).contains(2) && w.graph.neighbors(1).contains(3));
        assert_eq!(w.graph.edge(2, 4).unwrap().label, "link", "existing edge not overwritten");
        assert_eq!(w.state.clusters.len(), 1);
        assert!(w.sink.entries.iter().any(|e| matches!(e.event, KarmicEvent::ClusterBuilt { .. })));
        assert_eq!(report.karmic_entries as usize, w.sink.entries.len());
    }

    #[test]
    fn repeated_cycles_do_not_compound_depth() {
        let mut w = World::new();
        similar_triplet(&mut w);
        let gate = calibrated_gate();
        w.run(&BhavanaPolicy::default(), Some(&gate));
        let r2 = w.run(&BhavanaPolicy::default(), Some(&gate));
        let r3 = w.run(&BhavanaPolicy::default(), Some(&gate));
        assert_eq!(r2.clusters.len(), 0);
        assert_eq!(r3.clusters_unchanged, 1);
        for id in 1..=3 {
            assert_eq!(w.store.get(id).unwrap().consolidation_depth, 1);
        }
        let built = w.sink.entries.iter().filter(|e| matches!(e.event, KarmicEvent::ClusterBuilt { .. })).count();
        assert_eq!(built, 1);
    }

    #[test]
    fn growing_cluster_deepens_only_new_members() {
        let mut w = World::new();
        similar_triplet(&mut w);
        let gate = calibrated_gate();
        w.run(&BhavanaPolicy::default(), Some(&gate));
        w.add(5, vec![0.97, 0.03, 0.0], "alpha five");
        let report = w.run(&BhavanaPolicy::default(), Some(&gate));
        assert_eq!(report.clusters.len(), 1);
        assert_eq!(report.clusters[0].members, vec![1, 2, 3, 5]);
        for id in 1..=3 {
            assert_eq!(w.store.get(id).unwrap().consolidation_depth, 1);
        }
        assert_eq!(w.store.get(5).unwrap().consolidation_depth, 1);
        assert_eq!(w.state.clusters.len(), 1, "superseded cluster removed");
        assert_eq!(w.state.membership[&1], report.clusters[0].cluster_id);
    }

    #[test]
    fn no_gate_or_uncalibrated_or_contradiction_routes_to_review() {
        // no gate
        let mut w = World::new();
        similar_triplet(&mut w);
        let report = w.run(&BhavanaPolicy::default(), None);
        assert!(report.clusters.is_empty());
        assert_eq!(report.review_clusters.len(), 1);
        assert!(report.review_clusters[0].same_truth.is_none());
        assert_eq!(w.store.get(1).unwrap().consolidation_depth, 0);
        assert_eq!(report.consolidated, 0);
        // review logged once across cycles
        w.run(&BhavanaPolicy::default(), None);
        let reviews = w.sink.entries.iter().filter(|e| matches!(e.event, KarmicEvent::ClusterReview { .. })).count();
        assert_eq!(reviews, 1);

        // uncalibrated gate
        let mut w = World::new();
        similar_triplet(&mut w);
        let gate = StaticMergeGate {
            same_truth: 0.95,
            contradiction: 0.0,
            calibrated: false,
        };
        let report = w.run(&BhavanaPolicy::default(), Some(&gate));
        assert!(report.clusters.is_empty());
        assert_eq!(report.review_clusters.len(), 1);
        assert_eq!(w.store.get(1).unwrap().consolidation_depth, 0);

        // contradiction
        let mut w = World::new();
        similar_triplet(&mut w);
        let gate = StaticMergeGate {
            same_truth: 0.95,
            contradiction: 0.9,
            calibrated: true,
        };
        let report = w.run(&BhavanaPolicy::default(), Some(&gate));
        assert!(report.clusters.is_empty());
        assert_eq!(report.review_clusters[0].contradiction, Some(0.9));
        assert_eq!(w.store.get(1).unwrap().consolidation_depth, 0);
    }

    #[test]
    fn anchor_member_forces_review_and_pairs_need_legacy_policy() {
        let mut w = World::new();
        similar_triplet(&mut w);
        w.store.get_mut(2).unwrap().keystone = true;
        let gate = calibrated_gate();
        let report = w.run(&BhavanaPolicy::default(), Some(&gate));
        assert!(report.clusters.is_empty());
        assert_eq!(report.review_clusters.len(), 1);

        let mut w = World::new();
        w.add(1, vec![1.0, 0.0], "a");
        w.add(2, vec![0.99, 0.01], "b");
        let gate = calibrated_gate();
        assert!(w.run(&BhavanaPolicy::default(), Some(&gate)).clusters.is_empty());
        assert_eq!(w.run(&BhavanaPolicy::legacy_pairs(), Some(&gate)).clusters.len(), 1);
    }

    #[test]
    fn apply_release_changes_state_only_and_reports_purge_pending() {
        let mut w = World::new();
        w.add(1, vec![1.0, 0.0], "keep my bytes");
        w.add(2, vec![0.0, 1.0], "declined");
        let decisions = vec![
            ReleaseDecision {
                id: 1,
                kind: ReleaseKind::Upekkha,
                approved: true,
                decided_by: "operator:test".into(),
            },
            ReleaseDecision {
                id: 2,
                kind: ReleaseKind::Upekkha,
                approved: false,
                decided_by: "operator:test".into(),
            },
            ReleaseDecision {
                id: 2,
                kind: ReleaseKind::Nirodha,
                approved: true,
                decided_by: "operator:test".into(),
            },
        ];
        let out = apply_release(&mut w.store, &decisions, &agent(), &mut w.sink).unwrap();
        assert_eq!(out.applied, vec![1]);
        assert_eq!(out.declined, vec![2]);
        assert_eq!(out.invalid.len(), 1, "nirodha on an Active record is invalid");
        assert_eq!(out.text_purge_pending, vec![1]);
        assert_eq!(w.store.get(1).unwrap().state, LifecycleState::Forgiven);
        assert_eq!(w.store.get(2).unwrap().state, LifecycleState::Active);
        assert_eq!(w.texts()[&1], "keep my bytes");
        assert!(w.sink.entries.iter().any(|e| matches!(
            e.event,
            KarmicEvent::ReleaseApplied {
                text_purge_pending: true,
                ..
            }
        )));
        assert!(w.sink.entries.iter().any(|e| matches!(e.event, KarmicEvent::ReleaseDeclined { .. })));
    }

    #[test]
    fn sink_failure_aborts_the_cycle() {
        let mut w = World::new();
        w.add(1, vec![1.0, 0.0], "fading");
        w.store.get_mut(1).unwrap().fidelity = 0.1;
        let err = bhavana_cycle(
            &mut w.store,
            &w.engine,
            &mut w.graph,
            &mut w.skg,
            &w.prime_tree,
            &mut w.state,
            &BhavanaPolicy::default(),
            None,
            &agent(),
            1.0,
            &[],
            &mut FailingSink,
        );
        assert!(err.is_err());
    }

    #[test]
    fn cluster_id_is_stable_and_order_independent() {
        assert_eq!(cluster_id_for(&[1, 2, 3]), cluster_id_for(&[1, 2, 3]));
        assert_ne!(cluster_id_for(&[1, 2, 3]), cluster_id_for(&[1, 2, 4]));
        let mut w = World::new();
        similar_triplet(&mut w);
        let gate = calibrated_gate();
        let a = w.run(&BhavanaPolicy::default(), Some(&gate)).clusters[0].cluster_id;
        assert_eq!(a, cluster_id_for(&[1, 2, 3]));
    }
}
