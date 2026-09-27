//! Pluggable inference router for the single Steve identity.
//!
//! Selection is offline and deterministic: ordered provider fallback, capability
//! and context filtering, per-route daily dollar budgets, concurrency metadata,
//! timeouts, and escalation reasons. Network I/O is confined to the
//! [`InferenceTransport`] trait so unit tests never leave the process.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model_config::{
    ModelCapability, ModelProfile, ModelRoutingConfig, ProviderKind, TaskClass,
};

/// Why the router chose a non-preferred step (or no model at all).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    /// First eligible step in the ordered chain.
    Preferred,
    /// Earlier step skipped because estimated spend would breach its budget.
    BudgetExhausted { profile_id: String },
    /// Earlier step lacked a required capability.
    MissingCapability {
        profile_id: String,
        capability: String,
    },
    /// Earlier step context window too small for the request.
    ContextTooSmall {
        profile_id: String,
        needed: u32,
        available: u32,
    },
    /// Earlier step was a paid model with zero remaining budget for free work.
    PaidRouteBlocked { profile_id: String },
    /// Deterministic no-model path selected by policy.
    NoModelPolicy,
    /// An eligible provider failed during execution, so the next configured
    /// route step was attempted.
    ProviderFailed { profile_id: String },
    /// A paid profile has unresolved usage from an earlier call today.
    UsageUnreported { profile_id: String },
    /// No configured step satisfied filters.
    NoEligibleRoute,
}

impl EscalationReason {
    pub fn as_code(&self) -> &'static str {
        match self {
            EscalationReason::Preferred => "preferred",
            EscalationReason::BudgetExhausted { .. } => "budget_exhausted",
            EscalationReason::MissingCapability { .. } => "missing_capability",
            EscalationReason::ContextTooSmall { .. } => "context_too_small",
            EscalationReason::PaidRouteBlocked { .. } => "paid_route_blocked",
            EscalationReason::NoModelPolicy => "no_model_policy",
            EscalationReason::ProviderFailed { .. } => "provider_failed",
            EscalationReason::UsageUnreported { .. } => "usage_unreported",
            EscalationReason::NoEligibleRoute => "no_eligible_route",
        }
    }
}

/// Inputs that affect offline selection (never triggers network I/O).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceRequest {
    pub task_class: TaskClass,
    /// Approximate prompt size in tokens for context filtering.
    pub estimated_input_tokens: u32,
    /// Expected completion size used for budget estimation.
    pub estimated_output_tokens: u32,
    /// Required capabilities (all must match).
    #[serde(default)]
    pub required_capabilities: Vec<ModelCapability>,
    pub system: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

impl InferenceRequest {
    pub fn new(task_class: TaskClass, user_text: impl Into<String>) -> Self {
        Self {
            task_class,
            estimated_input_tokens: 512,
            estimated_output_tokens: 256,
            required_capabilities: Vec::new(),
            system: String::new(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: user_text.into(),
            }],
            max_tokens: Some(1024),
            temperature: Some(0.2),
        }
    }

    pub fn with_tokens(mut self, input: u32, output: u32) -> Self {
        self.estimated_input_tokens = input;
        self.estimated_output_tokens = output;
        self
    }

    pub fn require(mut self, capability: ModelCapability) -> Self {
        self.required_capabilities.push(capability);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Result of offline route selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
    pub task_class: TaskClass,
    pub profile_id: String,
    pub provider: ProviderKind,
    pub model: String,
    pub daily_budget_usd: f64,
    pub estimated_cost_usd: f64,
    pub timeout_ms: u64,
    pub max_concurrency: u32,
    pub api_key_env: String,
    pub base_url: String,
    pub no_model: bool,
    pub step_index: usize,
    pub escalation: EscalationReason,
    /// Prior steps that were skipped, with reasons (ordered).
    pub skipped: Vec<EscalationReason>,
}

/// OpenAI-compatible chat completions request body (no secrets).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiChatRequest {
    pub model: String,
    pub messages: Vec<OpenAiChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiChatMessage {
    pub role: String,
    pub content: String,
}

/// Anthropic Messages API request body (no secrets).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnthropicMessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: String,
}

/// Wire package ready for a transport (keys remain env-name references).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum ProviderRequest {
    OpenAiCompatible {
        url: String,
        api_key_env: String,
        timeout_ms: u64,
        body: OpenAiChatRequest,
    },
    Anthropic {
        url: String,
        api_key_env: String,
        timeout_ms: u64,
        body: AnthropicMessagesRequest,
    },
    /// Deterministic local path; transports must not open a socket.
    NoModel { reason: String, echo: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub text: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub raw: Value,
}

/// Network execution boundary. Production impls may call reqwest; tests use
/// [`RecordingTransport`] or [`NoopTransport`].
pub trait InferenceTransport: Send + Sync {
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse>;
}

/// Transport that never touches the network.
#[derive(Debug, Default)]
pub struct NoopTransport;

impl InferenceTransport for NoopTransport {
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        match request {
            ProviderRequest::NoModel { reason, echo } => Ok(ProviderResponse {
                text: format!("[no-model:{reason}] {echo}"),
                input_tokens: 0,
                output_tokens: 0,
                raw: json!({ "mode": "no_model", "reason": reason, "echo": echo }),
            }),
            ProviderRequest::OpenAiCompatible { .. } | ProviderRequest::Anthropic { .. } => {
                bail!("NoopTransport refuses network protocols; inject a real InferenceTransport")
            }
        }
    }
}

/// Records provider requests for offline assertions.
#[derive(Debug, Default)]
pub struct RecordingTransport {
    pub calls: Mutex<Vec<ProviderRequest>>,
    pub scripted: Mutex<BTreeMap<String, ProviderResponse>>,
}

impl RecordingTransport {
    pub fn script(&self, model: &str, response: ProviderResponse) {
        self.scripted
            .lock()
            .expect("scripted poisoned")
            .insert(model.to_string(), response);
    }
}

impl InferenceTransport for RecordingTransport {
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        self.calls
            .lock()
            .expect("calls poisoned")
            .push(request.clone());
        match request {
            ProviderRequest::NoModel { reason, echo } => Ok(ProviderResponse {
                text: format!("[no-model:{reason}] {echo}"),
                input_tokens: 0,
                output_tokens: 0,
                raw: json!({ "mode": "no_model", "reason": reason }),
            }),
            ProviderRequest::OpenAiCompatible { body, .. } => {
                let scripted = self.scripted.lock().expect("scripted poisoned");
                if let Some(resp) = scripted.get(&body.model) {
                    return Ok(resp.clone());
                }
                Ok(ProviderResponse {
                    text: format!("mock-openai:{}", body.model),
                    input_tokens: 10,
                    output_tokens: 5,
                    raw: json!({ "mock": "openai", "model": body.model }),
                })
            }
            ProviderRequest::Anthropic { body, .. } => {
                let scripted = self.scripted.lock().expect("scripted poisoned");
                if let Some(resp) = scripted.get(&body.model) {
                    return Ok(resp.clone());
                }
                Ok(ProviderResponse {
                    text: format!("mock-anthropic:{}", body.model),
                    input_tokens: 12,
                    output_tokens: 8,
                    raw: json!({ "mock": "anthropic", "model": body.model }),
                })
            }
        }
    }
}

/// One recorded inference spend event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageEntry {
    /// Missing provider usage is reserved using request bounds, not reported as free.
    /// Paid profiles with unresolved estimated usage are blocked for the day.
    #[serde(default)]
    pub usage_estimated: bool,
    pub day: String,
    pub task_class: TaskClass,
    pub profile_id: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
    pub unix_secs: u64,
}

/// In-memory daily spend ledger keyed by `(day, profile_id)`.
#[derive(Debug, Default)]
pub struct UsageLedger {
    entries: Mutex<Vec<UsageEntry>>,
}

impl UsageLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: Vec<UsageEntry>) -> Self {
        Self {
            entries: Mutex::new(entries),
        }
    }

    pub fn record(
        &self,
        task_class: TaskClass,
        profile_id: impl Into<String>,
        input_tokens: u32,
        output_tokens: u32,
        cost_usd: f64,
        now: SystemTime,
    ) {
        self.record_with_estimate(task_class, profile_id, input_tokens, output_tokens, cost_usd, now, false);
    }

    fn record_with_estimate(
        &self, task_class: TaskClass, profile_id: impl Into<String>,
        input_tokens: u32, output_tokens: u32, cost_usd: f64,
        now: SystemTime, usage_estimated: bool,
    ) {
        let unix_secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let day = utc_day(unix_secs);
        self.entries
            .lock()
            .expect("ledger poisoned")
            .push(UsageEntry {
                usage_estimated,
                day,
                task_class,
                profile_id: profile_id.into(),
                input_tokens,
                output_tokens,
                cost_usd,
                unix_secs,
            });
    }

    pub fn has_unreported_usage_today(&self, profile_id: &str, now: SystemTime) -> bool {
        let day = utc_day(now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs());
        self.entries.lock().expect("ledger poisoned").iter().any(|entry|
            entry.day == day && entry.profile_id == profile_id && entry.usage_estimated)
    }

    pub fn spent_usd_today(&self, profile_id: &str, now: SystemTime) -> f64 {
        let unix_secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let day = utc_day(unix_secs);
        normalize_zero(
            self.entries
                .lock()
                .expect("ledger poisoned")
                .iter()
                .filter(|e| e.day == day && e.profile_id == profile_id)
                .map(|e| e.cost_usd)
                .sum(),
        )
    }

    pub fn total_usd_today(&self, now: SystemTime) -> f64 {
        let unix_secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let day = utc_day(unix_secs);
        normalize_zero(
            self.entries
                .lock()
                .expect("ledger poisoned")
                .iter()
                .filter(|e| e.day == day)
                .map(|e| e.cost_usd)
                .sum(),
        )
    }

    pub fn entries(&self) -> Vec<UsageEntry> {
        self.entries.lock().expect("ledger poisoned").clone()
    }
}

fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

fn utc_day(unix_secs: u64) -> String {
    // Simple YYYY-MM-DD without external crates (good enough for daily buckets).
    const DAY: u64 = 86_400;
    let days = unix_secs / DAY;
    // Civil date from days since Unix epoch (Howard Hinnant algorithm).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Offline model router bound to one Steve configuration.
pub struct ModelRouter {
    config: ModelRoutingConfig,
    ledger: UsageLedger,
}

impl ModelRouter {
    pub fn new(config: ModelRoutingConfig) -> Result<Self> {
        Self::with_usage(config, Vec::new())
    }

    pub fn with_usage(config: ModelRoutingConfig, usage: Vec<UsageEntry>) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            ledger: UsageLedger::from_entries(usage),
        })
    }

    pub fn with_defaults() -> Result<Self> {
        Self::new(ModelRoutingConfig::steve_safe_defaults())
    }

    pub fn config(&self) -> &ModelRoutingConfig {
        &self.config
    }

    pub fn ledger(&self) -> &UsageLedger {
        &self.ledger
    }

    pub fn identity_name(&self) -> &str {
        &self.config.identity.name
    }

    /// Select a route without performing any network I/O.
    pub fn select(&self, request: &InferenceRequest, now: SystemTime) -> Result<RouteDecision> {
        self.select_from(request, now, 0, Vec::new())
    }

    fn select_from(
        &self,
        request: &InferenceRequest,
        now: SystemTime,
        start_step: usize,
        mut skipped: Vec<EscalationReason>,
    ) -> Result<RouteDecision> {
        let Some(route) = self.config.route_for(request.task_class) else {
            bail!("no route configured for task class {}", request.task_class);
        };

        for (step_index, step) in route.steps.iter().enumerate().skip(start_step) {
            let profile = self
                .config
                .profile(&step.profile_id)
                .ok_or_else(|| anyhow::anyhow!("missing profile {}", step.profile_id))?;

            if let Some(reason) = self.filter_step(profile, step.daily_budget_usd, request, now) {
                skipped.push(reason);
                continue;
            }

            let estimated_cost_usd = profile.cost.estimate_usd(
                request.estimated_input_tokens,
                request.estimated_output_tokens,
            );
            let escalation = if step_index == 0 {
                if profile.is_no_model() {
                    EscalationReason::NoModelPolicy
                } else {
                    EscalationReason::Preferred
                }
            } else {
                skipped
                    .last()
                    .cloned()
                    .unwrap_or(EscalationReason::Preferred)
            };

            return Ok(RouteDecision {
                task_class: request.task_class,
                profile_id: profile.id.clone(),
                provider: profile.provider,
                model: profile.model.clone(),
                daily_budget_usd: step.daily_budget_usd,
                estimated_cost_usd,
                timeout_ms: profile.timeout_ms,
                max_concurrency: profile.max_concurrency,
                api_key_env: profile.api_key_env.clone(),
                base_url: profile.base_url.clone(),
                no_model: profile.is_no_model(),
                step_index,
                escalation,
                skipped,
            });
        }

        Ok(RouteDecision {
            task_class: request.task_class,
            profile_id: "no_model".into(),
            provider: ProviderKind::None,
            model: "none".into(),
            daily_budget_usd: 0.0,
            estimated_cost_usd: 0.0,
            timeout_ms: 1,
            max_concurrency: 1024,
            api_key_env: String::new(),
            base_url: String::new(),
            no_model: true,
            step_index: 0,
            escalation: EscalationReason::NoEligibleRoute,
            skipped,
        })
    }

    fn filter_step(
        &self,
        profile: &ModelProfile,
        daily_budget_usd: f64,
        request: &InferenceRequest,
        now: SystemTime,
    ) -> Option<EscalationReason> {
        for cap in &request.required_capabilities {
            if !profile.has_capability(*cap) && !profile.is_no_model() {
                return Some(EscalationReason::MissingCapability {
                    profile_id: profile.id.clone(),
                    capability: format!("{cap:?}").to_ascii_lowercase(),
                });
            }
        }

        let needed = request
            .estimated_input_tokens
            .saturating_add(request.estimated_output_tokens);
        if !profile.is_no_model() && needed > profile.context_tokens {
            return Some(EscalationReason::ContextTooSmall {
                profile_id: profile.id.clone(),
                needed,
                available: profile.context_tokens,
            });
        }

        if (profile.cost.input_per_mtok > 0.0 || profile.cost.output_per_mtok > 0.0)
            && self.ledger.has_unreported_usage_today(&profile.id, now)
        {
            return Some(EscalationReason::UsageUnreported { profile_id: profile.id.clone() });
        }
        let estimated = profile.cost.estimate_usd(
            request.estimated_input_tokens,
            request.estimated_output_tokens,
        );
        if estimated > 0.0 {
            if daily_budget_usd <= 0.0 {
                return Some(EscalationReason::PaidRouteBlocked {
                    profile_id: profile.id.clone(),
                });
            }
            let spent = self.ledger.spent_usd_today(&profile.id, now);
            if spent + estimated > daily_budget_usd + f64::EPSILON {
                return Some(EscalationReason::BudgetExhausted {
                    profile_id: profile.id.clone(),
                });
            }
        }

        None
    }

    /// Build a provider-specific request representation (still offline).
    pub fn materialize(
        &self,
        decision: &RouteDecision,
        request: &InferenceRequest,
    ) -> Result<ProviderRequest> {
        if decision.no_model || decision.provider == ProviderKind::None {
            let echo = request
                .messages
                .last()
                .map(|m| m.content.clone())
                .unwrap_or_default();
            return Ok(ProviderRequest::NoModel {
                reason: decision.escalation.as_code().into(),
                echo,
            });
        }

        match decision.provider {
            ProviderKind::OpenAiCompatible => {
                let mut messages = Vec::new();
                if !request.system.trim().is_empty() {
                    messages.push(OpenAiChatMessage {
                        role: "system".into(),
                        content: request.system.clone(),
                    });
                }
                for msg in &request.messages {
                    messages.push(OpenAiChatMessage {
                        role: msg.role.clone(),
                        content: msg.content.clone(),
                    });
                }
                let base = if decision.base_url.is_empty() {
                    "http://127.0.0.1:11434/v1".to_string()
                } else {
                    decision.base_url.trim_end_matches('/').to_string()
                };
                Ok(ProviderRequest::OpenAiCompatible {
                    url: format!("{base}/chat/completions"),
                    api_key_env: decision.api_key_env.clone(),
                    timeout_ms: decision.timeout_ms,
                    body: OpenAiChatRequest {
                        model: decision.model.clone(),
                        messages,
                        max_tokens: request.max_tokens,
                        temperature: request.temperature,
                    },
                })
            }
            ProviderKind::Anthropic => {
                let messages = request
                    .messages
                    .iter()
                    .map(|m| AnthropicMessage {
                        role: m.role.clone(),
                        content: m.content.clone(),
                    })
                    .collect();
                let base = if decision.base_url.is_empty() {
                    "https://api.anthropic.com".to_string()
                } else {
                    decision.base_url.trim_end_matches('/').to_string()
                };
                Ok(ProviderRequest::Anthropic {
                    url: format!("{base}/v1/messages"),
                    api_key_env: decision.api_key_env.clone(),
                    timeout_ms: decision.timeout_ms,
                    body: AnthropicMessagesRequest {
                        model: decision.model.clone(),
                        max_tokens: request.max_tokens.unwrap_or(1024),
                        system: if request.system.trim().is_empty() {
                            None
                        } else {
                            Some(request.system.clone())
                        },
                        messages,
                        temperature: request.temperature,
                    },
                })
            }
            ProviderKind::None => Ok(ProviderRequest::NoModel {
                reason: "no_model_policy".into(),
                echo: String::new(),
            }),
        }
    }

    /// Select, materialize, execute via transport, and record usage.
    /// Selection itself does not call the transport.
    pub fn complete<T: InferenceTransport + ?Sized>(
        &self,
        request: &InferenceRequest,
        transport: &T,
        now: SystemTime,
    ) -> Result<(RouteDecision, ProviderResponse)> {
        self.complete_with_budget(request, transport, now, f64::INFINITY)
    }

    pub fn complete_with_budget<T: InferenceTransport + ?Sized>(
        &self,
        request: &InferenceRequest,
        transport: &T,
        now: SystemTime,
        global_daily_budget_usd: f64,
    ) -> Result<(RouteDecision, ProviderResponse)> {
        let mut start_step = 0;
        let mut skipped = Vec::new();
        loop {
            let decision = self.select_from(request, now, start_step, skipped.clone())?;
            if !decision.no_model
                && self.ledger.total_usd_today(now) + decision.estimated_cost_usd
                    > global_daily_budget_usd
            {
                skipped = decision.skipped.clone();
                skipped.push(EscalationReason::BudgetExhausted {
                    profile_id: decision.profile_id.clone(),
                });
                start_step = decision.step_index + 1;
                continue;
            }
            let provider_request = self.materialize(&decision, request)?;
            match transport.execute(&provider_request) {
                Ok(response) => {
                    let profile = self.config.profile(&decision.profile_id);
                    let (input_path, output_path) = match decision.provider {
                        ProviderKind::Anthropic => ("/usage/input_tokens", "/usage/output_tokens"),
                        _ => ("/usage/prompt_tokens", "/usage/completion_tokens"),
                    };
                    let read_count = |path: &str| response.raw.pointer(path)
                        .and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
                    let input = read_count(input_path);
                    let output = read_count(output_path);
                    let usage_estimated = !decision.no_model && (input.is_none() || output.is_none());
                    let input_tokens = if decision.no_model { 0 }
                        else { input.unwrap_or(request.estimated_input_tokens) };
                    let output_tokens = if decision.no_model { 0 } else {
                        output.unwrap_or(request.max_tokens.unwrap_or(request.estimated_output_tokens))
                    };
                    let cost = profile.map(|p| p.cost.estimate_usd(input_tokens, output_tokens))
                        .unwrap_or(0.0);
                    self.ledger.record_with_estimate(
                        request.task_class, &decision.profile_id, input_tokens,
                        output_tokens, cost, now, usage_estimated,
                    );
                    return Ok((decision, response));
                }
                Err(error) if !decision.no_model => {
                    skipped = decision.skipped.clone();
                    skipped.push(EscalationReason::ProviderFailed {
                        profile_id: decision.profile_id.clone(),
                    });
                    start_step = decision.step_index + 1;
                    let route_len = self
                        .config
                        .route_for(request.task_class)
                        .map(|route| route.steps.len())
                        .unwrap_or(0);
                    if start_step >= route_len {
                        return Err(error.context(format!(
                            "all configured providers failed for {}",
                            request.task_class
                        )));
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Deterministic no-model completion used for mechanical work and
    /// headless operation.
    pub fn complete_no_model(&self, request: &InferenceRequest) -> Result<ProviderResponse> {
        let decision = RouteDecision {
            task_class: request.task_class,
            profile_id: "no_model".into(),
            provider: ProviderKind::None,
            model: "none".into(),
            daily_budget_usd: 0.0,
            estimated_cost_usd: 0.0,
            timeout_ms: 1,
            max_concurrency: 1024,
            api_key_env: String::new(),
            base_url: String::new(),
            no_model: true,
            step_index: 0,
            escalation: EscalationReason::NoModelPolicy,
            skipped: Vec::new(),
        };
        let provider_request = self.materialize(&decision, request)?;
        NoopTransport.execute(&provider_request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_config::{ModelCost, ModelRoutingConfig, RouteStep, TaskRoute};

    fn fixed_now() -> SystemTime {
        UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn single_steve_identity() {
        let router = ModelRouter::with_defaults().unwrap();
        assert_eq!(router.identity_name(), "Steve Jobs");
        assert_eq!(router.config().identity.agent_id, "ferricula-stevejobs");
    }

    #[test]
    fn mechanical_is_deterministic_no_model() {
        let router = ModelRouter::with_defaults().unwrap();
        let req = InferenceRequest::new(TaskClass::Mechanical, "noop");
        let decision = router.select(&req, fixed_now()).unwrap();
        assert!(decision.no_model);
        assert_eq!(decision.profile_id, "no_model");
        assert_eq!(decision.escalation, EscalationReason::NoModelPolicy);

        let resp = router.complete_no_model(&req).unwrap();
        assert!(resp.text.contains("no-model"));
        assert!(resp.text.contains("noop"));
    }

    #[test]
    fn ordered_fallback_prefers_local_then_escalates() {
        let router = ModelRouter::with_defaults().unwrap();
        let req = InferenceRequest::new(TaskClass::Social, "hello");
        let decision = router.select(&req, fixed_now()).unwrap();
        assert_eq!(decision.profile_id, "local_ollama");
        assert_eq!(decision.escalation, EscalationReason::Preferred);
        assert_eq!(decision.step_index, 0);
    }

    #[test]
    fn capability_filter_skips_local_for_tools() {
        let router = ModelRouter::with_defaults().unwrap();
        let req =
            InferenceRequest::new(TaskClass::Social, "use tools").require(ModelCapability::Tools);
        let decision = router.select(&req, fixed_now()).unwrap();
        assert_eq!(decision.profile_id, "anthropic_haiku");
        assert!(!decision.skipped.is_empty());
        assert!(matches!(
            decision.skipped[0],
            EscalationReason::MissingCapability { .. }
        ));
    }

    #[test]
    fn context_filter_skips_small_windows() {
        let router = ModelRouter::with_defaults().unwrap();
        let req = InferenceRequest::new(TaskClass::Scan, "huge").with_tokens(50_000, 1_000);
        let decision = router.select(&req, fixed_now()).unwrap();
        // local_ollama is 8192 context; falls through to no_model
        assert!(decision.no_model || decision.profile_id != "local_ollama");
        assert!(
            decision
                .skipped
                .iter()
                .any(|s| matches!(s, EscalationReason::ContextTooSmall { .. }))
        );
    }

    #[test]
    fn daily_budget_blocks_paid_route() {
        let router = ModelRouter::with_defaults().unwrap();
        // Burn haiku budget.
        router.ledger.record(
            TaskClass::Code,
            "anthropic_haiku",
            1_000_000,
            1_000_000,
            2.0,
            fixed_now(),
        );
        let req = InferenceRequest::new(TaskClass::Code, "impl")
            .require(ModelCapability::Code)
            .with_tokens(10_000, 2_000);
        let decision = router.select(&req, fixed_now()).unwrap();
        assert_eq!(decision.profile_id, "anthropic_sonnet");
        assert!(
            decision
                .skipped
                .iter()
                .any(|s| matches!(s, EscalationReason::BudgetExhausted { .. }))
        );
    }

    #[test]
    fn concurrency_and_timeout_metadata_present() {
        let router = ModelRouter::with_defaults().unwrap();
        let req = InferenceRequest::new(TaskClass::Deliberate, "think");
        let decision = router.select(&req, fixed_now()).unwrap();
        assert!(decision.max_concurrency >= 1);
        assert!(decision.timeout_ms >= 1);
    }

    #[test]
    fn openai_and_anthropic_request_shapes() {
        let router = ModelRouter::with_defaults().unwrap();

        let local = router
            .select(
                &InferenceRequest::new(TaskClass::Scan, "scan me"),
                fixed_now(),
            )
            .unwrap();
        let openai = router
            .materialize(&local, &InferenceRequest::new(TaskClass::Scan, "scan me"))
            .unwrap();
        match openai {
            ProviderRequest::OpenAiCompatible {
                url,
                api_key_env,
                body,
                ..
            } => {
                assert!(url.ends_with("/chat/completions"));
                assert!(api_key_env.is_empty() || !api_key_env.contains("sk-"));
                assert_eq!(body.model, "gemma4:e2b");
                assert_eq!(body.messages.last().unwrap().content, "scan me");
            }
            other => panic!("expected openai request, got {other:?}"),
        }

        let tools =
            InferenceRequest::new(TaskClass::Code, "write rust").require(ModelCapability::Code);
        let decision = router.select(&tools, fixed_now()).unwrap();
        let anthropic = router.materialize(&decision, &tools).unwrap();
        match anthropic {
            ProviderRequest::Anthropic {
                url,
                api_key_env,
                body,
                ..
            } => {
                assert!(url.ends_with("/v1/messages"));
                assert_eq!(api_key_env, "ANTHROPIC_API_KEY");
                assert!(!api_key_env.contains("sk-"));
                assert!(
                    body.model.contains("claude")
                        || body.model.contains("haiku")
                        || body.model.contains("sonnet")
                );
                assert_eq!(body.messages[0].content, "write rust");
            }
            other => panic!("expected anthropic request, got {other:?}"),
        }
    }

    struct LocalFailure;

    impl InferenceTransport for LocalFailure {
        fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
            match request {
                ProviderRequest::OpenAiCompatible { .. } => bail!("local unavailable"),
                ProviderRequest::Anthropic { body, .. } => Ok(ProviderResponse {
                    text: format!("fallback:{}", body.model),
                    input_tokens: 10,
                    output_tokens: 5,
                    raw: json!({ "fallback": true }),
                }),
                ProviderRequest::NoModel { .. } => NoopTransport.execute(request),
            }
        }
    }

    #[test]
    fn provider_failure_uses_ordered_fallback() {
        let router = ModelRouter::with_defaults().unwrap();
        let request = InferenceRequest::new(TaskClass::Social, "hello");
        let (decision, response) = router
            .complete(&request, &LocalFailure, fixed_now())
            .unwrap();
        assert_eq!(decision.profile_id, "anthropic_haiku");
        assert!(
            decision
                .skipped
                .iter()
                .any(|reason| matches!(reason, EscalationReason::ProviderFailed { .. }))
        );
        assert!(response.text.starts_with("fallback:"));
    }

    #[test]
    fn selection_makes_no_transport_calls() {
        let router = ModelRouter::with_defaults().unwrap();
        let transport = RecordingTransport::default();
        let req = InferenceRequest::new(TaskClass::Social, "hi");
        let _ = router.select(&req, fixed_now()).unwrap();
        assert!(transport.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn complete_records_usage_via_transport() {
        let router = ModelRouter::with_defaults().unwrap();
        let transport = RecordingTransport::default();
        let req = InferenceRequest::new(TaskClass::Scan, "scan").with_tokens(100, 50);
        let (decision, response) = router.complete(&req, &transport, fixed_now()).unwrap();
        assert_eq!(decision.profile_id, "local_ollama");
        assert!(response.text.contains("mock-openai") || response.text.contains("no-model"));
        assert_eq!(router.ledger.entries().len(), 1);
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn noop_transport_serves_no_model_only() {
        let router = ModelRouter::with_defaults().unwrap();
        let req = InferenceRequest::new(TaskClass::Mechanical, "tick");
        let (decision, response) = router.complete(&req, &NoopTransport, fixed_now()).unwrap();
        assert!(decision.no_model);
        assert!(response.text.contains("tick"));
    }

    #[test]
    fn api_keys_are_env_names_not_values() {
        let router = ModelRouter::with_defaults().unwrap();
        for profile in &router.config().profiles {
            assert!(
                !profile.api_key_env.starts_with("sk-"),
                "profile {} embeds secret",
                profile.id
            );
            if !profile.api_key_env.is_empty() {
                assert!(
                    profile
                        .api_key_env
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                    "bad env name {}",
                    profile.api_key_env
                );
            }
        }
    }

    #[test]
    fn broken_config_rejected_on_construct() {
        let mut config = ModelRoutingConfig::steve_safe_defaults();
        config.routes.push(TaskRoute {
            task_class: TaskClass::Scan,
            steps: vec![RouteStep {
                profile_id: "ghost".into(),
                daily_budget_usd: 1.0,
            }],
        });
        // duplicate Scan route with bad profile — validate should fail either way
        config.routes.pop();
        config.routes[1].steps[0].profile_id = "ghost".into();
        assert!(ModelRouter::new(config).is_err());
    }

    #[test]
    fn utc_day_is_stable() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(86_400), "1970-01-02");
    }

    #[test]
    fn free_cost_estimate_is_zero() {
        assert_eq!(ModelCost::FREE.estimate_usd(999, 999), 0.0);
    }
}
