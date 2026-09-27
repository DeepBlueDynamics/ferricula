//! Optional `sati-recall` reranker hook — Ollaya `/api/decide` gate on the
//! hybrid top-N (Lume × Ollaya × Abhidhamma plan §8.1/§8.3, milestone M5).
//!
//! Recall path wiring:
//! ```text
//! query → lume hybrid search → [caller filters agent_id/state — §3.3]
//!      → sati-recall gate on top-N (N ≈ 20) → rank by gate verdict
//! ```
//!
//! Contract alignment with `ferricula-cognition` gates (API_PROPOSAL §2.4):
//! - The gate returns **typed answers or a typed abstention** — `Abstain`
//!   keeps the original retrieval order for that hit; an abstention is
//!   never fabricated into a score ("no fake judgments").
//! - `state_truncated` decisions are logged and treated as low-trust
//!   (plan §3.4): they abstain from reordering rather than steering it.
//! - `answers_query` maps 1:1 to cognition's `SatiRecallVerdict.answers_query`,
//!   `relevance` (expected level 0..3) to `.relevance`.
//! - The `× ojā` multiplier in the plan's ranking is applied by the memory
//!   system when it builds `RawMemory` (ojā is not a retrieval-side signal);
//!   inside the lume CLI the gate ordering is `answers_query`, then
//!   `relevance`, then the original hybrid score.
//!
//! Failure containment: the hook is optional and fail-soft. With the gate
//! down, unreachable, or malformed, every hit abstains and the caller sees
//! an unchanged order with the reason recorded (`NoModel` /
//! `ProviderError`) — never a failed search, never an invented verdict.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::hybrid::HybridHitDetails;

/// Default Ollaya endpoint (plan §8.4: local only).
pub const DEFAULT_OLLAYA_HOST: &str = "127.0.0.1:11435";
/// Default gate name — one Ollaya model per gate (plan §4.4).
pub const DEFAULT_GATE: &str = "sati-recall";
/// Default top-N judged per query (plan §8.1: N ≈ 20).
pub const DEFAULT_TOP_N: usize = 20;
/// laya:en works inside a 512-token window; ~1800 chars is a safe section
/// cap. Longer sections are head-truncated and the verdict abstains from
/// reordering (plan §3.4).
pub const DEFAULT_MAX_CHARS: usize = 1800;
const DEFAULT_TIMEOUT_MS: u64 = 3000;

/// Why a hit was NOT judged by the gate. Mirrors cognition
/// `AbstainReason` (the retrieval-side subset).
#[derive(Debug, Clone, PartialEq)]
pub enum AbstainReason {
    /// Gate disabled or not configured for this search.
    NoModel,
    /// The state exceeded the context budget (locally or server-side) —
    /// low-trust decisions do not steer reordering.
    StateTruncated,
    /// The gate answered below confidence floor.
    LowConfidence,
    /// Transport/parse failure; the message is retained for the report.
    ProviderError(String),
}

/// One gate verdict for one candidate. `abstain_reason: None` means judged.
#[derive(Debug, Clone)]
pub struct SatiVerdict {
    /// Calibrated probability the memory answers the query (plan §4.4:
    /// `answers_query.noul`).
    pub answers_query: f32,
    /// Expected relevance level 0..3 (plan §4.4: `relevance.score`).
    pub relevance: f32,
    pub abstain_reason: Option<AbstainReason>,
    /// Echoed by Ollaya when the state was truncated server-side; local
    /// head-truncation sets it too.
    pub state_truncated: bool,
    pub latency_us: u128,
    pub gate: String,
    pub model: Option<String>,
}

impl SatiVerdict {
    pub fn judged(&self) -> bool {
        self.abstain_reason.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct RerankConfig {
    /// Master switch. The lume CLI turns this on with `--rerank` or
    /// `LUME_SATI_RERANK=1`; the default search behavior is unchanged.
    pub enabled: bool,
    /// `host:port` for Ollaya (env `OLLAYA_HOST`; plan §8.4 default is
    /// local-only, hence no scheme).
    pub host: String,
    /// Gate model name (env `LUME_SATI_GATE`).
    pub gate: String,
    /// How many top hits are judged (env `LUME_SATI_TOP_N`).
    pub top_n: usize,
    /// Per-request timeout (env `LUME_SATI_TIMEOUT_MS`).
    pub timeout: Duration,
    /// Char cap on the `memory` field (env `LUME_SATI_MAX_CHARS`).
    pub max_chars: usize,
    /// Keep-alive so gate models stay warm between queries (plan §8.4).
    pub keep_alive: String,
}

impl RerankConfig {
    pub fn from_env(enabled: bool) -> Self {
        Self {
            enabled,
            host: std::env::var("OLLAYA_HOST")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_OLLAYA_HOST.to_string()),
            gate: std::env::var("LUME_SATI_GATE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_GATE.to_string()),
            top_n: std::env::var("LUME_SATI_TOP_N")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_TOP_N),
            timeout: Duration::from_millis(
                std::env::var("LUME_SATI_TIMEOUT_MS")
                    .ok()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(DEFAULT_TIMEOUT_MS),
            ),
            max_chars: std::env::var("LUME_SATI_MAX_CHARS")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_MAX_CHARS),
            keep_alive: std::env::var("LUME_SATI_KEEP_ALIVE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "30m".to_string()),
        }
    }

    fn decide_url(&self) -> String {
        format!("http://{}/api/decide", self.host.trim_end_matches('/'))
    }
}

/// Aggregate outcome for one reranked query.
#[derive(Debug, Clone)]
pub struct RerankStats {
    pub enabled: bool,
    pub gate: String,
    pub top_n: usize,
    pub judged: usize,
    pub abstained: usize,
    pub truncated: usize,
    /// At most the first few distinct provider errors, for the report line.
    pub errors: Vec<String>,
    pub p50_latency_us: u128,
    pub p95_latency_us: u128,
}

/// Result of reranking: the verdict for every hit (indexed by the original
/// hit position) plus the new ordering as a permutation of hit indices.
#[derive(Debug, Clone)]
pub struct RerankOutcome {
    /// Verdicts aligned with the INPUT slice (abstains included).
    pub verdicts: Vec<SatiVerdict>,
    /// New hit order: indices into the input slice. Abstained hits retain
    /// their relative input order after judged hits; disabled/no-model runs
    /// return the identity permutation.
    pub order: Vec<usize>,
    pub stats: RerankStats,
}

/// Head-truncate on a char boundary so a multi-byte UTF-8 sequence is never
/// split. Everything up to the cap is verbatim section text.
fn truncate_chars(text: &str, max_chars: usize) -> (&str, bool) {
    if text.chars().count() <= max_chars {
        return (text, false);
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    (&text[..end], true)
}

fn abstain(cfg: &RerankConfig, reason: AbstainReason, start: Instant) -> SatiVerdict {
    SatiVerdict {
        answers_query: 0.0,
        relevance: 0.0,
        abstain_reason: Some(reason),
        state_truncated: false,
        latency_us: start.elapsed().as_micros(),
        gate: cfg.gate.clone(),
        model: None,
    }
}

fn no_model_verdict(cfg: &RerankConfig) -> SatiVerdict {
    SatiVerdict {
        answers_query: 0.0,
        relevance: 0.0,
        abstain_reason: Some(AbstainReason::NoModel),
        state_truncated: false,
        latency_us: 0,
        gate: cfg.gate.clone(),
        model: None,
    }
}

/// Call the sati-recall gate once. Returns a verdict or an abstain reason.
fn judge_one(query: &str, memory: &str, cfg: &RerankConfig) -> SatiVerdict {
    let start = Instant::now();
    let (memory_state, locally_truncated) = truncate_chars(memory, cfg.max_chars);
    let url = cfg.decide_url();
    let body = serde_json::json!({
        "model": cfg.gate,
        "state": { "query": query, "memory": memory_state },
        "keep_alive": cfg.keep_alive,
    });

    let response = match ureq::post(&url).timeout(cfg.timeout).send_json(&body) {
        Ok(res) => res,
        Err(ureq::Error::Status(status, response)) => {
            return abstain(
                cfg,
                AbstainReason::ProviderError(format!(
                    "status {status} {}",
                    response.status_text()
                )),
                start,
            );
        }
        Err(e) => {
            let mut msg = format!("{e}");
            if url.contains("localhost") || url.contains("127.0.0.1") {
                msg.push_str(
                    " (Hint: inside a Docker container localhost is the container \
                     itself; point OLLAYA_HOST at the host, e.g. \
                     OLLAYA_HOST=host.docker.internal:11435)",
                );
            }
            return abstain(cfg, AbstainReason::ProviderError(msg), start);
        }
    };

    let parsed: Value = match response.into_json() {
        Ok(v) => v,
        Err(e) => {
            return abstain(
                cfg,
                AbstainReason::ProviderError(format!("response parse: {e}")),
                start,
            )
        }
    };

    // Plan Appendix A: answers.answers_query.noul + answers.relevance.score.
    let answers = parsed.get("answers").cloned().unwrap_or(Value::Null);
    let answers_query = answers
        .get("answers_query")
        .and_then(|v| v.get("noul"))
        .and_then(Value::as_f64);
    let relevance = answers
        .get("relevance")
        .and_then(|v| v.get("score"))
        .and_then(Value::as_f64);
    let state_truncated = parsed
        .get("state_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let model = parsed
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);

    let (Some(aq), Some(rel)) = (answers_query, relevance) else {
        return abstain(
            cfg,
            AbstainReason::ProviderError(
                "response missing answers.answers_query.noul or answers.relevance.score".into(),
            ),
            start,
        );
    };

    // Plan §3.4: truncated decisions are low-trust — they abstain from
    // reordering (the caller keeps retrieval order) instead of steering it.
    let truncated = state_truncated || locally_truncated;
    if truncated {
        let mut verdict = abstain(cfg, AbstainReason::StateTruncated, start);
        verdict.state_truncated = true;
        return verdict;
    }

    SatiVerdict {
        answers_query: aq as f32,
        relevance: rel as f32,
        abstain_reason: None,
        state_truncated: false,
        latency_us: start.elapsed().as_micros(),
        gate: cfg.gate.clone(),
        model,
    }
}

/// Judge the top-N hybrid hits through the sati-recall gate and produce the
/// reordering. Never fails: disabled or fully-abstaining runs return the
/// identity order with `NoModel`/`ProviderError` verdicts.
pub fn rerank_hits(query: &str, hits: &[HybridHitDetails], cfg: &RerankConfig) -> RerankOutcome {
    if !cfg.enabled {
        let verdicts: Vec<SatiVerdict> =
            (0..hits.len()).map(|_| no_model_verdict(cfg)).collect();
        return RerankOutcome {
            verdicts,
            order: (0..hits.len()).collect(),
            stats: RerankStats {
                enabled: false,
                gate: cfg.gate.clone(),
                top_n: cfg.top_n,
                judged: 0,
                abstained: hits.len(),
                truncated: 0,
                errors: Vec::new(),
                p50_latency_us: 0,
                p95_latency_us: 0,
            },
        };
    }

    let judged_n = cfg.top_n.min(hits.len());
    let mut verdicts = Vec::with_capacity(hits.len());
    for hit in hits.iter().take(judged_n) {
        verdicts.push(judge_one(query, &hit.body, cfg));
    }
    // Hits beyond top_n keep the hybrid order (never gate-judged, still
    // retrievable — recall is not truncated by the reranker).
    for _ in judged_n..hits.len() {
        verdicts.push(no_model_verdict(cfg));
    }

    let mut judged_idx: Vec<usize> = (0..judged_n).filter(|&i| verdicts[i].judged()).collect();
    judged_idx.sort_by(|&a, &b| {
        let (va, vb) = (&verdicts[a], &verdicts[b]);
        // answers_query desc, relevance desc, hybrid_score desc, rank asc.
        vb.answers_query
            .partial_cmp(&va.answers_query)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                vb.relevance
                    .partial_cmp(&va.relevance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                hits[b]
                    .hybrid_score
                    .partial_cmp(&hits[a].hybrid_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.cmp(&b))
    });

    let mut order = judged_idx;
    for i in 0..hits.len() {
        if !order.contains(&i) {
            order.push(i);
        }
    }

    let mut latencies: Vec<u128> = verdicts
        .iter()
        .take(judged_n)
        .map(|v| v.latency_us)
        .collect();
    latencies.sort_unstable();
    let percentile = |q: f64| -> u128 {
        if latencies.is_empty() {
            0
        } else {
            let idx = ((q * latencies.len() as f64).ceil() as usize)
                .saturating_sub(1)
                .min(latencies.len() - 1);
            latencies[idx]
        }
    };

    let judged = verdicts.iter().filter(|v| v.judged()).count();
    let truncated = verdicts.iter().filter(|v| v.state_truncated).count();
    let mut errors: Vec<String> = verdicts
        .iter()
        .filter_map(|v| match &v.abstain_reason {
            Some(AbstainReason::ProviderError(e)) => Some(e.clone()),
            _ => None,
        })
        .take(3)
        .collect();
    errors.sort();
    errors.dedup();

    RerankOutcome {
        verdicts,
        order,
        stats: RerankStats {
            enabled: true,
            gate: cfg.gate.clone(),
            top_n: cfg.top_n,
            judged,
            abstained: hits.len() - judged,
            truncated,
            errors,
            p50_latency_us: percentile(0.50),
            p95_latency_us: percentile(0.95),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(rank: usize, title: &str, body: &str, hybrid: f64) -> HybridHitDetails {
        HybridHitDetails {
            rank,
            section_index: rank,
            title: title.to_string(),
            filename: Some("corpus.md".to_string()),
            line_number: rank + 1,
            body: body.to_string(),
            bm25_score: hybrid * 0.5,
            semantic_score: hybrid * 0.5,
            skg_score: 0.0,
            hybrid_score: hybrid,
            boosted: false,
        }
    }

    /// Minimal loopback HTTP server answering every POST through
    /// `responder(request_body)`. Requests hit a real TCP stack — the
    /// hook's HTTP path is exercised end to end without Ollaya.
    fn spawn_gate_server(responder: fn(&[u8]) -> String) -> std::net::SocketAddr {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { break };
                let mut req = Vec::new();
                let mut buf = [0u8; 8192];
                // Read headers, then Content-Length bytes of body.
                let header_end = loop {
                    let n = match stream.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    req.extend_from_slice(&buf[..n]);
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&req[..header_end]).to_lowercase();
                let want = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while req.len() < header_end + want {
                    let n = match stream.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    req.extend_from_slice(&buf[..n]);
                }
                let body = responder(&req[header_end..]);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        addr
    }

    fn cfg_for(addr: std::net::SocketAddr) -> RerankConfig {
        RerankConfig {
            enabled: true,
            host: addr.to_string(),
            gate: "sati-recall".to_string(),
            top_n: 20,
            timeout: Duration::from_secs(2),
            max_chars: 1800,
            keep_alive: "30m".to_string(),
        }
    }

    /// Hit bodies containing "alpha" get a low gate score; "beta" a high
    /// one — the permutation must flip the input order.
    fn divergent_scores(req: &[u8]) -> String {
        if req.windows(4).any(|w| w == b"beta") {
            r#"{"model":"sati-recall","answers":{"answers_query":{"noul":0.95},"relevance":{"score":3.0}},"state_truncated":false}"#
                .to_string()
        } else {
            r#"{"model":"sati-recall","answers":{"answers_query":{"noul":0.10},"relevance":{"score":0.0}},"state_truncated":false}"#
                .to_string()
        }
    }

    fn malformed(_req: &[u8]) -> String {
        "not json at all".to_string()
    }

    #[test]
    fn disabled_rerank_is_identity() {
        let hits = vec![hit(1, "a", "alpha", 1.0), hit(2, "b", "beta", 0.9)];
        let cfg = RerankConfig::from_env(false);
        let out = rerank_hits("q", &hits, &cfg);
        assert_eq!(out.order, vec![0, 1]);
        assert_eq!(out.stats.judged, 0);
        assert!(out
            .verdicts
            .iter()
            .all(|v| v.abstain_reason == Some(AbstainReason::NoModel)));
    }

    #[test]
    fn gate_reorders_by_answers_query() {
        let addr = spawn_gate_server(divergent_scores);
        let hits = vec![
            hit(1, "a", "alpha body", 1.0),
            hit(2, "b", "beta body", 0.9),
        ];
        let out = rerank_hits("q", &hits, &cfg_for(addr));
        assert_eq!(out.stats.judged, 2);
        // "beta" judged 0.95 > "alpha" 0.10 — permutation flips the order.
        assert_eq!(out.order, vec![1, 0]);
        assert_eq!(out.verdicts[1].answers_query, 0.95);
        assert!(!out.verdicts.iter().any(|v| v.state_truncated));
        assert_eq!(out.stats.p95_latency_us, out.stats.p50_latency_us.max(out.stats.p95_latency_us));
    }

    #[test]
    fn provider_error_abstains_and_keeps_order() {
        let addr = spawn_gate_server(malformed);
        let hits = vec![hit(1, "a", "alpha", 1.0), hit(2, "b", "beta", 0.9)];
        let out = rerank_hits("q", &hits, &cfg_for(addr));
        assert_eq!(out.stats.judged, 0);
        assert_eq!(out.stats.abstained, 2);
        assert_eq!(out.order, vec![0, 1], "abstain keeps retrieval order");
        assert!(!out.stats.errors.is_empty());
        assert!(out.verdicts[0].latency_us > 0, "error round-trip measured");
    }

    #[test]
    fn truncated_state_abstains_from_reordering() {
        // Server marks every decision truncated: low-trust → no steering.
        fn truncated_response(_req: &[u8]) -> String {
            r#"{"model":"sati-recall","answers":{"answers_query":{"noul":0.99},"relevance":{"score":3.0}},"state_truncated":true}"#
                .to_string()
        }
        let addr = spawn_gate_server(truncated_response);
        let hits = vec![hit(1, "a", "alpha", 0.5), hit(2, "b", "beta", 1.0)];
        let out = rerank_hits("q", &hits, &cfg_for(addr));
        assert_eq!(out.stats.judged, 0);
        assert_eq!(out.stats.truncated, 2);
        assert_eq!(out.order, vec![0, 1], "truncated verdicts keep hybrid order");
    }

    #[test]
    fn beyond_top_n_keeps_order() {
        let addr = spawn_gate_server(divergent_scores);
        let mut cfg = cfg_for(addr);
        cfg.top_n = 1;
        let hits = vec![hit(1, "a", "alpha", 0.5), hit(2, "b", "beta", 0.4)];
        let out = rerank_hits("q", &hits, &cfg);
        assert_eq!(out.stats.judged, 1);
        assert_eq!(out.verdicts[1].abstain_reason, Some(AbstainReason::NoModel));
        // Judged hit stays first; the unjudged one keeps second place.
        assert_eq!(out.order, vec![0, 1]);
    }

    #[test]
    fn long_memory_is_truncated_on_char_boundary() {
        let (text, truncated) = truncate_chars("苹果电脑苹果电脑", 4);
        assert!(truncated);
        assert_eq!(text, "苹果电脑");
        let (text, truncated) = truncate_chars("短", 4);
        assert!(!truncated);
        assert_eq!(text, "短");
        // Never splits a multi-byte char.
        let (text, truncated) = truncate_chars("ab苹果", 3);
        assert!(truncated);
        assert_eq!(text, "ab苹");
    }
}