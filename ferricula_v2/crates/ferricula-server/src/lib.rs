use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use ferricula_core::{DurableEngine, LifecycleState};
use serde::{Deserialize, Serialize};

pub mod api;
pub mod harness;
pub mod curation;
pub mod mcp;
pub mod evidence_card;
pub mod autonomy;
pub mod config;
pub mod feeds;
pub mod memory;
pub use ferricula_episode::memory_overlay;
pub use ferricula_episode;
pub mod mention_ingest;
pub mod model;
pub mod model_config;
pub mod model_transport;
pub mod nutnews;
pub mod nutnews_events;
pub mod runtime;
pub mod sleep_cycle;

pub const CRATE_NAME: &str = "ferricula-server";

#[cfg(test)]
mod identity_tests {
    #[test]
    fn wrong_memory_identity_rejected_before_runtime_state_creation() {
        let temp = std::env::current_dir().unwrap().join("target")
            .join(format!("identity-test-{}", uuid::Uuid::new_v4()));
        let memory = temp.join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        std::fs::write(memory.join("identity.json"),
            r#"{"agent_id":"ferricula-stevejobs","name":"Steve Jobs"}"#).unwrap();
        let inspection = super::inspect_data_dir(&memory).unwrap();
        let mut config = crate::config::RuntimeConfig::default();
        config.memory_dir = memory;
        config.state_dir = temp.join("must-not-be-created");
        config.expected_agent_id = "ferricula-memory-bench".into();
        config.models.identity.agent_id = config.expected_agent_id.clone();
        let state = config.state_dir.clone();
        let result = crate::runtime::SteveRuntime::open(config, inspection);
        assert!(matches!(result, Err(ref error) if error.to_string().contains("mounted memory identity")));
        assert!(!state.exists());
        std::fs::remove_dir_all(temp).unwrap();
    }
}

#[derive(Deserialize)]
struct IdentityHeader {
    agent_id: String,
    name: String,
}

/// Read-only summary proving that a legacy Ferricula data directory can be
/// opened by the v2 persistence layer. Opening replays the WAL in memory but
/// does not checkpoint or otherwise modify the directory.
#[derive(Debug, Serialize)]
pub struct Inspection {
    pub agent_id: String,
    pub name: String,
    pub rows: usize,
    pub memories: usize,
    pub active: usize,
    pub forgiven: usize,
    pub archived: usize,
    pub keystones: usize,
    pub graph_nodes: usize,
    pub graph_edges: usize,
    pub prime_tree_terms: usize,
    pub prime_tree_nodes: usize,
    pub prime_tree_members: u64,
}

pub fn inspect_data_dir(data_dir: impl AsRef<Path>) -> Result<Inspection> {
    let data_dir = data_dir.as_ref();
    let identity_path = data_dir.join("identity.json");
    let identity: IdentityHeader = serde_json::from_slice(
        &fs::read(&identity_path)
            .with_context(|| format!("failed to read {}", identity_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", identity_path.display()))?;

    let durable = DurableEngine::open(data_dir)
        .with_context(|| format!("failed to open memory store {}", data_dir.display()))?;
    let records = durable.memory_store();

    Ok(Inspection {
        agent_id: identity.agent_id,
        name: identity.name,
        rows: durable.engine().row_count(),
        memories: records.len(),
        active: records.in_state(LifecycleState::Active).len(),
        forgiven: records.in_state(LifecycleState::Forgiven).len(),
        archived: records.in_state(LifecycleState::Archived).len(),
        keystones: records.keystones().len(),
        graph_nodes: durable.graph().node_count(),
        graph_edges: durable.graph().edge_count(),
        prime_tree_terms: durable.prime_tree().root_count(),
        prime_tree_nodes: durable.prime_tree().node_count(),
        prime_tree_members: durable.prime_tree().total_members(),
    })
}
