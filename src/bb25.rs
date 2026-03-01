//! Bayesian BM25 (BB25) stub: deterministic transform of BM25 into calibrated probabilities.
//! Agents can fill in exact sigmoid/prior math from Cognica bb25 specs.

#[derive(Debug, Clone)]
pub struct Bb25Config {
    pub k1: f64,
    pub b: f64,
    pub prior: f64,       // prior probability (0,1)
    pub temperature: f64, // scaling for sigmoid
}

impl Default for Bb25Config {
    fn default() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            prior: 0.5,
            temperature: 1.0,
        }
    }
}

/// Classic BM25 score (deterministic).
pub fn bm25_score(tf: f64, df: f64, doc_len: f64, avg_dl: f64, cfg: &Bb25Config) -> f64 {
    if df == 0.0 || tf == 0.0 {
        return 0.0;
    }
    let idf = ((1.0 + (1e-9 + 1.0) / df).ln()).max(0.0); // placeholder idf; agents should plug exact formula
    let denom = tf + cfg.k1 * (1.0 - cfg.b + cfg.b * (doc_len / avg_dl.max(1.0)));
    idf * (tf * (cfg.k1 + 1.0)) / denom
}

/// BB25: wrap BM25 in a calibrated sigmoid with prior.
pub fn bb25_prob(bm25: f64, cfg: &Bb25Config) -> f64 {
    // Placeholder logistic; agents should replace with bb25’s exact calibration.
    let temp = cfg.temperature.max(1e-6);
    let z = (bm25 / temp).tanh(); // bounded in (-1,1)
    // Blend with prior to stay in (0,1)
    0.5 * (1.0 + z) * (1.0 - cfg.prior) + cfg.prior
}
