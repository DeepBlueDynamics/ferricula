use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::graph::{Edge, EdgeKind, MemoryGraph};
use crate::memory::{MemoryRecord, MemoryStore};
use crate::model::{QueryResult, Row};
use crate::prime_tree::{PrimeTree, PrimeTreeSnapshot};
use crate::skg::{SkgSnapshot, SkgState};

const SNAPSHOT_V1_FILE: &str = "snapshot.bin";
const SNAPSHOT_V2_FILE: &str = "snapshot_v2.bin";
const SNAPSHOT_V3_FILE: &str = "snapshot_v3.bin";
const SNAPSHOT_V4_FILE: &str = "snapshot_v4.bin";
const WAL_FILE: &str = "wal.log";
const SNAPSHOT_TMP_FILE: &str = "snapshot_v4.bin.tmp";

#[derive(Debug)]
pub struct DurableEngine {
    engine: Engine,
    memory_store: MemoryStore,
    graph: MemoryGraph,
    prime_tree: PrimeTree,
    skg: SkgState,
    persistence: Persistence,
}

impl DurableEngine {
    pub fn open(base_dir: impl AsRef<Path>) -> Result<Self> {
        let persistence = Persistence::new(base_dir)?;
        let (engine, memory_store, graph, prime_tree, skg) = persistence.load()?;
        Ok(Self {
            engine,
            memory_store,
            graph,
            prime_tree,
            skg,
            persistence,
        })
    }

    // --- Row operations (original API) ---

    pub fn upsert(&mut self, row: Row) -> Result<()> {
        self.engine.upsert(row.clone())?;
        self.persistence.append_wal(&WalEntry::Upsert(row))?;
        self.maybe_rotate()
    }

    pub fn delete(&mut self, id: u32) -> Result<bool> {
        let deleted = self.engine.delete(id);
        if deleted {
            self.persistence.append_wal(&WalEntry::Delete { id })?;
            self.maybe_rotate()?;
        }
        Ok(deleted)
    }

    pub fn execute_sql(&self, sql: &str) -> Result<QueryResult> {
        self.engine.execute_sql(sql)
    }

    pub fn execute_sql_with_embed(&self, sql: &str, chonk_url: Option<&str>) -> Result<QueryResult> {
        self.engine.execute_sql_with_embed(sql, chonk_url)
    }

    // --- Memory record operations ---

    pub fn remember(&mut self, row: Row, record: MemoryRecord) -> Result<()> {
        let r = row.clone();
        let rec = record.clone();
        self.engine.upsert(row)?;
        self.memory_store.insert(record);
        self.persistence.append_wal(&WalEntry::Remember {
            row: r,
            record: rec,
        })?;
        self.maybe_rotate()
    }

    pub fn update_record(&mut self, record: MemoryRecord) -> Result<()> {
        let rec = record.clone();
        self.memory_store.insert(record);
        self.persistence.append_wal(&WalEntry::UpdateRecord(rec))?;
        self.maybe_rotate()
    }

    pub fn remove_memory(&mut self, id: u32) -> Result<bool> {
        let deleted = self.engine.delete(id);
        self.memory_store.remove(id);
        self.graph.remove_node(id);
        self.prime_tree.remove_member(id);
        if deleted {
            self.persistence
                .append_wal(&WalEntry::RemoveMemory { id })?;
            self.maybe_rotate()?;
        }
        Ok(deleted)
    }

    // --- Graph operations ---

    pub fn connect(&mut self, a: u32, b: u32, label: String, weight: f32, kind: EdgeKind) -> Result<()> {
        self.graph.connect(a, b, label.clone(), weight, kind);
        match kind {
            EdgeKind::Semantic => {
                self.persistence.append_wal(&WalEntry::Connect {
                    a,
                    b,
                    label,
                    weight,
                })?;
            }
            EdgeKind::Causal => {
                self.persistence.append_wal(&WalEntry::ConnectCausal {
                    from: a,
                    to: b,
                    label,
                    weight,
                })?;
            }
        }
        self.maybe_rotate()
    }

    pub fn disconnect(&mut self, a: u32, b: u32) -> Result<()> {
        self.graph.disconnect(a, b);
        self.persistence.append_wal(&WalEntry::Disconnect { a, b })?;
        self.maybe_rotate()
    }

    // --- Prime tree operations ---

    pub fn insert_term(&mut self, term: &str, memory_id: u32) -> Result<u64> {
        let node_id = self.prime_tree.insert(term, memory_id);
        self.persistence.append_wal(&WalEntry::InsertTerm {
            term: term.to_string(),
            memory_id,
        })?;
        self.maybe_rotate()?;
        Ok(node_id)
    }

    // --- Dream cycle ---

    pub fn dream(&mut self, chonk_url: Option<&str>) -> crate::dream::DreamReport {
        crate::dream::dream_cycle(
            &mut self.memory_store,
            &self.engine,
            &mut self.graph,
            &mut self.skg,
            &self.prime_tree,
            chonk_url,
        )
    }

    pub fn dream_with_intensity(
        &mut self,
        intensity: f32,
        entropy_seed: &[u8],
        chonk_url: Option<&str>,
    ) -> crate::dream::DreamReport {
        crate::dream::dream_cycle_with_intensity(
            &mut self.memory_store,
            &self.engine,
            &mut self.graph,
            &mut self.skg,
            &self.prime_tree,
            intensity,
            entropy_seed,
            chonk_url,
        )
    }

    // --- WAL rotation ---

    /// Auto-checkpoint if WAL exceeds size threshold.
    fn maybe_rotate(&mut self) -> Result<()> {
        if self.persistence.wal_bytes >= WAL_ROTATE_BYTES {
            eprintln!(
                "[wal] auto-checkpoint at {} bytes",
                self.persistence.wal_bytes
            );
            self.checkpoint()?;
        }
        Ok(())
    }

    // --- Checkpoint & accessors ---

    pub fn checkpoint(&mut self) -> Result<()> {
        self.persistence.checkpoint(
            &self.engine,
            &self.memory_store,
            &self.graph,
            &self.prime_tree,
            &self.skg,
        )
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    pub fn memory_store(&self) -> &MemoryStore {
        &self.memory_store
    }

    pub fn memory_store_mut(&mut self) -> &mut MemoryStore {
        &mut self.memory_store
    }

    pub fn graph(&self) -> &MemoryGraph {
        &self.graph
    }

    pub fn graph_mut(&mut self) -> &mut MemoryGraph {
        &mut self.graph
    }

    pub fn prime_tree(&self) -> &PrimeTree {
        &self.prime_tree
    }

    pub fn prime_tree_mut(&mut self) -> &mut PrimeTree {
        &mut self.prime_tree
    }

    pub fn skg(&self) -> &SkgState {
        &self.skg
    }

    pub fn skg_mut(&mut self) -> &mut SkgState {
        &mut self.skg
    }
}

/// Max WAL size before auto-checkpoint (64 MB).
const WAL_ROTATE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
struct Persistence {
    #[allow(dead_code)]
    base_dir: PathBuf,
    snapshot_v1_path: PathBuf,
    snapshot_v2_path: PathBuf,
    snapshot_v3_path: PathBuf,
    snapshot_v4_path: PathBuf,
    snapshot_tmp_path: PathBuf,
    wal_path: PathBuf,
    wal_bytes: u64,
}

impl Persistence {
    fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        fs::create_dir_all(&base_dir)?;
        let wal_path = base_dir.join(WAL_FILE);
        let wal_bytes = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            snapshot_v1_path: base_dir.join(SNAPSHOT_V1_FILE),
            snapshot_v2_path: base_dir.join(SNAPSHOT_V2_FILE),
            snapshot_v3_path: base_dir.join(SNAPSHOT_V3_FILE),
            snapshot_v4_path: base_dir.join(SNAPSHOT_V4_FILE),
            snapshot_tmp_path: base_dir.join(SNAPSHOT_TMP_FILE),
            wal_path,
            wal_bytes,
            base_dir,
        })
    }

    fn load(&self) -> Result<(Engine, MemoryStore, MemoryGraph, PrimeTree, SkgState)> {
        let mut engine = Engine::new();
        let mut memory_store = MemoryStore::new();
        let mut graph = MemoryGraph::new();
        let mut prime_tree = PrimeTree::new();
        let mut skg = SkgState::new();

        // Try V4 snapshot first (edges with EdgeKind)
        if self.snapshot_v4_path.exists() {
            let bytes = fs::read(&self.snapshot_v4_path)?;
            if !bytes.is_empty() {
                let snap: SnapshotV4 = postcard::from_bytes(&bytes)?;
                for row in snap.rows {
                    engine.upsert(row)?;
                }
                memory_store.load_records(snap.records);
                graph.load_edges(snap.edges);
                prime_tree.load_snapshot(snap.tree);
                skg = SkgState::load_snapshot(snap.skg);
            }
        } else if self.snapshot_v3_path.exists() {
            // V3: legacy edges (no EdgeKind) — migrate to Semantic
            let bytes = fs::read(&self.snapshot_v3_path)?;
            if !bytes.is_empty() {
                let snap: SnapshotV3 = postcard::from_bytes(&bytes)?;
                for row in snap.rows {
                    engine.upsert(row)?;
                }
                memory_store.load_records(snap.records);
                graph.load_edges(snap.edges.into_iter().map(LegacyEdge::into_edge).collect());
                prime_tree.load_snapshot(snap.tree);
                skg = SkgState::load_snapshot(snap.skg);
            }
        } else if self.snapshot_v2_path.exists() {
            // V2: legacy edges, no SKG — migrate to Semantic
            let bytes = fs::read(&self.snapshot_v2_path)?;
            if !bytes.is_empty() {
                let snap: SnapshotV2 = postcard::from_bytes(&bytes)?;
                for row in snap.rows {
                    engine.upsert(row)?;
                }
                memory_store.load_records(snap.records);
                graph.load_edges(snap.edges.into_iter().map(LegacyEdge::into_edge).collect());
                prime_tree.load_snapshot(snap.tree);
            }
        } else if self.snapshot_v1_path.exists() {
            // Fall back to V1 (rows only)
            let bytes = fs::read(&self.snapshot_v1_path)?;
            if !bytes.is_empty() {
                let snap: SnapshotV1 = postcard::from_bytes(&bytes)?;
                for row in snap.rows {
                    engine.upsert(row)?;
                }
            }
        }

        // Replay WAL on top
        if self.wal_path.exists() {
            for entry in self.read_wal()? {
                Self::apply_wal_entry(
                    &mut engine,
                    &mut memory_store,
                    &mut graph,
                    &mut prime_tree,
                    entry,
                )?;
            }
        }

        Ok((engine, memory_store, graph, prime_tree, skg))
    }

    fn apply_wal_entry(
        engine: &mut Engine,
        memory_store: &mut MemoryStore,
        graph: &mut MemoryGraph,
        prime_tree: &mut PrimeTree,
        entry: WalEntry,
    ) -> Result<()> {
        match entry {
            WalEntry::Upsert(row) => {
                engine.upsert(row)?;
            }
            WalEntry::Delete { id } => {
                engine.delete(id);
            }
            WalEntry::Remember { row, record } => {
                engine.upsert(row)?;
                memory_store.insert(record);
            }
            WalEntry::UpdateRecord(record) => {
                memory_store.insert(record);
            }
            WalEntry::RemoveMemory { id } => {
                engine.delete(id);
                memory_store.remove(id);
                graph.remove_node(id);
                prime_tree.remove_member(id);
            }
            WalEntry::Connect {
                a,
                b,
                label,
                weight,
            } => {
                graph.connect(a, b, label, weight, EdgeKind::Semantic);
            }
            WalEntry::Disconnect { a, b } => {
                graph.disconnect(a, b);
            }
            WalEntry::ConnectCausal {
                from,
                to,
                label,
                weight,
            } => {
                graph.connect(from, to, label, weight, EdgeKind::Causal);
            }
            WalEntry::InsertTerm { term, memory_id } => {
                prime_tree.insert(&term, memory_id);
            }
        }
        Ok(())
    }

    /// Append entry to WAL, tracking cumulative size.
    /// Returns true if WAL has exceeded rotation threshold.
    fn append_wal(&mut self, entry: &WalEntry) -> Result<bool> {
        let mut writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.wal_path)
            .with_context(|| format!("open WAL at {}", self.wal_path.display()))?;
        let payload = postcard::to_stdvec(entry)?;
        let len = payload.len() as u32;
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(&payload)?;
        writer.flush()?;
        self.wal_bytes += 4 + payload.len() as u64;
        Ok(self.wal_bytes >= WAL_ROTATE_BYTES)
    }

    fn checkpoint(
        &mut self,
        engine: &Engine,
        memory_store: &MemoryStore,
        graph: &MemoryGraph,
        prime_tree: &PrimeTree,
        skg: &SkgState,
    ) -> Result<()> {
        let snapshot = SnapshotV4 {
            rows: engine.rows_iter().cloned().collect(),
            records: memory_store.all_records(),
            edges: graph.all_edges(),
            tree: prime_tree.snapshot(),
            skg: skg.snapshot(),
        };
        let payload = postcard::to_stdvec(&snapshot)?;

        {
            let tmp_file = File::create(&self.snapshot_tmp_path)?;
            let mut writer = BufWriter::new(tmp_file);
            writer.write_all(&payload)?;
            writer.flush()?;
        }

        fs::rename(&self.snapshot_tmp_path, &self.snapshot_v4_path)?;
        // Clean up old snapshots if they exist
        if self.snapshot_v3_path.exists() {
            let _ = fs::remove_file(&self.snapshot_v3_path);
        }
        if self.snapshot_v2_path.exists() {
            let _ = fs::remove_file(&self.snapshot_v2_path);
        }
        if self.snapshot_v1_path.exists() {
            let _ = fs::remove_file(&self.snapshot_v1_path);
        }
        // Truncate WAL
        File::create(&self.wal_path)?;
        self.wal_bytes = 0;
        Ok(())
    }

    fn read_wal(&self) -> Result<Vec<WalEntry>> {
        let file = File::open(&self.wal_path)?;
        let mut reader = BufReader::new(file);
        let mut out = Vec::new();
        loop {
            let mut len_bytes = [0_u8; 4];
            match reader.read_exact(&mut len_bytes) {
                Ok(()) => {
                    let len = u32::from_le_bytes(len_bytes) as usize;
                    let mut payload = vec![0_u8; len];
                    reader.read_exact(&mut payload)?;
                    let entry: WalEntry = postcard::from_bytes(&payload)?;
                    out.push(entry);
                }
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    break;
                }
                Err(err) => return Err(err.into()),
            }
        }
        Ok(out)
    }
}

/// V1 snapshot: rows only (backward compat).
#[derive(Debug, Serialize, Deserialize)]
struct SnapshotV1 {
    rows: Vec<Row>,
}

/// Edge format from V2/V3 snapshots (no EdgeKind field).
#[derive(Debug, Serialize, Deserialize)]
struct LegacyEdge {
    from: u32,
    to: u32,
    label: String,
    weight: f32,
}

impl LegacyEdge {
    fn into_edge(self) -> Edge {
        Edge {
            from: self.from,
            to: self.to,
            label: self.label,
            weight: self.weight,
            kind: EdgeKind::Semantic,
        }
    }
}

/// V2 snapshot: full system state (no SKG, legacy edges).
#[derive(Debug, Serialize, Deserialize)]
struct SnapshotV2 {
    rows: Vec<Row>,
    records: Vec<MemoryRecord>,
    edges: Vec<LegacyEdge>,
    tree: PrimeTreeSnapshot,
}

/// V3 snapshot: full system state + SKG (legacy edges).
#[derive(Debug, Serialize, Deserialize)]
struct SnapshotV3 {
    rows: Vec<Row>,
    records: Vec<MemoryRecord>,
    edges: Vec<LegacyEdge>,
    tree: PrimeTreeSnapshot,
    skg: SkgSnapshot,
}

/// V4 snapshot: full system state + SKG + directed edges.
#[derive(Debug, Serialize, Deserialize)]
struct SnapshotV4 {
    rows: Vec<Row>,
    records: Vec<MemoryRecord>,
    edges: Vec<Edge>,
    tree: PrimeTreeSnapshot,
    skg: SkgSnapshot,
}

/// WAL entry — new variants appended at end for backward compat.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum WalEntry {
    // V1 entries (keep these first for backward compat)
    Upsert(Row),
    Delete {
        id: u32,
    },
    // V2 entries
    Remember {
        row: Row,
        record: MemoryRecord,
    },
    UpdateRecord(MemoryRecord),
    RemoveMemory {
        id: u32,
    },
    Connect {
        a: u32,
        b: u32,
        label: String,
        weight: f32,
    },
    Disconnect {
        a: u32,
        b: u32,
    },
    InsertTerm {
        term: String,
        memory_id: u32,
    },
    // V4 entry — causal (directed) edge
    ConnectCausal {
        from: u32,
        to: u32,
        label: String,
        weight: f32,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use super::*;
    use crate::memory::MemoryRecord;

    fn sample_row(id: u32, region: &str) -> Row {
        let mut tags = BTreeMap::new();
        tags.insert("region".to_string(), region.to_string());
        Row {
            id,
            tags,
            vector: vec![id as f32, 0.0, 1.0],
        }
    }

    #[test]
    fn wal_replay_and_checkpoint_restore() {
        let dir = tempdir().unwrap();
        {
            let mut db = DurableEngine::open(dir.path()).unwrap();
            db.upsert(sample_row(1, "us")).unwrap();
            db.upsert(sample_row(2, "eu")).unwrap();
            db.delete(2).unwrap();
            db.checkpoint().unwrap();
        }

        let db = DurableEngine::open(dir.path()).unwrap();
        assert_eq!(db.engine().row_count(), 1);
        assert!(db.engine().get(1).is_some());
    }

    #[test]
    fn wal_replay_without_checkpoint() {
        let dir = tempdir().unwrap();
        {
            let mut db = DurableEngine::open(dir.path()).unwrap();
            db.upsert(sample_row(1, "us")).unwrap();
            db.upsert(sample_row(2, "eu")).unwrap();
        }
        let db = DurableEngine::open(dir.path()).unwrap();
        assert_eq!(db.engine().row_count(), 2);
    }

    #[test]
    fn remember_persists_memory_record() {
        let dir = tempdir().unwrap();
        {
            let mut db = DurableEngine::open(dir.path()).unwrap();
            let row = sample_row(1, "us");
            let record = MemoryRecord::new(1);
            db.remember(row, record).unwrap();
            db.checkpoint().unwrap();
        }

        let db = DurableEngine::open(dir.path()).unwrap();
        assert_eq!(db.engine().row_count(), 1);
        assert!(db.memory_store().get(1).is_some());
        assert_eq!(db.memory_store().get(1).unwrap().fidelity, 1.0);
    }

    #[test]
    fn graph_edges_persist() {
        let dir = tempdir().unwrap();
        {
            let mut db = DurableEngine::open(dir.path()).unwrap();
            db.remember(sample_row(1, "us"), MemoryRecord::new(1))
                .unwrap();
            db.remember(sample_row(2, "eu"), MemoryRecord::new(2))
                .unwrap();
            db.connect(1, 2, "related".into(), 0.9, EdgeKind::Semantic).unwrap();
            db.checkpoint().unwrap();
        }

        let db = DurableEngine::open(dir.path()).unwrap();
        assert!(db.graph().neighbors(1).contains(2));
        assert_eq!(db.graph().edge_count(), 1);
    }

    #[test]
    fn prime_tree_persists() {
        let dir = tempdir().unwrap();
        {
            let mut db = DurableEngine::open(dir.path()).unwrap();
            db.remember(sample_row(1, "us"), MemoryRecord::new(1))
                .unwrap();
            db.insert_term("weather", 1).unwrap();
            db.checkpoint().unwrap();
        }

        let db = DurableEngine::open(dir.path()).unwrap();
        let result = db.prime_tree().search_exact("weather");
        assert!(result.contains(1));
    }

    #[test]
    fn wal_replay_all_entry_types() {
        let dir = tempdir().unwrap();
        {
            let mut db = DurableEngine::open(dir.path()).unwrap();
            db.remember(sample_row(1, "us"), MemoryRecord::new(1))
                .unwrap();
            db.remember(sample_row(2, "eu"), MemoryRecord::new(2))
                .unwrap();
            db.connect(1, 2, "link".into(), 1.0, EdgeKind::Semantic).unwrap();
            db.insert_term("test", 1).unwrap();
            db.disconnect(1, 2).unwrap();
            db.remove_memory(2).unwrap();
            // No checkpoint — everything should replay from WAL
        }

        let db = DurableEngine::open(dir.path()).unwrap();
        assert_eq!(db.engine().row_count(), 1);
        assert_eq!(db.memory_store().len(), 1);
        assert_eq!(db.graph().edge_count(), 0);
        assert!(db.prime_tree().search_exact("test").contains(1));
    }
}
