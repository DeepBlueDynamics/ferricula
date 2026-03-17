//! Archetype sub-agents — five roles cast from identity entropy.
//!
//! Each archetype has its own hexagram, horoscope, and emotion profile.
//! Phase 1: initialized, persisted, and logged. Behavioral effects are stubs.

use serde::{Deserialize, Serialize};

use crate::casting::{self, HexagramCast, HoroscopeCast, trigram_emotion, zodiac_from_epoch};

/// The five archetype roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchetypeRole {
    Intuition,
    Fortune,
    Craft,
    Ethics,
    Advocate,
}

impl ArchetypeRole {
    pub fn name(self) -> &'static str {
        match self {
            Self::Intuition => "Intuition",
            Self::Fortune => "Fortune",
            Self::Craft => "Craft",
            Self::Ethics => "Ethics",
            Self::Advocate => "Advocate",
        }
    }

    pub fn all() -> [Self; 5] {
        [
            Self::Intuition,
            Self::Fortune,
            Self::Craft,
            Self::Ethics,
            Self::Advocate,
        ]
    }
}

/// Archetype state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchetypeState {
    Dormant,
    Listening,
    Engaged,
    Reflecting,
}

impl ArchetypeState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dormant => "Dormant",
            Self::Listening => "Listening",
            Self::Engaged => "Engaged",
            Self::Reflecting => "Reflecting",
        }
    }
}

/// A single archetype with its own identity cast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Archetype {
    pub role: ArchetypeRole,
    pub hexagram: HexagramCast,
    pub horoscope: HoroscopeCast,
    pub primary_emotion: String,
    pub secondary_emotion: String,
    pub seed: u32,
    pub active: bool,
    pub state: ArchetypeState,
}

impl Archetype {
    pub fn activate(&mut self) {
        self.active = true;
        self.state = ArchetypeState::Listening;
    }

    pub fn deactivate(&mut self) {
        self.active = false;
        self.state = ArchetypeState::Dormant;
    }
}

/// Cast all 5 archetypes from identity entropy.
///
/// Each archetype gets 6 entropy bytes offset by its role index.
/// Horoscope uses identity epoch + role offset (each born seconds apart).
pub fn cast_all_archetypes(identity_entropy: &[u8], identity_epoch: u64) -> Vec<Archetype> {
    ArchetypeRole::all()
        .iter()
        .enumerate()
        .map(|(i, &role)| {
            // Derive 6 bytes for this archetype by offsetting + wrapping
            let mut entropy = [0u8; 6];
            for j in 0..6 {
                let idx = (i * 6 + j) % identity_entropy.len().max(1);
                // XOR with role index for differentiation when entropy is short
                entropy[j] = identity_entropy.get(idx).copied().unwrap_or(0) ^ (i as u8 * 37);
            }

            let hexagram = casting::cast_hexagram(&entropy);
            let horoscope = zodiac_from_epoch(identity_epoch + i as u64 * 3600);

            let primary_emotion = trigram_emotion(hexagram.upper_trigram).to_string();
            let secondary_emotion = trigram_emotion(hexagram.lower_trigram).to_string();

            let seed =
                casting::identity_seed(hexagram.number, &hexagram.lines, identity_epoch + i as u64);

            Archetype {
                role,
                hexagram,
                horoscope,
                primary_emotion,
                secondary_emotion,
                seed,
                active: false,
                state: ArchetypeState::Dormant,
            }
        })
        .collect()
}

/// Classify entropy intensity into archetype activation tiers.
pub fn activation_tier(intensity: f32) -> ActivationTier {
    if intensity < 0.25 {
        ActivationTier::Minimal
    } else if intensity < 0.75 {
        ActivationTier::Moderate
    } else {
        ActivationTier::Full
    }
}

/// Which archetypes should activate at each tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationTier {
    /// < 0.25: decay only (thermodynamic minimum)
    Minimal,
    /// 0.25..0.75: decay + Intuition + Fortune hints
    Moderate,
    /// >= 0.75: full dream + all 5 archetype hints
    Full,
}

impl ActivationTier {
    /// Which roles are active at this tier.
    pub fn active_roles(self) -> Vec<ArchetypeRole> {
        match self {
            Self::Minimal => vec![],
            Self::Moderate => vec![ArchetypeRole::Intuition, ArchetypeRole::Fortune],
            Self::Full => ArchetypeRole::all().to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_all_produces_five() {
        let entropy = [42u8, 128, 200, 10, 180, 90, 55, 100, 30, 250];
        let archetypes = cast_all_archetypes(&entropy, 1772150400);
        assert_eq!(archetypes.len(), 5);

        // Each should have unique seeds (very likely with different entropy)
        let seeds: Vec<u32> = archetypes.iter().map(|a| a.seed).collect();
        for (i, s) in seeds.iter().enumerate() {
            for (j, t) in seeds.iter().enumerate() {
                if i != j {
                    assert_ne!(s, t, "archetypes {i} and {j} have same seed");
                }
            }
        }
    }

    #[test]
    fn cast_all_roles_correct() {
        let entropy = [42u8, 128, 200, 10, 180, 90];
        let archetypes = cast_all_archetypes(&entropy, 1000);
        assert_eq!(archetypes[0].role, ArchetypeRole::Intuition);
        assert_eq!(archetypes[1].role, ArchetypeRole::Fortune);
        assert_eq!(archetypes[2].role, ArchetypeRole::Craft);
        assert_eq!(archetypes[3].role, ArchetypeRole::Ethics);
        assert_eq!(archetypes[4].role, ArchetypeRole::Advocate);
    }

    #[test]
    fn all_start_dormant() {
        let entropy = [42u8; 6];
        let archetypes = cast_all_archetypes(&entropy, 1000);
        for a in &archetypes {
            assert_eq!(a.state, ArchetypeState::Dormant);
            assert!(!a.active);
        }
    }

    #[test]
    fn activation_tiers() {
        assert_eq!(activation_tier(0.0), ActivationTier::Minimal);
        assert_eq!(activation_tier(0.1), ActivationTier::Minimal);
        assert_eq!(activation_tier(0.25), ActivationTier::Moderate);
        assert_eq!(activation_tier(0.5), ActivationTier::Moderate);
        assert_eq!(activation_tier(0.75), ActivationTier::Full);
        assert_eq!(activation_tier(1.0), ActivationTier::Full);
    }

    #[test]
    fn tier_active_roles() {
        assert!(ActivationTier::Minimal.active_roles().is_empty());
        assert_eq!(ActivationTier::Moderate.active_roles().len(), 2);
        assert_eq!(ActivationTier::Full.active_roles().len(), 5);
    }
}
