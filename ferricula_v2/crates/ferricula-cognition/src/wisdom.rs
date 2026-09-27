//! The Wisdom Kings are bounded perspectives inside one mind.
//!
//! They do not own memories, tools, credentials, schedulers, or public
//! identities.  A council invocation produces traceable suggestions that the
//! single Steve decision loop may integrate or reject.

use serde::{Deserialize, Serialize};

/// The five inherited perspectives stored in legacy Ferricula identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WisdomKing {
    Intuition,
    Fortune,
    Craft,
    Ethics,
    Advocate,
}

impl WisdomKing {
    pub const ALL: [Self; 5] = [
        Self::Intuition,
        Self::Fortune,
        Self::Craft,
        Self::Ethics,
        Self::Advocate,
    ];
}

/// Shared, bounded controls that perspectives may propose changing for one
/// decision. Values are normalized to 0..=1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CognitiveControls {
    pub associative_breadth: f32,
    pub novelty_tolerance: f32,
    pub exploration: f32,
    pub revision_pressure: f32,
    pub evidence_threshold: f32,
    pub publication_threshold: f32,
    pub audience_simulation: f32,
}

impl Default for CognitiveControls {
    fn default() -> Self {
        Self {
            associative_breadth: 0.5,
            novelty_tolerance: 0.5,
            exploration: 0.5,
            revision_pressure: 0.5,
            evidence_threshold: 0.5,
            publication_threshold: 0.5,
            audience_simulation: 0.5,
        }
    }
}

impl CognitiveControls {
    fn apply_delta(&mut self, field: ControlField, delta: f32) {
        let value = match field {
            ControlField::AssociativeBreadth => &mut self.associative_breadth,
            ControlField::NoveltyTolerance => &mut self.novelty_tolerance,
            ControlField::Exploration => &mut self.exploration,
            ControlField::RevisionPressure => &mut self.revision_pressure,
            ControlField::EvidenceThreshold => &mut self.evidence_threshold,
            ControlField::PublicationThreshold => &mut self.publication_threshold,
            ControlField::AudienceSimulation => &mut self.audience_simulation,
        };
        *value = (*value + delta).clamp(0.0, 1.0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlField {
    AssociativeBreadth,
    NoveltyTolerance,
    Exploration,
    RevisionPressure,
    EvidenceThreshold,
    PublicationThreshold,
    AudienceSimulation,
}

/// Context is deliberately small and contains no tool handles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhisperContext {
    pub task_kind: String,
    pub public_action: bool,
    pub novelty: f32,
    pub uncertainty: f32,
    pub cognitive_heat: f32,
    /// Preserves Ferricula's activation tiers: <0.25 is thermodynamic-only,
    /// 0.25..0.75 activates Intuition and Fortune, >=0.75 invites all five.
    #[serde(default = "full_intensity")]
    pub intensity: f32,
}

fn full_intensity() -> f32 {
    1.0
}

/// One auditable, expiring modulation proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Whisper {
    pub king: WisdomKing,
    pub field: ControlField,
    pub delta: f32,
    pub reason: String,
    /// The modulation is scoped to this many decisions. Initial
    /// implementation always emits one-decision whispers.
    pub expires_after_decisions: u8,
}

#[derive(Debug, Clone, Default)]
pub struct WisdomCouncil;

impl WisdomCouncil {
    /// Deterministic baseline whispers. An LLM may later propose prose or
    /// additional deltas, but it must return through the same bounded type.
    pub fn whisper(&self, ctx: &WhisperContext) -> Vec<Whisper> {
        let intensity = ctx.intensity.clamp(0.0, 1.0);
        if intensity < 0.25 {
            return Vec::new();
        }
        let full_council = intensity >= 0.75;
        let mut out = vec![
            Whisper {
                king: WisdomKing::Intuition,
                field: ControlField::AssociativeBreadth,
                delta: 0.10 + 0.10 * ctx.novelty.clamp(0.0, 1.0),
                reason: "Widen association around novel material".into(),
                expires_after_decisions: 1,
            },
            Whisper {
                king: WisdomKing::Fortune,
                field: ControlField::Exploration,
                delta: if ctx.cognitive_heat < 0.7 {
                    0.10
                } else {
                    -0.15
                },
                reason: "Explore when capacity is available; conserve when hot".into(),
                expires_after_decisions: 1,
            },
        ];
        if full_council {
            out.extend([
                Whisper {
                    king: WisdomKing::Craft,
                    field: ControlField::RevisionPressure,
                    delta: if ctx.public_action { 0.25 } else { 0.10 },
                    reason: "Public work should survive another editing pass".into(),
                    expires_after_decisions: 1,
                },
                Whisper {
                    king: WisdomKing::Ethics,
                    field: ControlField::PublicationThreshold,
                    delta: if ctx.public_action {
                        0.15 + 0.15 * ctx.uncertainty.clamp(0.0, 1.0)
                    } else {
                        0.0
                    },
                    reason: "Raise the bar for uncertain public actions".into(),
                    expires_after_decisions: 1,
                },
                Whisper {
                    king: WisdomKing::Advocate,
                    field: ControlField::AudienceSimulation,
                    delta: if ctx.public_action { 0.25 } else { 0.05 },
                    reason: "Test the idea against the reader's strongest objection".into(),
                    expires_after_decisions: 1,
                },
            ]);
        }

        if full_council && ctx.uncertainty > 0.65 {
            out.push(Whisper {
                king: WisdomKing::Craft,
                field: ControlField::EvidenceThreshold,
                delta: 0.20,
                reason: "Uncertainty requires stronger evidence".into(),
                expires_after_decisions: 1,
            });
        }
        out
    }

    pub fn integrate(
        &self,
        baseline: &CognitiveControls,
        whispers: &[Whisper],
    ) -> CognitiveControls {
        let mut controls = baseline.clone();
        for whisper in whispers {
            controls.apply_delta(whisper.field, whisper.delta.clamp(-0.35, 0.35));
        }
        controls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_uncertain_action_raises_guardrails() {
        let council = WisdomCouncil;
        let baseline = CognitiveControls::default();
        let whispers = council.whisper(&WhisperContext {
            task_kind: "comment".into(),
            public_action: true,
            novelty: 0.8,
            uncertainty: 0.9,
            cognitive_heat: 0.2,
            intensity: 1.0,
        });
        let controls = council.integrate(&baseline, &whispers);
        assert!(controls.publication_threshold > baseline.publication_threshold);
        assert!(controls.revision_pressure > baseline.revision_pressure);
        assert!(controls.evidence_threshold > baseline.evidence_threshold);
        assert!(whispers.iter().all(|w| w.expires_after_decisions == 1));
    }

    #[test]
    fn integration_is_bounded() {
        let council = WisdomCouncil;
        let controls = council.integrate(
            &CognitiveControls::default(),
            &[Whisper {
                king: WisdomKing::Intuition,
                field: ControlField::Exploration,
                delta: 50.0,
                reason: "test".into(),
                expires_after_decisions: 1,
            }],
        );
        assert_eq!(controls.exploration, 0.85);
    }

    #[test]
    fn activation_tiers_preserve_original_council_shape() {
        let council = WisdomCouncil;
        let mut context = WhisperContext {
            task_kind: "dream".into(),
            public_action: false,
            novelty: 0.5,
            uncertainty: 0.5,
            cognitive_heat: 0.2,
            intensity: 0.1,
        };
        assert!(council.whisper(&context).is_empty());
        context.intensity = 0.5;
        assert_eq!(council.whisper(&context).len(), 2);
        context.intensity = 1.0;
        assert_eq!(council.whisper(&context).len(), 5);
    }
}
