//! Provider-neutral decision-gate backends (Gates lane — Splendid Angelfish).
//!
//! Implements the gate traits from `ferricula-cognition::gates`
//! (VedanaGate, MergeGate, SatiRecallGate, TaskSucceededGate) against
//! Ollama-compatible chat endpoints (`/v1/chat/completions`).
//!
//! Calibration discipline (plan §6.3–6.4, Eldest Dog 2026-09-26):
//! self-reported chat probabilities are NOT recovered classifier posteriors.
//! They are only re-scaled (temperature over z ∝ T·log p) and only flagged
//! `calibrated = true` when a held-out ECE measurement authorized it; the
//! calibration file hash is always carried in provenance.

use std::time::Instant;

use ferricula_cognition::gates::{
    sha256_hex, AbstainReason, GateProvenance, Judged, MergeGate, MergeVerdict, SatiRecallGate,
    SatiRecallVerdict, TaskSucceededGate, TaskSucceededVerdict, UsageCost, VedanaGate,
    VedanaVerdict, Verdict,
};
use ferricula_cognition::outcome::Evidence;
use ferricula_cognition::sati::Valence;
use ferricula_cognition::AgentId;
use serde::{Deserialize, Serialize};

pub const VERSION: &str = "ferricula-gates/2.0.0-alpha.0";
/// Prompt-set version; bump on any prompt text change.
pub const PROMPT_VERSION: &str = "prompts/2026-09-26.v1";

/// Nightly calibration file (plan §6.3 output), loaded per gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calibration {
    /// sha256 of the calibration file bytes, recorded in provenance.
    pub sha256: String,
    pub temperature: f32,
    /// True only when held-out ECE < target authorized lifecycle use.
    pub lifecycle_authorized: bool,
}

#[derive(Debug, Clone)]
pub struct GateBackend {
    pub base_url: String,
    pub model: String,
    pub timeout_ms: u64,
    pub calibration: Option<Calibration>,
    pub gate: &'static str,
}

impl GateBackend {
    pub fn new(gate: &'static str, base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            timeout_ms: 90_000,
            calibration: None,
            gate,
        }
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}
#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}
#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}
#[derive(Deserialize)]
struct ChatUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

fn provenance(b: &GateBackend, features: &[&str], latency_us: u64, usage: Option<ChatUsage>) -> GateProvenance {
    GateProvenance {
        gate: b.gate.to_string(),
        gate_version: format!("{}|{}|{}", VERSION, PROMPT_VERSION, b.model),
        calibration_sha256: b.calibration.as_ref().map(|c| c.sha256.clone()),
        calibrated: b
            .calibration
            .as_ref()
            .is_some_and(|c| c.lifecycle_authorized),
        features_sha256: sha256_hex(features),
        latency_us,
        state_truncated: false,
        cost: usage.map(|u| UsageCost {
            input_tokens: u.prompt_tokens.unwrap_or(0),
            output_tokens: u.completion_tokens.unwrap_or(0),
            usd: None,
            profile: format!("api:ollama:{}", b.model),
        }),
    }
}

struct CallOk {
    text: String,
    usage: Option<ChatUsage>,
    latency_us: u64,
}

/// POST /v1/chat/completions at temperature 0.
fn chat(b: &GateBackend, prompt: &str) -> Result<CallOk, AbstainReason> {
    let t0 = Instant::now();
    let body = serde_json::json!({
        "model": b.model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.0,
        "stream": false,
    });
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_millis(b.timeout_ms))
        .build();
    let res = agent
        .post(&format!("{}/v1/chat/completions", b.base_url.trim_end_matches('/')))
        .set("Content-Type", "application/json")
        .send_json(body);
    let latency_us = t0.elapsed().as_micros() as u64;
    match res {
        Ok(resp) => {
            let cr: ChatResponse = resp.into_json().map_err(|e| AbstainReason::InvalidOutput {
                message: format!("response json: {e}"),
            })?;
            let text = cr
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.message.content)
                .unwrap_or_default();
            if text.trim().is_empty() {
                Err(AbstainReason::InvalidOutput {
                    message: "empty completion".into(),
                })
            } else {
                Ok(CallOk { text, usage: cr.usage, latency_us })
            }
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(AbstainReason::ProviderError {
                message: format!("http {code}: {}", body.chars().take(160).collect::<String>()),
            })
        }
        Err(e) => Err(AbstainReason::ProviderError {
            message: format!("transport: {e}"),
        }),
    }
}

fn abstain<T>(b: &GateBackend, reason: AbstainReason, features: &[&str], latency_us: u64) -> Judged<T> {
    Judged { verdict: Verdict::Abstain(reason), provenance: provenance(b, features, latency_us, None) }
}

/// Extract the first balanced {...} JSON object (tolerates code fences).
pub fn extract_first_json(s: &str) -> Option<serde_json::Value> {
    let start = s.find('{')?;
    let mut depth = 0i32;
    for (rel, ch) in s[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&s[start..=start + rel]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// Validate + re-normalize an option-probability dict; apply temperature.
/// `obj` must carry a "probs" object; every class must be present.
fn probs_from(
    b: &GateBackend,
    obj: &serde_json::Value,
    classes: &[&str],
) -> Result<Vec<(String, f32)>, AbstainReason> {
    let probs = obj
        .get("probs")
        .and_then(|p| p.as_object())
        .ok_or_else(|| AbstainReason::InvalidOutput { message: "missing probs object".into() })?;
    let mut v: Vec<(String, f32)> = Vec::with_capacity(classes.len());
    for c in classes {
        let x = probs
            .get(*c)
            .and_then(|x| x.as_f64())
            .ok_or_else(|| AbstainReason::InvalidOutput {
                message: format!("class {c} missing or non-numeric"),
            })? as f32;
        if !(0.0..=1.0).contains(&x) {
            return Err(AbstainReason::InvalidOutput {
                message: format!("class {c} = {x} out of [0,1]"),
            });
        }
        v.push(((*c).to_string(), x));
    }
    let total: f32 = v.iter().map(|(_, x)| x).sum();
    if !(0.95..=1.05).contains(&total) {
        return Err(AbstainReason::InvalidOutput {
            message: format!("probs sum {total} outside [0.95,1.05]"),
        });
    }
    for (_, x) in &mut v {
        *x /= total;
    }
    if let Some(cal) = &b.calibration {
        // Temperature rescale (plan §6.2): z ∝ T·log p; p' = softmax(z / T').
        let t = cal.temperature.max(0.05);
        let logits: Vec<f32> = v.iter().map(|(_, x)| x.max(1e-9).ln() / t).collect();
        let max = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let exps: Vec<f32> = logits.iter().map(|z| (z - max).exp()).collect();
        let sum: f32 = exps.iter().sum();
        for ((_, x), e) in v.iter_mut().zip(exps) {
            *x = e / sum;
        }
    }
    Ok(v)
}

fn argmax(v: &[(String, f32)]) -> (String, f32) {
    v.iter()
        .cloned()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or_else(|| (String::new(), 0.0))
}

// ---------------------------------------------------------------- prompts
// English prompts mirror research/gates/prompts (PROMPT_VERSION pinned).

const VEDANA_PROMPT: &str = include_str!("../../../../research/gates/prompts/gate_vedana_en.txt");

const SATI_RECALL_PROMPT: &str = r#"You are the sati-recall gate of an Abhidhamma memory system.

Query:
"""
{query}
"""

Memory:
"""
{memory}
"""

Judge:
- answers_query: the memory contains information that answers or directly helps with the query (probability 0..1).
- relevance: expected level, 0=unrelated, 1=same topic only, 2=partially answers, 3=directly answers.

Return ONLY valid JSON, no prose:
{"probs": {"answers_query": 0.0}, "relevance": 0}
"#;

const MERGE_PROMPT: &str = r#"You are the sankhara-merge gate of an Abhidhamma memory system. The state holds a candidate cluster of memory texts.

Members:
"""
{members}
"""

Judge both:
- same_truth: these memories describe the same underlying fact, event or understanding and can be merged into one (probability 0..1).
- contradiction: at least two of these memories contradict each other (probability 0..1).

Return ONLY valid JSON, no prose:
{"probs": {"same_truth": 0.0, "contradiction": 0.0}}
"#;

const TASK_SUCCEEDED_PROMPT: &str = r#"You are the task_succeeded gate. Credit only outcomes that external evidence confirms (tool results, state changes); ignore the agent's own claims of success; treat ambiguous or partial outcomes as failures.

Transcript:
"""
{transcript}
"""

Evidence:
{evidence}

Return ONLY valid JSON:
{"probs": {"task_succeeded": 0.0}}
where the value is the probability (0..1) that completion is confirmed by external evidence in the transcript.
"#;

// ---------------------------------------------------------------- gates

pub struct ChatVedanaGate {
    pub b: GateBackend,
}

impl VedanaGate for ChatVedanaGate {
    fn judge(&self, _agent: &AgentId, text: &str) -> Judged<VedanaVerdict> {
        let features = [PROMPT_VERSION, text];
        let prompt = VEDANA_PROMPT.replace("{text}", text);
        let call = match chat(&self.b, &prompt) {
            Ok(c) => c,
            Err(reason) => return abstain(&self.b, reason, &features, 0),
        };
        let obj = match extract_first_json(&call.text) {
            Some(o) => o,
            None => {
                return abstain(&self.b, AbstainReason::InvalidOutput { message: "no JSON object".into() }, &features, call.latency_us)
            }
        };
        let v = match probs_from(&self.b, &obj, &["sukha", "dukkha", "adukkhamasukha"]) {
            Ok(v) => v,
            Err(reason) => return abstain(&self.b, reason, &features, call.latency_us),
        };
        let intensity = match obj
            .get("intensity")
            .or_else(|| obj.get("tibbatā"))
            .and_then(|x| x.as_f64())
            .map(|x| x as f32)
        {
            Some(i) if (0.0..=4.0).contains(&i) => i,
            _ => {
                return abstain(&self.b, AbstainReason::InvalidOutput { message: "intensity missing or out of 0..4".into() }, &features, call.latency_us)
            }
        };
        let (pred, p) = argmax(&v);
        if p < 0.50 {
            return abstain(&self.b, AbstainReason::LowConfidence { p_max: p }, &features, call.latency_us);
        }
        let valence = match pred.as_str() {
            "sukha" => Valence::Sukha,
            "dukkha" => Valence::Dukkha,
            _ => Valence::Neutral,
        };
        Judged {
            verdict: Verdict::Answer(VedanaVerdict { valence, intensity, p }),
            provenance: provenance(&self.b, &features, call.latency_us, call.usage),
        }
    }
}

pub struct ChatMergeGate {
    pub b: GateBackend,
}

impl MergeGate for ChatMergeGate {
    fn judge(&self, _agent: &AgentId, members: &[&str]) -> Judged<MergeVerdict> {
        let joined = members
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{}. {}", i + 1, m))
            .collect::<Vec<_>>()
            .join("\n");
        let mut features: Vec<&str> = vec![PROMPT_VERSION];
        features.extend(members.iter().copied());
        let prompt = MERGE_PROMPT.replace("{members}", &joined);
        let call = match chat(&self.b, &prompt) {
            Ok(c) => c,
            Err(reason) => return abstain(&self.b, reason, &features, 0),
        };
        let obj = match extract_first_json(&call.text) {
            Some(o) => o,
            None => return abstain(&self.b, AbstainReason::InvalidOutput { message: "no JSON object".into() }, &features, call.latency_us),
        };
        let v = match probs_from(&self.b, &obj, &["same_truth", "contradiction"]) {
            Ok(v) => v,
            Err(reason) => return abstain(&self.b, reason, &features, call.latency_us),
        };
        let same_truth = v.iter().find(|(k, _)| k == "same_truth").map(|(_, x)| *x).unwrap_or(0.0);
        let contradiction = v.iter().find(|(k, _)| k == "contradiction").map(|(_, x)| *x).unwrap_or(0.0);
        Judged {
            verdict: Verdict::Answer(MergeVerdict { same_truth, contradiction }),
            provenance: provenance(&self.b, &features, call.latency_us, call.usage),
        }
    }
}

pub struct ChatSatiRecallGate {
    pub b: GateBackend,
}

impl SatiRecallGate for ChatSatiRecallGate {
    fn judge(&self, _agent: &AgentId, query: &str, memory: &str) -> Judged<SatiRecallVerdict> {
        let features = [PROMPT_VERSION, query, memory];
        let prompt = SATI_RECALL_PROMPT.replace("{query}", query).replace("{memory}", memory);
        let call = match chat(&self.b, &prompt) {
            Ok(c) => c,
            Err(reason) => return abstain(&self.b, reason, &features, 0),
        };
        let obj = match extract_first_json(&call.text) {
            Some(o) => o,
            None => return abstain(&self.b, AbstainReason::InvalidOutput { message: "no JSON object".into() }, &features, call.latency_us),
        };
        let v = match probs_from(&self.b, &obj, &["answers_query"]) {
            Ok(v) => v,
            Err(reason) => return abstain(&self.b, reason, &features, call.latency_us),
        };
        let relevance = match obj.get("relevance").and_then(|x| x.as_f64()).map(|x| (x as f32).clamp(0.0, 3.0)) {
            Some(r) => r,
            None => return abstain(&self.b, AbstainReason::InvalidOutput { message: "relevance missing".into() }, &features, call.latency_us),
        };
        Judged {
            verdict: Verdict::Answer(SatiRecallVerdict { answers_query: v[0].1, relevance }),
            provenance: provenance(&self.b, &features, call.latency_us, call.usage),
        }
    }
}

pub struct ChatTaskSucceededGate {
    pub b: GateBackend,
}

impl TaskSucceededGate for ChatTaskSucceededGate {
    fn judge(&self, _agent: &AgentId, transcript: &str, evidence: &[Evidence]) -> Judged<TaskSucceededVerdict> {
        let ev = serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "[]".into());
        let features = [PROMPT_VERSION, transcript, ev.as_str()];
        let prompt = TASK_SUCCEEDED_PROMPT.replace("{transcript}", transcript).replace("{evidence}", &ev);
        let call = match chat(&self.b, &prompt) {
            Ok(c) => c,
            Err(reason) => return abstain(&self.b, reason, &features, 0),
        };
        let obj = match extract_first_json(&call.text) {
            Some(o) => o,
            None => return abstain(&self.b, AbstainReason::InvalidOutput { message: "no JSON object".into() }, &features, call.latency_us),
        };
        let v = match probs_from(&self.b, &obj, &["task_succeeded"]) {
            Ok(v) => v,
            Err(reason) => return abstain(&self.b, reason, &features, call.latency_us),
        };
        Judged {
            verdict: Verdict::Answer(TaskSucceededVerdict { p: v[0].1 }),
            provenance: provenance(&self.b, &features, call.latency_us, call.usage),
        }
    }
}

// Note: the saññā gate has no trait in cognition::gates yet (§2.4 extension
// is Burning Dingo's). Its prompt + measurement live in research/gates until
// the trait lands; do not approximate it through another gate's types.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_first_json_handles_fences() {
        let s = "Here you go:\n```json\n{\"probs\": {\"a\": 1.0}}\n```\nthanks";
        let v = extract_first_json(s).expect("json found");
        assert_eq!(v["probs"]["a"].as_f64().unwrap(), 1.0);
    }

    #[test]
    fn probs_renorm_within_tolerance() {
        let b = GateBackend::new("vedana", "http://127.0.0.1:1", "m");
        let obj = serde_json::json!({"probs": {"sukha": 0.2, "dukkha": 0.4, "adukkhamasukha": 0.39}});
        let v = probs_from(&b, &obj, &["sukha", "dukkha", "adukkhamasukha"]).unwrap();
        let s: f32 = v.iter().map(|(_, x)| x).sum();
        assert!((s - 1.0).abs() < 1e-6);
        assert!((v[1].1 - 0.4040).abs() < 0.001);
    }

    #[test]
    fn probs_reject_bad_sum() {
        let b = GateBackend::new("vedana", "http://127.0.0.1:1", "m");
        let obj = serde_json::json!({"probs": {"sukha": 0.2, "dukkha": 0.4, "adukkhamasukha": 0.2}});
        assert!(matches!(
            probs_from(&b, &obj, &["sukha", "dukkha", "adukkhamasukha"]),
            Err(AbstainReason::InvalidOutput { .. })
        ));
    }

    #[test]
    fn temperature_sharpens_and_flattens() {
        let mut b = GateBackend::new("vedana", "http://127.0.0.1:1", "m");
        let obj = serde_json::json!({"probs": {"sukha": 0.5, "dukkha": 0.3, "adukkhamasukha": 0.2}});
        b.calibration = Some(Calibration { sha256: "x".into(), temperature: 0.5, lifecycle_authorized: false });
        let cold = probs_from(&b, &obj, &["sukha", "dukkha", "adukkhamasukha"]).unwrap();
        assert!(cold[0].1 > 0.5, "T<1 sharpens the mode");
        b.calibration = Some(Calibration { sha256: "x".into(), temperature: 4.0, lifecycle_authorized: false });
        let hot = probs_from(&b, &obj, &["sukha", "dukkha", "adukkhamasukha"]).unwrap();
        assert!(hot[0].1 < 0.5, "T>1 flattens the mode");
    }

    #[test]
    fn calibrated_flag_requires_authorization() {
        let mut b = GateBackend::new("vedana", "http://127.0.0.1:1", "m");
        b.calibration = Some(Calibration { sha256: "x".into(), temperature: 1.0, lifecycle_authorized: false });
        let p = provenance(&b, &["a"], 1, None);
        assert!(!p.calibrated, "calibration file without held-out ECE pass never authorizes");
        assert!(p.calibration_sha256.is_some());
    }
}
