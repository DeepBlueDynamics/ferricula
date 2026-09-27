//! Decision-gate contract (plan §4, Addendum §A.3.2).
//!
//! Decision ≠ generation. A gate returns a typed answer or a typed abstention
//! with provenance; it never returns an echo dressed as a judgment. Cognition
//! consumes these; Ollaya implementations live in the Gates lane. The only
//! implementations here are [`NoModelGate`] (always abstains) and
//! [`StaticMergeGate`] (harness/test fixture, explicitly not a judgment).
//!
//! Lifecycle rule (plan §6.4): probabilities may drive the lifecycle only
//! after measured calibration. [`Judged::calibrated_answer`] enforces that.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::outcome::Evidence;
use crate::sati::Valence;
use crate::scope::AgentId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AbstainReason {
    NoModel,
    StateTruncated,
    LowConfidence { p_max: f32 },
    ProviderError { message: String },
    NotApplicable,
    InvalidOutput { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict<T> {
    Answer(T),
    Abstain(AbstainReason),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageCost {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub usd: Option<f64>,
    /// "local" or "api:<profile name>"
    pub profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateProvenance {
    pub gate: String,
    pub gate_version: String,
    pub calibration_sha256: Option<String>,
    /// True only when the gate's dev ECE was measured below target for this
    /// calibration file. Uncalibrated answers never drive the lifecycle.
    pub calibrated: bool,
    pub features_sha256: String,
    pub latency_us: u64,
    pub state_truncated: bool,
    pub cost: Option<UsageCost>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Judged<T> {
    pub verdict: Verdict<T>,
    pub provenance: GateProvenance,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateError {
    OutOfRange { gate: String, field: &'static str, value: f32 },
    CalibratedWithoutHash { gate: String },
    EmptyProvenance { field: &'static str },
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GateError::OutOfRange { gate, field, value } => {
                write!(f, "gate {gate}: {field} = {value} is not finite within its range")
            }
            GateError::CalibratedWithoutHash { gate } => {
                write!(f, "gate {gate}: claims calibration without a calibration hash")
            }
            GateError::EmptyProvenance { field } => write!(f, "gate provenance field {field} is empty"),
        }
    }
}

impl std::error::Error for GateError {}

/// Fields that must be finite within a closed range.
pub trait Bounded {
    fn bounded_fields(&self) -> Vec<(&'static str, f32, f32, f32)>; // (name, value, min, max)
}

impl<T: Bounded> Judged<T> {
    pub fn validate(&self) -> Result<(), GateError> {
        if self.provenance.gate.is_empty() {
            return Err(GateError::EmptyProvenance { field: "gate" });
        }
        if self.provenance.gate_version.is_empty() {
            return Err(GateError::EmptyProvenance { field: "gate_version" });
        }
        if self.provenance.features_sha256.is_empty() {
            return Err(GateError::EmptyProvenance { field: "features_sha256" });
        }
        if self.provenance.calibrated && self.provenance.calibration_sha256.is_none() {
            return Err(GateError::CalibratedWithoutHash {
                gate: self.provenance.gate.clone(),
            });
        }
        if let Verdict::Answer(a) = &self.verdict {
            for (field, value, min, max) in a.bounded_fields() {
                if !value.is_finite() || value < min || value > max {
                    return Err(GateError::OutOfRange {
                        gate: self.provenance.gate.clone(),
                        field,
                        value,
                    });
                }
            }
        }
        Ok(())
    }

    /// Validated answer; `None` on abstention.
    pub fn answer(&self) -> Result<Option<&T>, GateError> {
        self.validate()?;
        Ok(match &self.verdict {
            Verdict::Answer(a) => Some(a),
            Verdict::Abstain(_) => None,
        })
    }

    /// Validated answer usable for lifecycle decisions: requires measured
    /// calibration and an untruncated state.
    pub fn calibrated_answer(&self) -> Result<Option<&T>, GateError> {
        let a = self.answer()?;
        if !self.provenance.calibrated || self.provenance.state_truncated {
            return Ok(None);
        }
        Ok(a)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SatiRecallVerdict {
    /// Probability the memory answers the query.
    pub answers_query: f32,
    /// Expected relevance level 0..3 (plan §4.4).
    pub relevance: f32,
}

impl Bounded for SatiRecallVerdict {
    fn bounded_fields(&self) -> Vec<(&'static str, f32, f32, f32)> {
        vec![
            ("answers_query", self.answers_query, 0.0, 1.0),
            ("relevance", self.relevance, 0.0, 3.0),
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeVerdict {
    pub same_truth: f32,
    pub contradiction: f32,
}

impl Bounded for MergeVerdict {
    fn bounded_fields(&self) -> Vec<(&'static str, f32, f32, f32)> {
        vec![
            ("same_truth", self.same_truth, 0.0, 1.0),
            ("contradiction", self.contradiction, 0.0, 1.0),
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskSucceededVerdict {
    pub p: f32,
}

impl Bounded for TaskSucceededVerdict {
    fn bounded_fields(&self) -> Vec<(&'static str, f32, f32, f32)> {
        vec![("p", self.p, 0.0, 1.0)]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VedanaVerdict {
    pub valence: Valence,
    /// 0..4 (plan §4.1 intensity score).
    pub intensity: f32,
    pub p: f32,
}

impl Bounded for VedanaVerdict {
    fn bounded_fields(&self) -> Vec<(&'static str, f32, f32, f32)> {
        vec![("intensity", self.intensity, 0.0, 4.0), ("p", self.p, 0.0, 1.0)]
    }
}

pub trait SatiRecallGate {
    fn judge(&self, agent: &AgentId, query: &str, memory: &str) -> Judged<SatiRecallVerdict>;
}

pub trait MergeGate {
    fn judge(&self, agent: &AgentId, members: &[&str]) -> Judged<MergeVerdict>;
}

pub trait TaskSucceededGate {
    fn judge(&self, agent: &AgentId, transcript: &str, evidence: &[Evidence]) -> Judged<TaskSucceededVerdict>;
}

pub trait VedanaGate {
    fn judge(&self, agent: &AgentId, text: &str) -> Judged<VedanaVerdict>;
}

/// SHA-256 over the concatenation of `parts`, hex encoded. Used for
/// `features_sha256` and for text hashes in receipts.
pub fn sha256_hex(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn no_model_provenance(gate: &str, features: &[&str]) -> GateProvenance {
    GateProvenance {
        gate: gate.to_string(),
        gate_version: "none/0".to_string(),
        calibration_sha256: None,
        calibrated: false,
        features_sha256: sha256_hex(features),
        latency_us: 0,
        state_truncated: false,
        cost: None,
    }
}

/// Always abstains with [`AbstainReason::NoModel`]. Never an echo.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoModelGate;

impl SatiRecallGate for NoModelGate {
    fn judge(&self, agent: &AgentId, query: &str, memory: &str) -> Judged<SatiRecallVerdict> {
        Judged {
            verdict: Verdict::Abstain(AbstainReason::NoModel),
            provenance: no_model_provenance("sati-recall", &[agent.as_str(), query, memory]),
        }
    }
}

impl MergeGate for NoModelGate {
    fn judge(&self, agent: &AgentId, members: &[&str]) -> Judged<MergeVerdict> {
        let mut features = vec![agent.as_str()];
        features.extend_from_slice(members);
        Judged {
            verdict: Verdict::Abstain(AbstainReason::NoModel),
            provenance: no_model_provenance("sankhara-merge", &features),
        }
    }
}

impl TaskSucceededGate for NoModelGate {
    fn judge(&self, agent: &AgentId, transcript: &str, _evidence: &[Evidence]) -> Judged<TaskSucceededVerdict> {
        Judged {
            verdict: Verdict::Abstain(AbstainReason::NoModel),
            provenance: no_model_provenance("task_succeeded", &[agent.as_str(), transcript]),
        }
    }
}

impl VedanaGate for NoModelGate {
    fn judge(&self, agent: &AgentId, text: &str) -> Judged<VedanaVerdict> {
        Judged {
            verdict: Verdict::Abstain(AbstainReason::NoModel),
            provenance: no_model_provenance("vedana", &[agent.as_str(), text]),
        }
    }
}

/// Harness/test fixture: returns a fixed merge verdict. It is NOT a judgment
/// of the members; it exists so integration tests can exercise the index
/// path without Ollaya. `calibrated` is a fixture flag, not a measurement.
#[derive(Debug, Clone)]
pub struct StaticMergeGate {
    pub same_truth: f32,
    pub contradiction: f32,
    pub calibrated: bool,
}

impl MergeGate for StaticMergeGate {
    fn judge(&self, agent: &AgentId, members: &[&str]) -> Judged<MergeVerdict> {
        let mut features = vec![agent.as_str()];
        features.extend_from_slice(members);
        Judged {
            verdict: Verdict::Answer(MergeVerdict {
                same_truth: self.same_truth,
                contradiction: self.contradiction,
            }),
            provenance: GateProvenance {
                gate: "sankhara-merge".into(),
                gate_version: "fixture/static".into(),
                calibration_sha256: if self.calibrated {
                    Some("fixture".into())
                } else {
                    None
                },
                calibrated: self.calibrated,
                features_sha256: sha256_hex(&features),
                latency_us: 0,
                state_truncated: false,
                cost: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> AgentId {
        AgentId::new("agent_test").unwrap()
    }

    #[test]
    fn no_model_gate_abstains_with_provenance() {
        let j = SatiRecallGate::judge(&NoModelGate, &agent(), "q", "m");
        assert_eq!(j.answer().unwrap(), None);
        assert!(matches!(j.verdict, Verdict::Abstain(AbstainReason::NoModel)));
        assert!(!j.provenance.calibrated);
        assert_eq!(j.provenance.features_sha256.len(), 64);
    }

    #[test]
    fn out_of_range_probability_is_rejected() {
        let mut j = MergeGate::judge(
            &StaticMergeGate {
                same_truth: 1.2,
                contradiction: 0.0,
                calibrated: true,
            },
            &agent(),
            &["a", "b", "c"],
        );
        assert!(matches!(j.validate(), Err(GateError::OutOfRange { field: "same_truth", .. })));
        if let Verdict::Answer(a) = &mut j.verdict {
            a.same_truth = f32::NAN;
        }
        assert!(j.validate().is_err());
    }

    #[test]
    fn calibrated_answer_requires_calibration_and_full_state() {
        let uncal = MergeGate::judge(
            &StaticMergeGate {
                same_truth: 0.9,
                contradiction: 0.0,
                calibrated: false,
            },
            &agent(),
            &["a", "b", "c"],
        );
        assert!(uncal.answer().unwrap().is_some());
        assert!(uncal.calibrated_answer().unwrap().is_none());

        let mut cal = MergeGate::judge(
            &StaticMergeGate {
                same_truth: 0.9,
                contradiction: 0.0,
                calibrated: true,
            },
            &agent(),
            &["a", "b", "c"],
        );
        assert!(cal.calibrated_answer().unwrap().is_some());
        cal.provenance.state_truncated = true;
        assert!(cal.calibrated_answer().unwrap().is_none());
    }

    #[test]
    fn calibrated_without_hash_is_invalid() {
        let mut j = MergeGate::judge(
            &StaticMergeGate {
                same_truth: 0.9,
                contradiction: 0.0,
                calibrated: true,
            },
            &agent(),
            &["a", "b", "c"],
        );
        j.provenance.calibration_sha256 = None;
        assert!(matches!(j.validate(), Err(GateError::CalibratedWithoutHash { .. })));
    }
}
