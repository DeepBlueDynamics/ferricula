//! Sovereign choice belongs to the simulated person, not to trigger code.
//!
//! External policy can bound capability (secrets, budgets, duplicate writes,
//! safety) but cannot compel attention, speech, agreement, or publication.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Engage,
    Observe,
    Ignore,
    Defer,
    EstablishBoundary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgencyDecision {
    pub disposition: Disposition,
    /// Private deliberative rationale. Never publish this automatically.
    pub private_reason: String,
    pub confidence: f32,
    /// A decision may be revisited; `None` means no scheduled reconsideration.
    pub revisit_after: Option<u64>,
    /// True only when Steve's cognition selected the disposition. Routers,
    /// schedulers, mention detectors, and operators must leave this false.
    pub self_authored: bool,
}

impl AgencyDecision {
    /// Safe state before cognition has made a choice. This does not imply a
    /// later response and must never be converted into one automatically.
    pub fn awaiting_choice() -> Self {
        Self {
            disposition: Disposition::Defer,
            private_reason: "No self-authored decision has been made".into(),
            confidence: 0.0,
            revisit_after: None,
            self_authored: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposedActionKind {
    NoAction,
    PrivateReflection,
    PublicComment,
    PublicSubmission,
    ToolCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityVerdict {
    pub permitted: bool,
    pub constraint: Option<String>,
}

/// Hard constraints veto unsafe or unauthorized actions. They intentionally
/// have no method that can require an action.
pub trait CapabilityPolicy {
    fn permits(&self, action: ProposedActionKind) -> CapabilityVerdict;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undecided_is_not_an_implicit_reply() {
        let decision = AgencyDecision::awaiting_choice();
        assert_eq!(decision.disposition, Disposition::Defer);
        assert!(!decision.self_authored);
    }
}
