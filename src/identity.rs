//! Identity system — singleton agent identity with hexagram + horoscope.
//!
//! Persists as `identity.json` in the data directory. Created once from
//! entropy, then loaded on subsequent starts.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::archetypes::Archetype;
use crate::casting::{
    self, HexagramCast, HoroscopeCast, identity_seed, seed_to_vector, trigram_emotion,
    zodiac_from_epoch,
};
use crate::ec_key::{derive_shared_secret, public_from_private, seed_from_shared};
use crate::memory::{Emotion, MemoryRecord, now_epoch};
use crate::model::Row;
use crate::transform::orthogonal_from_seed;

const IDENTITY_FILE: &str = "identity.json";

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
    pub archetypes: Vec<Archetype>,
    #[serde(default)]
    pub cognitive_heat: f32,
    #[serde(default)]
    pub last_heat_update: u64,
    #[serde(skip)]
    pub transform: Option<Vec<Vec<f64>>>,
    #[serde(skip)]
    pub vector_transform: Option<crate::transform::VectorTransform>,
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

    /// Activate archetypes based on dream intensity tier.
    pub fn activate_for_tier(&mut self, tier: crate::archetypes::ActivationTier) {
        let active = tier.active_roles();
        for arch in &mut self.archetypes {
            if active.contains(&arch.role) {
                arch.activate();
            } else {
                arch.deactivate();
            }
        }
    }

    /// Activate archetypes based on a completed dream report.
    pub fn activate_from_report(&mut self, report: &crate::dream::DreamReport) {
        for arch in &mut self.archetypes {
            let name = arch.role.name().to_string();
            if report.active_archetypes.contains(&name) {
                arch.activate();
            } else {
                arch.deactivate();
            }
        }
    }

    /// Apply passive cooling based on elapsed time since last update.
    pub fn apply_passive_cooling(&mut self) {
        let now = crate::memory::now_epoch();
        let elapsed = now.saturating_sub(self.last_heat_update) as f32;
        self.cognitive_heat = (self.cognitive_heat - elapsed * crate::memory::HEAT_COOL_RATE).max(0.0);
        self.last_heat_update = now;
    }

    /// Add heat from a recall transaction.
    pub fn add_recall_heat(&mut self, count: u32) {
        self.apply_passive_cooling();
        self.cognitive_heat += count as f32 * crate::memory::HEAT_PER_RECALL;
    }

    /// Cool the agent after a dream cycle.
    pub fn dream_cool(&mut self) {
        self.apply_passive_cooling();
        self.cognitive_heat = (self.cognitive_heat - crate::memory::HEAT_DREAM_COOL).max(0.0);
    }

    /// Return resonance gates for currently active archetypes.
    /// Dormant archetypes don't gate — their dimension is open.
    pub fn active_resonance_gates(&self) -> Vec<crate::memory::ResonanceGate> {
        use crate::memory::ResonanceGate;
        use crate::archetypes::ArchetypeRole;
        let mut gates = Vec::new();
        for arch in &self.archetypes {
            if !arch.active {
                continue;
            }
            match arch.role {
                ArchetypeRole::Advocate  => gates.push(ResonanceGate::Fidelity),
                ArchetypeRole::Ethics    => gates.push(ResonanceGate::Lifecycle),
                ArchetypeRole::Intuition => gates.push(ResonanceGate::Temporal),
                ArchetypeRole::Fortune   => gates.push(ResonanceGate::AgentCapacity),
                ArchetypeRole::Craft     => {} // Kuṇḍali — Phase 2
            }
        }
        gates
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

pub fn load_or_create(data_dir: &str, entropy: &[u8]) -> (IdentityState, bool) {
    let path = Path::new(data_dir).join(IDENTITY_FILE);

    // Try loading existing
    if let Ok(contents) = fs::read_to_string(&path) {
        if let Ok(mut state) = serde_json::from_str::<IdentityState>(&contents) {
            // Regenerate runtime-only fields from seed via HKDF expansion
            let seed_bytes = expand_seed(state.identity_seed);
            state.transform = orthogonal_from_seed(&seed_bytes, 4).ok();
            state.vector_transform = crate::transform::VectorTransform::from_seed(&seed_bytes, 768).ok();
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

    // Cast archetypes from identity entropy
    let archetypes = crate::archetypes::cast_all_archetypes(&padded, now);

    let mut state = IdentityState {
        agent_id,
        name,
        hexagram,
        horoscope,
        primary_emotion,
        secondary_emotion,
        identity_seed: seed,
        created_at: now,
        archetypes,
        cognitive_heat: 0.0,
        last_heat_update: now,
        transform: None,
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
    state.vector_transform = crate::transform::VectorTransform::from_seed(&seed_bytes, 768).ok();

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
        assert_eq!(state.archetypes.len(), 5);

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
