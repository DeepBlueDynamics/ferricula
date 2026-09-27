//! Provider adapter for an ephemeral read-time briefing.
//! The caller supplies scoped source records; this module never opens a
//! memory writer and never stores generated text.
use std::time::SystemTime;

use anyhow::{Result, bail};
use ferricula_cognition::curator::{
    Briefing, CurationRequest, Curator, ExtractiveCurator, GenerativeResponse,
    build_prompt, parse_generative_response, select_candidates,
};
use ferricula_cognition::sati::Dial;
use crate::model::{InferenceRequest, InferenceTransport, ModelRouter};
use crate::model_config::{ModelCapability, TaskClass};

/// Adapt the immutable, identity-verified legacy store's real text tags.
/// Metadata without text is not invented into source content. Legacy facts
/// remain unclassified facts rather than being declared successful tasks.
pub fn from_recovered_hits(
    agent: ferricula_cognition::scope::AgentId,
    task: String,
    hits: &[crate::memory::MemoryHit],
    timestamp: u64,
) -> CurationRequest {
    use ferricula_cognition::curator::{MemoryKind, RawMemory};
    use ferricula_cognition::outcome::Pool;
    use ferricula_cognition::sati::{CueSource, RecallCue};
    use crate::memory::LifecycleStateView;
    let candidates = hits.iter().take(20).enumerate().filter_map(|(rank, hit)| {
        if !matches!(hit.state, LifecycleStateView::Active) { return None; }
        let text = hit.tags.get("text").filter(|text| !text.trim().is_empty())?;
        Some(RawMemory {
            id: hit.id, agent: agent.clone(), text: text.clone(),
            oja: hit.fidelity, state: ferricula_core::LifecycleState::Active,
            vedana: None, pool: Pool::Unclassified, kind: MemoryKind::Raw,
            sealed: false, keystone: hit.keystone,
            // Preserve source ranking without comparing unlike raw score scales.
            retrieval_score: 1.0 / (rank + 1) as f32, sati_recall: None,
        })
    }).collect();
    CurationRequest {
        agent, cue: RecallCue {
            source: CueSource::Task,
            query_sha256: ferricula_cognition::gates::sha256_hex(&[&task]),
            ts: timestamp,
        },
        task, candidates, k: CurationRequest::DEFAULT_K, include_failures: false,
    }
}

/// Generative curation using a configured local or explicitly authorized API
/// model. Empty selections use the no-memory response without inference.
/// Run outside durable locks; the returned briefing must remain ephemeral.
pub fn curate_with_router(
    request: &CurationRequest,
    router: &ModelRouter,
    transport: &dyn InferenceTransport,
    daily_budget_usd: f64,
) -> Result<Briefing> {
    if request.task.trim().is_empty() || request.task.len() > 8192
        || request.candidates.len() > 20
    {
        bail!("curation request exceeds task or candidate bounds");
    }
    let selection = select_candidates(request, Dial::Service)?;
    if selection.chosen.is_empty() {
        return ExtractiveCurator::new().curate(request, &selection);
    }
    let prompt = build_prompt(request, &selection)?;
    let prompt_bytes = prompt.system.len().saturating_add(prompt.user.len());
    if prompt_bytes > 16384 {
        bail!("curation input exceeds context budget; no source was truncated");
    }
    let mut inference = InferenceRequest::new(TaskClass::Summarize, prompt.user)
        .require(ModelCapability::Chat)
        .require(ModelCapability::PrivateContext);
    inference.system = prompt.system;
    inference.estimated_input_tokens = u32::try_from(prompt_bytes + 128)?;
    inference.estimated_output_tokens = prompt.max_output_tokens;
    inference.max_tokens = Some(prompt.max_output_tokens);
    inference.temperature = Some(prompt.temperature);
    let (decision, response) = router.complete_with_budget(
        &inference, transport, SystemTime::now(), daily_budget_usd,
    )?;
    if decision.no_model {
        bail!("no authorized generative curator model is available");
    }
    if response.text.len() > 8192 {
        bail!("curator output exceeds briefing budget");
    }
    let truncated = response.raw.pointer("/choices/0/finish_reason").and_then(|v| v.as_str())
        == Some("length")
        || response.raw.get("stop_reason").and_then(|v| v.as_str()) == Some("max_tokens");
    if truncated {
        bail!("curator response was truncated");
    }
    parse_generative_response(&GenerativeResponse {
        text: response.text, model: decision.model, cost: None, truncated,
    }, request, &selection)
}
