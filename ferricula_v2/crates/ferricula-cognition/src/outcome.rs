//! Storage quality filter (Addendum §A.3) and the self-judgment gap (§A.5.3(4)).
//!
//! `task_succeeded` rule, verbatim: "credit only outcomes that external
//! evidence confirms (tool results, state changes), ignore the agent's own
//! claims of success, and treat ambiguous or partial outcomes as failures."

use serde::{Deserialize, Serialize};

use crate::gates::{Judged, TaskSucceededVerdict};
use crate::karmic::{KarmicEntry, KarmicEvent, KarmicSink};
use crate::scope::AgentId;

/// Which recall pool a memory belongs to.
///
/// `Unclassified` is for legacy factual/episodic memories that never passed
/// through a task outcome. They are recalled normally (Coordinator, 2026-09-26:
/// "do not classify legacy facts as failed tasks").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pool {
    Success,
    Failure,
    Unclassified,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    ToolResult { ok: bool, summary: String },
    StateChange { summary: String },
    TestPass { name: String },
    UserAcceptance,
}

#[derive(Debug, Clone)]
pub struct TaskOutcome {
    pub task_id: String,
    pub agent: AgentId,
    /// The agent's own claim. Never decides storage.
    pub self_verdict: Option<bool>,
    pub evidence: Vec<Evidence>,
    pub gate: Option<Judged<TaskSucceededVerdict>>,
    /// Benchmark ground truth, when it exists. Never decides storage either
    /// (benchmarks §4.2: the gate decides, preventing label leakage); it only
    /// feeds the self-judgment and judge-agreement metrics.
    pub ground_truth: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PoolAssignment {
    pub pool: Pool,
    pub reason: String,
}

/// Strict assignment. Success requires external evidence AND a valid
/// `task_succeeded` answer with `p >= tau`. Everything else is `Failure`.
/// Never returns `Unclassified`: that pool is reserved for memories that were
/// never task trajectories.
pub fn assign_pool(outcome: &TaskOutcome, tau: f32) -> PoolAssignment {
    if outcome.evidence.is_empty() {
        return PoolAssignment {
            pool: Pool::Failure,
            reason: "no external evidence".into(),
        };
    }
    let Some(gate) = &outcome.gate else {
        return PoolAssignment {
            pool: Pool::Failure,
            reason: "no task_succeeded verdict".into(),
        };
    };
    match gate.answer() {
        Err(e) => PoolAssignment {
            pool: Pool::Failure,
            reason: format!("invalid task_succeeded verdict: {e}"),
        },
        Ok(None) => PoolAssignment {
            pool: Pool::Failure,
            reason: "task_succeeded abstained".into(),
        },
        Ok(Some(v)) if v.p >= tau => PoolAssignment {
            pool: Pool::Success,
            reason: format!("task_succeeded p={:.3} >= tau={:.3} with {} evidence items", v.p, tau, outcome.evidence.len()),
        },
        Ok(Some(v)) => PoolAssignment {
            pool: Pool::Failure,
            reason: format!("task_succeeded p={:.3} < tau={:.3}", v.p, tau),
        },
    }
}

/// Evidence-based verdict as a probability: ground truth when present, else
/// the validated gate answer. `None` when neither exists.
pub fn evidence_verdict(outcome: &TaskOutcome) -> Option<f32> {
    if let Some(gt) = outcome.ground_truth {
        return Some(if gt { 1.0 } else { 0.0 });
    }
    outcome.gate.as_ref()?.answer().ok()?.map(|v| v.p)
}

/// Motivated self-judgment, made measurable: `self − evidence`. A persistent
/// positive gap is a samudaya signal (Addendum §A.5.3(4)).
pub fn self_judgment_gap(outcome: &TaskOutcome) -> Option<f32> {
    let self_p = if outcome.self_verdict? { 1.0 } else { 0.0 };
    Some(self_p - evidence_verdict(outcome)?)
}

/// Assign and log. The log write must succeed for the assignment to count.
pub fn record_pool(
    outcome: &TaskOutcome,
    tau: f32,
    sink: &mut dyn KarmicSink,
) -> anyhow::Result<PoolAssignment> {
    let assignment = assign_pool(outcome, tau);
    sink.append(KarmicEntry::now(
        "outcome",
        KarmicEvent::PoolAssigned {
            task_id: outcome.task_id.clone(),
            pool: assignment.pool,
            reason: assignment.reason.clone(),
        },
        outcome.gate.as_ref().map(|g| g.provenance.gate_version.clone()),
        true,
        Some(&outcome.agent),
    ))?;
    Ok(assignment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gates::{AbstainReason, GateProvenance, Verdict};
    use crate::karmic::{FailingSink, VecSink};

    fn judged(p: f32) -> Judged<TaskSucceededVerdict> {
        Judged {
            verdict: Verdict::Answer(TaskSucceededVerdict { p }),
            provenance: GateProvenance {
                gate: "task_succeeded".into(),
                gate_version: "fixture".into(),
                calibration_sha256: None,
                calibrated: false,
                features_sha256: "f".into(),
                latency_us: 1,
                state_truncated: false,
                cost: None,
            },
        }
    }

    fn outcome(self_verdict: Option<bool>, evidence: Vec<Evidence>, gate: Option<Judged<TaskSucceededVerdict>>) -> TaskOutcome {
        TaskOutcome {
            task_id: "t1".into(),
            agent: AgentId::new("a").unwrap(),
            self_verdict,
            evidence,
            gate,
            ground_truth: None,
        }
    }

    #[test]
    fn self_claim_without_evidence_is_failure() {
        let o = outcome(Some(true), vec![], Some(judged(0.99)));
        assert_eq!(assign_pool(&o, 0.5).pool, Pool::Failure);
    }

    #[test]
    fn evidence_and_confident_gate_is_success() {
        let o = outcome(
            Some(false),
            vec![Evidence::TestPass { name: "unit".into() }],
            Some(judged(0.8)),
        );
        assert_eq!(assign_pool(&o, 0.5).pool, Pool::Success);
    }

    #[test]
    fn abstain_or_missing_gate_is_failure() {
        let mut j = judged(0.9);
        j.verdict = Verdict::Abstain(AbstainReason::StateTruncated);
        let o = outcome(Some(true), vec![Evidence::UserAcceptance], Some(j));
        assert_eq!(assign_pool(&o, 0.5).pool, Pool::Failure);
        let o = outcome(Some(true), vec![Evidence::UserAcceptance], None);
        assert_eq!(assign_pool(&o, 0.5).pool, Pool::Failure);
    }

    #[test]
    fn gap_is_self_minus_evidence() {
        let mut o = outcome(Some(true), vec![Evidence::UserAcceptance], Some(judged(0.25)));
        assert!((self_judgment_gap(&o).unwrap() - 0.75).abs() < 1e-6);
        o.ground_truth = Some(true);
        assert!((self_judgment_gap(&o).unwrap()).abs() < 1e-6);
        o.self_verdict = None;
        assert_eq!(self_judgment_gap(&o), None);
    }

    #[test]
    fn record_pool_fails_when_sink_fails() {
        let o = outcome(None, vec![Evidence::UserAcceptance], Some(judged(0.9)));
        assert!(record_pool(&o, 0.5, &mut FailingSink).is_err());
        let mut sink = VecSink::default();
        let a = record_pool(&o, 0.5, &mut sink).unwrap();
        assert_eq!(a.pool, Pool::Success);
        assert_eq!(sink.entries.len(), 1);
        assert_eq!(sink.entries[0].component, "outcome");
    }
}
