//! Section suppression ledger — `upekkhā`/`nirodha` forgetting and sealing.
//!
//! Implements the Lume-side of the plan §8.3 lifecycle table
//! (`research/lume-ollaya-abhidhamma-plan.md`):
//!
//! | Memory operation | Lume effect                        |
//! |------------------|------------------------------------|
//! | Upekkhā (forgive)| delete section from index          |
//! | Nirodha (archive)| delete section from index          |
//! | Seal             | separate sealed index, not searched by default |
//! | Revive           | re-index from seed                 |
//!
//! Sections are keyed by their content hash (`hybrid::section_hash`, the
//! ExactID), so suppression survives re-indexes and incremental semantic
//! ingests. Forgetting is a retrieval-level text blackout: the section can
//! no longer surface from BM25, the semantic blend, the SKG expansion or
//! eval — the plan's §10.3 invariant ("forgive a memory, then assert that
//! no Lume hit ... contains its text"). Sealed sections are excluded from
//! default search but can be opted back in explicitly
//! (`--include-sealed`), mirroring "sealed index (not searchable by
//! default)".
//!
//! Persisted as `suppression.json` next to the index db (via the
//! `hybrid::set_cache_dir` anchor, so it follows the index like the session
//! caches do).

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SUPPRESSION_FILE: &str = "suppression.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SuppressionLedger {
    /// `upekkhā`/`nirodha`: section text must never surface again.
    #[serde(default)]
    forgotten: Vec<String>,
    /// Sealed: excluded from default search; retrievable only when the
    /// caller explicitly opts in.
    #[serde(default)]
    sealed: Vec<String>,
}

impl SuppressionLedger {
    /// Load the ledger from `<db dir>/suppression.json`. A missing file is
    /// an empty ledger, not an error.
    pub fn load(db_dir: &Path) -> Self {
        Self::load_at(&db_dir.join(SUPPRESSION_FILE))
    }

    /// Load the ledger from an explicit file path (the hybrid search path
    /// resolves it via `hybrid::suppression_ledger_path`).
    pub fn load_at(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist the ledger next to the index db. `Vec`s are sorted/deduped so
    /// repeated CLI calls converge on a stable file.
    pub fn save(&self, db_dir: &Path) -> io::Result<()> {
        self.save_at(&db_dir.join(SUPPRESSION_FILE))
    }

    /// Persist the ledger to an explicit file path.
    pub fn save_at(&self, path: &Path) -> io::Result<()> {
        let mut this = self.clone();
        this.forgotten.sort();
        this.forgotten.dedup();
        this.sealed.sort();
        this.sealed.dedup();
        let content = serde_json::to_string_pretty(&this)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, content)
    }

    /// Upekkhā/nirodha: black the section out of retrieval. Returns true
    /// when the ledger changed.
    pub fn forget(&mut self, section_hash: &str) -> bool {
        let changed = !self.forgotten.contains(&section_hash.to_string());
        if changed {
            self.forgotten.push(section_hash.to_string());
            // A forgotten hash is also removed from the sealed set: both
            // kinds exclude from default search, and forgotten is stronger.
            self.sealed.retain(|h| h != section_hash);
        }
        changed
    }

    /// Seal: exclude from default search but keep explicitly opt-in-able.
    /// Returns true when the ledger changed.
    pub fn seal(&mut self, section_hash: &str) -> bool {
        if self.forgotten.contains(&section_hash.to_string()) {
            return false;
        }
        let changed = !self.sealed.contains(&section_hash.to_string());
        if changed {
            self.sealed.push(section_hash.to_string());
        }
        changed
    }

    /// Revive: clear either kind of suppression.
    pub fn restore(&mut self, section_hash: &str) -> bool {
        let before = self.forgotten.len() + self.sealed.len();
        self.forgotten.retain(|h| h != section_hash);
        self.sealed.retain(|h| h != section_hash);
        before != self.forgotten.len() + self.sealed.len()
    }

    pub fn is_forgotten(&self, section_hash: &str) -> bool {
        self.forgotten.iter().any(|h| h == section_hash)
    }

    pub fn is_sealed(&self, section_hash: &str) -> bool {
        self.sealed.iter().any(|h| h == section_hash)
    }

    /// Whether a section may be returned by default search: not forgotten,
    /// not sealed. Sealed sections pass only when the caller opted in.
    pub fn is_searchable(&self, section_hash: &str, include_sealed: bool) -> bool {
        if self.is_forgotten(section_hash) {
            return false;
        }
        if self.is_sealed(section_hash) {
            return include_sealed;
        }
        true
    }

    pub fn forgotten_hashes(&self) -> &[String] {
        &self.forgotten
    }

    pub fn sealed_hashes(&self) -> &[String] {
        &self.sealed
    }

    pub fn is_empty(&self) -> bool {
        self.forgotten.is_empty() && self.sealed.is_empty()
    }

    /// Number of suppressed sections (either kind).
    pub fn len(&self) -> usize {
        self.forgotten.len() + self.sealed.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lume-suppress-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn forget_blacklists_and_survives_reload() {
        let dir = tmp_dir("forget");
        let mut ledger = SuppressionLedger::default();
        assert!(ledger.forget("aabbccdd00112233"));
        assert!(!ledger.forget("aabbccdd00112233"), "second forget is a no-op");
        assert!(!ledger.is_searchable("aabbccdd00112233", false));
        assert!(!ledger.is_searchable("aabbccdd00112233", true), "forgotten never surfaces");
        ledger.save(&dir).unwrap();
        let reloaded = SuppressionLedger::load(&dir);
        assert!(reloaded.is_forgotten("aabbccdd00112233"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn seal_is_optin_retrievable() {
        let mut ledger = SuppressionLedger::default();
        assert!(ledger.seal("deadbeefdeadbeef"));
        assert!(!ledger.is_searchable("deadbeefdeadbeef", false));
        assert!(ledger.is_searchable("deadbeefdeadbeef", true), "explicit opt-in re-enables");
        // Sealing a forgotten section must not weaken the blackout.
        assert!(ledger.forget("deadbeefdeadbeef"));
        assert!(!ledger.seal("deadbeefdeadbeef"));
        assert!(!ledger.is_searchable("deadbeefdeadbeef", true));
    }

    #[test]
    fn restore_clears_both_kinds() {
        let mut ledger = SuppressionLedger::default();
        ledger.seal("1111");
        ledger.forget("2222");
        assert!(ledger.restore("1111"));
        assert!(ledger.restore("2222"));
        assert!(!ledger.restore("1111"), "nothing left to restore");
        assert!(ledger.is_searchable("1111", false));
        assert!(ledger.is_searchable("2222", false));
    }

    #[test]
    fn save_normalizes_duplicates() {
        let dir = tmp_dir("dedupe");
        let mut ledger = SuppressionLedger::default();
        ledger.forget("b");
        ledger.forget("a");
        ledger.forget("b");
        ledger.save(&dir).unwrap();
        let reloaded = SuppressionLedger::load(&dir);
        assert_eq!(reloaded.forgotten_hashes(), &["a", "b"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_empty_ledger() {
        let dir = tmp_dir("missing");
        assert!(SuppressionLedger::load(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_file_is_empty_ledger() {
        let dir = tmp_dir("corrupt");
        std::fs::write(dir.join(SUPPRESSION_FILE), "{not json").unwrap();
        assert!(SuppressionLedger::load(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}