//! Semantic Knowledge Graph — materialized edges from prime tree posting list intersections.
//!
//! Grainger's SKG projects relationships dynamically from bitmap intersections.
//! Weber's bracket `[ṡ² + s·s̈]` tracks velocity and acceleration of semantic
//! distance (Jaccard similarity) over dream cycles.

use std::collections::HashMap;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::memory::MemoryStore;
use crate::prime_tree::PrimeTree;

/// Canonical term pair key — a <= b lexicographically.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TermPairKey {
    pub a: String,
    pub b: String,
}

impl TermPairKey {
    pub fn new(x: &str, y: &str) -> Self {
        if x <= y {
            Self {
                a: x.to_string(),
                b: y.to_string(),
            }
        } else {
            Self {
                a: y.to_string(),
                b: x.to_string(),
            }
        }
    }
}

/// Ring buffer of (dream_tick, jaccard_score) pairs.
const HISTORY_CAP: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JaccardHistory {
    entries: [(u64, f32); HISTORY_CAP],
    head: usize,
    len: usize,
}

impl Default for JaccardHistory {
    fn default() -> Self {
        Self {
            entries: [(0, 0.0); HISTORY_CAP],
            head: 0,
            len: 0,
        }
    }
}

impl JaccardHistory {
    /// Push a new (tick, score) observation.
    pub fn push(&mut self, tick: u64, score: f32) {
        self.entries[self.head] = (tick, score);
        self.head = (self.head + 1) % HISTORY_CAP;
        if self.len < HISTORY_CAP {
            self.len += 1;
        }
    }

    /// Number of entries currently stored.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Most recent tick in the buffer.
    pub fn last_tick(&self) -> u64 {
        if self.len == 0 {
            return 0;
        }
        let idx = if self.head == 0 {
            HISTORY_CAP - 1
        } else {
            self.head - 1
        };
        self.entries[idx].0
    }

    /// Get the i-th oldest entry (0 = oldest).
    fn get_ordered(&self, i: usize) -> Option<(u64, f32)> {
        if i >= self.len {
            return None;
        }
        let start = if self.len < HISTORY_CAP {
            0
        } else {
            self.head
        };
        let idx = (start + i) % HISTORY_CAP;
        Some(self.entries[idx])
    }

    /// Compute velocity (ṡ) from the two most recent entries.
    /// Returns None if fewer than 2 entries.
    pub fn velocity(&self) -> Option<f32> {
        if self.len < 2 {
            return None;
        }
        let (t1, s1) = self.get_ordered(self.len - 2)?;
        let (t2, s2) = self.get_ordered(self.len - 1)?;
        let dt = (t2 as f64 - t1 as f64) as f32;
        if dt.abs() < f32::EPSILON {
            return None;
        }
        Some((s2 - s1) / dt)
    }

    /// Compute acceleration (s̈) from the three most recent entries.
    /// Returns None if fewer than 3 entries.
    pub fn acceleration(&self) -> Option<f32> {
        if self.len < 3 {
            return None;
        }
        let (t0, s0) = self.get_ordered(self.len - 3)?;
        let (t1, s1) = self.get_ordered(self.len - 2)?;
        let (t2, s2) = self.get_ordered(self.len - 1)?;

        let dt_10 = (t1 as f64 - t0 as f64) as f32;
        let dt_21 = (t2 as f64 - t1 as f64) as f32;
        if dt_10.abs() < f32::EPSILON || dt_21.abs() < f32::EPSILON {
            return None;
        }

        let v_10 = (s1 - s0) / dt_10;
        let v_21 = (s2 - s1) / dt_21;
        let dt_avg = ((t2 as f64 - t0 as f64) as f32) / 2.0;
        if dt_avg.abs() < f32::EPSILON {
            return None;
        }
        Some((v_21 - v_10) / dt_avg)
    }

    /// Weber bracket: B = ṡ² + s · s̈
    /// where s is the most recent Jaccard score.
    /// Positive = emerging, negative = decaying.
    pub fn weber_bracket(&self) -> Option<f32> {
        let v = self.velocity()?;
        let a = self.acceleration()?;
        let (_, s) = self.get_ordered(self.len - 1)?;
        Some(v * v + s * a)
    }

    /// Most recent Jaccard score.
    pub fn current_jaccard(&self) -> Option<f32> {
        self.get_ordered(self.len.checked_sub(1)?)
            .map(|(_, s)| s)
    }
}

/// A single SKG edge with computed dynamics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkgEdge {
    pub term_a: String,
    pub term_b: String,
    pub jaccard: f32,
    pub velocity: Option<f32>,
    pub acceleration: Option<f32>,
    pub weber_bracket: Option<f32>,
    pub history_len: usize,
}

/// Summary of an SKG update cycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkgUpdateSummary {
    pub pairs_sampled: u32,
    pub pairs_tracked: u32,
    pub top_emerging: Vec<SkgEdge>,
    pub top_decaying: Vec<SkgEdge>,
}

/// Persistent snapshot of SKG state.
#[derive(Debug, Serialize, Deserialize)]
pub struct SkgSnapshot {
    pub histories: Vec<(TermPairKey, JaccardHistory)>,
    pub dream_tick: u64,
}

/// The SKG engine — tracks Jaccard co-occurrence dynamics between term pairs.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SkgState {
    pub histories: HashMap<TermPairKey, JaccardHistory>,
    pub dream_tick: u64,
}

impl SkgState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run one SKG update cycle during dream.
    ///
    /// 1. Find keystone-adjacent terms in the prime tree
    /// 2. Compute Jaccard for candidate term pairs
    /// 3. Push to ring buffers, evict stale pairs
    /// 4. Return top emerging/decaying edges
    pub fn update(
        &mut self,
        prime_tree: &PrimeTree,
        store: &MemoryStore,
        _engine: &Engine,
        max_terms: usize,
    ) -> SkgUpdateSummary {
        self.dream_tick += 1;
        let mut summary = SkgUpdateSummary::default();

        // Step 1: Find keystone IDs
        let keystone_ids: RoaringBitmap = store
            .keystones()
            .iter()
            .map(|r| r.id)
            .collect();

        if keystone_ids.is_empty() {
            return summary;
        }

        // Step 2: Find terms whose posting lists contain any keystone
        let all_terms = prime_tree.terms();
        let mut term_cards: Vec<(String, u64)> = Vec::new();
        for term in &all_terms {
            let members = prime_tree.search_exact(term);
            if !members.is_disjoint(&keystone_ids) {
                term_cards.push((term.clone(), members.len()));
            }
        }

        // Sort by cardinality desc, take top N
        term_cards.sort_by(|a, b| b.1.cmp(&a.1));
        term_cards.truncate(max_terms);

        let terms: Vec<String> = term_cards.into_iter().map(|(t, _)| t).collect();

        // Step 3: Compute Jaccard for all pairs
        for i in 0..terms.len() {
            for j in (i + 1)..terms.len() {
                let bm_a = prime_tree.search_exact(&terms[i]);
                let bm_b = prime_tree.search_exact(&terms[j]);
                let jaccard = bitmap_jaccard(&bm_a, &bm_b);

                let key = TermPairKey::new(&terms[i], &terms[j]);
                let history = self.histories.entry(key).or_default();
                history.push(self.dream_tick, jaccard);
                summary.pairs_sampled += 1;
            }
        }

        // Step 4: Evict stale pairs (not updated in 32 dream ticks)
        let tick = self.dream_tick;
        self.histories
            .retain(|_, h| tick.saturating_sub(h.last_tick()) <= 32);

        summary.pairs_tracked = self.histories.len() as u32;

        // Step 5: Collect edges with Weber brackets, sort into emerging/decaying
        let mut emerging: Vec<SkgEdge> = Vec::new();
        let mut decaying: Vec<SkgEdge> = Vec::new();

        for (key, hist) in &self.histories {
            let edge = SkgEdge {
                term_a: key.a.clone(),
                term_b: key.b.clone(),
                jaccard: hist.current_jaccard().unwrap_or(0.0),
                velocity: hist.velocity(),
                acceleration: hist.acceleration(),
                weber_bracket: hist.weber_bracket(),
                history_len: hist.len(),
            };

            if let Some(wb) = edge.weber_bracket {
                if wb > 0.0 {
                    emerging.push(edge);
                } else if wb < 0.0 {
                    decaying.push(edge);
                }
            }
        }

        emerging.sort_by(|a, b| {
            b.weber_bracket
                .unwrap_or(0.0)
                .total_cmp(&a.weber_bracket.unwrap_or(0.0))
        });
        decaying.sort_by(|a, b| {
            a.weber_bracket
                .unwrap_or(0.0)
                .total_cmp(&b.weber_bracket.unwrap_or(0.0))
        });

        emerging.truncate(5);
        decaying.truncate(5);

        summary.top_emerging = emerging;
        summary.top_decaying = decaying;

        summary
    }

    /// Create a snapshot for persistence.
    pub fn snapshot(&self) -> SkgSnapshot {
        SkgSnapshot {
            histories: self.histories.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            dream_tick: self.dream_tick,
        }
    }

    /// Load from a snapshot.
    pub fn load_snapshot(snap: SkgSnapshot) -> Self {
        Self {
            histories: snap.histories.into_iter().collect(),
            dream_tick: snap.dream_tick,
        }
    }

    /// Get all edges for a specific term.
    pub fn edges_for_term(&self, term: &str) -> Vec<SkgEdge> {
        let term_lower = term.to_lowercase();
        let mut edges: Vec<SkgEdge> = self
            .histories
            .iter()
            .filter(|(key, _)| key.a == term_lower || key.b == term_lower)
            .map(|(key, hist)| SkgEdge {
                term_a: key.a.clone(),
                term_b: key.b.clone(),
                jaccard: hist.current_jaccard().unwrap_or(0.0),
                velocity: hist.velocity(),
                acceleration: hist.acceleration(),
                weber_bracket: hist.weber_bracket(),
                history_len: hist.len(),
            })
            .collect();

        edges.sort_by(|a, b| {
            let abs_a = a.weber_bracket.unwrap_or(0.0).abs();
            let abs_b = b.weber_bracket.unwrap_or(0.0).abs();
            abs_b.total_cmp(&abs_a)
        });

        edges
    }
}

/// Jaccard similarity between two RoaringBitmaps.
fn bitmap_jaccard(a: &RoaringBitmap, b: &RoaringBitmap) -> f32 {
    let intersection = a.intersection_len(b);
    let union = a.union_len(b);
    if union == 0 {
        return 0.0;
    }
    intersection as f32 / union as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::memory::{MemoryRecord, MemoryStore};
    use crate::prime_tree::PrimeTree;

    #[test]
    fn ring_buffer_push_and_wrap() {
        let mut h = JaccardHistory::default();
        for i in 0..20u64 {
            h.push(i, i as f32 * 0.05);
        }
        // Should wrap at 16, keeping most recent 16
        assert_eq!(h.len(), HISTORY_CAP);
        // Oldest should be tick 4 (20 - 16)
        let oldest = h.get_ordered(0).unwrap();
        assert_eq!(oldest.0, 4);
        // Newest should be tick 19
        let newest = h.get_ordered(HISTORY_CAP - 1).unwrap();
        assert_eq!(newest.0, 19);
    }

    #[test]
    fn velocity_from_two_entries() {
        let mut h = JaccardHistory::default();
        h.push(1, 0.2);
        h.push(3, 0.6);
        let v = h.velocity().unwrap();
        // (0.6 - 0.2) / (3 - 1) = 0.2
        assert!((v - 0.2).abs() < 1e-5);
    }

    #[test]
    fn acceleration_requires_three() {
        let mut h = JaccardHistory::default();
        h.push(1, 0.1);
        h.push(2, 0.2);
        assert!(h.acceleration().is_none());

        h.push(3, 0.4);
        assert!(h.acceleration().is_some());
    }

    #[test]
    fn weber_bracket_emerging() {
        // Increasing Jaccard with increasing rate → positive bracket
        let mut h = JaccardHistory::default();
        h.push(1, 0.1);
        h.push(2, 0.2);
        h.push(3, 0.4); // accelerating increase
        let wb = h.weber_bracket().unwrap();
        assert!(wb > 0.0, "expected positive bracket for emerging, got {wb}");
    }

    #[test]
    fn weber_bracket_decaying() {
        // For B = ṡ² + s·s̈ < 0 we need large s and strongly negative s̈
        // High current score with accelerating collapse
        let mut h = JaccardHistory::default();
        h.push(1, 0.95);
        h.push(2, 0.90);
        h.push(3, 0.50); // sudden collapse — s̈ very negative, s still large
        // ṡ = (0.50-0.90)/1 = -0.4, v_10 = -0.05, s̈ = (-0.4-(-0.05))/1 = -0.35
        // B = 0.16 + 0.50*(-0.35) = 0.16 - 0.175 = -0.015
        let wb = h.weber_bracket().unwrap();
        assert!(wb < 0.0, "expected negative bracket for decaying, got {wb}");
    }

    #[test]
    fn term_pair_key_canonical() {
        let k1 = TermPairKey::new("beta", "alpha");
        let k2 = TermPairKey::new("alpha", "beta");
        assert_eq!(k1, k2);
        assert_eq!(k1.a, "alpha");
        assert_eq!(k1.b, "beta");
    }

    #[test]
    fn skg_update_with_prime_tree() {
        let mut prime_tree = PrimeTree::new();
        let mut store = MemoryStore::new();
        let engine = Engine::new();

        // Create two keystones
        let mut r1 = MemoryRecord::new_at(1, 0);
        r1.keystone = true;
        store.insert(r1);

        let mut r2 = MemoryRecord::new_at(2, 0);
        r2.keystone = true;
        store.insert(r2);

        // Insert terms that share members
        prime_tree.insert("weather", 1);
        prime_tree.insert("weather", 2);
        prime_tree.insert("forecast", 1);
        prime_tree.insert("forecast", 2);

        let mut skg = SkgState::new();
        let summary = skg.update(&prime_tree, &store, &engine, 20);

        assert!(summary.pairs_sampled > 0, "should have sampled pairs");
        assert!(summary.pairs_tracked > 0, "should have tracked pairs");

        // Verify history was created for the weather/forecast pair
        let key = TermPairKey::new("weather", "forecast");
        assert!(skg.histories.contains_key(&key));
    }

    #[test]
    fn snapshot_round_trip() {
        let mut skg = SkgState::new();
        let key = TermPairKey::new("alpha", "beta");
        let mut h = JaccardHistory::default();
        h.push(1, 0.5);
        h.push(2, 0.6);
        skg.histories.insert(key.clone(), h);
        skg.dream_tick = 42;

        let snap = skg.snapshot();
        let restored = SkgState::load_snapshot(snap);

        assert_eq!(restored.dream_tick, 42);
        assert!(restored.histories.contains_key(&key));
        let rh = &restored.histories[&key];
        assert_eq!(rh.len(), 2);
        assert!((rh.current_jaccard().unwrap() - 0.6).abs() < 1e-5);
    }
}
