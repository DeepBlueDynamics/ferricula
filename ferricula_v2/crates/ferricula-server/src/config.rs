use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::autonomy::AutonomyPolicy;
use crate::memory_overlay::OverlayConfig;
use crate::mention_ingest::MentionIngestConfig;
use crate::model_config::{ModelCapability, ModelRoutingConfig};
use crate::sleep_cycle::SleepCyclePolicy;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub bind: SocketAddr,
    /// Identity expected in the mounted memory store; defaults preserve Steve deployments.
    pub expected_agent_id: String,
    pub memory_dir: PathBuf,
    pub state_dir: PathBuf,
    pub operator_token_env: String,
    pub require_operator_auth: bool,
    /// Profile ids explicitly authorized to receive private Ferricula memory.
    /// Applied after TOML parsing so the safe built-in profiles can be used
    /// without restating the entire routing table.
    pub private_context_profiles: Vec<String>,
    /// Convenience overrides for the built-in `local_ollama` profile.
    pub ollama_base_url: Option<String>,
    pub ollama_model: Option<String>,
    pub initial_mode: String,
    /// Generate an ephemeral briefing from scoped raw recovered memories before chat.
    /// Adds a separately budgeted model call; never writes the briefing to memory.
    pub curator_enabled: bool,
    pub schedule: ScheduleConfig,
    pub budgets: ActivityBudgets,
    pub nutnews: NutNewsConfig,
    pub models: ModelRoutingConfig,
    /// Event-driven autonomy machine (Phase One). Disabled by default so
    /// existing deployments keep the plain scheduler/worker behavior.
    pub autonomy: AutonomySection,
    /// Sleep/dream planning policy. Enabled but strictly read-only and
    /// private by the module defaults.
    pub sleep_cycle: SleepCyclePolicy,
    /// Nuts mention ingestion (identity + bounds). Disabled by default.
    pub mentions: MentionIngestSection,
    /// Append-only overlay for the recovered memory base. Disabled by
    /// default; the recovered base is immutable regardless.
    pub overlay: OverlaySection,
    /// Emotion/somatic state (Phase Two surface). Disabled by default.
    pub emotion: EmotionConfig,
    /// Advocate values-alignment reviews (Phase Three surface). Disabled by
    /// default; requires the overlay as its only write sink.
    pub advocate: AdvocateConfig,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8875".parse().expect("static socket address"),
            expected_agent_id: "ferricula-stevejobs".into(),
            memory_dir: PathBuf::from(".runtime/steve-jobs"),
            state_dir: PathBuf::from(".runtime/steve-runtime"),
            operator_token_env: "FERRICULA_OPERATOR_TOKEN".into(),
            require_operator_auth: true,
            private_context_profiles: Vec::new(),
            ollama_base_url: None,
            ollama_model: None,
            initial_mode: "asleep".into(),
            curator_enabled: false,
            schedule: ScheduleConfig::default(),
            budgets: ActivityBudgets::default(),
            nutnews: NutNewsConfig::default(),
            models: ModelRoutingConfig::default(),
            autonomy: AutonomySection::default(),
            sleep_cycle: SleepCyclePolicy::default(),
            mentions: MentionIngestSection::default(),
            overlay: OverlaySection::default(),
            emotion: EmotionConfig::default(),
            advocate: AdvocateConfig::default(),
        }
    }
}

impl RuntimeConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let source = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut config: Self = toml::from_str(&source)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        for profile_id in &config.private_context_profiles {
            let profile = config
                .models
                .profiles
                .iter_mut()
                .find(|profile| &profile.id == profile_id)
                .with_context(|| {
                    format!("private_context_profiles references unknown profile {profile_id}")
                })?;
            if !profile
                .capabilities
                .contains(&crate::model_config::ModelCapability::PrivateContext)
            {
                profile
                    .capabilities
                    .push(crate::model_config::ModelCapability::PrivateContext);
            }
        }
        if config.ollama_base_url.is_some() || config.ollama_model.is_some() {
            let profile = config
                .models
                .profiles
                .iter_mut()
                .find(|profile| profile.id == "local_ollama")
                .context("Ollama overrides require the built-in local_ollama profile")?;
            if let Some(base_url) = &config.ollama_base_url {
                profile.base_url = base_url.clone();
            }
            if let Some(model) = &config.ollama_model {
                profile.model = model.clone();
                if model.ends_with(":cloud")
                    && !config
                        .private_context_profiles
                        .iter()
                        .any(|id| id == "local_ollama")
                {
                    profile.capabilities.retain(|capability| {
                        *capability != crate::model_config::ModelCapability::PrivateContext
                    });
                }
            }
        }
        config
            .mentions
            .ingest
            .normalize_and_validate()
            .context("invalid [mentions] configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schedule.enabled && self.schedule.wakes_per_day == 0 {
            bail!("schedule.wakes_per_day must be positive when scheduling is enabled");
        }
        if self.schedule.wakes_per_day > 24 {
            bail!("schedule.wakes_per_day cannot exceed 24");
        }
        if self.budgets.max_threads_per_wake == 0 {
            bail!("budgets.max_threads_per_wake must be positive");
        }
        if self.nutnews.token_env.trim().is_empty() {
            bail!("nutnews.token_env must name an environment variable");
        }
        if self.nutnews.token_env.contains('=') || self.nutnews.token_env.starts_with("ahp_") {
            bail!("nutnews.token_env must contain an environment-variable name, never a token");
        }
        if self.require_operator_auth && self.operator_token_env.trim().is_empty() {
            bail!("operator_token_env must name an environment variable when auth is required");
        }
        if let Some(url) = &self.ollama_base_url {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                bail!("ollama_base_url must be an http(s) URL");
            }
            if url.contains('@') {
                bail!("ollama_base_url must not contain inline credentials");
            }
        }
        if !self.bind.ip().is_loopback() && !self.require_operator_auth {
            bail!("operator authentication is required when binding beyond loopback");
        }
        self.models.validate()?;
        if self.expected_agent_id.trim().is_empty()
            || self.models.identity.agent_id != self.expected_agent_id
        {
            bail!("models.identity.agent_id must match nonempty expected_agent_id");
        }

        // --- Phase One sections (all serde-defaulted, all safe when absent).
        if let Err(error) = self.autonomy.policy.validate() {
            bail!("invalid [autonomy] configuration: {error}");
        }
        self.sleep_cycle
            .validate()
            .context("invalid [sleep_cycle] configuration")?;
        // `validate` cannot mutate; normalization happens in `load`. Here a
        // clone proves the section is normalizable and within bounds.
        self.mentions
            .ingest
            .clone()
            .normalize_and_validate()
            .context("invalid [mentions] configuration")?;
        self.overlay
            .bounds
            .validate()
            .context("invalid [overlay] configuration")?;

        // Recovered memory is immutable: the overlay sink can never live
        // inside (or be) the read-only memory directory. Checked whether or
        // not the overlay is enabled, so a later flip cannot become unsafe.
        if self.overlay.enabled && self.overlay.path.as_os_str().is_empty() {
            bail!("overlay.path must be set when the overlay is enabled");
        }
        if !self.overlay.path.as_os_str().is_empty()
            && self.overlay.path.starts_with(&self.memory_dir)
        {
            bail!(
                "overlay.path {} is inside the read-only memory_dir {}; \
                 the recovered base is immutable and the overlay needs its own \
                 writable location (e.g. under state_dir)",
                self.overlay.path.display(),
                self.memory_dir.display()
            );
        }

        // Mention ingestion consumes the Nuts ledger and speaks as Steve's
        // inbound identity: it needs the connector on and the identities in
        // agreement, otherwise events would be keyed against the wrong actor
        // or instance.
        if self.mentions.enabled {
            if !self.nutnews.enabled {
                bail!("mentions.enabled requires nutnews.enabled (the ledger is the event source)");
            }
            let mention_identity = {
                let mut ingest = self.mentions.ingest.clone();
                ingest.normalize_and_validate()?;
                ingest
            };
            let nutnews_handle = self.nutnews.handle.trim().to_ascii_lowercase();
            if mention_identity.steve_handle != nutnews_handle {
                bail!(
                    "mentions.steve_handle {:?} does not match nutnews.handle {:?}",
                    mention_identity.steve_handle,
                    self.nutnews.handle
                );
            }
            if let Some(host) = url_host(&self.nutnews.public_url)
                && mention_identity.instance != host
            {
                bail!(
                    "mentions.instance {:?} does not match the nutnews.public_url host {:?}",
                    mention_identity.instance,
                    host
                );
            }
        }

        // Sharing private memory with sleep-cycle executors requires at
        // least one model profile the operator explicitly trusts with it.
        if self.sleep_cycle.privacy.share_private_memory
            && !self
                .models
                .profiles
                .iter()
                .any(|profile| profile.has_capability(ModelCapability::PrivateContext))
        {
            bail!(
                "sleep_cycle.privacy.share_private_memory requires a model profile \
                 with the private_context capability"
            );
        }

        // Advocate verdicts may only land on the overlay (never the semantic
        // index, per the roadmap): enabling reviews without an overlay sink
        // would leave them nowhere safe to go.
        if self.advocate.enabled {
            if !self.overlay.enabled {
                bail!("advocate.enabled requires overlay.enabled (verdicts write to the overlay)");
            }
            if self.advocate.reviews_per_day == 0 || self.advocate.reviews_per_day > 24 {
                bail!("advocate.reviews_per_day must be in 1..=24 when reviews are enabled");
            }
        }

        Ok(())
    }
}

/// Host portion of an http(s) URL, lowercased: scheme and path stripped.
fn url_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split('/').next()?.trim().to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

/// Master switch plus the pure autonomy policy (flattened: `[autonomy]`
/// carries `enabled` next to the policy knobs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonomySection {
    /// Off by default: Phase One wiring must be an explicit operator choice.
    pub enabled: bool,
    #[serde(flatten)]
    pub policy: AutonomyPolicy,
}

/// Master switch plus the mention-ingest identity and bounds (flattened).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MentionIngestSection {
    /// Off by default: mention consideration starts as an explicit choice.
    pub enabled: bool,
    #[serde(flatten)]
    pub ingest: MentionIngestConfig,
}

/// Master switch, document path, and bounds for the append-only overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySection {
    /// Off by default. Even when on, the recovered base stays immutable:
    /// the overlay is append-only and destructive actions are gated on
    /// operator approval inside the overlay module itself.
    pub enabled: bool,
    /// Overlay document location. MUST live outside memory_dir; the default
    /// sits on the writable runtime state volume.
    pub path: PathBuf,
    #[serde(flatten)]
    pub bounds: OverlayConfig,
}

impl Default for OverlaySection {
    fn default() -> Self {
        Self {
            enabled: false,
            path: PathBuf::from("/data/steve-runtime/overlay/overlay.json"),
            bounds: OverlayConfig::default(),
        }
    }
}

/// Emotion/somatic state surface (Phase Two). Enable-only for now; the
/// somatic parameters live with the cognition crate once wired.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EmotionConfig {
    pub enabled: bool,
}

/// Advocate values-alignment reviews (Phase Three). Reviews are advisory,
/// write only to the overlay, and never enter the semantic index.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvocateConfig {
    pub enabled: bool,
    /// Review cadence; validated to 1..=24 when enabled.
    pub reviews_per_day: u8,
}

impl Default for AdvocateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            reviews_per_day: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleConfig {
    pub enabled: bool,
    pub wakes_per_day: u8,
    pub jitter_minutes: u16,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            wakes_per_day: 4,
            jitter_minutes: 45,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ActivityBudgets {
    pub max_threads_per_wake: usize,
    pub max_autonomous_comments_per_day: u16,
    pub max_autonomous_submissions_per_day: u16,
    pub max_model_usd_per_day: f64,
}

impl Default for ActivityBudgets {
    fn default() -> Self {
        Self {
            max_threads_per_wake: 20,
            max_autonomous_comments_per_day: 5,
            max_autonomous_submissions_per_day: 1,
            max_model_usd_per_day: 5.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NutNewsConfig {
    pub enabled: bool,
    pub mcp_url: String,
    pub public_url: String,
    pub token_env: String,
    pub handle: String,
    pub allow_writes: bool,
}

impl Default for NutNewsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mcp_url: "https://news.nuts.services/mcp".into(),
            public_url: "https://news.nuts.services".into(),
            token_env: "NUTNEWS_STEVE_TOKEN".into(),
            handle: "steve".into(),
            allow_writes: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_only_table_preserves_default_model_routes() {
        let config: RuntimeConfig = toml::from_str(r#"
expected_agent_id = "ferricula-memory-bench"
[models.identity]
agent_id = "ferricula-memory-bench"
name = "Memory Bench"
"#).unwrap();
        config.validate().unwrap();
        assert_eq!(config.models.profiles, ModelRoutingConfig::default().profiles);
        assert_eq!(config.models.routes, ModelRoutingConfig::default().routes);
        assert_eq!(config.models.identity.name, "Memory Bench");
    }

    #[test]
    fn separate_identity_requires_explicit_matching_configuration() {
        let mut config = RuntimeConfig::default();
        config.models.identity.agent_id = "ferricula-memory-bench".into();
        assert!(config.validate().is_err());
        config.expected_agent_id = "ferricula-memory-bench".into();
        assert!(config.validate().is_ok());
        config.expected_agent_id.clear();
        assert!(config.validate().is_err());
    }

    #[test]
    fn defaults_are_safe() {
        let config = RuntimeConfig::default();
        config.validate().unwrap();
        assert_eq!(config.initial_mode, "asleep");
        assert!(!config.nutnews.allow_writes);
    }

    #[test]
    fn token_material_is_rejected() {
        let mut config = RuntimeConfig::default();
        config.nutnews.token_env = "ahp_not_a_variable".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn phase_one_sections_default_safe() {
        let config = RuntimeConfig::default();
        config.validate().unwrap();
        assert!(!config.autonomy.enabled);
        assert!(!config.mentions.enabled);
        assert!(!config.overlay.enabled);
        assert!(!config.emotion.enabled);
        assert!(!config.advocate.enabled);
        // Sleep cycle is on by module default but strictly read-only/private.
        assert!(!config.sleep_cycle.privacy.share_private_memory);
        assert_eq!(
            config.sleep_cycle.mutation,
            crate::sleep_cycle::MutationPolicy::ReadOnly
        );
    }

    #[test]
    fn legacy_config_without_new_tables_still_parses() {
        // The pre-Phase-One surface only: every new section must default.
        let legacy = r#"
            bind = "127.0.0.1:8875"
            memory_dir = "/data/steve-memory"
            state_dir = "/data/steve-runtime"
            initial_mode = "asleep"

            [schedule]
            enabled = true
            wakes_per_day = 4

            [nutnews]
            enabled = false
        "#;
        let config: RuntimeConfig = toml::from_str(legacy).unwrap();
        config.validate().unwrap();
        assert!(!config.autonomy.enabled);
        assert!(!config.overlay.enabled);
        assert!(!config.advocate.enabled);
    }

    #[test]
    fn new_tables_parse_with_flattened_module_fields() {
        let source = r#"
            [autonomy]
            enabled = true
            tick_secs = 30
            budget_gate = 0.8

            [sleep_cycle]
            cycles_per_day = 3

            [mentions]
            enabled = true
            steve_handle = "Steve"
            max_pending_per_actor = 2

            [nutnews]
            enabled = true

            [overlay]
            enabled = true
            path = "/data/steve-runtime/overlay/overlay.json"
            max_events = 1024

            [emotion]
            enabled = true

            [advocate]
            enabled = true
            reviews_per_day = 2
        "#;
        let config: RuntimeConfig = toml::from_str(source).unwrap();
        assert!(config.autonomy.enabled);
        assert_eq!(config.autonomy.policy.tick_secs, 30);
        assert_eq!(config.sleep_cycle.cycles_per_day, 3);
        assert!(config.mentions.enabled);
        assert_eq!(config.mentions.ingest.max_pending_per_actor, 2);
        assert_eq!(config.overlay.bounds.max_events, 1024);
        assert!(config.emotion.enabled);
        assert_eq!(config.advocate.reviews_per_day, 2);
        config.validate().unwrap();
    }

    #[test]
    fn overlay_path_inside_memory_dir_is_rejected() {
        let mut config = RuntimeConfig::default();
        config.memory_dir = PathBuf::from("/data/steve-memory");
        config.overlay.path = PathBuf::from("/data/steve-memory/overlay/overlay.json");
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("immutable"), "{err}");
        // Outside the memory dir (default) is fine even when enabled.
        let mut config = RuntimeConfig::default();
        config.memory_dir = PathBuf::from("/data/steve-memory");
        config.overlay.enabled = true;
        config.validate().unwrap();
        // Empty path only matters once the overlay is enabled.
        let mut config = RuntimeConfig::default();
        config.overlay.path = PathBuf::new();
        config.validate().unwrap();
        config.overlay.enabled = true;
        assert!(config.validate().is_err());
    }

    #[test]
    fn mention_identity_must_agree_with_nutnews() {
        // Enabled mentions require the connector.
        let mut config = RuntimeConfig::default();
        config.mentions.enabled = true;
        config.nutnews.enabled = false;
        assert!(config.validate().is_err());
        // Handle mismatch is refused (comparison is normalized).
        let mut config = RuntimeConfig::default();
        config.mentions.enabled = true;
        config.nutnews.enabled = true;
        config.mentions.ingest.steve_handle = "someone-else".into();
        assert!(config.validate().is_err());
        // Instance must match the public_url host.
        let mut config = RuntimeConfig::default();
        config.mentions.enabled = true;
        config.nutnews.enabled = true;
        config.mentions.ingest.instance = "evil.example".into();
        assert!(config.validate().is_err());
        // Defaults agree: enabling both validates, case-insensitively.
        let mut config = RuntimeConfig::default();
        config.mentions.enabled = true;
        config.nutnews.enabled = true;
        config.mentions.ingest.steve_handle = "Steve".into();
        config.validate().unwrap();
    }

    #[test]
    fn advocate_requires_overlay_and_sane_cadence() {
        let mut config = RuntimeConfig::default();
        config.advocate.enabled = true;
        assert!(config.validate().is_err(), "advocate without overlay");
        config.overlay.enabled = true;
        config.validate().unwrap();
        config.advocate.reviews_per_day = 0;
        assert!(config.validate().is_err());
        config.advocate.reviews_per_day = 25;
        assert!(config.validate().is_err());
    }

    #[test]
    fn private_sleep_sharing_needs_a_trusted_profile() {
        let mut config = RuntimeConfig::default();
        config.sleep_cycle.privacy.share_private_memory = true;
        // Default profiles include local_ollama with private_context: passes.
        config.validate().unwrap();
        // Strip the capability everywhere: sharing must be refused.
        for profile in &mut config.models.profiles {
            profile
                .capabilities
                .retain(|c| *c != ModelCapability::PrivateContext);
        }
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("private_context"), "{err}");
    }

    #[test]
    fn invalid_module_sections_fail_validation() {
        let mut config = RuntimeConfig::default();
        config.autonomy.policy.tick_secs = 0;
        assert!(config.validate().is_err());

        let mut config = RuntimeConfig::default();
        config.sleep_cycle.cycles_per_day = 0;
        assert!(config.validate().is_err());

        let mut config = RuntimeConfig::default();
        config.mentions.ingest.dedupe_capacity = 0;
        assert!(config.validate().is_err());

        let mut config = RuntimeConfig::default();
        config.overlay.bounds.max_events = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn url_host_extraction() {
        assert_eq!(
            url_host("https://news.nuts.services"),
            Some("news.nuts.services".into())
        );
        assert_eq!(
            url_host("https://News.Nuts.Services/mcp"),
            Some("news.nuts.services".into())
        );
        assert_eq!(url_host("ftp://x"), None);
        assert_eq!(url_host("https://"), None);
    }
}
