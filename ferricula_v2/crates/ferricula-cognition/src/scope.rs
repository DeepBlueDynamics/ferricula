//! Agent identity scope. Plan §3.3: "The recall path must filter by agent_id
//! and memory state before any text reaches an Ollaya gate." Every cognition
//! entry point that could hand plaintext to a provider takes an [`AgentId`]
//! and rejects a mismatch *before* building any request.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Explicit agent identity. Never derived from a memory id alone.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Result<Self, ScopeError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ScopeError::EmptyAgentId);
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    EmptyAgentId,
    /// A memory belonging to another agent reached a scoped operation.
    AgentMismatch {
        expected: AgentId,
        found: AgentId,
        memory_id: u32,
    },
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScopeError::EmptyAgentId => write!(f, "agent id must not be empty"),
            ScopeError::AgentMismatch {
                expected,
                found,
                memory_id,
            } => write!(
                f,
                "memory {memory_id} belongs to agent {found}, operation is scoped to {expected}"
            ),
        }
    }
}

impl std::error::Error for ScopeError {}

/// Reject any memory whose agent differs from `expected`.
pub fn require_same_agent<'a, I>(expected: &AgentId, items: I) -> Result<(), ScopeError>
where
    I: IntoIterator<Item = (u32, &'a AgentId)>,
{
    for (memory_id, found) in items {
        if found != expected {
            return Err(ScopeError::AgentMismatch {
                expected: expected.clone(),
                found: found.clone(),
                memory_id,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_agent_id_rejected() {
        assert_eq!(AgentId::new("  "), Err(ScopeError::EmptyAgentId));
    }

    #[test]
    fn mismatch_names_the_memory() {
        let a = AgentId::new("a").unwrap();
        let b = AgentId::new("b").unwrap();
        let err = require_same_agent(&a, [(1, &a), (7, &b)]).unwrap_err();
        assert_eq!(
            err,
            ScopeError::AgentMismatch {
                expected: a,
                found: b,
                memory_id: 7
            }
        );
    }
}
