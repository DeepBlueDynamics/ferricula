//! Sparse inverted index stubs for term-based scoring (BM25/BB25).
//! This is a minimal placeholder so agents can wire in a real implementation later.

use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct Posting {
    pub doc_id: u32,
    pub term_freq: u32,
}

#[derive(Debug, Default, Clone)]
pub struct DocStats {
    pub length: u32,
}

/// Simple inverted index map: term -> postings.
#[derive(Debug, Default, Clone)]
pub struct InvertedIndex {
    pub postings: HashMap<String, Vec<Posting>>,
    pub doc_stats: HashMap<u32, DocStats>,
    pub total_len: u64,
}

impl InvertedIndex {
    pub fn add_doc(&mut self, doc_id: u32, terms: impl IntoIterator<Item = String>) {
        let mut tf: HashMap<String, u32> = HashMap::new();
        for term in terms {
            *tf.entry(term).or_default() += 1;
        }
        let len: u32 = tf.values().copied().sum();
        for (term, freq) in tf {
            self.postings.entry(term).or_default().push(Posting {
                doc_id,
                term_freq: freq,
            });
        }
        self.doc_stats
            .insert(doc_id, DocStats { length: len as u32 });
        self.total_len += len as u64;
    }

    pub fn df(&self, term: &str) -> u32 {
        self.postings.get(term).map(|p| p.len() as u32).unwrap_or(0)
    }

    pub fn avg_dl(&self) -> f64 {
        if self.doc_stats.is_empty() {
            return 0.0;
        }
        self.total_len as f64 / self.doc_stats.len() as f64
    }
}
