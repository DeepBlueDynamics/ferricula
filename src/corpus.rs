//! Corpus-level statistics for BM25 search, maintained incrementally.
//! Wires bb25 scoring through prime_tree posting lists.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bb25::{self, Bb25State, CorpusStats};
use crate::prime_tree::PrimeTree;
use crate::tokenizer;

/// Full search engine state wrapping corpus stats + calibration.
#[derive(Debug, Clone)]
pub struct SearchEngine {
    pub corpus: CorpusStats,
    pub state: Bb25State,
}

impl SearchEngine {
    pub fn new() -> Self {
        Self {
            corpus: CorpusStats::new(),
            state: Bb25State::default(),
        }
    }

    /// Index a document's text into corpus stats.
    /// Applies Pali expansion so both vocabularies are indexed.
    pub fn add_document(&mut self, id: u32, text: &str) {
        let expanded = crate::pali::expand(text);
        let terms = tokenizer::extract_terms(&expanded);
        self.corpus.add_document(id, &terms);
    }

    /// Remove a document from corpus stats.
    pub fn remove_document(&mut self, id: u32) {
        self.corpus.remove_document(id);
    }

    /// Recalibrate alpha/beta from current corpus.
    pub fn recalibrate(&mut self) {
        let (alpha, beta) = bb25::auto_estimate(&self.corpus);
        self.state.config.alpha = alpha;
        self.state.config.beta = beta;
        self.state.alpha_avg = alpha;
        self.state.beta_avg = beta;
    }

    /// BM25 search using prime_tree as the inverted index.
    /// Applies Pali expansion to queries so searching "anicca" finds "decay".
    pub fn bm25_search(
        &self,
        query: &str,
        k: usize,
        prime_tree: &PrimeTree,
    ) -> Vec<SearchHit> {
        let expanded = crate::pali::expand(query);
        let query_terms = tokenizer::extract_terms(&expanded);
        if query_terms.is_empty() {
            return Vec::new();
        }

        let avgdl = self.corpus.avgdl();
        let n_docs = self.corpus.n_docs as f64;

        // Accumulate BM25 scores per document
        let mut doc_scores: HashMap<u32, f64> = HashMap::new();
        let mut doc_max_tf: HashMap<u32, f64> = HashMap::new();

        for term in &query_terms {
            // Get posting list from prime_tree
            let posting = prime_tree.search_exact(term);
            let df = self.corpus.df.get(term).copied().unwrap_or(0) as f64;

            for doc_id in posting.iter() {
                if let Some(doc) = self.corpus.doc_stats.get(&doc_id) {
                    let tf = doc.term_freqs.get(term).copied().unwrap_or(0) as f64;
                    if tf > 0.0 {
                        let score = bb25::bm25_term_score(
                            tf,
                            df,
                            n_docs,
                            doc.doc_len as f64,
                            avgdl,
                            &self.state.config,
                        );
                        *doc_scores.entry(doc_id).or_insert(0.0) += score;
                        let max_tf = doc_max_tf.entry(doc_id).or_insert(0.0);
                        if tf > *max_tf {
                            *max_tf = tf;
                        }
                    }
                }
            }
        }

        // Convert scores to calibrated probabilities
        let mut hits: Vec<SearchHit> = doc_scores
            .into_iter()
            .map(|(id, bm25_score)| {
                let tf = doc_max_tf.get(&id).copied().unwrap_or(1.0);
                let doc_len_ratio = self
                    .corpus
                    .doc_stats
                    .get(&id)
                    .map(|d| d.doc_len as f64 / avgdl)
                    .unwrap_or(1.0);
                let probability =
                    bb25::score_to_probability(bm25_score, tf, doc_len_ratio, &self.state);
                SearchHit {
                    id,
                    bm25_score,
                    probability,
                }
            })
            .collect();

        hits.sort_by(|a, b| b.probability.partial_cmp(&a.probability).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(k);
        hits
    }

    /// Hybrid search: combine vector cosine scores with BM25.
    /// `vector_hits` should be (id, cosine_similarity) pairs.
    pub fn hybrid_search(
        &self,
        query: &str,
        k: usize,
        weight: f64,
        prime_tree: &PrimeTree,
        vector_hits: &[(u32, f32)],
    ) -> Vec<SearchHit> {
        // BM25 results
        let bm25_hits = self.bm25_search(query, k * 2, prime_tree);
        let bm25_map: HashMap<u32, f64> = bm25_hits.iter().map(|h| (h.id, h.probability)).collect();

        // Vector results as probabilities (cosine in [-1,1] → scale to [0,1])
        let vec_map: HashMap<u32, f64> = vector_hits
            .iter()
            .map(|&(id, cos)| (id, ((cos as f64) + 1.0) / 2.0))
            .collect();

        // Union of candidates
        let mut all_ids: Vec<u32> = bm25_map.keys().chain(vec_map.keys()).copied().collect();
        all_ids.sort_unstable();
        all_ids.dedup();

        let mut hits: Vec<SearchHit> = all_ids
            .into_iter()
            .map(|id| {
                let p_bm25 = bm25_map.get(&id).copied().unwrap_or(0.1);
                let p_vec = vec_map.get(&id).copied().unwrap_or(0.1);
                let fused = bb25::log_odds_fusion(p_vec, p_bm25, weight);
                SearchHit {
                    id,
                    bm25_score: bm25_map
                        .get(&id)
                        .copied()
                        .unwrap_or(0.0),
                    probability: fused,
                }
            })
            .collect();

        hits.sort_by(|a, b| b.probability.partial_cmp(&a.probability).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(k);
        hits
    }

    /// Serialize for persistence.
    pub fn snapshot(&self) -> SearchEngineSnapshot {
        let doc_entries: Vec<(u32, u32, Vec<(String, u32)>)> = self
            .corpus
            .doc_stats
            .iter()
            .map(|(&id, ds)| {
                let tfs: Vec<(String, u32)> = ds.term_freqs.iter().map(|(k, &v)| (k.clone(), v)).collect();
                (id, ds.doc_len, tfs)
            })
            .collect();
        let df_entries: Vec<(String, u32)> = self.corpus.df.iter().map(|(k, &v)| (k.clone(), v)).collect();

        SearchEngineSnapshot {
            n_docs: self.corpus.n_docs,
            total_doc_len: self.corpus.total_doc_len,
            doc_entries,
            df_entries,
            alpha: self.state.config.alpha,
            beta: self.state.config.beta,
            n_updates: self.state.n_updates,
        }
    }

    /// Load from persistence snapshot.
    pub fn load_snapshot(snap: SearchEngineSnapshot) -> Self {
        let mut doc_stats = HashMap::new();
        for (id, doc_len, tfs) in snap.doc_entries {
            let term_freqs: HashMap<String, u32> = tfs.into_iter().collect();
            doc_stats.insert(
                id,
                crate::bb25::DocStats { doc_len, term_freqs },
            );
        }
        let df: HashMap<String, u32> = snap.df_entries.into_iter().collect();

        let corpus = CorpusStats {
            n_docs: snap.n_docs,
            total_doc_len: snap.total_doc_len,
            doc_stats,
            df,
        };

        let mut state = Bb25State::default();
        state.config.alpha = snap.alpha;
        state.config.beta = snap.beta;
        state.alpha_avg = snap.alpha;
        state.beta_avg = snap.beta;
        state.n_updates = snap.n_updates;

        Self { corpus, state }
    }
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: u32,
    pub bm25_score: f64,
    pub probability: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchEngineSnapshot {
    pub n_docs: u32,
    pub total_doc_len: u64,
    pub doc_entries: Vec<(u32, u32, Vec<(String, u32)>)>, // (id, doc_len, [(term, freq)])
    pub df_entries: Vec<(String, u32)>,
    pub alpha: f64,
    pub beta: f64,
    pub n_updates: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tree_and_engine() -> (PrimeTree, SearchEngine) {
        let mut tree = PrimeTree::new();
        let mut engine = SearchEngine::new();

        let docs = vec![
            (1, "the weber bracket departs from newton"),
            (2, "weber electrodynamics and force law"),
            (3, "newton gravity apple falling tree"),
            (4, "quantum mechanics wave function"),
            (5, "weber force between charged particles"),
        ];

        for (id, text) in &docs {
            engine.add_document(*id, text);
            let terms = tokenizer::extract_terms(text);
            for term in &terms {
                tree.insert(term, *id);
            }
        }

        (tree, engine)
    }

    #[test]
    fn bm25_search_returns_ranked() {
        let (tree, engine) = make_tree_and_engine();
        let hits = engine.bm25_search("weber force", 5, &tree);
        assert!(!hits.is_empty());
        // Top hit should be a doc containing both "weber" and "force"
        assert!(hits[0].id == 2 || hits[0].id == 5);
        // Probabilities should be in (0, 1)
        for h in &hits {
            assert!(h.probability > 0.0 && h.probability < 1.0);
        }
    }

    #[test]
    fn bm25_search_respects_k() {
        let (tree, engine) = make_tree_and_engine();
        let hits = engine.bm25_search("weber", 2, &tree);
        assert!(hits.len() <= 2);
    }

    #[test]
    fn remove_document_updates_search() {
        let (mut tree, mut engine) = make_tree_and_engine();

        // Search should find doc 1
        let hits = engine.bm25_search("weber bracket", 5, &tree);
        let ids: Vec<u32> = hits.iter().map(|h| h.id).collect();
        assert!(ids.contains(&1));

        // Remove doc 1
        engine.remove_document(1);
        tree.remove_member(1);

        let hits = engine.bm25_search("weber bracket", 5, &tree);
        let ids: Vec<u32> = hits.iter().map(|h| h.id).collect();
        assert!(!ids.contains(&1));
    }

    #[test]
    fn snapshot_round_trip() {
        let (_, engine) = make_tree_and_engine();
        let snap = engine.snapshot();
        let restored = SearchEngine::load_snapshot(snap);
        assert_eq!(restored.corpus.n_docs, engine.corpus.n_docs);
        assert_eq!(restored.corpus.df.len(), engine.corpus.df.len());
    }
}
