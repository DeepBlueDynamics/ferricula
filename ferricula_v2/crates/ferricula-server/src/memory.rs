use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use ferricula_core::{DurableEngine, LifecycleState};
use serde::Serialize;

/// Read access to the recovery copy. Experience commits remain serialized by
/// the Steve task loop; candidate lookup itself never mutates memory.
pub struct MemoryRuntime {
    engine: Mutex<DurableEngine>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryHit {
    pub id: u32,
    pub score: f32,
    pub state: LifecycleStateView,
    pub fidelity: f32,
    pub importance: f32,
    pub keystone: bool,
    pub tags: BTreeMap<String, String>,
    pub refs: Option<ferricula_core::MemoryRef>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStateView {
    Active,
    Forgiven,
    Archived,
}

impl From<LifecycleState> for LifecycleStateView {
    fn from(value: LifecycleState) -> Self {
        match value {
            LifecycleState::Active => Self::Active,
            LifecycleState::Forgiven => Self::Forgiven,
            LifecycleState::Archived => Self::Archived,
        }
    }
}

impl MemoryRuntime {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            engine: Mutex::new(DurableEngine::open(path)?),
        })
    }

    /// Provider-free lexical candidate retrieval keeps Steve's continuity
    /// available before an embedding service is configured. Dense/hybrid
    /// ranking can refine these candidates later.
    pub fn recall_candidates(&self, query: &str, limit: usize) -> Vec<MemoryHit> {
        let tokens = tokens(query);
        if tokens.is_empty() || limit == 0 {
            return Vec::new();
        }
        let engine = self.engine.lock().expect("memory engine poisoned");
        let mut hits = Vec::new();
        for row in engine.engine().rows_iter() {
            let Some(record) = engine.memory_store().get(row.id) else {
                continue;
            };
            // Deliberate release excludes plaintext before any ranking or model call.
            // Low-priority Active memories remain eligible; decay is not deletion.
            if record.state != LifecycleState::Active {
                continue;
            }
            let haystack = row
                .tags
                .iter()
                .map(|(key, value)| format!("{key} {value}"))
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            let matched = tokens
                .iter()
                .filter(|token| haystack.contains(*token))
                .count();
            if matched == 0 {
                continue;
            }
            let coverage = matched as f32 / tokens.len() as f32;
            let state_weight = match record.state {
                LifecycleState::Active => 1.0,
                LifecycleState::Forgiven => 0.55,
                LifecycleState::Archived => 0.35,
            };
            let editorial =
                1.0 + record.importance.max(0.0) * 0.15 + f32::from(record.keystone) * 0.15;
            hits.push(MemoryHit {
                id: row.id,
                score: coverage * record.fidelity * state_weight * editorial,
                state: record.state.into(),
                fidelity: record.fidelity,
                importance: record.importance,
                keystone: record.keystone,
                tags: row.tags.clone(),
                refs: row.refs.clone(),
            });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        hits.truncate(limit.min(50));
        hits
    }
}

fn tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| part.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_text_is_filtered_after_restart_but_decayed_active_text_remains() {
        use ferricula_core::memory::MemoryRecord;
        use ferricula_core::model::Row;
        let dir = std::env::current_dir().unwrap().join("target")
            .join(format!("recall-release-{}", uuid::Uuid::new_v4()));
        let mut engine = DurableEngine::open(&dir).unwrap();
        for id in 1..=3 {
            let row = Row {
                id, vector: vec![1.0, 0.0], refs: None,
                tags: BTreeMap::from([("text".into(), format!("ÜBER marker {id}"))]),
            };
            let mut record = MemoryRecord::new(id);
            if id == 1 { record.fidelity = 0.0; }
            if id >= 2 { record.forgive(); }
            if id == 3 { record.archive(); record.keystone = true; }
            engine.remember(row, record).unwrap();
        }
        drop(engine);
        let runtime = MemoryRuntime::open(&dir).unwrap();
        let before = runtime.engine.lock().unwrap().memory_store().get(1).unwrap().recall_count;
        let hits = runtime.recall_candidates("über", 10);
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(hits[0].fidelity, 0.0);
        assert_eq!(hits[0].tags["text"], "ÜBER marker 1");
        let engine = runtime.engine.lock().unwrap();
        assert_eq!(engine.memory_store().len(), 3);
        assert_eq!(engine.memory_store().get(1).unwrap().recall_count, before);
        drop(engine);
        drop(runtime);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tokenization_is_stable_and_deduplicated() {
        assert_eq!(
            tokens("Design, design—great products!"),
            ["design", "great", "products"]
        );
    }
}
