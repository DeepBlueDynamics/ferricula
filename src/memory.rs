use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Minimum adaptive decay rate.
pub const ALPHA_MIN: f32 = 0.001;
/// Maximum adaptive decay rate.
pub const ALPHA_MAX: f32 = 0.02;
/// Default initial decay rate.
pub const ALPHA_DEFAULT: f32 = 0.01;
/// Fidelity threshold — memories below this cannot persist as Active.
pub const FIDELITY_GATE: f32 = 0.75;
/// Recall dampening factor (shrinks alpha on recall).
pub const RECALL_SHRINK: f32 = 0.95;
/// Neglect growth factor (grows alpha when ignored).
pub const NEGLECT_GROW: f32 = 1.005;
/// Keystone halo dampening factor (shrinks alpha on proximity to a keystone).
/// Weaker than RECALL_SHRINK: proximity is not the same as being recalled.
pub const HALO_SHRINK: f32 = 0.99;

pub const HEAT_CEILING: f32 = 10.0;
pub const HEAT_PER_RECALL: f32 = 0.3;
pub const HEAT_COOL_RATE: f32 = 0.1;
pub const HEAT_DREAM_COOL: f32 = 3.0;

/// Gates that control whether a memory responds to a recall query.
/// Each gate maps to a Wisdom King archetype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResonanceGate {
    /// Acala (Center) — fidelity must be above gate threshold.
    Fidelity,
    /// Yamāntaka (West) — memory must be in Active lifecycle state.
    Lifecycle,
    /// Trailokyavijaya (East) — not recalled too recently, not neglected too long.
    Temporal,
    /// Vajrayakṣa (North) — agent has capacity to absorb (heat check).
    AgentCapacity,
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// One-way lifecycle: Active → Forgiven → Archived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState {
    Active,
    Forgiven,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Emotion {
    pub primary: String,
    pub secondary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Provenance {
    Ingested,
    Consolidated { from: Vec<u32> },
    Revived { seed_id: u32 },
}

/// Thermodynamic envelope around a stored row.
/// The raw data (tags, vector) lives in Engine; this tracks the memory's
/// lifecycle, fidelity, decay rate, and relational metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: u32,
    pub fidelity: f32,
    pub decay_alpha: f32,
    pub state: LifecycleState,
    pub keystone: bool,
    pub created_at: u64,
    pub last_recalled: u64,
    pub recall_count: u32,
    pub consolidation_depth: u32,
    pub importance: f32,
    pub emotion: Option<Emotion>,
    pub provenance: Provenance,
}

impl MemoryRecord {
    pub fn new(id: u32) -> Self {
        let now = now_epoch();
        Self {
            id,
            fidelity: 1.0,
            decay_alpha: ALPHA_DEFAULT,
            state: LifecycleState::Active,
            keystone: false,
            created_at: now,
            last_recalled: now,
            recall_count: 0,
            consolidation_depth: 0,
            importance: 0.0,
            emotion: None,
            provenance: Provenance::Ingested,
        }
    }

    /// Create with a fixed timestamp (for testing / deserialization).
    pub fn new_at(id: u32, created_at: u64) -> Self {
        Self {
            id,
            fidelity: 1.0,
            decay_alpha: ALPHA_DEFAULT,
            state: LifecycleState::Active,
            keystone: false,
            created_at,
            last_recalled: created_at,
            recall_count: 0,
            consolidation_depth: 0,
            importance: 0.0,
            emotion: None,
            provenance: Provenance::Ingested,
        }
    }

    /// One tick of exponential decay.
    /// Keystones and non-Active records are immune.
    /// Consolidation depth reduces effective alpha.
    pub fn decay_tick(&mut self) {
        if self.keystone || self.state != LifecycleState::Active {
            return;
        }
        let eff = self.effective_alpha();
        self.fidelity *= (-eff).exp();
        self.fidelity = self.fidelity.clamp(0.0, 1.0);
    }

    /// α_eff = α / (1 + ln(1 + consolidation_depth))
    pub fn effective_alpha(&self) -> f32 {
        self.decay_alpha / (1.0 + (1.0 + self.consolidation_depth as f32).ln())
    }

    /// Called when this memory is recalled. Shrinks alpha, updates timestamps.
    pub fn on_recall(&mut self) {
        self.recall_count += 1;
        self.last_recalled = now_epoch();
        self.decay_alpha = (self.decay_alpha * RECALL_SHRINK).max(ALPHA_MIN);
    }

    /// Called during dream for memories not recently recalled.
    pub fn on_neglect(&mut self) {
        self.decay_alpha = (self.decay_alpha * NEGLECT_GROW).min(ALPHA_MAX);
    }

    /// Called during dream for memories that are direct graph neighbors of a
    /// keystone. Proximity to something editorially important is itself a weak
    /// form of attention — the dialectical context of a keystoned quote is
    /// worth preserving. Shrinks alpha by a smaller factor than on_recall,
    /// without updating last_recalled or recall_count (this is not a recall).
    /// Keystones and non-Active records are immune (nothing to protect).
    pub fn on_halo_touch(&mut self) {
        if self.keystone || self.state != LifecycleState::Active {
            return;
        }
        self.decay_alpha = (self.decay_alpha * HALO_SHRINK).max(ALPHA_MIN);
    }

    /// True if fidelity is at or above the survival gate.
    pub fn above_gate(&self) -> bool {
        self.fidelity >= FIDELITY_GATE
    }

    /// Active → Forgiven. Returns false if not Active.
    pub fn forgive(&mut self) -> bool {
        if self.state != LifecycleState::Active {
            return false;
        }
        self.state = LifecycleState::Forgiven;
        true
    }

    /// Forgiven → Archived. Returns false if not Forgiven.
    pub fn archive(&mut self) -> bool {
        if self.state != LifecycleState::Forgiven {
            return false;
        }
        self.state = LifecycleState::Archived;
        true
    }

    /// Seconds since last recall.
    pub fn staleness(&self) -> u64 {
        now_epoch().saturating_sub(self.last_recalled)
    }

    /// Age in seconds.
    pub fn age(&self) -> u64 {
        now_epoch().saturating_sub(self.created_at)
    }

    /// Wheeler-Feynman resonance check — does this memory respond to the query?
    /// Keystones always resonate. Other memories must pass all active gates.
    pub fn resonates(&self, agent_heat: f32, active_gates: &[ResonanceGate]) -> bool {
        if self.keystone {
            return true;
        }
        for gate in active_gates {
            match gate {
                ResonanceGate::Fidelity => {
                    if self.fidelity < FIDELITY_GATE {
                        return false;
                    }
                }
                ResonanceGate::Lifecycle => {
                    if self.state != LifecycleState::Active {
                        return false;
                    }
                }
                ResonanceGate::Temporal => {
                    let since_recall = now_epoch().saturating_sub(self.last_recalled);
                    if since_recall < 2 {
                        return false; // saturated — recalled too recently
                    }
                    if since_recall > 172800 {
                        return false; // 48h neglect — out of phase
                    }
                }
                ResonanceGate::AgentCapacity => {
                    if agent_heat > HEAT_CEILING {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// In-memory store of thermodynamic envelopes, keyed by row ID.
#[derive(Debug, Default)]
pub struct MemoryStore {
    records: HashMap<u32, MemoryRecord>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, record: MemoryRecord) {
        self.records.insert(record.id, record);
    }

    pub fn get(&self, id: u32) -> Option<&MemoryRecord> {
        self.records.get(&id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut MemoryRecord> {
        self.records.get_mut(&id)
    }

    pub fn remove(&mut self, id: u32) -> Option<MemoryRecord> {
        self.records.remove(&id)
    }

    pub fn contains(&self, id: u32) -> bool {
        self.records.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&u32, &MemoryRecord)> {
        self.records.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&u32, &mut MemoryRecord)> {
        self.records.iter_mut()
    }

    /// All records in a given lifecycle state.
    pub fn in_state(&self, state: LifecycleState) -> Vec<&MemoryRecord> {
        self.records.values().filter(|r| r.state == state).collect()
    }

    /// All keystone records.
    pub fn keystones(&self) -> Vec<&MemoryRecord> {
        self.records.values().filter(|r| r.keystone).collect()
    }

    /// Snapshot all records for persistence.
    pub fn all_records(&self) -> Vec<MemoryRecord> {
        self.records.values().cloned().collect()
    }

    /// Load records from a persistence snapshot.
    pub fn load_records(&mut self, records: Vec<MemoryRecord>) {
        for r in records {
            self.records.insert(r.id, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_reduces_fidelity() {
        let mut r = MemoryRecord::new_at(1, 0);
        assert_eq!(r.fidelity, 1.0);
        r.decay_tick();
        assert!(r.fidelity < 1.0);
        assert!(r.fidelity > 0.0);
    }

    #[test]
    fn keystone_immune_to_decay() {
        let mut r = MemoryRecord::new_at(1, 0);
        r.keystone = true;
        r.decay_tick();
        assert_eq!(r.fidelity, 1.0);
    }

    #[test]
    fn recall_shrinks_alpha() {
        let mut r = MemoryRecord::new_at(1, 0);
        let before = r.decay_alpha;
        r.on_recall();
        assert!(r.decay_alpha < before);
        assert_eq!(r.recall_count, 1);
    }

    #[test]
    fn neglect_grows_alpha() {
        let mut r = MemoryRecord::new_at(1, 0);
        let before = r.decay_alpha;
        r.on_neglect();
        assert!(r.decay_alpha > before);
    }

    #[test]
    fn alpha_respects_bounds() {
        let mut r = MemoryRecord::new_at(1, 0);
        r.decay_alpha = ALPHA_MIN;
        r.on_recall();
        assert_eq!(r.decay_alpha, ALPHA_MIN);

        r.decay_alpha = ALPHA_MAX;
        r.on_neglect();
        assert_eq!(r.decay_alpha, ALPHA_MAX);
    }

    #[test]
    fn lifecycle_transitions() {
        let mut r = MemoryRecord::new_at(1, 0);
        assert_eq!(r.state, LifecycleState::Active);

        // Can't archive from Active
        assert!(!r.archive());
        assert_eq!(r.state, LifecycleState::Active);

        // Active → Forgiven
        assert!(r.forgive());
        assert_eq!(r.state, LifecycleState::Forgiven);

        // Can't forgive again
        assert!(!r.forgive());

        // Forgiven → Archived
        assert!(r.archive());
        assert_eq!(r.state, LifecycleState::Archived);

        // Can't archive again
        assert!(!r.archive());
    }

    #[test]
    fn fidelity_gate_works() {
        let mut r = MemoryRecord::new_at(1, 0);
        assert!(r.above_gate());
        r.fidelity = 0.5;
        assert!(!r.above_gate());
        r.fidelity = FIDELITY_GATE;
        assert!(r.above_gate());
    }

    #[test]
    fn consolidation_slows_decay() {
        let mut a = MemoryRecord::new_at(1, 0);
        let mut b = MemoryRecord::new_at(2, 0);
        b.consolidation_depth = 5;

        a.decay_tick();
        b.decay_tick();
        // b should have higher fidelity because consolidation reduces effective alpha
        assert!(b.fidelity > a.fidelity);
    }

    #[test]
    fn store_basics() {
        let mut store = MemoryStore::new();
        assert!(store.is_empty());

        store.insert(MemoryRecord::new_at(1, 0));
        store.insert(MemoryRecord::new_at(2, 0));
        assert_eq!(store.len(), 2);
        assert!(store.contains(1));

        store.remove(1);
        assert_eq!(store.len(), 1);
        assert!(!store.contains(1));
    }

    #[test]
    fn store_state_queries() {
        let mut store = MemoryStore::new();
        let mut r1 = MemoryRecord::new_at(1, 0);
        r1.keystone = true;
        let mut r2 = MemoryRecord::new_at(2, 0);
        r2.forgive();

        store.insert(r1);
        store.insert(r2);
        store.insert(MemoryRecord::new_at(3, 0));

        assert_eq!(store.in_state(LifecycleState::Active).len(), 2);
        assert_eq!(store.in_state(LifecycleState::Forgiven).len(), 1);
        assert_eq!(store.keystones().len(), 1);
    }

    // --- resonance gate tests ---

    fn make_record(id: u32) -> MemoryRecord {
        MemoryRecord::new_at(id, 0)
    }

    #[test]
    fn resonance_keystone_always_passes() {
        let mut r = make_record(1);
        r.keystone = true;
        r.fidelity = 0.01; // way below gate
        assert!(r.resonates(100.0, &[ResonanceGate::Fidelity, ResonanceGate::AgentCapacity]));
    }

    #[test]
    fn resonance_fidelity_gate() {
        let mut r = make_record(1);
        r.fidelity = FIDELITY_GATE + 0.01;
        assert!(r.resonates(0.0, &[ResonanceGate::Fidelity]));
        r.fidelity = FIDELITY_GATE - 0.01;
        assert!(!r.resonates(0.0, &[ResonanceGate::Fidelity]));
    }

    #[test]
    fn resonance_lifecycle_gate() {
        let mut r = make_record(1);
        assert!(r.resonates(0.0, &[ResonanceGate::Lifecycle]));
        r.forgive();
        assert!(!r.resonates(0.0, &[ResonanceGate::Lifecycle]));
    }

    #[test]
    fn resonance_agent_capacity_gate() {
        let r = make_record(1);
        assert!(r.resonates(9.0, &[ResonanceGate::AgentCapacity]));
        assert!(!r.resonates(11.0, &[ResonanceGate::AgentCapacity]));
    }

    #[test]
    fn resonance_no_gates_always_passes() {
        let mut r = make_record(1);
        r.fidelity = 0.01;
        r.forgive();
        // No gates = everything passes (dormant archetypes)
        assert!(r.resonates(100.0, &[]));
    }
}
