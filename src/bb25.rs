//! Bayesian BM25 (BB25): calibrated probability from BM25 relevance scores.
//! Full port of bayesian-bm25's probability.py — deterministic transform of
//! BM25 into posterior probabilities via sigmoid calibration + Bayesian priors.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Config + State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Bb25Config {
    pub k1: f64,    // BM25 saturation (default 1.2)
    pub b: f64,     // length normalization (default 0.75)
    pub alpha: f64, // sigmoid steepness (default 1.0)
    pub beta: f64,  // sigmoid midpoint (default 0.0)
    pub base_rate: Option<f64>,
}

impl Default for Bb25Config {
    fn default() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            alpha: 1.0,
            beta: 0.0,
            base_rate: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Bb25State {
    pub config: Bb25Config,
    pub n_updates: u64,
    pub grad_alpha_ema: f64,
    pub grad_beta_ema: f64,
    pub alpha_avg: f64, // Polyak average
    pub beta_avg: f64,
}

impl Default for Bb25State {
    fn default() -> Self {
        let cfg = Bb25Config::default();
        let alpha_avg = cfg.alpha;
        let beta_avg = cfg.beta;
        Self {
            config: cfg,
            n_updates: 0,
            grad_alpha_ema: 0.0,
            grad_beta_ema: 0.0,
            alpha_avg,
            beta_avg,
        }
    }
}

/// Per-document statistics for BM25 computation.
#[derive(Debug, Clone)]
pub struct DocStats {
    pub doc_len: u32,
    pub term_freqs: HashMap<String, u32>,
}

/// Corpus-level statistics maintained incrementally.
#[derive(Debug, Clone)]
pub struct CorpusStats {
    pub n_docs: u32,
    pub total_doc_len: u64,
    pub doc_stats: HashMap<u32, DocStats>,
    pub df: HashMap<String, u32>, // term → document frequency
}

impl CorpusStats {
    pub fn new() -> Self {
        Self {
            n_docs: 0,
            total_doc_len: 0,
            doc_stats: HashMap::new(),
            df: HashMap::new(),
        }
    }

    /// Add a document's terms to the corpus.
    pub fn add_document(&mut self, id: u32, terms: &[String]) {
        // If doc already exists, remove old stats first
        if self.doc_stats.contains_key(&id) {
            self.remove_document(id);
        }

        let mut term_freqs: HashMap<String, u32> = HashMap::new();
        for term in terms {
            *term_freqs.entry(term.clone()).or_insert(0) += 1;
        }

        // Update document frequency (each unique term in doc contributes 1)
        for term in term_freqs.keys() {
            *self.df.entry(term.clone()).or_insert(0) += 1;
        }

        let doc_len = terms.len() as u32;
        self.total_doc_len += doc_len as u64;
        self.n_docs += 1;
        self.doc_stats.insert(id, DocStats { doc_len, term_freqs });
    }

    /// Remove a document from the corpus.
    pub fn remove_document(&mut self, id: u32) {
        if let Some(stats) = self.doc_stats.remove(&id) {
            self.total_doc_len = self.total_doc_len.saturating_sub(stats.doc_len as u64);
            self.n_docs = self.n_docs.saturating_sub(1);
            for (term, _) in &stats.term_freqs {
                if let Some(count) = self.df.get_mut(term) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        self.df.remove(term);
                    }
                }
            }
        }
    }

    pub fn avgdl(&self) -> f64 {
        if self.n_docs == 0 {
            return 1.0;
        }
        self.total_doc_len as f64 / self.n_docs as f64
    }

    pub fn idf(&self, term: &str) -> f64 {
        let df = self.df.get(term).copied().unwrap_or(0) as f64;
        let n = self.n_docs as f64;
        ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
    }
}

// ---------------------------------------------------------------------------
// Core math
// ---------------------------------------------------------------------------

/// Numerically stable sigmoid.
pub fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        let z = (-x).exp();
        1.0 / (1.0 + z)
    } else {
        let z = x.exp();
        z / (1.0 + z)
    }
}

/// Logit (inverse sigmoid) with clamping.
pub fn logit(p: f64) -> f64 {
    let p = clamp_prob(p);
    (p / (1.0 - p)).ln()
}

/// Clamp probability to avoid log(0).
pub fn clamp_prob(p: f64) -> f64 {
    p.clamp(1e-10, 1.0 - 1e-10)
}

// ---------------------------------------------------------------------------
// BM25 core (Eq. 1-3)
// ---------------------------------------------------------------------------

/// Classic BM25 score for a single term in a single document.
pub fn bm25_term_score(
    tf: f64,
    df: f64,
    n_docs: f64,
    doc_len: f64,
    avgdl: f64,
    cfg: &Bb25Config,
) -> f64 {
    if tf == 0.0 {
        return 0.0;
    }
    let idf = ((n_docs - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.0);
    let tf_norm = (tf * (cfg.k1 + 1.0))
        / (tf + cfg.k1 * (1.0 - cfg.b + cfg.b * doc_len / avgdl.max(1.0)));
    idf * tf_norm
}

// ---------------------------------------------------------------------------
// Bayesian calibration (Eq. 20-27)
// ---------------------------------------------------------------------------

/// P(relevant | score) via sigmoid calibration.
pub fn likelihood(score: f64, alpha: f64, beta: f64) -> f64 {
    sigmoid(alpha * (score - beta))
}

/// Term frequency prior: 0.2 base, saturates at tf=10.
pub fn tf_prior(tf: f64) -> f64 {
    0.2 + 0.7 * (tf / 10.0).min(1.0)
}

/// Document length normalization prior.
pub fn norm_prior(doc_len_ratio: f64) -> f64 {
    0.3 + 0.6 * (1.0 - ((doc_len_ratio - 0.5).abs() * 2.0).min(1.0))
}

/// Composite prior blending tf + normalization.
pub fn composite_prior(tf: f64, doc_len_ratio: f64) -> f64 {
    let p = 0.7 * tf_prior(tf) + 0.3 * norm_prior(doc_len_ratio);
    p.clamp(0.1, 0.9)
}

/// Bayesian posterior: P(relevant | score, prior, base_rate).
pub fn posterior(lik: f64, prior: f64, base_rate: Option<f64>) -> f64 {
    let lik = clamp_prob(lik);
    let prior = clamp_prob(prior);

    // First Bayes update: likelihood * prior
    let p1 = (lik * prior) / (lik * prior + (1.0 - lik) * (1.0 - prior));
    let p1 = clamp_prob(p1);

    // Optional second update with base_rate
    match base_rate {
        Some(br) => {
            let br = clamp_prob(br);
            let p2 = (p1 * br) / (p1 * br + (1.0 - p1) * (1.0 - br));
            clamp_prob(p2)
        }
        None => p1,
    }
}

/// Full pipeline: score → calibrated probability.
pub fn score_to_probability(
    score: f64,
    tf: f64,
    doc_len_ratio: f64,
    state: &Bb25State,
) -> f64 {
    let lik = likelihood(score, state.config.alpha, state.config.beta);
    let prior = composite_prior(tf, doc_len_ratio);
    posterior(lik, prior, state.config.base_rate)
}

// ---------------------------------------------------------------------------
// Online learning (Algorithm 8.3.1)
// ---------------------------------------------------------------------------

/// Update calibration parameters from a relevance judgment.
/// `label` should be 1.0 (relevant) or 0.0 (not relevant).
pub fn update(state: &mut Bb25State, score: f64, label: f64, lr: f64) {
    state.n_updates += 1;
    let t = state.n_updates as f64;

    let lik = likelihood(score, state.config.alpha, state.config.beta);
    let err = label - lik;
    let lik_deriv = lik * (1.0 - lik); // sigmoid derivative

    // Gradients
    let grad_alpha = err * lik_deriv * (score - state.config.beta);
    let grad_beta = -err * lik_deriv * state.config.alpha;

    // EMA (momentum = 0.9)
    let momentum = 0.9;
    state.grad_alpha_ema = momentum * state.grad_alpha_ema + (1.0 - momentum) * grad_alpha;
    state.grad_beta_ema = momentum * state.grad_beta_ema + (1.0 - momentum) * grad_beta;

    // Bias correction
    let bc = 1.0 - momentum.powi(t as i32);
    let corrected_alpha = state.grad_alpha_ema / bc;
    let corrected_beta = state.grad_beta_ema / bc;

    // Learning rate decay
    let effective_lr = lr / (1.0 + 0.01 * t);

    // Update with L2 clipping
    state.config.alpha += effective_lr * corrected_alpha;
    state.config.beta += effective_lr * corrected_beta;

    // Clip alpha to positive, beta is unbounded
    state.config.alpha = state.config.alpha.max(0.01);

    // Polyak averaging
    state.alpha_avg = state.alpha_avg * (1.0 - 1.0 / t) + state.config.alpha / t;
    state.beta_avg = state.beta_avg * (1.0 - 1.0 / t) + state.config.beta / t;
}

// ---------------------------------------------------------------------------
// Auto-estimation from corpus
// ---------------------------------------------------------------------------

/// Estimate (alpha, beta) from corpus statistics by sampling pseudo-queries.
/// Returns (alpha, beta) suitable for Bb25Config.
pub fn auto_estimate(corpus: &CorpusStats) -> (f64, f64) {
    if corpus.n_docs < 2 {
        return (1.0, 0.0);
    }

    let cfg = Bb25Config::default();
    let avgdl = corpus.avgdl();
    let n = corpus.n_docs as f64;

    // Collect sample BM25 scores from top-df terms
    let mut top_terms: Vec<(&String, &u32)> = corpus.df.iter().collect();
    top_terms.sort_by(|a, b| b.1.cmp(a.1));
    top_terms.truncate(50);

    let mut scores = Vec::new();
    for &(term, df_count) in &top_terms {
        let df = *df_count as f64;
        // Simulate a query for this term against docs that contain it
        for (_, doc) in &corpus.doc_stats {
            if let Some(&tf) = doc.term_freqs.get(term) {
                let s = bm25_term_score(tf as f64, df, n, doc.doc_len as f64, avgdl, &cfg);
                if s > 0.0 {
                    scores.push(s);
                }
            }
        }
    }

    if scores.is_empty() {
        return (1.0, 0.0);
    }

    scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Beta = median score
    let beta = scores[scores.len() / 2];

    // Alpha = 1 / std_dev (steepness inversely proportional to score spread)
    let mean = scores.iter().sum::<f64>() / scores.len() as f64;
    let variance = scores.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / scores.len() as f64;
    let std_dev = variance.sqrt().max(0.01);
    let alpha = (1.0 / std_dev).clamp(0.1, 10.0);

    (alpha, beta)
}

// ---------------------------------------------------------------------------
// Log-odds fusion (for hybrid vector + BM25)
// ---------------------------------------------------------------------------

/// Combine two probability scores via balanced log-odds fusion.
/// `weight` controls balance: 0.5 = equal, >0.5 favors p1.
pub fn log_odds_fusion(p1: f64, p2: f64, weight: f64) -> f64 {
    let lo1 = logit(p1);
    let lo2 = logit(p2);
    let fused = weight * lo1 + (1.0 - weight) * lo2;
    sigmoid(fused)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_basic() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-10);
        assert!(sigmoid(10.0) > 0.99);
        assert!(sigmoid(-10.0) < 0.01);
    }

    #[test]
    fn logit_inverse_sigmoid() {
        for &p in &[0.1, 0.3, 0.5, 0.7, 0.9] {
            assert!((sigmoid(logit(p)) - p).abs() < 1e-8);
        }
    }

    #[test]
    fn bm25_positive_for_nonzero_tf() {
        let cfg = Bb25Config::default();
        let score = bm25_term_score(3.0, 5.0, 100.0, 50.0, 60.0, &cfg);
        assert!(score > 0.0);
    }

    #[test]
    fn bm25_zero_for_zero_tf() {
        let cfg = Bb25Config::default();
        let score = bm25_term_score(0.0, 5.0, 100.0, 50.0, 60.0, &cfg);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn posterior_reasonable() {
        let p = posterior(0.8, 0.5, None);
        assert!(p > 0.5);
        assert!(p < 1.0);
    }

    #[test]
    fn score_to_probability_bounded() {
        let state = Bb25State::default();
        let p = score_to_probability(3.0, 2.0, 1.0, &state);
        assert!(p > 0.0 && p < 1.0);
    }

    #[test]
    fn log_odds_fusion_equal() {
        let fused = log_odds_fusion(0.8, 0.8, 0.5);
        assert!((fused - 0.8).abs() < 1e-6);
    }

    #[test]
    fn corpus_stats_incremental() {
        let mut cs = CorpusStats::new();
        cs.add_document(1, &["weber".into(), "bracket".into(), "forc".into()]);
        cs.add_document(2, &["weber".into(), "newton".into()]);
        assert_eq!(cs.n_docs, 2);
        assert_eq!(cs.df["weber"], 2);
        assert_eq!(cs.df["bracket"], 1);

        cs.remove_document(1);
        assert_eq!(cs.n_docs, 1);
        assert_eq!(cs.df["weber"], 1);
        assert!(!cs.df.contains_key("bracket"));
    }

    #[test]
    fn auto_estimate_returns_valid() {
        let mut cs = CorpusStats::new();
        for i in 0..20 {
            let terms: Vec<String> = vec![
                "weber".into(),
                "forc".into(),
                format!("term{}", i % 5),
            ];
            cs.add_document(i, &terms);
        }
        let (alpha, beta) = auto_estimate(&cs);
        assert!(alpha > 0.0);
        assert!(beta >= 0.0);
    }
}
