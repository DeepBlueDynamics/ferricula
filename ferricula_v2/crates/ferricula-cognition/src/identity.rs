//! Identity system — singleton agent identity with hexagram + horoscope.
//!
//! Persists as `identity.json` in the data directory. Created once from
//! entropy, then loaded on subsequent starts.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::casting::{
    self, HexagramCast, HoroscopeCast, identity_seed, seed_to_vector, trigram_emotion,
    zodiac_from_epoch,
};
use ferricula_core::memory::{self, Emotion, MemoryRecord, now_epoch};
use ferricula_core::model::Row;
use ferricula_core::transform::orthogonal_from_seed;

const IDENTITY_FILE: &str = "identity.json";

/// Per-agent thermodynamic constants, derived from physical entropy at
/// identity creation. Each agent's memory physics jitters within an
/// architecturally safe range — same architecture, different exact dynamics.
///
/// This is the source of identity-from-entropy at the thermodynamic level:
/// not only is the hexagram/horoscope/archetype set cast from radio bytes,
/// the constants governing how that identity's memory decays, reinforces,
/// and overheats are themselves entropy-sourced. Two agents born from the
/// same radio stream at the same moment still diverge measurably because
/// the underlying physics of each agent is distinct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermodynamicConstants {
    /// Heat added per recalled memory in a recall transaction.
    pub heat_per_recall: f32,
    /// Heat lost per second (passive cooling rate).
    pub heat_cool_rate: f32,
    /// Heat lost per dream cycle.
    pub heat_dream_cool: f32,
    /// Heat ceiling — at this value the AgentCapacity gate blocks recall.
    pub heat_ceiling: f32,
    /// Default decay rate for new memories.
    pub alpha_default: f32,
    /// Recall dampening factor applied to a recalled memory's alpha.
    pub recall_shrink: f32,
    /// Neglect growth factor applied to alpha when a memory isn't recalled.
    pub neglect_grow: f32,
}

impl Default for ThermodynamicConstants {
    fn default() -> Self {
        Self {
            heat_per_recall: memory::HEAT_PER_RECALL,
            heat_cool_rate: memory::HEAT_COOL_RATE,
            heat_dream_cool: memory::HEAT_DREAM_COOL,
            heat_ceiling: memory::HEAT_CEILING,
            alpha_default: memory::ALPHA_DEFAULT,
            recall_shrink: memory::RECALL_SHRINK,
            neglect_grow: memory::NEGLECT_GROW,
        }
    }
}

impl ThermodynamicConstants {
    /// Derive per-agent thermodynamic constants from physical entropy.
    /// Each value is jittered within an architecturally safe range of its
    /// baseline. Bytes 8..16 of the entropy stream are used (bytes 0..6 are
    /// reserved for hexagram casting); falls back to baseline if entropy
    /// is shorter.
    ///
    /// Jitter ranges per field:
    ///   heat_per_recall: ±15%
    ///   heat_cool_rate:  ±15%
    ///   heat_dream_cool: ±15%
    ///   heat_ceiling:    ±15%
    ///   alpha_default:   ±15% (clamped to [ALPHA_MIN, ALPHA_MAX])
    ///   recall_shrink:   ±5%  (clamped to [0.85, 1.0])
    ///   neglect_grow:    ±2%  (clamped to [1.0, 1.05])
    pub fn from_entropy(entropy: &[u8]) -> Self {
        let base = Self::default();
        let zeros = [0u8; 8];
        let bytes: &[u8] = if entropy.len() >= 16 {
            &entropy[8..16]
        } else {
            &zeros
        };
        let jitter = |idx: usize, baseline: f32, range: f32| -> f32 {
            let b = bytes.get(idx).copied().unwrap_or(128);
            let normalized = (b as f32 - 128.0) / 128.0; // [-1, 1)
            baseline * (1.0 + normalized * range)
        };
        Self {
            heat_per_recall: jitter(0, base.heat_per_recall, 0.15).max(0.01),
            heat_cool_rate: jitter(1, base.heat_cool_rate, 0.15).max(0.001),
            heat_dream_cool: jitter(2, base.heat_dream_cool, 0.15).max(0.5),
            heat_ceiling: jitter(3, base.heat_ceiling, 0.15).max(1.0),
            alpha_default: jitter(4, base.alpha_default, 0.15)
                .clamp(memory::ALPHA_MIN, memory::ALPHA_MAX),
            recall_shrink: jitter(5, base.recall_shrink, 0.05).clamp(0.85, 1.0),
            neglect_grow: jitter(6, base.neglect_grow, 0.02).clamp(1.0, 1.05),
        }
    }
}

/// Full identity state for a ferricula agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityState {
    pub agent_id: String,
    pub name: String,
    pub hexagram: HexagramCast,
    pub horoscope: HoroscopeCast,
    pub primary_emotion: String,
    pub secondary_emotion: String,
    pub identity_seed: u32,
    pub created_at: u64,
    #[serde(default)]
    pub cognitive_heat: f32,
    #[serde(default)]
    pub last_heat_update: u64,
    /// Per-agent thermodynamic constants jittered from radio entropy at
    /// identity creation. Legacy identity.json files without this field
    /// load with `Default::default()` values (= module-const baselines).
    #[serde(default)]
    pub thermo: ThermodynamicConstants,
    #[serde(skip)]
    pub transform: Option<Vec<Vec<f64>>>,
    #[serde(skip)]
    pub vector_transform: Option<ferricula_core::transform::VectorTransform>,
    #[serde(skip)]
    pub private_key: Option<[u8; 32]>,
    #[serde(skip)]
    pub public_key: Option<[u8; 32]>,
}

impl IdentityState {
    /// Serialize to JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Persist this identity to `<data_dir>/identity.json`. Used after an
    /// in-place mutation (e.g. thermodynamic recast) so subsequent boots
    /// see the updated state.
    pub fn save(&self, data_dir: &str) -> std::io::Result<()> {
        let path = Path::new(data_dir).join(IDENTITY_FILE);
        let json = self.to_json();
        fs::write(&path, json)
    }

    /// Re-derive thermodynamic constants from fresh physical entropy.
    /// Updates `self.thermo` in place. Used by legacy agents whose
    /// identity.json predates the per-agent thermo field — calling this
    /// once promotes them from baseline (Default::default) to radio-jittered.
    /// Leaves hexagram, horoscope, agent_id, and all other identity fields
    /// untouched.
    pub fn recast_thermo(&mut self, entropy: &[u8]) {
        self.thermo = ThermodynamicConstants::from_entropy(entropy);
    }

    /// Apply passive cooling based on elapsed time since last update.
    /// Uses this agent's per-identity cool rate (jittered from radio entropy).
    pub fn apply_passive_cooling(&mut self) {
        let now = now_epoch();
        let elapsed = now.saturating_sub(self.last_heat_update) as f32;
        self.cognitive_heat = (self.cognitive_heat - elapsed * self.thermo.heat_cool_rate).max(0.0);
        self.last_heat_update = now;
    }

    /// Add heat from a recall transaction.
    /// Uses this agent's per-identity heat-per-recall.
    pub fn add_recall_heat(&mut self, count: u32) {
        self.apply_passive_cooling();
        self.cognitive_heat += count as f32 * self.thermo.heat_per_recall;
    }

    /// Cool the agent after a dream cycle.
    /// Uses this agent's per-identity dream-cool magnitude.
    pub fn dream_cool(&mut self) {
        self.apply_passive_cooling();
        self.cognitive_heat = (self.cognitive_heat - self.thermo.heat_dream_cool).max(0.0);
    }

    /// Return the resonance gates applied during recall. The Wisdom-King
    /// archetype layer that previously selected gates per active role is gone
    /// (PLAN §5a); the deterministic default is to apply all gates, matching
    /// the prior Full-tier behavior where every role was active.
    pub fn active_resonance_gates(&self) -> Vec<ferricula_core::ResonanceGate> {
        use ferricula_core::ResonanceGate;
        vec![
            ResonanceGate::Fidelity,
            ResonanceGate::Lifecycle,
            ResonanceGate::Temporal,
            ResonanceGate::AgentCapacity,
            ResonanceGate::Load,
        ]
    }
}

/// Load existing identity or create a new one.
///
/// Returns `(state, is_new)` — caller should write anchor memory if `is_new`.
/// Expand a u32 identity seed into a full 32-byte key via HKDF-SHA256.
fn expand_seed(seed: u32) -> [u8; 32] {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let ikm = seed.to_le_bytes();
    let hk = Hkdf::<Sha256>::new(Some(b"ferricula-identity"), &ikm);
    let mut okm = [0u8; 32];
    hk.expand(b"ferricula-vector-transform", &mut okm)
        .expect("hkdf expand");
    okm
}

fn public_from_private(private: &[u8; 32]) -> [u8; 32] {
    let sk = x25519_dalek::StaticSecret::from(*private);
    let pk = x25519_dalek::PublicKey::from(&sk);
    pk.to_bytes()
}

pub fn load_or_create(data_dir: &str, entropy: &[u8]) -> (IdentityState, bool) {
    let path = Path::new(data_dir).join(IDENTITY_FILE);

    // Try loading existing
    if let Ok(contents) = fs::read_to_string(&path) {
        if let Ok(mut state) = serde_json::from_str::<IdentityState>(&contents) {
            // Regenerate runtime-only fields from seed via HKDF expansion
            let seed_bytes = expand_seed(state.identity_seed);
            state.transform = orthogonal_from_seed(&seed_bytes, 4).ok();
            state.vector_transform =
                ferricula_core::transform::VectorTransform::from_seed(&seed_bytes, 768).ok();
            return (state, false);
        }
    }

    // Create new identity
    let now = now_epoch();

    // Need at least 6 bytes for hexagram
    let padded = if entropy.len() >= 6 {
        entropy.to_vec()
    } else {
        // Fallback: derive bytes from timestamp
        let mut bytes = vec![0u8; 6];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = ((now >> (i * 8)) & 0xFF) as u8;
        }
        bytes
    };

    let hexagram = casting::cast_hexagram(&padded);
    let horoscope = zodiac_from_epoch(now);

    let primary_emotion = trigram_emotion(hexagram.upper_trigram).to_string();
    let secondary_emotion = trigram_emotion(hexagram.lower_trigram).to_string();

    let seed = identity_seed(hexagram.number, &hexagram.lines, now);

    let agent_id = format!("ferricula-{:08x}", seed);
    let name = format!("{} ({})", hexagram.name, horoscope.sign_name);

    // Derive per-agent thermodynamic constants from radio entropy.
    // Each agent gets a slightly different memory-physics — same architecture,
    // distinct exact dynamics, sourced from physical noise not pseudo-random.
    let thermo = ThermodynamicConstants::from_entropy(&padded);

    let mut state = IdentityState {
        agent_id,
        name,
        hexagram,
        horoscope,
        primary_emotion,
        secondary_emotion,
        identity_seed: seed,
        created_at: now,
        cognitive_heat: 0.0,
        last_heat_update: now,
        thermo,
        transform: None,
        vector_transform: None,
        private_key: None,
        public_key: None,
    };

    // Derive ECC keypair deterministically from seed (placeholder: hash-based)
    let mut priv_key = [0u8; 32];
    for (i, b) in priv_key.iter_mut().enumerate() {
        *b = ((seed as u64 >> ((i % 4) * 8)) & 0xFF) as u8 ^ padded[i % padded.len()];
    }
    let pub_key = public_from_private(&priv_key);
    state.private_key = Some(priv_key);
    state.public_key = Some(pub_key);

    // Self-transform seed from identity seed via HKDF expansion
    let seed_bytes = expand_seed(seed);
    let ortho = orthogonal_from_seed(&seed_bytes, 4).ok();
    state.transform = ortho;
    // 768-dimensional vector encryption for geometric trust
    state.vector_transform =
        ferricula_core::transform::VectorTransform::from_seed(&seed_bytes, 768).ok();

    // Save to disk
    if let Ok(json) = serde_json::to_string_pretty(&state) {
        let _ = fs::write(&path, json);
    }

    (state, true)
}

/// Create the anchor memory Row + MemoryRecord for a new identity.
pub fn create_anchor(state: &IdentityState) -> (Row, MemoryRecord) {
    let mut tags = std::collections::BTreeMap::new();
    tags.insert("type".to_string(), "identity_anchor".to_string());
    tags.insert("agent_id".to_string(), state.agent_id.clone());
    tags.insert(
        "hexagram".to_string(),
        format!("{} - {}", state.hexagram.number, state.hexagram.name),
    );
    tags.insert("horoscope".to_string(), state.horoscope.sign_name.clone());
    tags.insert(
        "text".to_string(),
        format!(
            "Identity anchor: {} | {} | {}/{}",
            state.agent_id, state.name, state.primary_emotion, state.secondary_emotion
        ),
    );

    let vector = seed_to_vector(state.identity_seed);

    // Use a high ID unlikely to collide (seed-based)
    let anchor_id = state.identity_seed;

    let row = Row {
        id: anchor_id,
        tags,
        vector,
        refs: None,
    };

    let mut record = MemoryRecord::new(anchor_id);
    record.keystone = true;
    record.importance = 1.0;
    record.emotion = Some(Emotion {
        primary: state.primary_emotion.clone(),
        secondary: Some(state.secondary_emotion.clone()),
    });
    // agent_id is embedded in the text tag above

    (row, record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_create_new() {
        let dir = std::env::temp_dir().join("ferricula_test_identity");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let entropy = [42u8, 128, 200, 10, 180, 90];
        let (state, is_new) = load_or_create(dir.to_str().unwrap(), &entropy);

        assert!(is_new);
        assert!(!state.agent_id.is_empty());
        assert!(!state.name.is_empty());
        assert!(state.hexagram.number >= 1 && state.hexagram.number <= 64);
        assert!(!state.primary_emotion.is_empty());

        // Cleanup
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_create_reload() {
        let dir = std::env::temp_dir().join("ferricula_test_identity_reload");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let entropy = [42u8, 128, 200, 10, 180, 90];
        let (state1, is_new1) = load_or_create(dir.to_str().unwrap(), &entropy);
        assert!(is_new1);

        let (state2, is_new2) = load_or_create(dir.to_str().unwrap(), &entropy);
        assert!(!is_new2);
        assert_eq!(state1.agent_id, state2.agent_id);
        assert_eq!(state1.identity_seed, state2.identity_seed);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn anchor_creation() {
        let dir = std::env::temp_dir().join("ferricula_test_anchor");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let entropy = [42u8, 128, 200, 10, 180, 90];
        let (state, _) = load_or_create(dir.to_str().unwrap(), &entropy);
        let (row, record) = create_anchor(&state);

        assert!(record.keystone);
        assert_eq!(record.importance, 1.0);
        assert!(record.emotion.is_some());
        assert_eq!(row.vector.len(), 768);
        assert!(row.tags.contains_key("type"));
        assert_eq!(row.tags["type"], "identity_anchor");

        let _ = fs::remove_dir_all(&dir);
    }
}
