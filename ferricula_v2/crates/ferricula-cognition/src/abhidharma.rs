use ferricula_core::ResonanceGate;
use ferricula_core::memory::Emotion;

/// The Citta-vīthi (cognitive process) stage at the time of advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CittaVithi {
    /// Āvajjana — adverting of consciousness toward the object.
    Avajjana,
    /// Javana — dynamic impulsion (experiential/active phase).
    Javana,
    /// Tadālambana — registration (re-identifying and storing).
    Tadalambana,
}

/// Context representing the internal mind state (Abhidharma terms)
/// of the agent when requesting advice from the internal agent.
#[derive(Debug, Clone)]
pub struct AbhidharmaContext {
    /// The cognitive stage of the operation.
    pub stage: CittaVithi,
    /// The feeling tone/emotion associated with the memory context.
    pub emotion: Option<Emotion>,
    /// The calculated/measured fidelity of the memory structure.
    pub fidelity: f32,
    /// The readout of functional resonance gates (passed/failed status).
    pub gate_readout: Vec<(ResonanceGate, bool)>,
    /// Freeform description or prompt question context.
    pub question: String,
}

/// The decision outcome returned by the internal agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Approve the operation (e.g. promotional merge/echo acceptance).
    Approve(Option<String>),
    /// Reject/prune the memory structure.
    Reject(Option<String>),
    /// Defer the decision/operation.
    Defer(Option<String>),
}

/// Trait for the internal agent that counsels memory operations.
pub trait InternalAgent {
    fn advise(&self, ctx: &AbhidharmaContext) -> Decision;
}

/// Headless/deterministic agent that returns default decisions.
pub struct NoAgent;

impl InternalAgent for NoAgent {
    /// Returns Approve if the Fidelity gate passed (or fidelity >= 0.5), Defer otherwise.
    fn advise(&self, ctx: &AbhidharmaContext) -> Decision {
        let fidelity_passed = ctx
            .gate_readout
            .iter()
            .find(|(gate, _)| *gate == ResonanceGate::Fidelity)
            .map(|(_, passed)| *passed)
            .unwrap_or(ctx.fidelity >= 0.5);

        if fidelity_passed {
            Decision::Approve(Some(
                "Fidelity gate passed (or above threshold)".to_string(),
            ))
        } else {
            Decision::Defer(Some("Fidelity gate failed / below threshold".to_string()))
        }
    }
}

/// Active LLM-backed agent utilizing Anthropic Claude APIs.
pub struct LlmAgent {
    /// Planner containing the API key and client configuration.
    pub planner: crate::planner::Planner,
}

impl LlmAgent {
    pub fn new(agent_key: Option<String>) -> Self {
        Self {
            planner: crate::planner::Planner::new(agent_key),
        }
    }
}

impl InternalAgent for LlmAgent {
    fn advise(&self, ctx: &AbhidharmaContext) -> Decision {
        // TODO: Wire live Anthropic Claude client calls for judgment.
        // For now, delegate to NoAgent as fallback.
        let fallback = NoAgent;
        fallback.advise(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_agent_approve_on_fidelity() {
        let agent = NoAgent;
        let ctx = AbhidharmaContext {
            stage: CittaVithi::Javana,
            emotion: None,
            fidelity: 0.8,
            gate_readout: vec![(ResonanceGate::Fidelity, true)],
            question: "Approve this high-fidelity memory?".to_string(),
        };

        assert_eq!(
            agent.advise(&ctx),
            Decision::Approve(Some(
                "Fidelity gate passed (or above threshold)".to_string()
            ))
        );
    }

    #[test]
    fn test_no_agent_defer_on_low_fidelity() {
        let agent = NoAgent;
        let ctx = AbhidharmaContext {
            stage: CittaVithi::Javana,
            emotion: None,
            fidelity: 0.2,
            gate_readout: vec![(ResonanceGate::Fidelity, false)],
            question: "Approve this low-fidelity memory?".to_string(),
        };

        assert_eq!(
            agent.advise(&ctx),
            Decision::Defer(Some("Fidelity gate failed / below threshold".to_string()))
        );
    }
}
