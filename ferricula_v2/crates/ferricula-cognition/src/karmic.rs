//! Karmic log contract (plan §9 Rule 6, Addendum Rule 10).
//!
//! Every automatic lifecycle action, every drift noting event and every
//! curator briefing is written here with the component version (and gate
//! version when a gate was involved). Cognition never writes the WAL: it
//! appends typed entries to a [`KarmicSink`] the integrator supplies. The
//! sink is fallible on purpose — a persistence failure must surface as an
//! error, never vanish (Coordinator constraint, 2026-09-26).

use serde::{Deserialize, Serialize};

use ferricula_core::memory::now_epoch;

use crate::bhavana::{ReleaseDecision, ReleaseProposal};
use crate::curator::BriefingReceipt;
use crate::outcome::Pool;
use crate::sati::NotingEvent;
use crate::scope::AgentId;

/// Version stamp on every entry cognition emits.
pub const COGNITION_VERSION: &str = concat!("ferricula-cognition/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KarmicEntry {
    pub ts: u64,
    /// "bhavana" | "curator" | "sati" | "outcome"
    pub component: String,
    pub version: String,
    pub gate_version: Option<String>,
    pub sati_enabled: bool,
    pub agent: Option<AgentId>,
    pub event: KarmicEvent,
}

impl KarmicEntry {
    pub fn now(
        component: &str,
        event: KarmicEvent,
        gate_version: Option<String>,
        sati_enabled: bool,
        agent: Option<&AgentId>,
    ) -> Self {
        Self {
            ts: now_epoch(),
            component: component.to_string(),
            version: COGNITION_VERSION.to_string(),
            gate_version,
            sati_enabled,
            agent: agent.cloned(),
            event,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KarmicEvent {
    Noting {
        event: NotingEvent,
    },
    /// A consolidation index was built. Members keep text, state and vector.
    ClusterBuilt {
        cluster_id: u64,
        members: Vec<u32>,
        same_truth: f32,
        contradiction: f32,
        /// Members whose `consolidation_depth` was incremented this time.
        newly_joined: Vec<u32>,
    },
    /// A candidate cluster was routed to review instead of being indexed.
    ClusterReview {
        cluster_id: u64,
        members: Vec<u32>,
        mean_cosine: f32,
        reason: String,
    },
    ReleaseProposed {
        proposal: ReleaseProposal,
    },
    /// Lifecycle state changed. `text_purge_pending` is always `true` from
    /// cognition: the source purge is Engine's commit, not this crate's.
    ReleaseApplied {
        decision: ReleaseDecision,
        text_purge_pending: bool,
    },
    ReleaseDeclined {
        decision: ReleaseDecision,
    },
    Briefing {
        receipt: BriefingReceipt,
    },
    PoolAssigned {
        task_id: String,
        pool: Pool,
        reason: String,
    },
    /// A recall may write back only into a labelled overlay (Addendum §A.5.3(1)).
    /// Cognition proposes; storage of the overlay is the owner's decision.
    ReconstructionProposed {
        parent_id: u32,
        task_sha256: String,
        curator_version: String,
    },
}

/// Append-only sink. Errors must propagate.
pub trait KarmicSink {
    fn append(&mut self, entry: KarmicEntry) -> anyhow::Result<()>;
}

/// In-memory sink for tests and for integrators that flush per cycle.
#[derive(Debug, Default)]
pub struct VecSink {
    pub entries: Vec<KarmicEntry>,
}

impl KarmicSink for VecSink {
    fn append(&mut self, entry: KarmicEntry) -> anyhow::Result<()> {
        self.entries.push(entry);
        Ok(())
    }
}

/// Sink that always fails. Used to prove persistence failures surface.
#[derive(Debug, Default)]
pub struct FailingSink;

impl KarmicSink for FailingSink {
    fn append(&mut self, _entry: KarmicEntry) -> anyhow::Result<()> {
        anyhow::bail!("karmic sink unavailable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_round_trips_as_json_with_version() {
        let e = KarmicEntry::now(
            "bhavana",
            KarmicEvent::ClusterReview {
                cluster_id: 9,
                members: vec![1, 2, 3],
                mean_cosine: 0.9,
                reason: "no merge gate".into(),
            },
            None,
            true,
            None,
        );
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"cluster_review\""));
        assert!(json.contains("ferricula-cognition/"));
        let back: KarmicEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn failing_sink_returns_error() {
        let mut sink = FailingSink;
        let e = KarmicEntry::now(
            "sati",
            KarmicEvent::ReconstructionProposed {
                parent_id: 1,
                task_sha256: "x".into(),
                curator_version: "y".into(),
            },
            None,
            true,
            None,
        );
        assert!(sink.append(e).is_err());
    }
}
