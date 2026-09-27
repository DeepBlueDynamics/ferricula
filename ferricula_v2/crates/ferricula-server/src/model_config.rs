//! Declarative model-routing configuration for the single Steve identity.
//!
//! Profiles and routes are pure data: API material is referenced only by
//! environment-variable *name*, never as inline secrets. Validation rejects
//! broken routes and secret-shaped strings before any network work happens.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Canonical Steve identity for this runtime. One agent, many replaceable
/// providers underneath.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteveIdentity {
    pub agent_id: String,
    pub name: String,
}

impl SteveIdentity {
    pub fn steve_jobs() -> Self {
        Self {
            agent_id: "ferricula-stevejobs".into(),
            name: "Steve Jobs".into(),
        }
    }
}

impl Default for SteveIdentity {
    fn default() -> Self {
        Self::steve_jobs()
    }
}

/// Work classes that drive model selection. Mechanical work must never need a
/// frontier model; deliberate/code may escalate when budgets allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskClass {
    Mechanical,
    Scan,
    Summarize,
    Social,
    Deliberate,
    Code,
}

impl TaskClass {
    pub const ALL: [TaskClass; 6] = [
        TaskClass::Mechanical,
        TaskClass::Scan,
        TaskClass::Summarize,
        TaskClass::Social,
        TaskClass::Deliberate,
        TaskClass::Code,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskClass::Mechanical => "mechanical",
            TaskClass::Scan => "scan",
            TaskClass::Summarize => "summarize",
            TaskClass::Social => "social",
            TaskClass::Deliberate => "deliberate",
            TaskClass::Code => "code",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mechanical" => Ok(Self::Mechanical),
            "scan" => Ok(Self::Scan),
            "summarize" | "summary" => Ok(Self::Summarize),
            "social" => Ok(Self::Social),
            "deliberate" | "deliberative" => Ok(Self::Deliberate),
            "code" => Ok(Self::Code),
            other => bail!("unknown task class {other:?}"),
        }
    }
}

impl fmt::Display for TaskClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Wire protocol family used when materializing HTTP request bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// OpenAI chat-completions shape (also covers Ollama and compatible proxies).
    OpenAiCompatible,
    /// Anthropic Messages API.
    Anthropic,
    /// Explicit no-network provider; used by the deterministic no-model route.
    None,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => "openai_compatible",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::None => "none",
        }
    }
}

/// Capability bits a profile advertises for filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    Chat,
    Tools,
    Vision,
    Code,
    LongContext,
    Cheap,
    Local,
    Frontier,
    /// Operator explicitly permits this profile to receive private Ferricula
    /// memory context. Local profiles may enable it by default; cloud profiles
    /// require an intentional configuration change.
    PrivateContext,
}

impl ModelCapability {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "chat" => Ok(Self::Chat),
            "tools" | "tool_use" => Ok(Self::Tools),
            "vision" | "image" => Ok(Self::Vision),
            "code" => Ok(Self::Code),
            "long_context" | "long-context" => Ok(Self::LongContext),
            "cheap" => Ok(Self::Cheap),
            "local" => Ok(Self::Local),
            "frontier" => Ok(Self::Frontier),
            "private_context" | "private-context" => Ok(Self::PrivateContext),
            other => bail!("unknown model capability {other:?}"),
        }
    }
}

/// Per-token pricing used for budget estimates (USD).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelCost {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

impl ModelCost {
    pub const FREE: Self = Self {
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
    };

    /// Estimate USD cost from token counts.
    pub fn estimate_usd(self, input_tokens: u32, output_tokens: u32) -> f64 {
        (f64::from(input_tokens) / 1_000_000.0) * self.input_per_mtok
            + (f64::from(output_tokens) / 1_000_000.0) * self.output_per_mtok
    }
}

/// Named, replaceable model profile. Secrets are env-var *names* only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelProfile {
    pub id: String,
    pub provider: ProviderKind,
    /// Provider-specific model identifier (e.g. `gemma4:e2b`, `claude-haiku-4-5`).
    pub model: String,
    /// Base URL for OpenAI-compatible endpoints; Anthropic uses its default when empty.
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub capabilities: Vec<ModelCapability>,
    pub cost: ModelCost,
    /// Maximum context window in tokens.
    pub context_tokens: u32,
    /// Soft concurrency limit advertised for scheduling metadata.
    pub max_concurrency: u32,
    /// Request timeout in milliseconds.
    pub timeout_ms: u64,
    /// Environment variable holding the API key. Empty for local/none providers.
    #[serde(default)]
    pub api_key_env: String,
}

impl ModelProfile {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    pub fn has_capability(&self, cap: ModelCapability) -> bool {
        self.capabilities.contains(&cap)
    }

    pub fn is_no_model(&self) -> bool {
        self.provider == ProviderKind::None || self.model.eq_ignore_ascii_case("none")
    }
}

/// One ordered fallback entry for a task class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteStep {
    /// Profile id from [`ModelRoutingConfig::profiles`].
    pub profile_id: String,
    /// Hard daily spend ceiling for this step (USD). Zero means free-only.
    pub daily_budget_usd: f64,
}

/// Ordered fallback chain for one task class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRoute {
    pub task_class: TaskClass,
    /// Preferred providers first; later entries are fallbacks / escalations.
    pub steps: Vec<RouteStep>,
}

/// Complete routing configuration for the single Steve identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelRoutingConfig {
    #[serde(default)]
    pub identity: SteveIdentity,
    pub profiles: Vec<ModelProfile>,
    pub routes: Vec<TaskRoute>,
}

impl Default for ModelRoutingConfig {
    fn default() -> Self {
        Self::steve_safe_defaults()
    }
}

impl ModelRoutingConfig {
    /// Safe defaults: local Ollama for ordinary work, optional Anthropic
    /// escalation for deliberate/code, and a deterministic no-model route for
    /// mechanical tasks.
    pub fn steve_safe_defaults() -> Self {
        Self {
            identity: SteveIdentity::steve_jobs(),
            profiles: vec![
                ModelProfile {
                    id: "no_model".into(),
                    provider: ProviderKind::None,
                    model: "none".into(),
                    base_url: String::new(),
                    capabilities: vec![ModelCapability::Cheap, ModelCapability::Local],
                    cost: ModelCost::FREE,
                    context_tokens: u32::MAX,
                    max_concurrency: 1024,
                    timeout_ms: 1,
                    api_key_env: String::new(),
                },
                ModelProfile {
                    id: "local_ollama".into(),
                    provider: ProviderKind::OpenAiCompatible,
                    model: "gemma4:e2b".into(),
                    base_url: "http://127.0.0.1:11434/v1".into(),
                    capabilities: vec![
                        ModelCapability::Chat,
                        ModelCapability::Cheap,
                        ModelCapability::Local,
                        ModelCapability::PrivateContext,
                    ],
                    cost: ModelCost::FREE,
                    context_tokens: 8_192,
                    max_concurrency: 2,
                    timeout_ms: 120_000,
                    api_key_env: String::new(),
                },
                ModelProfile {
                    id: "anthropic_haiku".into(),
                    provider: ProviderKind::Anthropic,
                    model: "claude-haiku-4-5".into(),
                    base_url: "https://api.anthropic.com".into(),
                    capabilities: vec![
                        ModelCapability::Chat,
                        ModelCapability::Tools,
                        ModelCapability::Code,
                        ModelCapability::Cheap,
                    ],
                    cost: ModelCost {
                        input_per_mtok: 1.0,
                        output_per_mtok: 5.0,
                    },
                    context_tokens: 200_000,
                    max_concurrency: 4,
                    timeout_ms: 90_000,
                    api_key_env: "ANTHROPIC_API_KEY".into(),
                },
                ModelProfile {
                    id: "anthropic_sonnet".into(),
                    provider: ProviderKind::Anthropic,
                    model: "claude-sonnet-4-6".into(),
                    base_url: "https://api.anthropic.com".into(),
                    capabilities: vec![
                        ModelCapability::Chat,
                        ModelCapability::Tools,
                        ModelCapability::Code,
                        ModelCapability::LongContext,
                        ModelCapability::Frontier,
                    ],
                    cost: ModelCost {
                        input_per_mtok: 3.0,
                        output_per_mtok: 15.0,
                    },
                    context_tokens: 200_000,
                    max_concurrency: 2,
                    timeout_ms: 120_000,
                    api_key_env: "ANTHROPIC_API_KEY".into(),
                },
            ],
            routes: vec![
                TaskRoute {
                    task_class: TaskClass::Mechanical,
                    steps: vec![RouteStep {
                        profile_id: "no_model".into(),
                        daily_budget_usd: 0.0,
                    }],
                },
                TaskRoute {
                    task_class: TaskClass::Scan,
                    steps: vec![
                        RouteStep {
                            profile_id: "local_ollama".into(),
                            daily_budget_usd: 0.0,
                        },
                        RouteStep {
                            profile_id: "no_model".into(),
                            daily_budget_usd: 0.0,
                        },
                    ],
                },
                TaskRoute {
                    task_class: TaskClass::Summarize,
                    steps: vec![
                        RouteStep {
                            profile_id: "local_ollama".into(),
                            daily_budget_usd: 0.0,
                        },
                        RouteStep {
                            profile_id: "anthropic_haiku".into(),
                            daily_budget_usd: 1.0,
                        },
                        RouteStep {
                            profile_id: "no_model".into(),
                            daily_budget_usd: 0.0,
                        },
                    ],
                },
                TaskRoute {
                    task_class: TaskClass::Social,
                    steps: vec![
                        RouteStep {
                            profile_id: "local_ollama".into(),
                            daily_budget_usd: 0.0,
                        },
                        RouteStep {
                            profile_id: "anthropic_haiku".into(),
                            daily_budget_usd: 1.5,
                        },
                        RouteStep {
                            profile_id: "no_model".into(),
                            daily_budget_usd: 0.0,
                        },
                    ],
                },
                TaskRoute {
                    task_class: TaskClass::Deliberate,
                    steps: vec![
                        RouteStep {
                            profile_id: "local_ollama".into(),
                            daily_budget_usd: 0.0,
                        },
                        RouteStep {
                            profile_id: "anthropic_haiku".into(),
                            daily_budget_usd: 2.0,
                        },
                        RouteStep {
                            profile_id: "anthropic_sonnet".into(),
                            daily_budget_usd: 3.0,
                        },
                        RouteStep {
                            profile_id: "no_model".into(),
                            daily_budget_usd: 0.0,
                        },
                    ],
                },
                TaskRoute {
                    task_class: TaskClass::Code,
                    steps: vec![
                        RouteStep {
                            profile_id: "anthropic_haiku".into(),
                            daily_budget_usd: 2.0,
                        },
                        RouteStep {
                            profile_id: "anthropic_sonnet".into(),
                            daily_budget_usd: 5.0,
                        },
                        RouteStep {
                            profile_id: "no_model".into(),
                            daily_budget_usd: 0.0,
                        },
                    ],
                },
            ],
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let source = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            serde_json::from_str(&source)
                .with_context(|| format!("failed to parse json {}", path.display()))?
        } else {
            toml::from_str(&source)
                .with_context(|| format!("failed to parse toml {}", path.display()))?
        };
        config.validate()?;
        Ok(config)
    }

    pub fn profile(&self, id: &str) -> Option<&ModelProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn route_for(&self, task_class: TaskClass) -> Option<&TaskRoute> {
        self.routes.iter().find(|r| r.task_class == task_class)
    }

    pub fn profile_map(&self) -> BTreeMap<&str, &ModelProfile> {
        self.profiles.iter().map(|p| (p.id.as_str(), p)).collect()
    }

    /// Reject inline secrets, empty/broken routes, duplicate ids, and
    /// dangling profile references.
    pub fn validate(&self) -> Result<()> {
        if self.identity.agent_id.trim().is_empty() {
            bail!("identity.agent_id must be non-empty");
        }
        if self.identity.name.trim().is_empty() {
            bail!("identity.name must be non-empty");
        }

        let mut seen_profiles = BTreeSet::new();
        for profile in &self.profiles {
            if profile.id.trim().is_empty() {
                bail!("profile id must be non-empty");
            }
            if !seen_profiles.insert(profile.id.as_str()) {
                bail!("duplicate profile id {}", profile.id);
            }
            if profile.model.trim().is_empty() {
                bail!("profile {} model must be non-empty", profile.id);
            }
            if profile.context_tokens == 0 {
                bail!("profile {} context_tokens must be positive", profile.id);
            }
            if profile.max_concurrency == 0 {
                bail!("profile {} max_concurrency must be positive", profile.id);
            }
            if profile.timeout_ms == 0 && !profile.is_no_model() {
                bail!("profile {} timeout_ms must be positive", profile.id);
            }
            if profile.cost.input_per_mtok < 0.0 || profile.cost.output_per_mtok < 0.0 {
                bail!("profile {} cost values must be non-negative", profile.id);
            }
            validate_api_key_env(&profile.id, &profile.api_key_env, profile.provider)?;
            reject_inline_secret_fields(profile)?;
        }

        let mut seen_classes = BTreeSet::new();
        for route in &self.routes {
            if !seen_classes.insert(route.task_class) {
                bail!("duplicate route for task class {}", route.task_class);
            }
            if route.steps.is_empty() {
                bail!("route for {} has no steps", route.task_class);
            }
            for (idx, step) in route.steps.iter().enumerate() {
                if step.profile_id.trim().is_empty() {
                    bail!("route {} step {idx} has empty profile_id", route.task_class);
                }
                if self.profile(&step.profile_id).is_none() {
                    bail!(
                        "route {} step {idx} references unknown profile {}",
                        route.task_class,
                        step.profile_id
                    );
                }
                if !step.daily_budget_usd.is_finite() || step.daily_budget_usd < 0.0 {
                    bail!(
                        "route {} step {idx} daily_budget_usd must be finite and non-negative",
                        route.task_class
                    );
                }
            }
        }

        for task_class in TaskClass::ALL {
            if self.route_for(task_class).is_none() {
                bail!("missing route for task class {task_class}");
            }
        }

        for route in &self.routes {
            let has_no_model = route.steps.iter().any(|step| {
                self.profile(&step.profile_id)
                    .is_some_and(ModelProfile::is_no_model)
            });
            if !has_no_model {
                bail!(
                    "route for {} must include an explicit no-model fallback",
                    route.task_class
                );
            }
        }

        let mechanical = self
            .route_for(TaskClass::Mechanical)
            .expect("all task classes checked above");
        if mechanical.steps.iter().any(|step| {
            self.profile(&step.profile_id)
                .is_some_and(|profile| !profile.is_no_model())
        }) {
            bail!("mechanical route must remain deterministic and no-model");
        }

        Ok(())
    }
}

fn validate_api_key_env(profile_id: &str, env_name: &str, provider: ProviderKind) -> Result<()> {
    let trimmed = env_name.trim();
    if trimmed.is_empty() {
        match provider {
            ProviderKind::None | ProviderKind::OpenAiCompatible => return Ok(()),
            ProviderKind::Anthropic => {
                bail!("profile {profile_id} anthropic provider requires api_key_env");
            }
        }
    }
    if looks_like_inline_secret(trimmed) {
        bail!(
            "profile {profile_id} api_key_env must be an environment-variable name, never a secret"
        );
    }
    if !is_valid_env_name(trimmed) {
        bail!(
            "profile {profile_id} api_key_env {trimmed:?} is not a valid environment-variable name"
        );
    }
    Ok(())
}

fn reject_inline_secret_fields(profile: &ModelProfile) -> Result<()> {
    for (field, value) in [
        ("id", profile.id.as_str()),
        ("model", profile.model.as_str()),
        ("base_url", profile.base_url.as_str()),
        ("api_key_env", profile.api_key_env.as_str()),
    ] {
        if field != "api_key_env" && looks_like_inline_secret(value) {
            bail!(
                "profile {} field {field} looks like an inline secret; use api_key_env instead",
                profile.id
            );
        }
        if value.contains("sk-") && value.len() > 12 && field != "model" {
            // Allow model names that happen to contain short tokens; reject
            // long sk- style material elsewhere.
            if value.chars().filter(|c| c.is_ascii_alphanumeric()).count() > 20 {
                bail!(
                    "profile {} field {field} appears to embed an API key",
                    profile.id
                );
            }
        }
    }
    Ok(())
}

/// True when a string is clearly a secret value rather than an env-var name.
pub fn looks_like_inline_secret(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    if v.contains('=') {
        return true;
    }
    if v.starts_with("sk-") || v.starts_with("sk-ant-") {
        return true;
    }
    if v.starts_with("ahp_") || v.starts_with("ahp-") {
        return true;
    }
    if v.starts_with("Bearer ") {
        return true;
    }
    // Long opaque tokens without underscores (env names almost always use _).
    if v.len() >= 32
        && !v.contains('_')
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return true;
    }
    false
}

fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_cover_all_task_classes() {
        let config = ModelRoutingConfig::steve_safe_defaults();
        config.validate().unwrap();
        assert_eq!(config.identity.agent_id, "ferricula-stevejobs");
        assert_eq!(config.identity.name, "Steve Jobs");
        for class in TaskClass::ALL {
            assert!(
                config.route_for(class).is_some(),
                "missing route for {class}"
            );
        }
        assert!(config.profile("no_model").unwrap().is_no_model());
    }

    #[test]
    fn rejects_inline_api_key_material() {
        let mut config = ModelRoutingConfig::steve_safe_defaults();
        config.profiles[2].api_key_env = "sk-test-not-a-real-key-000000000000".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_ahp_token_as_env_name() {
        let mut config = ModelRoutingConfig::steve_safe_defaults();
        config.profiles[2].api_key_env = "ahp_not_a_variable".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_dangling_route_profile() {
        let mut config = ModelRoutingConfig::steve_safe_defaults();
        config.routes[0].steps[0].profile_id = "does_not_exist".into();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("unknown profile"), "{err}");
    }

    #[test]
    fn rejects_empty_route_steps() {
        let mut config = ModelRoutingConfig::steve_safe_defaults();
        config.routes[0].steps.clear();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_profile_ids() {
        let mut config = ModelRoutingConfig::steve_safe_defaults();
        let dup = config.profiles[0].clone();
        config.profiles.push(dup);
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_negative_budget() {
        let mut config = ModelRoutingConfig::steve_safe_defaults();
        config.routes[3].steps[1].daily_budget_usd = -1.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn task_class_parse_accepts_aliases() {
        assert_eq!(
            TaskClass::parse("deliberative").unwrap(),
            TaskClass::Deliberate
        );
        assert_eq!(TaskClass::parse("summary").unwrap(), TaskClass::Summarize);
    }

    #[test]
    fn cost_estimate_scales_with_tokens() {
        let cost = ModelCost {
            input_per_mtok: 1.0,
            output_per_mtok: 2.0,
        };
        let usd = cost.estimate_usd(1_000_000, 500_000);
        assert!((usd - 2.0).abs() < 1e-9);
    }

    #[test]
    fn serde_roundtrip_json() {
        let config = ModelRoutingConfig::steve_safe_defaults();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let back: ModelRoutingConfig = serde_json::from_str(&json).unwrap();
        back.validate().unwrap();
        assert_eq!(back.identity, config.identity);
        assert_eq!(back.profiles.len(), config.profiles.len());
        assert_eq!(back.routes.len(), config.routes.len());
    }

    #[test]
    fn env_name_validation() {
        assert!(is_valid_env_name("ANTHROPIC_API_KEY"));
        assert!(is_valid_env_name("_PRIVATE"));
        assert!(!is_valid_env_name("1BAD"));
        assert!(!is_valid_env_name("HAS-DASH"));
        assert!(looks_like_inline_secret("sk-abc123"));
        assert!(!looks_like_inline_secret("OPENAI_API_KEY"));
    }
}
