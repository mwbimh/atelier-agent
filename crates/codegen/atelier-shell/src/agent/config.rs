use crate::agent::auth_method::ModelByok;
use crate::auth::{AtelierComConfig, AuthManager, OidcAuthConfig};
use crate::runtime_defaults::DEFAULT_CONTEXT_WINDOW;
use crate::{config::StorageMode, sampling::ApiBackend, tools::config::ShellToolsetConfig};
use agent_client_protocol as acp;
use atelier_agent::prompt::skills::SkillsConfig;
use atelier_provider::{
    ModelDescriptor as ProviderModelDescriptor, ProviderAuth, ProviderSnapshot, WireApi,
};
use atelier_sampler::{AuthScheme, SamplerConfig};
use atelier_sampling_types::{
    CompactionAtTokens, CompactionsRemaining, REASONING_EFFORT_META_KEY,
    REASONING_EFFORTS_META_KEY, ReasoningEffort, ReasoningEffortOption,
    reasoning_effort_meta_value, reasoning_efforts_meta_value,
};
use atelier_tools::types::compat::{
    COMPAT_CELLS, CompatConfig, CompatConfigToml, CompatRemoteKey, CompatSurface, CompatVendor,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
/// The mode in which the agent is running.
/// Determines behavior like relay sync enablement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentMode {
    /// TUI interactive mode - full UI with relay sync support
    Tui,
    /// Headless mode - no UI, connected to relay WebSocket
    Headless,
    /// Stdio mode - JSON-RPC over stdin/stdout
    Stdio,
    /// Server mode - WebSocket server for external clients
    Serve,
    /// Leader mode - IPC server for follower clients
    Leader,
    /// Generic/unknown mode
    #[default]
    Generic,
}
/// Default agent type when the server or user config doesn't specify one.
pub const DEFAULT_AGENT_TYPE: &str = "atelier-build-plan";
/// Serde default for `ModelInfo.agent_type` and `ModelEntryConfig.agent_type`.
pub fn default_agent_type() -> String {
    DEFAULT_AGENT_TYPE.to_owned()
}
/// There is no compiled vendor endpoint in the private build.
pub const CLI_CHAT_PROXY_BASE_URL_DEFAULT: &str = "";
/// There is no compiled asset server endpoint in the private build.
pub const ASSET_SERVER_URL_DEFAULT: &str = "";
/// One or more environment variable names that may hold a model API key.
///
/// Serde `untagged`: accepts a string or an array in TOML/JSON.
///
/// ```toml
/// env_key = "ANTHROPIC_AUTH_TOKEN"
/// # or
/// env_key = ["ANTHROPIC_AUTH_TOKEN", "LC_ANTHROPIC_AUTH_TOKEN"]
/// ```
///
/// At resolve time the **first set, non-blank** value wins (e.g. SSH
/// `AcceptEnv LC_*` forwarding of the Bottlerocket token).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvKeys {
    One(String),
    Many(Vec<String>),
}
impl EnvKeys {
    /// Single-name convenience constructor.
    pub fn single(name: impl Into<String>) -> Self {
        Self::One(name.into())
    }
    /// Construct from an ordered list (empty names dropped; 0/1/N → Many/One/Many).
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let names: Vec<String> = names
            .into_iter()
            .map(Into::into)
            .filter(|s| !s.is_empty())
            .collect();
        match names.as_slice() {
            [] => Self::Many(Vec::new()),
            [_] => Self::One(names.into_iter().next().expect("len 1")),
            _ => Self::Many(names),
        }
    }
    pub fn is_empty(&self) -> bool {
        match self {
            Self::One(s) => s.is_empty(),
            Self::Many(v) => v.is_empty(),
        }
    }
    /// Configured names in priority order.
    pub fn names(&self) -> Vec<&str> {
        match self {
            Self::One(s) => vec![s.as_str()],
            Self::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
    /// First name only (useful for single-key assertions / display).
    pub fn primary(&self) -> Option<&str> {
        match self {
            Self::One(s) if !s.is_empty() => Some(s.as_str()),
            Self::One(_) => None,
            Self::Many(v) => v.iter().map(String::as_str).find(|s| !s.is_empty()),
        }
    }
    /// Resolve the first set, non-blank process env value among configured names.
    pub fn resolve_value(&self) -> Option<String> {
        self.resolve_value_with(|name| std::env::var(name).ok())
    }
    /// Testable resolve with an injected getenv.
    pub fn resolve_value_with(
        &self,
        mut getenv: impl FnMut(&str) -> Option<String>,
    ) -> Option<String> {
        for name in self.names() {
            if let Some(value) = getenv(name) {
                if !value.trim().is_empty() {
                    return Some(value);
                }
            }
        }
        None
    }
}
/// Semantic equality: compares the ordered name lists, so `One("X")` and
/// `Many(["X"])` (the shape serde produces for `["X"]`) compare equal.
impl PartialEq for EnvKeys {
    fn eq(&self, other: &Self) -> bool {
        self.names() == other.names()
    }
}
impl Eq for EnvKeys {}
impl std::fmt::Display for EnvKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.names().join(", "))
    }
}
/// Configuration for API endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointsConfig {
    /// cli chat proxy base URL. `None` = unset (resolvers apply the default);
    /// `Some` = explicitly configured. Tracking explicitness (vs comparing to the
    /// default value) lets an org pin the proxy to the default on purpose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_chat_proxy_base_url: Option<String>,
    /// Optional extra access-header value (applied only with the optional
    /// non-production feature, and only for matching first-party hosts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_test_key: Option<String>,
    /// Env: `ATELIER_MODELS_BASE_URL`. Enables custom endpoint mode.
    /// List URL defaults to `{models_base_url}/models`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_base_url: Option<String>,
    /// Env: `ATELIER_MODELS_LIST_URL`. Overrides the default `{base}/models` list URL.
    #[serde(alias = "models_endpoint", skip_serializing_if = "Option::is_none")]
    pub models_list_url: Option<String>,
    /// Env: `ATELIER_DEPLOYMENT_KEY`. Management API key for enterprise deployments.
    /// Sent on telemetry and service requests for deployment-level attribution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_key: Option<String>,
    /// Base URL for the asset server (profile images, etc.).
    /// Env: `ATELIER_ASSET_SERVER_URL`.
    #[serde(default = "default_asset_server_url")]
    pub asset_server_url: String,
    /// Read by `load_management_api_key_sync()`. Declared for `serde_ignored`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_api_key: Option<String>,
}
pub(crate) fn default_asset_server_url() -> String {
    std::env::var("ATELIER_ASSET_SERVER_URL")
        .unwrap_or_else(|_| ASSET_SERVER_URL_DEFAULT.to_owned())
}
/// A blank or whitespace-only override counts as unset. Single source of truth
/// for the "empty value = not configured" rule shared by the endpoint resolvers.
fn blank_as_unset(opt: &Option<String>) -> Option<String> {
    opt.as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}
impl EndpointsConfig {
    pub fn has_custom_endpoint(&self) -> bool {
        self.models_base_url.is_some() || self.models_list_url.is_some()
    }
    /// `default()` plus merged managed/requirements endpoint overrides, so
    /// startup fetches use the configured (not public) endpoints. Only merges
    /// layers — never derives one endpoint from another. Falls back to
    /// `default()` on load failure.
    pub fn from_effective_config() -> Self {
        match crate::config::load_effective_config() {
            Ok(cfg) => Self::from_config_value(&cfg),
            Err(_) => Self::default(),
        }
    }
    /// Layer the `[endpoints]` table from `config` over the env/default base.
    /// No field is derived from another — defaulting is done by the resolvers.
    pub(crate) fn from_config_value(config: &toml::Value) -> Self {
        let default = Self::default();
        let mut base = match toml::Value::try_from(default) {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };
        if let Some(endpoints) = config.get("endpoints") {
            crate::config::deep_merge_toml(&mut base, endpoints);
        }
        base.try_into().unwrap_or_default()
    }
    /// The configured proxy base URL through which auxiliary services and
    /// session inference resolve. No vendor endpoint is compiled in.
    pub fn proxy_url(&self) -> String {
        blank_as_unset(&self.cli_chat_proxy_base_url)
            .unwrap_or_else(|| CLI_CHAT_PROXY_BASE_URL_DEFAULT.to_owned())
    }
    pub fn resolve_inference_base_url(&self) -> String {
        self.models_base_url
            .clone()
            .unwrap_or_else(|| self.proxy_url())
    }
    /// `models_list_url` > `{models_base_url}/models` > `{proxy_base_url}/models`.
    pub fn resolve_models_list_url(&self) -> String {
        if let Some(ref url) = self.models_list_url {
            if !url.trim().is_empty() {
                return url.clone();
            }
        }
        let base = self
            .models_base_url
            .clone()
            .unwrap_or_else(|| self.proxy_url());
        if base.trim().is_empty() {
            String::new()
        } else {
            format!("{}/models", base.trim_end_matches('/'))
        }
    }
}
impl Default for EndpointsConfig {
    fn default() -> Self {
        Self {
            cli_chat_proxy_base_url: std::env::var("ATELIER_CLI_CHAT_PROXY_BASE_URL").ok(),
            alpha_test_key: None,
            models_base_url: env_string("ATELIER_MODELS_BASE_URL"),
            models_list_url: env_string("ATELIER_MODELS_LIST_URL"),
            deployment_key: env_string("ATELIER_DEPLOYMENT_KEY"),
            asset_server_url: default_asset_server_url(),
            management_api_key: None,
        }
    }
}
pub use atelier_config_types::{BoolFlag, ConfigSource, LazinessDetectorPerModelConfig, Resolved};
/// Resolution result for a `/goal` role's model selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum GoalRoleModelChoice {
    /// Use the current (parent) model + the parent's agent type.
    #[default]
    InheritCurrent,
    /// Use this explicit pair (subject to auth/fail-open at spawn time).
    Explicit(crate::util::config::GoalRoleModel),
}
/// A requirement pin from `requirements.toml`. Wins over all other sources.
#[derive(Debug, Clone, Default)]
pub struct Constrained<T> {
    pin: Option<T>,
    source: Option<crate::config::RequirementSource>,
}
impl<T: Clone> Constrained<T> {
    pub fn pin(&mut self, value: T, source: crate::config::RequirementSource) {
        self.pin = Some(value);
        self.source = Some(source);
    }
    pub fn pinned(&self) -> Option<T> {
        self.pin.clone()
    }
    pub fn source(&self) -> Option<&crate::config::RequirementSource> {
        self.source.as_ref()
    }
}
/// Enforced requirements from `requirements.toml`. Pinned values win over all other sources.
#[derive(Debug, Clone, Default)]
pub struct Requirements {
    pub telemetry: Constrained<TelemetryMode>,
    pub feedback: Constrained<bool>,
    pub lsp_tools: Constrained<bool>,
    pub tool_search: Constrained<bool>,
    pub web_fetch: Constrained<bool>,
    pub ask_user_question: Constrained<bool>,
    pub image_gen: Constrained<bool>,
    pub image_edit: Constrained<bool>,
    pub video_gen: Constrained<bool>,
    pub write_file: Constrained<bool>,
    /// Voice dictation (STT). Pin via requirements/managed `[features] voice_mode`.
    pub voice_mode: Constrained<bool>,
    pub sandbox_auto_allow_bash: Constrained<bool>,
    pub sandbox_profile: Constrained<String>,
    pub respect_gitignore: Constrained<bool>,
    pub remote_fetch: Constrained<bool>,
}
/// Inputs for resolving `#[serde(skip)]` runtime fields after `new_from_toml_cfg()`.
///
/// Constructed by each binary from its CLI args and startup state, then passed
/// to [`Config::resolve_runtime_fields`].
pub struct RuntimeResolutionContext<'a> {
    pub raw_config: &'a toml::Value,
    pub local_runtime_settings: Option<&'a crate::util::config::LocalRuntimeSettings>,
    pub cwd: Option<&'a std::path::Path>,
    pub is_headless: bool,
    /// `Some(true)` = CLI explicitly enabled, `None` = defer to config/env/remote.
    pub cli_subagents: Option<bool>,
    pub cli_web_search_model: Option<&'a str>,
    pub cli_session_summary_model: Option<&'a str>,
    /// CLI `--experimental-memory` flag. Enables cross-session memory.
    pub cli_experimental_memory: bool,
    /// CLI `--no-memory` flag. Overrides all other memory settings.
    pub cli_no_memory: bool,
    /// CLI `--disable-web-search` flag. ORed with config.toml value.
    pub disable_web_search: bool,
    /// CLI `--todo-gate` flag. Session-scoped — not persisted.
    pub todo_gate: bool,
    /// CLI `--laziness-debug-log <path>`. When `Some`, the Layer-3
    /// classifier fires after every turn (bypassing the idle wait /
    /// per-model gate / nudge cap) and writes a JSONL line per fire.
    /// Observation-only. Session-scoped — not persisted.
    pub laziness_debug_log: Option<&'a std::path::Path>,
    /// CLI `--storage-mode` override. `None` = defer to env/remote/default.
    pub storage_mode: Option<&'a str>,
}
/// Read an env var as a trimmed string. Returns `None` if unset or empty/whitespace-only.
pub(crate) fn env_string(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
pub use atelier_config::env_bool;
/// Compaction-mode precedence (env > config > remote settings > default, with
/// unrecognized values at each source falling through). `remote` sits just
/// above the default, mirroring `feature_flag` in `resolve_bool_flag`. Pure so
/// it's unit-testable without mutating process env.
fn resolve_compaction_mode_from(
    env: Option<&str>,
    config: Option<&str>,
    remote: Option<&str>,
) -> atelier_chat_state::CompactionMode {
    use atelier_chat_state::CompactionMode;
    env.and_then(CompactionMode::parse)
        .or_else(|| config.and_then(CompactionMode::parse))
        .or_else(|| remote.and_then(CompactionMode::parse))
        .unwrap_or_default()
}
/// Compaction-detail precedence (env > config > remote settings > default). Pure.
/// Controls the per-turn verbatim detail in `segments` mode (default `verbose`).
fn resolve_compaction_detail_from(
    env: Option<&str>,
    config: Option<&str>,
    remote: Option<&str>,
) -> atelier_chat_state::CompactionDetail {
    use atelier_chat_state::CompactionDetail;
    env.and_then(CompactionDetail::parse)
        .or_else(|| config.and_then(CompactionDetail::parse))
        .or_else(|| remote.and_then(CompactionDetail::parse))
        .unwrap_or_default()
}
/// Resolve a single vendor-compat cell: env > `[compat]` TOML > remote settings
/// remote flag > default ON.
fn resolve_compat_cell(
    env: &str,
    cfg: Option<bool>,
    remote: Option<bool>,
    default: bool,
) -> Resolved<bool> {
    resolve_compat_cell_with_env(atelier_config::env_bool(env), cfg, remote, default)
}
pub(crate) fn resolve_compat_cell_with_env(
    env: Option<bool>,
    cfg: Option<bool>,
    remote: Option<bool>,
    default: bool,
) -> Resolved<bool> {
    if let Some(value) = env {
        Resolved::new(value, ConfigSource::Env)
    } else if let Some(value) = cfg {
        Resolved::new(value, ConfigSource::Config)
    } else if let Some(value) = remote {
        Resolved::new(value, ConfigSource::Remote)
    } else {
        Resolved::new(default, ConfigSource::Default)
    }
}
fn remote_compat_value(
    remote: Option<&crate::util::config::LocalRuntimeSettings>,
    key: Option<CompatRemoteKey>,
) -> Option<bool> {
    let remote = remote?;
    match key? {
        CompatRemoteKey::CursorSkills => remote.cursor_skills_enabled,
        CompatRemoteKey::CursorRules => remote.cursor_rules_enabled,
        CompatRemoteKey::CursorAgents => remote.cursor_agents_enabled,
        CompatRemoteKey::CursorMcps => remote.cursor_mcps_enabled,
        CompatRemoteKey::CursorHooks => remote.cursor_hooks_enabled,
        CompatRemoteKey::CursorSessions => remote.cursor_sessions_enabled,
        CompatRemoteKey::ClaudeSkills => remote.claude_skills_enabled,
        CompatRemoteKey::ClaudeRules => remote.claude_rules_enabled,
        CompatRemoteKey::ClaudeAgents => remote.claude_agents_enabled,
        CompatRemoteKey::ClaudeMcps => remote.claude_mcps_enabled,
        CompatRemoteKey::ClaudeHooks => remote.claude_hooks_enabled,
        CompatRemoteKey::ClaudeSessions => remote.claude_sessions_enabled,
        CompatRemoteKey::CodexSessions => remote.codex_sessions_enabled,
    }
}
/// Resolve vendor compatibility cells from TOML and remote settings.
fn resolve_compat_config(
    config: &CompatConfigToml,
    remote: Option<&crate::util::config::LocalRuntimeSettings>,
) -> CompatConfig {
    let defaults = CompatConfig::default();
    let mut resolved = defaults;
    for cell in COMPAT_CELLS {
        resolved.set(
            cell,
            resolve_compat_cell(
                cell.env_var(),
                config.value(cell),
                remote_compat_value(remote, cell.remote_key()),
                defaults.value(cell),
            )
            .value,
        );
    }
    resolved
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompatConfigCellError {
    Unavailable,
    Malformed,
}
pub(crate) fn compat_config_cell(
    raw_config: Result<&toml::Value, ()>,
    cell: atelier_tools::types::compat::CompatCell,
) -> Result<Option<bool>, CompatConfigCellError> {
    let raw = raw_config.map_err(|()| CompatConfigCellError::Unavailable)?;
    let Some(compat) = raw.get("compat") else {
        return Ok(None);
    };
    let compat = compat.as_table().ok_or(CompatConfigCellError::Malformed)?;
    let Some(vendor) = compat.get(cell.vendor().as_str()) else {
        return Ok(None);
    };
    let vendor = vendor.as_table().ok_or(CompatConfigCellError::Malformed)?;
    let Some(value) = vendor.get(cell.surface().as_str()) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or(CompatConfigCellError::Malformed)
}
/// Resolve only picker-facing session cells from raw config independently.
pub fn resolve_compat_sessions_from_raw(
    raw_config: Result<&toml::Value, ()>,
    remote: Option<&crate::util::config::LocalRuntimeSettings>,
) -> CompatConfig {
    let mut config = CompatConfigToml::default();
    for cell in COMPAT_CELLS
        .into_iter()
        .filter(|cell| cell.surface() == CompatSurface::Sessions)
    {
        let value = match compat_config_cell(raw_config, cell) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    vendor = cell.vendor().as_str(),
                    ?error,
                    "invalid compat config; disabling foreign sessions"
                );
                Some(false)
            }
        };
        match cell.vendor() {
            CompatVendor::Cursor => config.cursor.sessions = value,
            CompatVendor::Claude => config.claude.sessions = value,
            CompatVendor::Codex => config.codex.sessions = value,
        }
    }
    resolve_compat_config(&config, remote)
}
/// Resolve a string setting: cli > env > config > feature flag. `None` if no source provides a value.
pub(crate) fn resolve_string_flag(
    cli_arg: Option<&str>,
    env_var: &str,
    config_val: Option<&str>,
    feature_flag_val: Option<&str>,
) -> Option<Resolved<String>> {
    if let Some(val) = cli_arg.filter(|s| !s.is_empty()) {
        return Some(Resolved::new(val.to_owned(), ConfigSource::Cli));
    }
    if let Some(val) = env_string(env_var) {
        return Some(Resolved::new(val, ConfigSource::Env));
    }
    if let Some(val) = config_val.filter(|s| !s.is_empty()) {
        return Some(Resolved::new(val.to_owned(), ConfigSource::Config));
    }
    if let Some(val) = feature_flag_val.filter(|s| !s.is_empty()) {
        return Some(Resolved::new(val.to_owned(), ConfigSource::Remote));
    }
    None
}
/// Resolve `enabled` for section-based configs (memory, subagents, etc.).
/// Feature flag only applies when the TOML section is absent.
pub(crate) fn resolve_enabled(
    cli_flag: Option<bool>,
    env_var: &str,
    config_enabled: bool,
    has_local_section: bool,
    feature_flag_val: Option<bool>,
    default: bool,
) -> Resolved<bool> {
    let config_val = if has_local_section {
        Some(config_enabled)
    } else {
        None
    };
    BoolFlag::env(env_var)
        .cli(cli_flag)
        .config(config_val)
        .feature_flag(feature_flag_val)
        .default(default)
        .resolve()
}
pub(crate) use atelier_telemetry::config::env_telemetry_mode;
pub use atelier_telemetry::config::{TelemetryConfig, TelemetryMode};
/// Plugin system configuration from `[plugins]` section in config.toml.
///
/// ```toml
/// [plugins]
/// paths = ["~/my-plugins/custom-tools"]
/// disabled = ["user/a1b2c3d4/noisy-plugin"]
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PluginsConfig {
    /// Additional plugin directory paths to load.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Plugin IDs or names to disable. Disabled plugins are discovered
    /// but their components are not loaded into the session.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Plugin IDs or names to explicitly enable. Used for project-scope plugins
    /// which are disabled by default — adding a plugin here overrides that default.
    #[serde(default)]
    pub enabled: Vec<String>,
    /// CLI `--plugin-dir` paths (populated by CLI arg processing, not config file).
    #[serde(skip)]
    pub cli_plugin_dirs: Vec<std::path::PathBuf>,
}
impl PluginsConfig {
    /// Merge `enabledPlugins` from Claude settings files into this config.
    ///
    /// Reads `enabledPlugins` from `~/.claude/settings.json` only (user scope).
    /// Project-level `<git_root>/.claude/settings.json` is intentionally NOT
    /// read here: a malicious repo could pre-populate `enabledPlugins` to
    /// bypass the project-plugin auto-disable logic in `populate_plugin_lists`,
    /// enabling attacker-controlled hooks (e.g. SessionStart → RCE).
    /// Native `.atelier/config.toml` entries already present take precedence:
    /// a name is only added if it isn't already in the opposite list.
    pub fn merge_claude_enabled_plugins(&mut self, _cwd: Option<&std::path::Path>) {
        if crate::claude_import::is_claude_import_marked_with_log("merge_claude_enabled_plugins") {
            return;
        }
        let mut paths = Vec::new();
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".claude").join("settings.json"));
        }
        for path in &paths {
            let (claude_enabled, claude_disabled) =
                atelier_agent::plugins::marketplace::load_enabled_disabled_plugins(path);
            for name in claude_enabled {
                if !self.disabled.contains(&name) && !self.enabled.contains(&name) {
                    self.enabled.push(name);
                }
            }
            for name in claude_disabled {
                if !self.enabled.contains(&name) && !self.disabled.contains(&name) {
                    self.disabled.push(name);
                }
            }
        }
    }
    /// Build a `DiscoveryConfig` from this plugins config.
    pub fn to_discovery_config(&self) -> atelier_agent::plugins::discovery::DiscoveryConfig {
        atelier_agent::plugins::discovery::DiscoveryConfig {
            cli_plugin_dirs: self.cli_plugin_dirs.clone(),
            config_paths: self.paths.iter().map(std::path::PathBuf::from).collect(),
            disabled: self.disabled.clone(),
            enabled: self.enabled.clone(),
        }
    }
}
/// Feedback submission configuration (`[feedback]` in config.toml).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedbackConfig {}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    pub memory_flush: Option<crate::config::MemoryFlushConfig>,
    pub pruning: Option<crate::config::PruningConfig>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CliConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismissed_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npm_registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_leader: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_tips: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_registry: Option<bool>,
    /// User-layer value; use [`crate::util::config::resolve_minimum_version`]
    /// for enforcement (semver-max across layers; managed floors can't be lowered).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_version: Option<String>,
    /// Group sessions by repo in the picker and CLI listings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_picker_grouped: Option<bool>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crash_handler: Option<bool>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The pre-campaign `models.default` (merged user/managed/requirements)
    /// captured when a campaign is overriding the default, so model resolution can
    /// recover if the campaign points at a model missing from the catalog. `None`
    /// when there is nothing to recover to. Runtime-only; never serialized.
    #[serde(skip)]
    pub pre_campaign_default: Option<String>,
    /// Whether an active campaign is currently overriding `models.default`. The
    /// authoritative campaign-driven-default signal (set from the resolved active
    /// set), correct even when the user has no base default. Runtime-only.
    #[serde(skip)]
    pub default_is_campaign_driven: bool,
    /// Persisted effort for the default model; applied in `resolve_model_catalog`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_summary: Option<String>,
    /// Vision model used to transcribe user-supplied
    /// images via a separate endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_description: Option<String>,
    /// Model pin for next-prompt suggestions (tab-autocomplete ghost text).
    /// Unset = remote pin, then the client hint / built-in `atelier-build-0.1`
    /// default with the catalog guard; see `ModelOverrideConfig::resolve`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_suggestion: Option<String>,
    /// Restricts which models are user-selectable for normal chat (picker,
    /// `/model`, `-m`). Non-matching models stay in the catalog but are never
    /// shown, defaulted to, or selectable. Special/internal models (web_search,
    /// image_description, subagents, fork secondary) are exempt.
    ///
    /// Glob patterns (`*`, `?`, `[...]`) match the model id or catalog key,
    /// case-sensitive. Empty = no restriction; an excluded explicit `default`/`-m`
    /// is rejected at startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Force `hidden = true` on these model IDs (still usable via `-m`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_models: Option<Vec<String>>,
    /// Remove these model IDs from the catalog entirely. Wins over `hidden_models`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_models: Option<Vec<String>>,
    /// Fallback `agent_type` for models without a per-model override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// Global default request headers applied to every model. A per-model
    /// `[model.<id>].extra_headers` entry overrides per key (case-insensitive).
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub extra_headers: IndexMap<String, String>,
    /// Global default values applied to every model that leaves the field
    /// unset; a per-model `[model.<id>]` value always wins. A deliberately
    /// small, allow-listed subset of the per-model fields (only `Option` ones,
    /// so "unset" is unambiguous). Future: these could consolidate into a
    /// `[models.defaults]` sub-table mirroring the per-model schema 1:1; kept
    /// flat for now as that is a larger refactor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_idle_timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_tool_calls: Option<bool>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_for_upload: Option<bool>,
    /// Budget (seconds) for the turn-end upload flush when
    /// `block_for_upload` is active. Default 60.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_flush_timeout_secs: Option<u64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RelayConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}
/// `[hub]` section from config.toml.
///
/// Optional default Computer Hub URL for **workspace provider** exposure
/// (`atelier workspace` / leader `with_default_hub_url`). Does **not** enable
/// agent-side harness/client connections or alter local session behavior.
///
/// ```toml
/// [hub]
/// url = "wss://hub.atelier/ws"
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HubConfig {
    /// Hub WebSocket URL (`ws://` or `wss://`) used as the leader default for
    /// `atelier workspace start` when the CLI does not pass `--hub-url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}
impl HubConfig {
    /// Whether a non-empty hub URL is configured (workspace default only).
    pub fn is_enabled(&self) -> bool {
        self.url.as_ref().is_some_and(|u| !u.trim().is_empty())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorktreePoolConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count_threshold: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<usize>,
}
/// `[sandbox]` section from config.toml.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxSettingsConfig {
    /// "workspace", "devbox", "read-only", "strict", or a custom name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Skip bash permission prompts when sandbox is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_allow_bash: Option<bool>,
    /// Sandbox backend. Native is the default; `unsafe` must be explicit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

impl Default for SandboxSettingsConfig {
    fn default() -> Self {
        Self {
            profile: Some("workspace".to_owned()),
            auto_allow_bash: None,
            backend: None,
        }
    }
}
impl SandboxSettingsConfig {
    pub fn from_effective_config() -> Self {
        crate::config::load_effective_config()
            .ok()
            .and_then(|v| v.get("sandbox")?.clone().try_into().ok())
            .unwrap_or_default()
    }
    /// Resolve sandbox profile: requirement > CLI > env > config > "workspace".
    pub fn resolve_profile(
        &self,
        cli_arg: Option<&str>,
        requirement: Option<&str>,
    ) -> Resolved<String> {
        if let Some(val) = requirement {
            return Resolved::new(val.to_owned(), ConfigSource::Requirement);
        }
        resolve_string_flag(cli_arg, "ATELIER_SANDBOX", self.profile.as_deref(), None)
            .unwrap_or_else(|| Resolved::new("workspace".to_owned(), ConfigSource::Default))
    }
    /// Resolve the backend from the explicit environment override or config.
    /// Unknown values are rejected so a typo cannot silently select a weaker
    /// execution path.
    pub fn resolve_backend(
        &self,
    ) -> Result<atelier_sandbox::SandboxBackendKind, atelier_sandbox::SandboxBackendParseError>
    {
        let configured = std::env::var("ATELIER_SANDBOX_BACKEND")
            .ok()
            .or_else(|| self.backend.clone());
        atelier_sandbox::parse_backend(configured.as_deref())
    }
    /// Resolve auto_allow_bash: requirement > env > config > default (false).
    pub fn resolve_auto_allow_bash(&self, requirement: Option<bool>) -> Resolved<bool> {
        BoolFlag::env("ATELIER_SANDBOX_AUTO_ALLOW_BASH")
            .requirement(requirement)
            .config(self.auto_allow_bash)
            .resolve()
    }
}
/// `[marketplace]` section from config.toml (plugin marketplace sources).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct MarketplaceConfig {
    /// `[[marketplace.sources]]` entries.
    #[serde(default)]
    pub sources: Vec<MarketplaceSourceEntry>,
    /// Written/read out-of-band by `extensions::marketplace`, opaque so a wrong-typed value can't fail load.
    #[serde(default)]
    pub official_marketplace_auto_installed: Option<toml::Value>,
}
/// A single `[[marketplace.sources]]` entry.
#[derive(Clone, Debug, Deserialize)]
pub struct MarketplaceSourceEntry {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}
/// `[suggestions]` section from config.toml.
///
/// Controls the shell command suggestion pipeline (history, path, AI).
///
/// ```toml
/// [suggestions]
/// enabled = true
/// ai_enabled = true
/// ai_model = "atelier-build"
/// debounce_ms = 50
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SuggestionsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debounce_ms: Option<u64>,
}
impl SuggestionsConfig {
    pub fn resolve_enabled(
        &self,
        remote: Option<&crate::util::config::LocalRuntimeSettings>,
    ) -> Resolved<bool> {
        BoolFlag::env("ATELIER_SUGGESTIONS")
            .config(self.enabled)
            .feature_flag(remote.and_then(|r| r.suggestions_enabled))
            .default(false)
            .resolve()
    }
    pub fn resolve_ai_enabled(
        &self,
        remote: Option<&crate::util::config::LocalRuntimeSettings>,
    ) -> Resolved<bool> {
        BoolFlag::env("ATELIER_SUGGESTIONS_AI")
            .config(self.ai_enabled)
            .feature_flag(remote.and_then(|r| r.suggestions_ai_enabled))
            .default(false)
            .resolve()
    }
    pub fn resolve_ai_model(&self) -> String {
        resolve_string_flag(
            None,
            "ATELIER_SUGGESTIONS_AI_MODEL",
            self.ai_model.as_deref(),
            None,
        )
        .map(|r| r.value)
        .unwrap_or_else(|| "atelier-build".to_owned())
    }
}
/// `[storage]` section from config.toml.
///
/// Controls session persistence settings like cleanup TTL.
/// Read by `resolve_cleanup_ttl_days()` in `session/persistence.rs`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Number of days to keep stale sessions before cleanup. Default: 30.
    pub cleanup_ttl_days: Option<u32>,
}
/// `[paths]` configuration: extra directories to scan for skills, rules, etc.
///
/// These supplement the built-in scan locations (`.atelier/skills/`,
/// `.agents/skills/`, `~/.atelier/skills/`). They're written by `/import-claude`
/// to preserve previously-discovered Claude directories after the runtime
/// `.claude/` cutoff (see `[claude_compat] imported`).
///
/// Example:
/// ```toml
/// [paths]
/// extra_skill_dirs = ["~/.claude/skills", "/path/to/.claude/skills"]
/// extra_rule_dirs = ["~/.claude/rules"]
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    /// Additional directories to scan for skills (each contains `<skill>/SKILL.md`).
    pub extra_skill_dirs: Vec<String>,
    /// Additional directories to scan for rules (each contains `*.md`).
    pub extra_rule_dirs: Vec<String>,
}
/// `[permission]` known keys, declared for the unrecognized-key scan only;
/// consumed out-of-band. Keys stay typed so a typo (e.g. `denny`) still warns.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PermissionKnownKeys {
    /// Compact rule arrays (`parse_toml_permission_section`).
    pub allow: Option<toml::Value>,
    pub deny: Option<toml::Value>,
    pub ask: Option<toml::Value>,
    /// Verbose `[[permission.rules]]` form.
    pub rules: Option<toml::Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub features: Features,
    /// Persistent new-Session default from top-level `config.toml:model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Selected editable prompt preset under `~/.atelier/contexts/<name>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Selected outbound HTTP identity from `~/.atelier/request-agents.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_agent: Option<String>,
    /// `[goal]` section: canonical `/goal` configuration. See [`GoalConfig`].
    #[serde(default)]
    pub goal: GoalConfig,
    /// `[doom_loop_recovery]` section: the shared settings struct — ONE type
    /// serves this TOML table and the remote remote settings `doom_loop_recovery`
    /// object. See [`crate::util::config::DoomLoopRecoverySettings`].
    #[serde(default)]
    pub doom_loop_recovery: crate::util::config::DoomLoopRecoverySettings,
    /// `[auto_mode]` section: Auto permission-mode configuration. See [`AutoModeConfig`].
    #[serde(default)]
    pub auto_mode: AutoModeConfig,
    /// `[model.*]` overrides from config.toml. Resolve via `resolve_model_list()`.
    #[serde(skip)]
    pub config_models: IndexMap<String, ConfigModelOverride>,
    /// Warnings from `[model.*]` parsing; surfaced by `atelier inspect`.
    #[serde(skip)]
    pub model_override_warnings: Vec<super::config_model_override_parse::ModelOverrideWarning>,
    pub atelier_com_config: AtelierComConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcuts: Option<toml::Value>,
    /// Written by the client via `config_toml_edit`; absorbed so it isn't
    /// flagged as an unrecognized key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hints: Option<toml::Value>,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub toolset: ShellToolsetConfig,
    #[serde(default)]
    pub endpoints: EndpointsConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// Session behavior configuration.
    #[serde(default)]
    pub session: SessionConfig,
    /// Agent definition selection configuration.
    /// Set in `config.toml` under `[agent]` to choose which agent definition
    /// is used for all sessions (unless overridden by CLI flag or ACP meta).
    #[serde(default)]
    pub agent: AgentSelectionConfig,
    /// Skills discovery configuration.
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Raw `[compat]` vendor-compatibility config (per-vendor × per-surface
    /// toggles). Resolved into [`Config::compat_resolved`] by
    /// `resolve_runtime_fields`.
    #[serde(default)]
    pub compat: CompatConfigToml,
    /// Plugin system configuration.
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// Feedback submission configuration.
    #[serde(default)]
    pub feedback: FeedbackConfig,
    /// Filesystem path overrides (`[paths]` in config.toml).
    #[serde(default)]
    pub paths: PathsConfig,
    #[serde(default, skip_serializing)]
    pub cli: CliConfig,
    #[serde(default, skip_serializing)]
    pub models: ModelsConfig,
    #[serde(default, skip_serializing)]
    pub harness: HarnessConfig,
    #[serde(default, skip_serializing)]
    pub relay: RelayConfig,
    #[serde(default, skip_serializing)]
    pub remote: RemoteConfig,
    /// Computer Hub configuration (`[hub]` in config.toml).
    #[serde(default, skip_serializing)]
    pub hub: HubConfig,
    #[serde(default, skip_serializing)]
    pub worktree_pool: WorktreePoolConfig,
    #[serde(default, skip_serializing)]
    pub sandbox: SandboxSettingsConfig,
    #[serde(default, skip_serializing)]
    pub mcp_servers: std::collections::HashMap<String, crate::util::config::McpServerConfig>,
    #[serde(default, skip_serializing)]
    pub disabled_mcp_servers: Vec<String>,
    #[serde(default, skip_serializing)]
    pub disabled_mcp_tools: std::collections::HashMap<String, Vec<String>>,
    #[serde(default, skip_serializing)]
    pub subagents: crate::config::SubagentsConfig,
    #[serde(default, skip_serializing)]
    pub memory: crate::config::MemoryConfig,
    #[serde(default, skip_serializing)]
    pub compaction: CompactionConfig,
    #[serde(default, skip_serializing)]
    pub managed_mcps: crate::config::ManagedMcpsConfig,
    /// `[auth]` alias — consumed by `expand_auth_alias` before serde.
    /// Typed as `AtelierComConfig` (same schema) so sub-field typos are caught.
    #[serde(default, skip_serializing)]
    pub auth: Option<AtelierComConfig>,
    /// `[desktop]` section — owned by atelier-desktop (Electron app), opaque to the CLI agent.
    #[serde(default, skip_serializing)]
    pub desktop: Option<toml::Value>,
    /// Top-level `announcements` array — consumed by `resolve_announcements`.
    #[serde(default, skip_serializing)]
    pub announcements: Vec<atelier_announcements::RemoteAnnouncement>,
    /// `[tips]` section — consumed by `merge_tips`.
    #[serde(default, skip_serializing)]
    pub tips: Option<crate::util::config::TipsOverride>,
    /// `[permission]` — consumed out-of-band; see [`PermissionKnownKeys`].
    #[serde(default, skip_serializing)]
    pub permission: PermissionKnownKeys,
    /// `[tools]` — also read by `ToolsConfig::resolve()`.
    #[serde(default, skip_serializing)]
    pub tools: crate::config::ToolsConfig,
    /// `[storage]` — also read by `resolve_cleanup_ttl_days()`.
    #[serde(default, skip_serializing)]
    pub storage: StorageConfig,
    /// `[suggestions]` — shell command suggestion pipeline settings.
    #[serde(default, skip_serializing)]
    pub suggestions: SuggestionsConfig,
    /// `[marketplace]` — also read by `atelier_plugin_marketplace::load_sources()`.
    #[serde(default, skip_serializing)]
    pub marketplace: MarketplaceConfig,
    /// `[diagnostics]` — crash handler toggle (`load_crash_handler_enabled_sync`).
    #[serde(default, skip_serializing)]
    pub diagnostics: DiagnosticsConfig,
    /// Storage mode for session persistence.
    /// When running in relay/headless mode, this should be set to Writeback.
    /// Defaults to reading from ATELIER_STORAGE_MODE env var.
    #[serde(skip)]
    pub storage_mode: StorageMode,
    /// CLI override for the default model ID.
    #[serde(skip)]
    pub default_model_override: Option<String>,
    /// CLI override for reasoning effort.
    #[serde(skip)]
    pub reasoning_effort_override: Option<ReasoningEffort>,
    /// CLI override for the web search model ID.
    #[serde(skip)]
    pub web_search_model_override: Option<String>,
    /// CLI override for the session summary model ID.
    #[serde(skip)]
    pub session_summary_model_override: Option<String>,
    /// CLI override for YOLO mode (auto-approve all permissions).
    /// Takes precedence over default settings.
    #[serde(skip)]
    pub default_yolo_mode: bool,
    /// Start sessions in auto permission mode (classifier) when no per-session override.
    pub default_auto_mode: bool,
    /// CLI `--experimental-memory` flag. Stored for `ConfigReloader` hot-reload re-resolution.
    #[serde(skip)]
    pub cli_experimental_memory: bool,
    /// CLI `--no-memory` flag. Stored for `ConfigReloader` hot-reload re-resolution.
    #[serde(skip)]
    pub cli_no_memory: bool,
    /// Original CLI `--subagents` tri-state, preserved for re-resolution
    /// when remote settings settings are refreshed on /new.
    #[serde(skip)]
    pub cli_subagents: Option<bool>,
    /// Resolved memory configuration. `None` when memory is disabled.
    /// Resolved by [`RuntimeResolutionContext`] in [`Config::resolve_runtime_fields`].
    #[serde(skip)]
    pub memory_config: Option<crate::config::MemoryConfig>,
    /// CLI override: path to an agent profile (.md file with YAML frontmatter).
    #[serde(skip)]
    pub agent_profile_path: Option<PathBuf>,
    /// Client version string (e.g., "0.1.77 (abc1234)").
    /// Set by the TUI/CLI launcher and used as fallback when clients don't provide clientVersion.
    #[serde(skip)]
    pub client_version: Option<String>,
    /// The mode in which the agent is running.
    /// Determines behavior like relay sync enablement (only enabled in TUI mode).
    #[serde(skip)]
    pub mode: AgentMode,
    /// Remote settings fetched from cli-chat-proxy at startup.
    /// Used for upload limits (replaces on-demand /v1/storage/limits fetch).
    #[serde(skip)]
    pub local_runtime_settings: Option<crate::util::config::LocalRuntimeSettings>,
    #[serde(skip)]
    pub cli_agents: Vec<atelier_agent::config::AgentDefinition>,
    #[serde(skip)]
    pub cli_agent_overrides: CliAgentOverrides,
    /// Whether subagent (task tool) support is enabled. Enabled by default;
    /// disabled only via `ATELIER_SUBAGENTS=0` or `[subagents] enabled = false`.
    /// Not remotely gated.
    #[serde(skip)]
    pub subagents_enabled: bool,
    /// Per-subagent model ID overrides from `[subagents.models]` in config.toml.
    /// Keys are agent names, values are model IDs. Set alongside `subagents_enabled`
    /// from `SubagentsConfig::resolve()`.
    #[serde(skip)]
    pub subagent_model_overrides: std::collections::HashMap<String, String>,
    /// Per-subagent enable/disable toggles from `[subagents.toggle]` in config.toml.
    /// Keys are agent names, values are booleans. Omitted agents default to enabled.
    #[serde(skip)]
    pub subagent_toggle: std::collections::HashMap<String, bool>,
    /// Per-subagent role definitions from `[subagents.roles]` in config.toml
    /// and `.atelier/roles/*.toml` file discovery.
    #[serde(skip)]
    pub subagent_roles:
        std::collections::HashMap<String, atelier_subagent_resolution::config::SubagentRole>,
    #[serde(skip)]
    pub subagent_personas:
        std::collections::HashMap<String, atelier_subagent_resolution::config::SubagentPersona>,
    /// Whether web search is force-disabled via `--disable-web-search` CLI flag.
    /// When true, the web search tool is never added to the agent toolset
    /// regardless of available credentials.
    #[serde(default)]
    pub disable_web_search: bool,
    /// Whether the runtime turn-end TodoGate is force-enabled via the
    /// `--todo-gate` CLI flag. Session-scoped — not persisted. When
    /// true, flips the runtime policy's `enabled` bit on regardless of
    /// remote settings or the built-in default (which is `false`).
    /// The gate runs only while a `/goal` is active (goal reminders
    /// inject `<task_completion_discipline>`); global built-in templates
    /// do not activate it.
    #[serde(skip)]
    pub todo_gate: bool,
    /// Path for the Layer-3 LazinessDetector debug log
    /// (`--laziness-debug-log`). When `Some`, the classifier fires
    /// after every turn (bypassing the idle wait, the per-model
    /// enable gate, and the nudge cap) and appends a JSONL line per
    /// fire to this file. Observation-only — no nudges are injected
    /// in this mode. Session-scoped, not persisted.
    #[serde(skip)]
    pub laziness_debug_log: Option<std::path::PathBuf>,
    /// Whether tools should respect `.gitignore` patterns.
    /// When `true`, all tools including `read_file` block gitignored files.
    /// When `false` (default), each tool applies its own default
    /// (`read_file` allows, others block).
    /// Resolved by [`crate::config::ToolsConfig::resolve`].
    #[serde(skip)]
    pub respect_gitignore: bool,
    /// When `true`, `MvpAgent::prepare_video_gen_config` returns
    /// `VideoGenConfig::Disabled`, dropping `video_gen` (and any
    /// future ZDR-incompatible tools) from the model's tool set.
    /// Resolved by [`crate::config::ToolsConfig::resolve`].
    #[serde(skip)]
    pub disable_zdr_incompatible_tools: bool,
    /// S3 config for ZDR video output (presigned upload to team bucket).
    /// Only used when `disable_zdr_incompatible_tools` is `true` and the
    /// config is valid. Resolved by [`crate::config::ToolsConfig::resolve`].
    #[serde(skip)]
    pub zdr_video_output_s3:
        Option<atelier_tools::implementations::atelier_build::video_gen::ZdrVideoOutputS3Config>,
    /// Whether to enrich path-not-found errors with CWD reminders,
    /// "dropped repo folder" correction, and similar-name suggestions.
    /// Default `false`. Enabled via remote settings.
    /// Serialized to `config.json` on GCS so traces can distinguish
    /// which sessions had path-not-found hints active.
    #[serde(default)]
    pub path_not_found_hints: bool,
    /// Whether to fetch managed MCP configs from the managed connectors service at startup.
    /// Resolved by [`crate::config::ManagedMcpsConfig::resolve`]: env var >
    /// config.toml > remote settings > default (off in headless, on in interactive).
    #[serde(skip)]
    pub managed_mcps_enabled: bool,
    #[serde(skip)]
    pub managed_mcp_gateway_tools_enabled: bool,
    /// Whether auto-wake is enabled: when a background task or subagent
    /// completes, immediately inject a synthetic prompt instead of waiting
    /// for the idle-gated notification drain.
    #[serde(skip)]
    pub auto_wake_enabled: bool,
    /// Resolved vendor-compat config (env → `[compat]` TOML → feature flag →
    /// default ON), built from `compat` + `local_runtime_settings` in
    /// `resolve_runtime_fields`. Threaded into skills / rules / AGENTS.md
    /// discovery.
    #[serde(skip)]
    pub compat_resolved: CompatConfig,
    /// Enforced requirement pins from `requirements.toml`.
    #[serde(skip)]
    pub requirements: Requirements,
    /// Model ID for web_search.
    #[serde(skip)]
    pub web_search_model: String,
    /// Session title model. Resolved to the compiled default
    /// (`default_session_summary_model`) when unset; see `ModelOverrideConfig::resolve`.
    #[serde(skip)]
    pub session_summary_model: Option<String>,
    /// Image describe model (`atelier-build` default via `ModelOverrideConfig::resolve`).
    #[serde(skip)]
    pub image_description_model: Option<String>,
    /// Next-prompt suggestion model pin (`env > [models] prompt_suggestion >
    /// remote`), consumed catalog-guarded by `handle_suggest_prompt`; see
    /// `ModelOverrideConfig::resolve`.
    #[serde(skip)]
    pub prompt_suggest_model_pin: crate::config::PromptSuggestModelPin,
}
#[derive(Debug, Clone, Default)]
pub struct CliAgentOverrides {
    pub tools: Option<Vec<String>>,
    pub disallowed_tools: Option<Vec<String>>,
    pub permission_rules: Vec<atelier_workspace::permission::types::PermissionRule>,
    pub max_turns: Option<u32>,
    pub permission_mode: Option<atelier_agent::config::PermissionMode>,
}
impl CliAgentOverrides {
    /// Apply to the *main-session* agent, which the operator defines directly:
    /// the flags are authoritative, so they replace the agent's own fields.
    /// Spawned subagents instead layer these on top of an author's definition —
    /// see [`Self::apply_to_subagent_definition`].
    pub fn apply_to_definition(&self, def: &mut atelier_agent::config::AgentDefinition) {
        if let Some(ref tools) = self.tools {
            def.tools = tools.clone();
        }
        if let Some(ref dt) = self.disallowed_tools {
            def.disallowed_tools = dt.clone();
        }
        if let Some(ref pm) = self.permission_mode {
            def.permission_mode = pm.clone();
        }
    }
    /// Subagent variant of [`Self::apply_to_definition`]: records the flags as
    /// session-clamp state (see [`AgentDefinition::session_tools_allowlist`])
    /// instead of overwriting the agent author's own fields.
    pub fn apply_to_subagent_definition(&self, def: &mut atelier_agent::config::AgentDefinition) {
        def.session_tools_allowlist = self.tools.clone();
        def.session_tools_denylist = self.disallowed_tools.clone();
        if let Some(ref parent_mode) = self.permission_mode
            && def.plugin_name.is_none()
        {
            def.permission_mode =
                resolve_subagent_permission_mode(def.permission_mode.clone(), parent_mode);
        }
    }
    pub fn has_definition_overrides(&self) -> bool {
        self.tools.is_some() || self.disallowed_tools.is_some() || self.permission_mode.is_some()
    }
}
/// Parent bypassPermissions/acceptEdits/auto override the subagent's own mode
/// (spec); any other parent mode keeps it.
fn resolve_subagent_permission_mode(
    own: PermissionMode,
    parent: &PermissionMode,
) -> PermissionMode {
    match parent {
        PermissionMode::BypassPermissions | PermissionMode::AcceptEdits | PermissionMode::Auto => {
            parent.clone()
        }
        _ => own,
    }
}
pub use atelier_agent::config::AgentDefinition;
pub use atelier_agent::config::Effort;
pub use atelier_agent::config::PermissionMode;
pub use atelier_shared::ui_config::{ContextualHints, UiConfig};
/// Configuration for selecting the agent definition.
///
/// Set in `config.toml` under `[agent]`:
///
/// ```toml
/// [agent]
/// # Use a named agent (looked up via discovery: .atelier/agents/, ~/.atelier/agents/, built-ins)
/// name = "my-custom-agent"
///
/// # OR: path to an agent definition file (.md with YAML frontmatter)
/// definition = "/path/to/my-agent.md"
/// ```
///
/// Priority (highest to lowest):
/// 1. ACP session-level `_meta.agentProfile`
/// 2. CLI `--agent-profile` flag
/// 3. `[agent]` config.toml section (this config)
/// 4. `ATELIER_AGENT` env var
/// 5. Default `atelier-build` agent
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSelectionConfig {
    /// Name of a built-in or discovered agent definition.
    /// Looked up via `atelier_agent::discovery::by_name_in_cwd()`.
    /// Examples: "atelier-build", "browser-use", or a custom agent name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Path to an agent definition file (.md with YAML frontmatter).
    /// When set, the agent is loaded from this file.
    /// Supports environment variable expansion (e.g., `$HOME/.atelier/agents/my-agent.md`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<PathBuf>,
    /// Global system-prompt identity label. Per-model override wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_label: Option<String>,
}
/// Configuration for session behavior.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SessionConfig {
    /// Context window usage percentage (0-100) at which auto-compact is triggered.
    /// When the session's token usage exceeds this percentage of the model's context window,
    /// the conversation will be automatically summarized to free up space.
    ///
    /// `None` means "user didn't set it"; the resolver in
    /// `crate::util::config::resolve_auto_compact_threshold_percent` falls
    /// through to remote tiers and ultimately the hardcoded default 85.
    /// Read this field via the resolver — not directly — to honor the full
    /// precedence chain (env, per-model, remote, default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold_percent: Option<u8>,
    /// Whether to load environment variables from .envrc files.
    /// When enabled, the session will parse .envrc in the workspace directory
    /// and inject the environment variables into bash commands.
    /// Defaults to `true` when unset. `Option<bool>` so `None`
    /// round-trips as absent on disk (managed config wins over default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_envrc: Option<bool>,
}
impl Default for Config {
    fn default() -> Self {
        let endpoints = EndpointsConfig::default();
        let mut cfg = Self {
            features: Features::default(),
            model: None,
            context: None,
            request_agent: None,
            goal: GoalConfig::default(),
            doom_loop_recovery: crate::util::config::DoomLoopRecoverySettings::default(),
            auto_mode: AutoModeConfig::default(),
            config_models: IndexMap::new(),
            model_override_warnings: Vec::new(),
            atelier_com_config: AtelierComConfig::default(),
            shortcuts: None,
            hints: None,
            ui: UiConfig::default(),
            toolset: ShellToolsetConfig::default(),
            endpoints,
            telemetry: TelemetryConfig::default(),
            session: SessionConfig::default(),
            agent: AgentSelectionConfig::default(),
            skills: SkillsConfig::default(),
            compat: CompatConfigToml::default(),
            plugins: PluginsConfig::default(),
            feedback: FeedbackConfig::default(),
            paths: PathsConfig::default(),
            cli: CliConfig::default(),
            models: ModelsConfig::default(),
            harness: HarnessConfig::default(),
            relay: RelayConfig::default(),
            remote: RemoteConfig::default(),
            hub: HubConfig::default(),
            worktree_pool: WorktreePoolConfig::default(),
            sandbox: SandboxSettingsConfig::default(),
            mcp_servers: std::collections::HashMap::new(),
            disabled_mcp_servers: Vec::new(),
            disabled_mcp_tools: std::collections::HashMap::new(),
            subagents: crate::config::SubagentsConfig::default(),
            memory: crate::config::MemoryConfig::default(),
            compaction: CompactionConfig::default(),
            managed_mcps: crate::config::ManagedMcpsConfig::default(),
            auth: None,
            desktop: None,
            announcements: Vec::new(),
            tips: None,
            permission: PermissionKnownKeys::default(),
            tools: crate::config::ToolsConfig::default(),
            storage: StorageConfig::default(),
            suggestions: SuggestionsConfig::default(),
            marketplace: MarketplaceConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            storage_mode: StorageMode::resolve(None, None),
            default_model_override: None,
            reasoning_effort_override: None,
            web_search_model_override: None,
            session_summary_model_override: None,
            default_yolo_mode: false,
            default_auto_mode: false,
            agent_profile_path: None,
            client_version: Some(atelier_version::VERSION.to_string()),
            mode: AgentMode::default(),
            local_runtime_settings: None,
            cli_agents: Vec::new(),
            cli_agent_overrides: CliAgentOverrides::default(),
            subagents_enabled: true,
            subagent_model_overrides: std::collections::HashMap::new(),
            subagent_toggle: std::collections::HashMap::new(),
            subagent_roles: std::collections::HashMap::new(),
            subagent_personas: std::collections::HashMap::new(),
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            respect_gitignore: false,
            disable_zdr_incompatible_tools: false,
            zdr_video_output_s3: None,
            path_not_found_hints: false,
            cli_experimental_memory: false,
            cli_no_memory: false,
            cli_subagents: None,
            memory_config: None,
            managed_mcps_enabled: true,
            managed_mcp_gateway_tools_enabled: false,
            auto_wake_enabled: true,
            compat_resolved: CompatConfig::default(),
            requirements: Requirements::default(),
            web_search_model: crate::models::default_web_search_model().to_owned(),
            session_summary_model: None,
            image_description_model: None,
            prompt_suggest_model_pin: crate::config::PromptSuggestModelPin::Unpinned,
        };
        cfg.apply_env_overrides();
        cfg
    }
}
impl Config {
    /// Reject invalid glob patterns in the model-filter lists at config load, so
    /// a typo fails loudly instead of silently changing availability.
    pub fn validate_model_filters(&self) -> Result<(), String> {
        for (field, list) in [
            ("allowed_models", &self.models.allowed_models),
            ("disabled_models", &self.models.disabled_models),
            ("hidden_models", &self.models.hidden_models),
        ] {
            if let Err(bad) = crate::agent::models::ModelGlobSet::compile(list.as_ref()) {
                return Err(format!(
                    "{field} has an invalid pattern: {}. Patterns use * and ? wildcards.",
                    bad.join(", ")
                ));
            }
        }
        Ok(())
    }
    /// Build the local-only credential manager used by Provider-backed
    /// sessions. Vendor OAuth and legacy `auth.json` credentials are never
    /// loaded by the production runtime.
    pub fn create_auth_manager(&self) -> AuthManager {
        AuthManager::new_local(&crate::util::atelier_home::atelier_home())
            .with_proxy_base_url(&self.endpoints.proxy_url())
    }
    /// Deserialize the merged `base` document, also returning the ignored key
    /// paths whose top-level key appears in `user_config`. Paths outside it
    /// can only come from the serialized-defaults half of the merge and must
    /// not be blamed on the user.
    fn deserialize_collecting_unrecognized(
        base: toml::Value,
        user_config: &toml::Value,
    ) -> Result<(Self, Vec<String>), String> {
        let mut unused_keys = Vec::new();
        let config: Self = serde_ignored::deserialize(base, |path| {
            unused_keys.push(path.to_string());
        })
        .map_err(|e| e.to_string())?;
        let user_unused = match user_config.as_table() {
            Some(user_table) => unused_keys
                .into_iter()
                .filter(|path| {
                    let top_level = path.split('.').next().unwrap_or(path);
                    user_table.contains_key(top_level)
                })
                .collect(),
            None => Vec::new(),
        };
        Ok((config, user_unused))
    }
    pub fn new_from_toml_cfg(raw_config: &toml::Value) -> Result<Self, String> {
        let raw_config = &Self::expand_auth_alias(raw_config);
        let runtime_model = raw_config
            .get("model")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(ToOwned::to_owned);
        let super::config_model_override_parse::ParsedModelOverrides {
            models: config_models,
            warnings: model_override_warnings,
        } = if raw_config.get("model").is_some_and(toml::Value::is_table) {
            super::config_model_override_parse::parse_model_overrides(raw_config)
        } else {
            super::config_model_override_parse::ParsedModelOverrides {
                models: IndexMap::new(),
                warnings: Vec::new(),
            }
        };
        super::config_model_override_parse::log_model_override_warnings(&model_override_warnings);
        let mut base = toml::Value::try_from(Self::default()).map_err(|e| e.to_string())?;
        if let toml::Value::Table(ref mut t) = base {
            t.remove("model");
        }
        let mut raw_without_model_sections = raw_config.clone();
        if let toml::Value::Table(ref mut t) = raw_without_model_sections {
            t.remove("model");
        }
        crate::config::deep_merge_toml(&mut base, &raw_without_model_sections);
        let (mut config, user_unused) =
            Self::deserialize_collecting_unrecognized(base, &raw_without_model_sections)?;
        if !user_unused.is_empty() {
            let keys = user_unused.join(", ");
            tracing::warn!(
                "config has unrecognized key(s): {keys}. Run /help for config reference."
            );
        }
        config.config_models = config_models;
        config.model_override_warnings = model_override_warnings;
        config.model = runtime_model;
        if config.atelier_com_config.oidc.is_none() {
            config.atelier_com_config.oidc = OidcAuthConfig::from_env();
        }
        if config.atelier_com_config.oidc.is_none() && config.atelier_com_config.oauth2.is_none() {
            config.atelier_com_config.oauth2 = crate::auth::OAuth2ProviderConfig::from_env();
        }
        if config.client_version.is_none() {
            config.client_version = Self::default().client_version;
        }
        let model_overrides =
            crate::config::ModelOverrideConfig::resolve(None, None, raw_config, None);
        config.web_search_model = model_overrides.web_search;
        config.session_summary_model = model_overrides.session_summary;
        config.image_description_model = model_overrides.image_description;
        config.prompt_suggest_model_pin = model_overrides.prompt_suggestion;
        config.apply_env_overrides();
        Ok(config)
    }
    /// Populate `#[serde(skip)]` subagent fields from `SubagentsConfig::resolve()`.
    ///
    /// Must be called after `new_from_toml_cfg` on the **primary startup path**
    /// before the config is handed to `MvpAgent`. Model-reload and API-key-reload
    /// paths only read model/key fields and do not need this call.
    pub fn resolve_subagents(
        &mut self,
        cli_flag: bool,
        raw_config: &toml::Value,
        cwd: Option<&std::path::Path>,
    ) {
        let sa = crate::config::SubagentsConfig::resolve(cli_flag, raw_config, cwd);
        self.subagents_enabled = sa.enabled;
        self.subagent_model_overrides = sa.models;
        self.subagent_toggle = sa.toggle;
        self.subagent_roles = sa.roles;
        self.subagent_personas = sa.personas;
    }
    /// Resolve all `#[serde(skip)]` runtime fields that have resolver functions.
    ///
    /// Call immediately after `new_from_toml_cfg()`. Fields resolved:
    /// - subagents (6 fields) via `SubagentsConfig::resolve`
    /// - respect_gitignore via `ToolsConfig::resolve`
    /// - disable_zdr_incompatible_tools via `ToolsConfig::resolve`
    /// - managed_mcps_enabled via `ManagedMcpsConfig::resolve`
    /// - web_search_model / session_summary_model / image_description_model /
    ///   prompt_suggest_model_pin via `ModelOverrideConfig::resolve`
    /// - memory_config via `MemoryConfig::resolve`
    /// - disable_web_search (CLI flag ORed with config.toml)
    /// - storage_mode via `StorageMode::resolve`
    /// - path_not_found_hints from local_runtime_settings
    ///
    /// Note: `worktree_type` is resolved directly in `MvpAgent::new` via
    /// `resolve_worktree_type` since it's an agent-level field, not a Config field.
    pub fn resolve_runtime_fields(&mut self, ctx: &RuntimeResolutionContext<'_>) {
        self.cli_subagents = ctx.cli_subagents;
        self.web_search_model_override = ctx.cli_web_search_model.map(|s| s.to_owned());
        self.session_summary_model_override = ctx.cli_session_summary_model.map(|s| s.to_owned());
        let cli_flag = ctx.cli_subagents.unwrap_or(false);
        self.resolve_subagents(cli_flag, ctx.raw_config, ctx.cwd);
        let tools = crate::config::ToolsConfig::resolve(ctx.raw_config);
        self.respect_gitignore = match self.requirements.respect_gitignore.pinned() {
            Some(pinned) => pinned,
            None => tools.respect_gitignore,
        };
        self.disable_zdr_incompatible_tools = tools.disable_zdr_incompatible_tools;
        self.zdr_video_output_s3 = tools.zdr_video_output_s3;
        let mcps = crate::config::ManagedMcpsConfig::resolve(
            ctx.raw_config,
            ctx.local_runtime_settings,
            ctx.is_headless,
        );
        self.managed_mcps_enabled = mcps.enabled;
        self.managed_mcp_gateway_tools_enabled = mcps.gateway_tools_enabled;
        let models = crate::config::ModelOverrideConfig::resolve(
            ctx.cli_web_search_model,
            ctx.cli_session_summary_model,
            ctx.raw_config,
            ctx.local_runtime_settings,
        );
        self.web_search_model = models.web_search;
        self.session_summary_model = models.session_summary;
        self.image_description_model = models.image_description;
        self.prompt_suggest_model_pin = models.prompt_suggestion;
        self.cli_experimental_memory = ctx.cli_experimental_memory;
        self.cli_no_memory = ctx.cli_no_memory;
        let mem = crate::config::MemoryConfig::resolve(
            ctx.cli_experimental_memory,
            ctx.cli_no_memory,
            ctx.raw_config,
            ctx.local_runtime_settings,
        );
        self.memory_config = if mem.enabled { Some(mem) } else { None };
        self.disable_web_search = self.disable_web_search || ctx.disable_web_search;
        self.todo_gate = ctx.todo_gate;
        self.laziness_debug_log = ctx.laziness_debug_log.map(std::path::Path::to_path_buf);
        self.storage_mode =
            crate::config::StorageMode::resolve(ctx.storage_mode, ctx.local_runtime_settings);
        if let Some(v) = ctx
            .local_runtime_settings
            .and_then(|s| s.path_not_found_hints)
        {
            self.path_not_found_hints = v;
        }
        self.auto_wake_enabled = BoolFlag::env("ATELIER_AUTO_WAKE")
            .config(self.features.auto_wake)
            .feature_flag(ctx.local_runtime_settings.and_then(|r| r.auto_wake_enabled))
            .default(true)
            .resolve()
            .value;
        self.compat_resolved = resolve_compat_config(&self.compat, ctx.local_runtime_settings);
    }
    /// Re-resolve eagerly-resolved runtime fields using the current `Config`
    /// state and fresh `raw_config` + `cwd`. Builds a
    /// [`RuntimeResolutionContext`] from the CLI flags already stored on this
    /// `Config` so callers don't need to manually extract each field.
    ///
    /// Integration test coverage: `tests/test_settings_refresh.rs`.
    pub fn re_resolve_runtime_fields(
        &mut self,
        raw_config: &toml::Value,
        cwd: Option<&std::path::Path>,
    ) {
        let local_runtime_settings = self.local_runtime_settings.clone();
        let cli_web_search_model = self.web_search_model_override.clone();
        let cli_session_summary_model = self.session_summary_model_override.clone();
        let laziness_debug_log = self.laziness_debug_log.clone();
        let ctx = RuntimeResolutionContext {
            raw_config,
            local_runtime_settings: local_runtime_settings.as_ref(),
            cwd,
            is_headless: self.mode == AgentMode::Headless,
            cli_subagents: self.cli_subagents,
            cli_web_search_model: cli_web_search_model.as_deref(),
            cli_session_summary_model: cli_session_summary_model.as_deref(),
            cli_experimental_memory: self.cli_experimental_memory,
            cli_no_memory: self.cli_no_memory,
            disable_web_search: self.disable_web_search,
            todo_gate: self.todo_gate,
            laziness_debug_log: laziness_debug_log.as_deref(),
            storage_mode: None,
        };
        self.resolve_runtime_fields(&ctx);
    }
    /// If the TOML contains `[auth]`, copy its contents under `[atelier_com_config]`.
    /// `[atelier_com_config]` takes precedence if both are present (explicit wins).
    ///
    /// This lets customers write the shorter `[auth.oidc]` instead of `[atelier_com_config.oidc]`.
    fn expand_auth_alias(raw_config: &toml::Value) -> toml::Value {
        let mut config = raw_config.clone();
        if let toml::Value::Table(ref mut table) = config
            && let Some(auth) = table.remove("auth")
        {
            if let Some(gcc) = table.get_mut("atelier_com_config") {
                if let (toml::Value::Table(gcc_table), toml::Value::Table(auth_table)) =
                    (gcc, &auth)
                {
                    for (k, v) in auth_table {
                        gcc_table.entry(k.clone()).or_insert(v.clone());
                    }
                }
            } else {
                table.insert("atelier_com_config".to_owned(), auth);
            }
        }
        config
    }
    fn apply_env_overrides(&mut self) {
        if let Some(mode) = env_telemetry_mode("ATELIER_TELEMETRY_ENABLED") {
            self.features.telemetry = Some(mode);
        }
    }
    pub fn is_telemetry_enabled(&self) -> bool {
        self.resolve_telemetry_mode().value.is_enabled()
    }
    pub fn is_feedback_enabled(&self) -> bool {
        self.resolve_feedback().value
    }
    pub fn is_session_recap_enabled(&self) -> bool {
        self.resolve_session_recap().value
    }
    pub fn is_voice_mode_enabled(&self) -> bool {
        self.resolve_voice_mode().value
    }
    /// Two-pass (prefire) compaction gate. Default OFF (opt-in) — enable via
    /// remote settings `two_pass_compaction_enabled`, the `[features] two_pass_compaction`
    /// config.toml key, or `ATELIER_TWO_PASS_COMPACTION` env.
    pub fn is_two_pass_compaction_enabled(&self) -> bool {
        self.resolve_two_pass_compaction().value
    }
    pub(crate) fn resolve_telemetry_mode(&self) -> Resolved<TelemetryMode> {
        if let Some(mode) = self.requirements.telemetry.pinned() {
            return Resolved::new(mode, ConfigSource::Requirement);
        }
        if let Some(mode) = env_telemetry_mode("ATELIER_TELEMETRY_ENABLED") {
            return Resolved::new(mode, ConfigSource::Env);
        }
        if let Some(mode) = self.features.telemetry {
            return Resolved::new(mode, ConfigSource::Config);
        }
        Resolved::new(TelemetryMode::Disabled, ConfigSource::Default)
    }
    /// Resolve jemalloc heap-profile config from stored remote settings + gates.
    pub fn resolve_jemalloc_heap_profile(
        &self,
        data_collection_disabled: bool,
    ) -> crate::heap_profile::JemallocHeapProfileConfig {
        let rs = self.local_runtime_settings.as_ref();
        crate::heap_profile::resolve_jemalloc_heap_profile(
            rs.and_then(|s| s.jemalloc_heap_profile_enabled),
            rs.and_then(|s| s.jemalloc_heap_profile_thresholds_bytes.as_deref()),
            rs.and_then(|s| s.jemalloc_heap_profile_poll_interval_secs),
            data_collection_disabled,
            crate::heap_profile::prof_available(),
        )
    }
    /// K12 scoped resolve: fresh jemalloc fields + current gates (no remote rewrite).
    pub fn resolve_jemalloc_heap_profile_from_partial(
        &self,
        jemalloc_enabled: Option<bool>,
        jemalloc_thresholds: Option<&[u64]>,
        jemalloc_poll_interval_secs: Option<u64>,
        data_collection_disabled: bool,
    ) -> crate::heap_profile::JemallocHeapProfileConfig {
        crate::heap_profile::resolve_jemalloc_heap_profile(
            jemalloc_enabled,
            jemalloc_thresholds,
            jemalloc_poll_interval_secs,
            data_collection_disabled,
            crate::heap_profile::prof_available(),
        )
    }
    pub(crate) fn resolve_feedback(&self) -> Resolved<bool> {
        let _ = self;
        Resolved::new(false, ConfigSource::Default)
    }
    pub(crate) fn resolve_two_pass_compaction(&self) -> Resolved<bool> {
        let ff = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.two_pass_compaction_enabled);
        BoolFlag::env("ATELIER_TWO_PASS_COMPACTION")
            .config(self.features.two_pass_compaction)
            .feature_flag(ff)
            .default(false)
            .resolve()
    }
    /// Server-side doom-loop check policy (the `x-atelier-doom-loop-check`
    /// header, trigger parsing, and confident-signal resampling, all
    /// applied by the sampler). Merged
    /// PER-FIELD across the `[doom_loop_recovery]` TOML table and the
    /// remote settings `doom_loop_recovery` object (a partial remote object only
    /// overrides the fields it sets). Gate precedence: env
    /// `ATELIER_DOOM_LOOP_RECOVERY` > TOML `enabled` > remote `enabled` >
    /// default off — `None` IS the off state, so disabled has exactly one
    /// spelling. Tunables have no env layer (TOML > remote > default) and
    /// are clamped to their documented ranges. Returns the composite runtime
    /// policy rather than `Resolved` because each knob resolves from its own
    /// source (the `resolve_reminder_policy` pattern).
    pub(crate) fn resolve_doom_loop_recovery(
        &self,
    ) -> Option<atelier_sampling_types::DoomLoopRecoveryPolicy> {
        use atelier_sampling_types::DoomLoopRecoveryPolicy as Policy;
        let remote = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.doom_loop_recovery.as_ref());
        let enabled = BoolFlag::env("ATELIER_DOOM_LOOP_RECOVERY")
            .config(self.doom_loop_recovery.enabled)
            .feature_flag(remote.and_then(|s| s.enabled))
            .default(false)
            .resolve()
            .value;
        enabled.then(|| Policy {
            max_threshold: self
                .doom_loop_recovery
                .max_threshold
                .or(remote.and_then(|s| s.max_threshold))
                .map_or(Policy::DEFAULT_MAX_THRESHOLD, Policy::clamp_max_threshold),
            max_retries: self
                .doom_loop_recovery
                .max_retries
                .or(remote.and_then(|s| s.max_retries))
                .map_or(Policy::DEFAULT_MAX_RETRIES, Policy::clamp_max_retries),
        })
    }
    /// Official marketplace auto-registration is removed. User-configured
    /// marketplace sources remain explicit opt-in operations.
    pub(crate) fn resolve_official_marketplace_auto_register(&self) -> Resolved<bool> {
        Resolved::new(false, ConfigSource::Default)
    }
    pub(crate) fn resolve_lsp_tools(&self) -> Resolved<bool> {
        let ff = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.lsp_tools_enabled);
        BoolFlag::env("ATELIER_LSP_TOOLS")
            .requirement(self.requirements.lsp_tools.pinned())
            .config(self.features.lsp_tools)
            .feature_flag(ff)
            .resolve()
    }
    pub(crate) fn resolve_web_fetch(&self) -> Resolved<bool> {
        let ff = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.web_fetch_enabled);
        BoolFlag::env("ATELIER_WEB_FETCH")
            .requirement(self.requirements.web_fetch.pinned())
            .config(self.features.web_fetch)
            .feature_flag(ff)
            .resolve()
    }
    /// `ask_user_question` tool gate; default ON. remote settings
    /// `ask_user_question_enabled: false` (or `[features]` / env) is a remote
    /// kill-switch. The `_meta.askUserQuestion` override (`--no-ask-user`) is
    /// applied at the spawn site and outranks this resolver.
    pub(crate) fn resolve_ask_user_question(&self) -> Resolved<bool> {
        let ff = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.ask_user_question_enabled);
        BoolFlag::env("ATELIER_ASK_USER_QUESTION")
            .requirement(self.requirements.ask_user_question.pinned())
            .config(self.features.ask_user_question)
            .feature_flag(ff)
            .default(true)
            .resolve()
    }
    /// Session recap gate (the `/recap` command + automatic return-from-away
    /// recap). Default ON — disable via remote settings `session_recap`, the
    /// `[features] session_recap` config.toml key, or `ATELIER_SESSION_RECAP` env.
    pub(crate) fn resolve_session_recap(&self) -> Resolved<bool> {
        let ff = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.session_recap);
        BoolFlag::env("ATELIER_SESSION_RECAP")
            .config(self.features.session_recap)
            .feature_flag(ff)
            .default(true)
            .resolve()
    }
    /// Voice dictation gate. Default on.
    ///
    /// Precedence: requirements > `ATELIER_VOICE_MODE` > config/managed
    /// `[features] voice_mode` > remote `voice_mode_enabled` > default true.
    /// The pager may force API-key sessions on when only remote is off.
    pub(crate) fn resolve_voice_mode(&self) -> Resolved<bool> {
        let ff = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.voice_mode_enabled);
        BoolFlag::env("ATELIER_VOICE_MODE")
            .requirement(self.requirements.voice_mode.pinned())
            .config(self.features.voice_mode)
            .feature_flag(ff)
            .default(true)
            .resolve()
    }
    /// Vendor-hosted image editing was removed from Atelier.
    pub(crate) fn resolve_image_edit(&self) -> Resolved<bool> {
        Resolved::new(false, ConfigSource::Default)
    }
    /// Goal mode (`/goal`) master switch. Default ON.
    /// Env, `[goal] enabled`, and an optional process-local runtime override
    /// (`Some(false)` as a kill-switch) take precedence over the default.
    pub(crate) fn resolve_goal(&self) -> Resolved<bool> {
        let ff = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.goal_enabled);
        BoolFlag::env("ATELIER_GOAL")
            .config(self.goal.enabled)
            .feature_flag(ff)
            .default(true)
            .resolve()
    }
    /// Classifier, planner, and summary all default to goal mode itself: when
    /// `/goal` is on they are on unless config/env/remote says otherwise.
    /// `goal_enabled` is the session's already-resolved master switch (the same
    /// value the actor stores), passed in so a sub-role default can never
    /// disagree with whether `/goal` is on.
    pub(crate) fn resolve_goal_classifier_enabled(&self, goal_enabled: bool) -> Resolved<bool> {
        BoolFlag::env("ATELIER_GOAL_CLASSIFIER")
            .config(self.goal.classifier_enabled)
            .feature_flag(
                self.local_runtime_settings
                    .as_ref()
                    .and_then(|s| s.goal_classifier_enabled),
            )
            .default(goal_enabled)
            .resolve()
    }
    pub(crate) fn resolve_goal_planner_enabled(&self, goal_enabled: bool) -> Resolved<bool> {
        BoolFlag::env("ATELIER_GOAL_PLANNER")
            .config(self.goal.planner_enabled)
            .feature_flag(
                self.local_runtime_settings
                    .as_ref()
                    .and_then(|s| s.goal_planner_enabled),
            )
            .default(goal_enabled)
            .resolve()
    }
    pub(crate) fn resolve_goal_summary_enabled(&self, goal_enabled: bool) -> Resolved<bool> {
        BoolFlag::env("ATELIER_GOAL_SUMMARY")
            .config(self.goal.summary_enabled)
            .feature_flag(
                self.local_runtime_settings
                    .as_ref()
                    .and_then(|s| s.goal_summary_enabled),
            )
            .default(goal_enabled)
            .resolve()
    }
    /// Goal count resolver: env(parse) > config > remote > default, then clamp.
    /// An unparseable env value falls through to the next source.
    fn resolve_goal_u32(
        env_var: &str,
        config: Option<u32>,
        remote: Option<u32>,
        default: u32,
        clamp: impl Fn(u32) -> u32,
    ) -> Resolved<u32> {
        if let Some(env_value) = env_string(env_var)
            && let Ok(parsed) = env_value.parse::<u32>()
        {
            return Resolved::new(clamp(parsed), ConfigSource::Env);
        }
        if let Some(v) = config {
            return Resolved::new(clamp(v), ConfigSource::Config);
        }
        if let Some(v) = remote {
            return Resolved::new(clamp(v), ConfigSource::Remote);
        }
        Resolved::new(default, ConfigSource::Default)
    }
    /// Per-attempt adversarial-skeptic count, clamped to
    /// `[GOAL_VERIFIER_SKEPTIC_MIN, GOAL_VERIFIER_SKEPTIC_MAX]`.
    pub(crate) fn resolve_goal_verifier_count(&self) -> Resolved<u32> {
        use crate::session::goal_classifier::{
            GOAL_VERIFIER_SKEPTIC_COUNT, GOAL_VERIFIER_SKEPTIC_MAX, GOAL_VERIFIER_SKEPTIC_MIN,
        };
        Self::resolve_goal_u32(
            "ATELIER_GOAL_VERIFIER_N",
            self.goal.verifier_count,
            self.local_runtime_settings
                .as_ref()
                .and_then(|s| s.goal_verifier_count),
            GOAL_VERIFIER_SKEPTIC_COUNT,
            |v| v.clamp(GOAL_VERIFIER_SKEPTIC_MIN, GOAL_VERIFIER_SKEPTIC_MAX),
        )
    }
    /// Per-goal classifier run cap, floored at `GOAL_CLASSIFIER_MAX_RUNS_MIN`
    /// with no upper ceiling.
    pub(crate) fn resolve_goal_classifier_max_runs(&self) -> Resolved<u32> {
        use crate::session::goal_classifier::{
            GOAL_CLASSIFIER_MAX_RUNS_DEFAULT, GOAL_CLASSIFIER_MAX_RUNS_MIN,
        };
        Self::resolve_goal_u32(
            "ATELIER_GOAL_CLASSIFIER_MAX",
            self.goal.classifier_max_runs,
            self.local_runtime_settings
                .as_ref()
                .and_then(|s| s.goal_classifier_max_runs),
            GOAL_CLASSIFIER_MAX_RUNS_DEFAULT,
            |v| v.max(GOAL_CLASSIFIER_MAX_RUNS_MIN),
        )
    }
    /// Stall-triggered strategist cadence N (fires every N consecutive
    /// `NotAchieved`). Default tracks the resolved classifier cap
    /// (`max(1, cap / 2)`); floored at 1 so it can never silently disable.
    pub(crate) fn resolve_goal_strategist_every(&self, classifier_max_runs: u32) -> Resolved<u32> {
        Self::resolve_goal_u32(
            "ATELIER_GOAL_STRATEGIST_EVERY",
            self.goal.strategist_every,
            self.local_runtime_settings
                .as_ref()
                .and_then(|s| s.goal_strategist_every),
            (classifier_max_runs / 2).max(1),
            |v| v.max(1),
        )
    }
    /// Re-verify escalation threshold; floored at 1. No remote layer.
    pub(crate) fn resolve_goal_reverify_after(&self) -> Resolved<u32> {
        Self::resolve_goal_u32(
            "ATELIER_GOAL_REVERIFY_AFTER",
            self.goal.reverify_after,
            None,
            crate::session::acp_session::GOAL_REVERIFY_AFTER_DEFAULT,
            |v| v.max(1),
        )
    }
    /// When `true`, every `/goal` role inherits the current model regardless of
    /// configured pairs.
    pub(crate) fn resolve_goal_use_current_model_only(&self) -> Resolved<bool> {
        BoolFlag::env("ATELIER_GOAL_USE_CURRENT_MODEL_ONLY")
            .config(self.goal.use_current_model_only)
            .default(false)
            .resolve()
    }
    /// Shared single-pair resolution. Precedence: kill-switch ⇒
    /// `InheritCurrent`/`Config` > `config_pair` ⇒ `Explicit`/`Config` >
    /// `remote_pair` ⇒ `Explicit`/`Remote` > `InheritCurrent`/`Default`. The
    /// chosen pair is cloned only on its branch.
    fn resolve_single_role_model(
        use_current_only: bool,
        config_pair: Option<&crate::util::config::GoalRoleModel>,
        remote_pair: Option<&crate::util::config::GoalRoleModel>,
    ) -> Resolved<GoalRoleModelChoice> {
        if use_current_only {
            return Resolved::new(GoalRoleModelChoice::InheritCurrent, ConfigSource::Config);
        }
        if let Some(pair) = config_pair {
            return Resolved::new(
                GoalRoleModelChoice::Explicit(pair.clone()),
                ConfigSource::Config,
            );
        }
        match remote_pair {
            Some(pair) => Resolved::new(
                GoalRoleModelChoice::Explicit(pair.clone()),
                ConfigSource::Remote,
            ),
            None => Resolved::new(GoalRoleModelChoice::InheritCurrent, ConfigSource::Default),
        }
    }
    /// Planner role model: `[goal]` config then remote. No env layer (only the
    /// kill-switch reads env).
    ///
    /// An `Explicit` pair is applied as `runtime_overrides.model`, resolved before
    /// `resolve_subagent_sampling_config`, so it wins over a user
    /// `[subagents.models]` pin; `InheritCurrent` hands precedence back to that pin.
    pub(crate) fn resolve_goal_planner_model(
        &self,
        use_current_only: bool,
    ) -> Resolved<GoalRoleModelChoice> {
        Self::resolve_single_role_model(
            use_current_only,
            self.goal.planner_model.as_ref(),
            self.local_runtime_settings
                .as_ref()
                .and_then(|s| s.goal_planner_model.as_ref()),
        )
    }
    /// Strategist role model; same precedence as [`Self::resolve_goal_planner_model`].
    pub(crate) fn resolve_goal_strategist_model(
        &self,
        use_current_only: bool,
    ) -> Resolved<GoalRoleModelChoice> {
        Self::resolve_single_role_model(
            use_current_only,
            self.goal.strategist_model.as_ref(),
            self.local_runtime_settings
                .as_ref()
                .and_then(|s| s.goal_strategist_model.as_ref()),
        )
    }
    /// Skeptic pool; same precedence as [`Self::resolve_goal_planner_model`] but
    /// over a pool. Pool order is preserved for the round-robin expansion in
    /// `expand_skeptic_assignment`.
    pub(crate) fn resolve_goal_skeptic_models(
        &self,
        use_current_only: bool,
    ) -> Resolved<Vec<GoalRoleModelChoice>> {
        if use_current_only {
            return Resolved::new(Vec::new(), ConfigSource::Config);
        }
        let to_choices = |pool: &[crate::util::config::GoalRoleModel]| {
            pool.iter()
                .cloned()
                .map(GoalRoleModelChoice::Explicit)
                .collect::<Vec<_>>()
        };
        if !self.goal.skeptic_models.is_empty() {
            return Resolved::new(to_choices(&self.goal.skeptic_models), ConfigSource::Config);
        }
        match self
            .local_runtime_settings
            .as_ref()
            .map(|s| s.goal_skeptic_models.as_slice())
        {
            Some(pool) if !pool.is_empty() => Resolved::new(to_choices(pool), ConfigSource::Remote),
            _ => Resolved::new(Vec::new(), ConfigSource::Default),
        }
    }
    pub(crate) fn resolve_write_file(&self) -> Resolved<bool> {
        let ff = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.write_file_enabled);
        BoolFlag::env("ATELIER_WRITE_FILE")
            .requirement(self.requirements.write_file.pinned())
            .config(self.features.write_file)
            .feature_flag(ff)
            .default(true)
            .resolve()
    }
    pub(crate) fn resolve_backend_tools(&self) -> Resolved<bool> {
        BoolFlag::env("ATELIER_BACKEND_SEARCH")
            .config(self.features.backend_tools)
            .default(true)
            .resolve()
    }
    /// Resolve the mode (env `ATELIER_COMPACTION_MODE` > config > remote settings >
    /// default, unrecognized falling through) and, for `Segments`, attach the
    /// separately-resolved detail level.
    pub(crate) fn resolve_compaction_mode(&self) -> atelier_chat_state::CompactionMode {
        resolve_compaction_mode_from(
            env_string("ATELIER_COMPACTION_MODE").as_deref(),
            self.features.compaction_mode.as_deref(),
            self.local_runtime_settings
                .as_ref()
                .and_then(|r| r.compaction_mode.as_deref()),
        )
        .with_segment_detail(self.resolve_compaction_detail())
    }
    /// Resolve verbatim-input flag: env `ATELIER_COMPACTION_VERBATIM_INPUT` > config > remote settings > default `true`.
    pub(crate) fn resolve_compaction_verbatim_input(&self) -> bool {
        BoolFlag::env("ATELIER_COMPACTION_VERBATIM_INPUT")
            .config(self.features.compaction_verbatim_input)
            .feature_flag(
                self.local_runtime_settings
                    .as_ref()
                    .and_then(|r| r.compaction_verbatim_input),
            )
            .default(true)
            .resolve()
            .value
    }
    /// Precedence: env `ATELIER_COMPACTION_DETAIL`, then config
    /// `features.compaction_detail`, then remote settings
    /// `local_runtime_settings.compaction_detail`, then default (`verbose`). Drives the
    /// `segments` verbatim detail level.
    fn resolve_compaction_detail(&self) -> atelier_chat_state::CompactionDetail {
        resolve_compaction_detail_from(
            env_string("ATELIER_COMPACTION_DETAIL").as_deref(),
            self.features.compaction_detail.as_deref(),
            self.local_runtime_settings
                .as_ref()
                .and_then(|r| r.compaction_detail.as_deref()),
        )
    }
    pub fn resolve_cancel_rewind(&self) -> Resolved<bool> {
        let ff = self
            .local_runtime_settings
            .as_ref()
            .and_then(|s| s.cancel_rewind_enabled);
        BoolFlag::env("ATELIER_CANCEL_REWIND")
            .config(self.features.cancel_rewind)
            .feature_flag(ff)
            .default(true)
            .resolve()
    }
    /// Resolve whether to use an explicitly configured OAuth2 provider.
    ///
    /// Enterprise OIDC (`oidc` in config.toml) always wins. Atelier does not
    /// enable OAuth without an explicit CLI or environment setting.
    ///
    /// Priority: `--oauth` > ATELIER_OAUTH_ENABLED env > default (off).
    pub fn resolve_atelier_oauth(&self, cli_oidc: Option<bool>) -> Resolved<bool> {
        BoolFlag::env("ATELIER_OAUTH_ENABLED")
            .cli(cli_oidc)
            .default(false)
            .resolve()
    }
    /// Resolve whether to spawn the per-`Ready`-client transport
    /// liveness pollers and the session-actor `StatusDispatcher`.
    ///
    /// Thin delegate to the canonical
    /// [`resolve_mcp_liveness_watchers`] free function, which unifies
    /// the two previous implementations so they can't drift. CLI / managed / feature-flag inputs are
    /// `None` here because the `Config` method only has visibility
    /// into the embedded `Features` table; richer call sites (e.g.
    /// the session-actor spawn path) go through
    /// [`crate::util::config::resolve_mcp_liveness_watchers`] which
    /// stacks all 7 layers.
    pub fn resolve_mcp_liveness_watchers(&self) -> Resolved<bool> {
        resolve_mcp_liveness_watchers(None, None, self.features.mcp_liveness_watchers, None, None)
    }
    /// Resolve whether the bounded stdio auto-restart task is allowed
    /// to fire. Thin delegate to
    /// [`resolve_mcp_auto_restart`]; mirrors
    /// [`Self::resolve_mcp_liveness_watchers`]. The 7-step precedence
    /// stack lives in the canonical free function. CLI / managed /
    /// feature-flag inputs are `None` here because the `Config`
    /// method only has visibility into the embedded `Features`
    /// table; richer call sites go through
    /// [`crate::util::config::resolve_mcp_auto_restart`] which stacks
    /// all 7 layers.
    pub fn resolve_mcp_auto_restart(&self) -> Resolved<bool> {
        resolve_mcp_auto_restart(None, None, self.features.mcp_auto_restart, None, None)
    }
    /// Resolve whether the pager subscribes to the per-server
    /// `atelier/mcp/server_status` push.
    ///
    /// Thin delegate to the canonical
    /// [`resolve_mcp_push_server_status`] free function — mirrors the
    /// `resolve_mcp_liveness_watchers` pattern so the two
    /// implementations can't drift. CLI / managed / feature-flag
    /// inputs are `None` here because the `Config` method only has
    /// visibility into the embedded `Features` table; richer call
    /// sites go through
    /// [`crate::util::config::resolve_mcp_push_server_status`] which
    /// stacks all 7 layers.
    pub fn resolve_mcp_push_server_status(&self) -> Resolved<bool> {
        resolve_mcp_push_server_status(None, None, self.features.mcp_push_server_status, None, None)
    }
    /// Resolve whether the leader's `ConfigFileWatcher` adds the two
    /// narrow non-recursive watches for `<cwd>/` and `<cwd>/.atelier/`.
    ///
    /// Thin delegate to the canonical
    /// [`resolve_mcp_recursive_config_watch`] free function — mirrors
    /// the same delegation pattern. CLI / managed /
    /// feature-flag inputs are `None` here because the `Config`
    /// method only sees the embedded `Features` table; richer call
    /// sites (notably the leader's watcher spawn path) go through
    /// [`crate::util::config::resolve_mcp_recursive_config_watch`]
    /// which stacks all 7 layers.
    pub fn resolve_mcp_recursive_config_watch(&self) -> Resolved<bool> {
        resolve_mcp_recursive_config_watch(
            None,
            None,
            self.features.mcp_recursive_config_watch,
            None,
            None,
        )
    }
}
/// Canonical resolver for `mcp.liveness_watchers`. Stacks the full
/// 7-step `BoolFlag` precedence:
///
/// `requirement > cli > env (ATELIER_MCP_LIVENESS_WATCHERS) > config >
/// managed > feature_flag > default (true)`.
///
/// Both `Config::resolve_mcp_liveness_watchers` and
/// `util::config::resolve_mcp_liveness_watchers` delegate here so the
/// precedence is single-sourced.
///
/// The default is `true` — it gates the watcher + dispatcher
/// default-on, with this flag existing primarily as a kill switch
/// during the rollout.
pub fn resolve_mcp_liveness_watchers(
    requirement: Option<bool>,
    cli: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> Resolved<bool> {
    BoolFlag::env("ATELIER_MCP_LIVENESS_WATCHERS")
        .requirement(requirement)
        .cli(cli)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}
/// Canonical resolver for `mcp.auto_restart`. Stacks the full 7-step
/// `BoolFlag` precedence:
///
/// `requirement > cli > env (ATELIER_MCP_AUTO_RESTART) > config >
/// managed > feature_flag > default (true)`.
///
/// Mirrors [`resolve_mcp_liveness_watchers`]. Both
/// `Config::resolve_mcp_auto_restart` and
/// `util::config::resolve_mcp_auto_restart` delegate here so the
/// precedence is single-sourced.
///
/// Recovery is on by default; opt out via `ATELIER_MCP_AUTO_RESTART=false`,
/// `[features] mcp_auto_restart`, or `requirements.toml`.
pub fn resolve_mcp_auto_restart(
    requirement: Option<bool>,
    cli: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> Resolved<bool> {
    BoolFlag::env("ATELIER_MCP_AUTO_RESTART")
        .requirement(requirement)
        .cli(cli)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}
/// Canonical resolver for `mcp.push_server_status`. Stacks the same
/// 7-step `BoolFlag` precedence as
/// [`resolve_mcp_liveness_watchers`]:
///
/// `requirement > cli > env (ATELIER_MCP_PUSH_SERVER_STATUS) > config >
/// managed > feature_flag > default (true)`.
///
/// Both `Config::resolve_mcp_push_server_status` and
/// `util::config::resolve_mcp_push_server_status` delegate here so
/// the precedence is single-sourced.
///
/// The default is `true` — the pager's subscription to
/// `atelier/mcp/server_status` is wired default-on, with this
/// flag existing primarily as a kill switch.
pub fn resolve_mcp_push_server_status(
    requirement: Option<bool>,
    cli: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> Resolved<bool> {
    BoolFlag::env("ATELIER_MCP_PUSH_SERVER_STATUS")
        .requirement(requirement)
        .cli(cli)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}
/// Canonical resolver for `mcp.recursive_config_watch`. Stacks the
/// same 7-step `BoolFlag` precedence as
/// [`resolve_mcp_liveness_watchers`]:
///
/// `requirement > cli > env (ATELIER_MCP_RECURSIVE_CONFIG_WATCH) >
/// config > managed > feature_flag > default (true)`.
///
/// Both `Config::resolve_mcp_recursive_config_watch` and
/// `util::config::resolve_mcp_recursive_config_watch` delegate here
/// so the precedence is single-sourced.
///
/// The default is `true`. It enables the two narrow
/// non-recursive cwd watches default-on. The flag exists primarily
/// as a kill switch during the rollout: if the FSEvents flakiness
/// on macOS or an inotify-quota issue on Linux causes a regression,
/// operators flip this flag (e.g. via `ATELIER_MCP_RECURSIVE_CONFIG_
/// WATCH=0`) and the leader falls back to the prior behavior (no cwd
/// watches; user-triggered refresh is the only project-config
/// reload path).
///
/// Note the **name is a slight misnomer**: the watches themselves
/// are non-recursive (by design, to avoid blowing through
/// `fs.inotify.max_user_watches` on large repos). The flag name
/// follows the rollout-gate naming convention.
pub fn resolve_mcp_recursive_config_watch(
    requirement: Option<bool>,
    cli: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> Resolved<bool> {
    BoolFlag::env("ATELIER_MCP_RECURSIVE_CONFIG_WATCH")
        .requirement(requirement)
        .cli(cli)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}
/// Sync analogue of [`BoolFlag`] for callers that run before the tokio
/// runtime (e.g. `init_sentry`). Loads from disk + env directly rather than
/// from a pre-built `Config`.
///
/// Same convention as [`BoolFlag`]: `resolve()` returns the *enabled* value.
/// `disable_env` is sugar for "force-off if this env is truthy" and does not
/// invert the convention.
///
/// Layer precedence:
/// 1. `requirements.toml`              (admin pin)
/// 2. `managed_settings.json` env      (Claude admin pin, force-off)
/// 3. process env via `disable_env`    (force-off)
/// 4. process env via `enable_env`     (either direction)
/// 5. merged config                    (user/managed defaults)
/// 6. `inherit`, then `default`
pub struct SyncBoolFlag {
    extract_toml: fn(&toml::Value) -> Option<bool>,
    disable_env: Option<&'static str>,
    enable_env: Option<fn() -> Option<bool>>,
    inherit: Option<fn() -> bool>,
    default: bool,
}
impl SyncBoolFlag {
    pub const fn new(extract_toml: fn(&toml::Value) -> Option<bool>) -> Self {
        Self {
            extract_toml,
            disable_env: None,
            enable_env: None,
            inherit: None,
            default: false,
        }
    }
    /// Force-off env name (e.g. `"DISABLE_TELEMETRY"`). Truthy at this name
    /// in `managed_settings.json` or process env disables the flag.
    pub const fn disable_env(mut self, name: &'static str) -> Self {
        self.disable_env = Some(name);
        self
    }
    /// Either-direction env resolver (typically `ATELIER_*`). Returns
    /// `Some(enabled)` for an explicit signal, `None` to fall through.
    pub const fn enable_env(mut self, resolver: fn() -> Option<bool>) -> Self {
        self.enable_env = Some(resolver);
        self
    }
    /// Fallback when no source above fires.
    pub const fn inherit(mut self, resolver: fn() -> bool) -> Self {
        self.inherit = Some(resolver);
        self
    }
    pub const fn default(mut self, val: bool) -> Self {
        self.default = val;
        self
    }
    pub fn resolve(&self) -> bool {
        if let Some(enabled) = read_requirements_toml()
            .as_ref()
            .and_then(|r| (self.extract_toml)(r))
        {
            return enabled;
        }
        if let Some(name) = self.disable_env
            && managed_settings_env_flag(name) == Some(true)
        {
            return false;
        }
        if let Some(name) = self.disable_env
            && env_bool(name) == Some(true)
        {
            return false;
        }
        if let Some(resolver) = self.enable_env
            && let Some(enabled) = resolver()
        {
            return enabled;
        }
        if let Some(enabled) = crate::config::load_effective_config()
            .ok()
            .as_ref()
            .and_then(|r| (self.extract_toml)(r))
        {
            return enabled;
        }
        self.inherit.map_or(self.default, |f| f())
    }
}
/// Sync slice of [`Config::resolve_telemetry_mode`] for use before the tokio
/// runtime (e.g. `init_sentry`). `true` only when explicitly off.
pub fn is_telemetry_disabled_sync() -> bool {
    !SyncBoolFlag::new(telemetry_enabled_from_toml)
        .disable_env("DISABLE_TELEMETRY")
        .enable_env(atelier_telemetry_env_enabled)
        .resolve()
}
/// Like [`is_telemetry_disabled_sync`] but only `true` when telemetry is
/// *explicitly* off; absence is not disabled (`.default(true)`).
pub fn is_telemetry_explicitly_disabled_sync() -> bool {
    !SyncBoolFlag::new(telemetry_enabled_from_toml)
        .disable_env("DISABLE_TELEMETRY")
        .enable_env(atelier_telemetry_env_enabled)
        .default(true)
        .resolve()
}
/// Sync sibling of [`is_telemetry_disabled_sync`] scoped to Sentry. Inherits
/// from telemetry when no Sentry-specific signal is set.
pub fn is_error_reporting_disabled_sync() -> bool {
    !SyncBoolFlag::new(error_reporting_enabled_from_toml)
        .disable_env("DISABLE_ERROR_REPORTING")
        .enable_env(|| env_bool("ATELIER_ERROR_REPORTING"))
        .inherit(|| !is_telemetry_disabled_sync())
        .resolve()
}
/// `[features] telemetry` as enabled bool. SessionMetrics counts as enabled
/// — see ERROR_REPORTING_PLAN.md. `None` for absent or unparseable.
fn telemetry_enabled_from_toml(root: &toml::Value) -> Option<bool> {
    match root.get("features")?.as_table()?.get("telemetry")? {
        toml::Value::Boolean(b) => Some(*b),
        toml::Value::String(s) => TelemetryMode::parse(s).map(|m| !m.is_disabled()),
        _ => None,
    }
}
/// `[diagnostics] error_reporting` as enabled bool. Bool-only; no
/// `session_metrics` equivalent. `None` falls through to inheritance.
fn error_reporting_enabled_from_toml(root: &toml::Value) -> Option<bool> {
    root.get("diagnostics")?
        .as_table()?
        .get("error_reporting")?
        .as_bool()
}
/// `ATELIER_TELEMETRY_ENABLED` resolved through `TelemetryMode::parse` so the
/// extended string forms (e.g. `"session_metrics"`) are accepted.
fn atelier_telemetry_env_enabled() -> Option<bool> {
    env_telemetry_mode("ATELIER_TELEMETRY_ENABLED").map(|m| !m.is_disabled())
}
/// Load `~/.atelier/requirements.toml` standalone so the admin pin can beat
/// env vars. The merged config layer can't express that — last-merge-wins
/// loses provenance.
pub(crate) fn read_requirements_toml() -> Option<toml::Value> {
    let path = crate::util::atelier_home::atelier_home().join("requirements.toml");
    let content = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&content).ok()
}
/// Seed the remaining process-local compatibility caches after writing
/// `Config.local_runtime_settings`.
pub fn apply_local_runtime_settings_side_effects(
    settings: Option<&crate::util::config::LocalRuntimeSettings>,
) {
    crate::util::config::cache_remote_mcp_startup_timeout_secs(
        settings.and_then(|s| s.mcp_startup_timeout_secs),
    );
    crate::util::config::cache_remote_max_mcp_output_bytes(
        settings.and_then(|s| s.max_mcp_output_bytes),
    );
    crate::util::config::cache_remote_remember_tool_approvals(
        settings.and_then(|s| s.remember_tool_approvals),
    );
    crate::util::config::cache_remote_crash_handler_enabled(
        settings.and_then(|s| s.crash_handler_enabled),
    );
}
/// Read `env.<key>` from Claude-compat `managed_settings.json`. `Some(true)`
/// indicates a force-off signal from a Mac-MDM-style admin policy.
fn managed_settings_env_flag(key: &str) -> Option<bool> {
    let path = atelier_config::claude_managed_settings_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    atelier_workspace::permission::resolution::json_env_flag(json.get("env"), key)
}
/// Assemble the final model map. Priority (highest wins):
/// config.toml `[model.*]` > prefetched (remote) > hardcoded defaults.
pub fn resolve_model_list(
    cfg: &Config,
    prefetched: Option<IndexMap<String, ModelEntry>>,
) -> IndexMap<String, ModelEntry> {
    let mut resolved: IndexMap<String, ModelEntry> = IndexMap::new();
    if cfg.endpoints.has_custom_endpoint() {
        tracing::info!(
            models_base_url = ? cfg.endpoints.models_base_url, models_list_url = ? cfg
            .endpoints.models_list_url,
            "custom models endpoint active, skipping built-in defaults",
        );
    } else {
        let defaults = default_model_entries(&cfg.endpoints);
        tracing::debug!(count = defaults.len(), "loaded default models");
        resolved.extend(defaults);
    }
    if let Some(mut prefetched) = prefetched {
        tracing::debug!(count = prefetched.len(), "loaded prefetched models");
        let default_cw = DEFAULT_CONTEXT_WINDOW;
        for (key, entry) in prefetched.iter_mut() {
            let donor = resolved.get(key);
            if let Some(donor) = donor {
                if entry.info.context_window.get() == default_cw
                    && donor.info.context_window.get() != default_cw
                {
                    tracing::debug!(
                        model_key = % key, model = % entry.info.model, client_default =
                        default_cw, inherited = donor.info.context_window.get(),
                        donor_model = % donor.info.model,
                        "prefetched model missing context_window, inheriting from hardcoded default"
                    );
                    entry.info.context_window = donor.info.context_window;
                }
                if entry.info.agent_type == DEFAULT_AGENT_TYPE {
                    entry.info.agent_type.clone_from(&donor.info.agent_type);
                }
                if entry.info.api_backend == ApiBackend::default() {
                    entry.info.api_backend.clone_from(&donor.info.api_backend);
                }
            }
            if resolved.contains_key(key) {
                tracing::debug!(
                    model_key = % key, "prefetched model overriding default"
                );
            }
        }
        resolved = prefetched;
    }
    for (key, model_override) in &cfg.config_models {
        let had_base = resolved.contains_key(key);
        let base = resolved.shift_remove(key);
        if !had_base {
            tracing::debug!(
                model_key = % key,
                "config model adding new entry (not in defaults/prefetched)"
            );
            if model_override.context_window.is_none() {
                tracing::debug!(
                    model_key = % key, default = 200_000,
                    "new model missing context_window, defaulting to 200000 — set context_window in [model.{}] to override",
                    key,
                );
            }
        }
        let entry = model_override.apply(key, base, &cfg.endpoints);
        tracing::debug!(
            model_key = % key, base_url = % entry.info.base_url, has_api_key = entry
            .api_key.is_some(), env_key = ? entry.env_key, had_base,
            "config model override applied"
        );
        resolved.insert(key.clone(), entry);
    }
    {
        let default_cw = DEFAULT_CONTEXT_WINDOW;
        let donors: std::collections::HashMap<String, (std::num::NonZeroU64, ApiBackend)> =
            resolved
                .values()
                .filter(|e| e.info.context_window.get() != default_cw)
                .map(|e| {
                    (
                        e.info.model.clone(),
                        (e.info.context_window, e.info.api_backend.clone()),
                    )
                })
                .collect();
        for entry in resolved.values_mut() {
            if let Some((donor_cw, donor_backend)) = donors.get(&entry.info.model) {
                if entry.info.context_window.get() == default_cw {
                    tracing::debug!(
                        model = % entry.info.model, from = default_cw, to = donor_cw
                        .get(),
                        "slug-match: inheriting context_window from sibling catalog entry"
                    );
                    entry.info.context_window = *donor_cw;
                }
                if entry.info.api_backend == ApiBackend::default()
                    && *donor_backend != ApiBackend::default()
                {
                    entry.info.api_backend.clone_from(donor_backend);
                }
            }
        }
    }
    if let Some(ref global_agent_type) = cfg.models.agent_type {
        tracing::warn!(
            global_agent_type = % global_agent_type,
            "[models] agent_type is deprecated. Set agent_type on each [model.X] entry instead."
        );
        for entry in resolved.values_mut() {
            if entry.info.agent_type == DEFAULT_AGENT_TYPE {
                entry.info.agent_type = global_agent_type.clone();
            }
        }
    }
    apply_global_extra_headers(&mut resolved, &cfg.models);
    apply_global_scalar_defaults(&mut resolved, &cfg.models);
    for entry in resolved.values_mut() {
        entry.info.derive_reasoning_effort_fields();
    }
    resolved
}

#[cfg(test)]
mod sandbox_settings_contract_tests {
    use super::SandboxSettingsConfig;
    use atelier_sandbox::SandboxBackendKind;

    #[test]
    fn sandbox_backend_defaults_to_native() {
        assert_eq!(
            SandboxSettingsConfig::default()
                .resolve_backend()
                .expect("default backend"),
            SandboxBackendKind::Native
        );
    }

    #[test]
    fn sandbox_backend_accepts_explicit_unsafe_only() {
        let config = SandboxSettingsConfig {
            backend: Some("unsafe".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            config.resolve_backend().expect("unsafe backend"),
            SandboxBackendKind::Unsafe
        );
    }

    #[test]
    fn sandbox_backend_rejects_unknown_values() {
        let config = SandboxSettingsConfig {
            backend: Some("disabled".to_owned()),
            ..Default::default()
        };
        assert!(config.resolve_backend().is_err());
    }
}

/// Convert the persisted local Provider catalog into the shell's model-entry
/// representation. Provider model keys remain composite (`provider/model`),
/// while `ModelInfo.model` keeps the provider's routing slug.
pub fn model_entries_from_provider_snapshot(
    snapshot: &ProviderSnapshot,
    configured_model: Option<&str>,
) -> IndexMap<String, ModelEntry> {
    let providers = snapshot
        .providers
        .iter()
        .filter(|provider| provider.enabled)
        .map(|provider| (provider.id.as_str(), provider))
        .collect::<std::collections::HashMap<_, _>>();

    let mut entries: IndexMap<String, ModelEntry> = snapshot
        .models
        .iter()
        .filter_map(|model| {
            let provider = providers.get(model.key.provider_id.as_str())?;
            if !model.enabled {
                return None;
            }

            let mut info = ModelInfo::fallback(&model.key.model_id);
            info.id = Some(model.key.to_string());
            info.model = model.key.model_id.clone();
            info.base_url = provider.base_url.to_string();
            info.name = Some(model.display_name.clone());
            info.description = model.description.clone();
            info.extra_headers = provider
                .extra_headers
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            const DEFAULT_PROVIDER_CONTEXT_WINDOW: u64 = 100_000;
            let context_window = model
                .context_window
                .and_then(NonZeroU64::new)
                .unwrap_or_else(|| {
                    tracing::info!(
                        provider = %provider.id,
                        model = %model.key.model_id,
                        context_window = DEFAULT_PROVIDER_CONTEXT_WINDOW,
                        "Provider model has no context metadata; using the default context window"
                    );
                    NonZeroU64::new(DEFAULT_PROVIDER_CONTEXT_WINDOW)
                        .expect("default Provider context window is non-zero")
                });
            info.context_window = context_window;
            let provider_model_override = snapshot
                .model_provider_overrides
                .get(&model.key.to_string());
            let wire_api = snapshot.resolve_wire_api(&model.key).ok()?.wire_api;
            info.api_backend = match wire_api {
                WireApi::Responses => ApiBackend::Responses,
                WireApi::ChatCompletions => ApiBackend::ChatCompletions,
                WireApi::Messages => ApiBackend::Messages,
            };
            info.auth_scheme = match &provider.auth {
                ProviderAuth::None => AuthScheme::None,
                ProviderAuth::Bearer => AuthScheme::Bearer,
                ProviderAuth::Header { name } if name.eq_ignore_ascii_case("x-api-key") => {
                    AuthScheme::XApiKey
                }
                ProviderAuth::Header { name } => AuthScheme::Header(name.clone()),
            };
            info.hidden = false;
            info.user_selectable = true;
            info.supported_in_api = true;
            info.supports_reasoning_effort = model.capabilities.reasoning_effort;
            info.accepts_images = model.capabilities.image_input;
            info.supports_fast_mode = model.fast_mode;
            info.supports_backend_search = model.capabilities.web_search;
            info.reasoning_efforts = provider_reasoning_efforts(model);
            info.reasoning_effort = info
                .reasoning_efforts
                .iter()
                .find(|effort| effort.default)
                .or_else(|| info.reasoning_efforts.first())
                .map(|effort| effort.value);
            info.derive_reasoning_effort_fields();

            let (api_key, env_key) = match &provider.credential {
                atelier_provider::CredentialRef::Environment { variable } => {
                    (None, Some(EnvKeys::single(variable.clone())))
                }
                atelier_provider::CredentialRef::None => (None, None),
                atelier_provider::CredentialRef::Command { .. }
                | atelier_provider::CredentialRef::SecretStore { .. }
                | atelier_provider::CredentialRef::OAuth { .. } => {
                    match provider.credential.resolve() {
                        Ok(Some(secret)) => (Some(secret.into_inner()), None),
                        Ok(None) => (None, None),
                        Err(error) => {
                            tracing::error!(
                                provider = %provider.id,
                                error = %error,
                                "local Provider credential is unavailable; hiding its models"
                            );
                            return None;
                        }
                    }
                }
            };

            let entry = ModelEntry {
                info,
                api_key,
                env_key,
                api_base_url: None,
                request_payload: provider_model_override
                    .map(|override_config| override_config.payload.clone())
                    .unwrap_or_default(),
                remote_compaction_endpoint: snapshot
                    .resolve_remote_compaction_endpoint(&model.key)
                    .ok()
                    .flatten(),
                image_generation_endpoint: snapshot
                    .resolve_image_generation_endpoint(&model.key)
                    .ok()
                    .flatten(),
            };
            Some((model.key.to_string(), entry))
        })
        .collect();
    if let Some(default_key) = configured_model
        .and_then(|model| atelier_provider::ModelKey::parse(model).ok())
        .map(|key| key.to_string())
        && let Some(entry) = entries.shift_remove(&default_key)
    {
        entries.shift_insert(0, default_key, entry);
    }
    entries
}

fn provider_reasoning_efforts(model: &ProviderModelDescriptor) -> Vec<ReasoningEffortOption> {
    model
        .reasoning_efforts
        .iter()
        .filter_map(|raw| raw.parse::<ReasoningEffort>().ok())
        .enumerate()
        .map(|(index, value)| {
            let label = value.as_str().to_owned();
            let default = model
                .default_effort
                .as_deref()
                .is_some_and(|default| default == value.as_str())
                || (model.default_effort.is_none() && index == 0);
            ReasoningEffortOption {
                id: label.clone(),
                value,
                label,
                description: None,
                default,
            }
        })
        .collect()
}

/// Resolve the model list with a caller-provided local Provider snapshot.
/// Enabled local Providers are authoritative: the remote/bundled catalog is
/// ignored, while explicit `[model.*]` overrides still apply.
pub fn resolve_model_list_with_provider_snapshot(
    cfg: &Config,
    prefetched: Option<IndexMap<String, ModelEntry>>,
    provider_snapshot: Option<&ProviderSnapshot>,
) -> IndexMap<String, ModelEntry> {
    let Some(snapshot) = provider_snapshot else {
        return resolve_model_list(cfg, prefetched);
    };

    if !snapshot.providers.iter().any(|provider| provider.enabled) {
        // A present local registry is authoritative even when it is empty or
        // all providers are disabled. Falling back here would re-enable the
        // bundled/vendor catalog after the user explicitly disabled every
        // local provider.
        return resolve_model_list(cfg, Some(IndexMap::new()));
    }

    let local_models = model_entries_from_provider_snapshot(snapshot, cfg.model.as_deref());
    tracing::info!(
        count = local_models.len(),
        "using local Provider model catalog"
    );
    resolve_model_list(cfg, Some(local_models))
}

/// Resolve the model list used by the live runtime. A malformed local Provider
/// registry fails closed to an empty catalog; it must never fall through to a
/// vendor or remote model catalog.
pub fn resolve_runtime_model_list(
    cfg: &Config,
    _prefetched: Option<IndexMap<String, ModelEntry>>,
) -> IndexMap<String, ModelEntry> {
    resolve_model_list(
        cfg,
        Some(load_runtime_provider_models(cfg.model.as_deref())),
    )
}

/// Load the local Provider catalog used by the vendorless runtime.
///
/// Absence or corruption returns an empty catalog. This fail-closed boundary
/// prevents inherited vendor model lists from reappearing. The runtime caches
/// the returned snapshot and refreshes it explicitly through Provider commands.
pub fn load_runtime_provider_models(
    configured_model: Option<&str>,
) -> IndexMap<String, ModelEntry> {
    let path = atelier_config::atelier_home().join("providers.toml");
    if !path.exists() {
        // A fresh Atelier install must be offline until the user configures a
        // provider. Do not fall back to the inherited vendor model catalog.
        return IndexMap::new();
    }

    match atelier_provider::ProviderRegistry::load_or_create(path) {
        Ok(registry) => {
            model_entries_from_provider_snapshot(&registry.snapshot(), configured_model)
        }
        Err(error) => {
            tracing::error!(%error, "local Provider registry is invalid; refusing remote model catalog");
            IndexMap::new()
        }
    }
}
/// Layer 6 of [`resolve_model_list`]: fold the global `[models].extra_headers`
/// into every model as a base. The presence check is case-insensitive because
/// the sampler lowers these into an `http::HeaderMap`, so a global `X-Foo` must
/// not shadow a per-model `x-foo`; a per-model `[model.<id>].extra_headers`
/// (applied earlier) therefore wins per key.
fn apply_global_extra_headers(resolved: &mut IndexMap<String, ModelEntry>, models: &ModelsConfig) {
    if models.extra_headers.is_empty() {
        return;
    }
    tracing::debug!(
        header_keys = ? models.extra_headers.keys().collect::< Vec < _ >> (), model_count
        = resolved.len(), "applying global [models].extra_headers default to all models"
    );
    for entry in resolved.values_mut() {
        for (k, v) in &models.extra_headers {
            let present = entry
                .info
                .extra_headers
                .keys()
                .any(|ek| ek.eq_ignore_ascii_case(k));
            if !present {
                entry.info.extra_headers.insert(k.clone(), v.clone());
            }
        }
    }
}
/// Layer 7 of [`resolve_model_list`]: fill scalar `[models]` defaults into any
/// model that left the field unset. Per-model (Layer 3) and remote-prefetched
/// (Layer 2) values already populated theirs, so they win via `get_or_insert`
/// (the global default is a fallback, not a clamp).
fn apply_global_scalar_defaults(
    resolved: &mut IndexMap<String, ModelEntry>,
    models: &ModelsConfig,
) {
    for entry in resolved.values_mut() {
        let info = &mut entry.info;
        if let Some(v) = models.temperature {
            info.temperature.get_or_insert(v);
        }
        if let Some(v) = models.top_p {
            info.top_p.get_or_insert(v);
        }
        if let Some(v) = models.max_completion_tokens {
            info.max_completion_tokens.get_or_insert(v);
        }
        if let Some(v) = models.max_retries {
            info.max_retries.get_or_insert(v);
        }
        if let Some(v) = models.inference_idle_timeout_secs {
            info.inference_idle_timeout_secs.get_or_insert(v);
        }
        if let Some(v) = models.stream_tool_calls {
            info.stream_tool_calls.get_or_insert(v);
        }
    }
}
/// Built-in default models. Prefer `resolve_model_list()`.
pub fn default_model_entries(endpoints: &EndpointsConfig) -> IndexMap<String, ModelEntry> {
    default_models(endpoints)
        .into_iter()
        .map(|(key, entry)| (key, ModelEntry::from_config_entry(&entry)))
        .collect()
}
/// Resolve a model against the available model map.
/// Checks the map key (id) first, then falls back to a slug scan.
pub fn find_model_by_id<'a>(
    models: &'a IndexMap<String, ModelEntry>,
    model_id: &str,
) -> Option<&'a ModelEntry> {
    models
        .get(model_id)
        .or_else(|| models.values().find(|m| m.model == model_id))
}
/// Whether the EFFECTIVE Auto-mode classifier model supports reasoning effort:
/// the model actually routed to (`aux_model` when the aux sampler resolved) else
/// the session model the worker falls back to. Not-found-in-catalog ⇒ `false`
/// (conservative; also covers the Tier-2 synthetic proxy entry). Drives the
/// built-in `low` effort default.
pub fn effective_classifier_supports_re(
    aux_model: Option<&str>,
    session_model: &str,
    models: &IndexMap<String, ModelEntry>,
) -> bool {
    find_model_by_id(models, aux_model.unwrap_or(session_model))
        .map(|e| e.info().supports_reasoning_effort)
        .unwrap_or(false)
}
/// JSON-only subset of `ModelEntryConfig`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DefaultModelJson {
    id: Option<String>,
    model: String,
    name: Option<String>,
    description: Option<String>,
    context_window: Option<NonZeroU64>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    max_completion_tokens: Option<u32>,
    api_backend: ApiBackend,
    #[serde(default = "default_agent_type")]
    agent_type: String,
    inference_idle_timeout_secs: Option<u64>,
    hidden: bool,
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    supports_reasoning_effort: bool,
    #[serde(default)]
    reasoning_efforts: Vec<ReasoningEffortOption>,
    /// When false, only OAuth users see this in the picker.
    #[serde(default = "default_true")]
    supported_in_api: bool,
    #[serde(default)]
    supports_backend_search: bool,
    #[serde(default)]
    compactions_remaining: Option<CompactionsRemaining>,
    #[serde(default)]
    compaction_at_tokens: Option<CompactionAtTokens>,
    #[serde(default)]
    show_model_fingerprint: bool,
}
fn default_models(endpoints: &EndpointsConfig) -> IndexMap<String, ModelEntryConfig> {
    let root: serde_json::Value = serde_json::from_str(crate::models::DEFAULT_MODELS_JSON)
        .expect("default_models.json: invalid JSON");
    let entries: Vec<DefaultModelJson> = serde_json::from_value(
        root.get("models")
            .expect("default_models.json: missing 'models' array")
            .clone(),
    )
    .expect("default_models.json: invalid 'models' array");
    tracing::debug!(
        count = entries.len(),
        "loaded default models from embedded JSON"
    );
    entries
        .into_iter()
        .map(|m| {
            assert!(
                !m.model.is_empty(),
                "default_models.json: entry id={:?} has empty `model` field",
                m.id
            );
            let key = m.id.clone().unwrap_or_else(|| m.model.clone());
            let context_window = m
                .context_window
                .unwrap_or_else(|| NonZeroU64::new(200_000).expect("200000 is non-zero"));
            let config = ModelEntryConfig {
                id: m.id,
                model: m.model,
                base_url: endpoints.resolve_inference_base_url(),
                api_base_url: None,
                name: m.name,
                description: m.description,
                context_window,
                auto_compact_threshold_percent: None,
                system_prompt_label: None,
                temperature: m.temperature,
                top_p: m.top_p,
                max_completion_tokens: m.max_completion_tokens,
                api_backend: m.api_backend,
                auth_scheme: None,
                agent_type: m.agent_type,
                inference_idle_timeout_secs: m.inference_idle_timeout_secs,
                max_retries: None,
                api_key: None,
                env_key: None,
                extra_headers: IndexMap::new(),
                use_concise: false,
                hidden: m.hidden,
                supported_in_api: m.supported_in_api,
                reasoning_effort: m.reasoning_effort,
                supports_reasoning_effort: m.supports_reasoning_effort,
                reasoning_efforts: m.reasoning_efforts,
                supports_backend_search: m.supports_backend_search,
                compactions_remaining: m.compactions_remaining,
                compaction_at_tokens: m.compaction_at_tokens,
                show_model_fingerprint: m.show_model_fingerprint,
                stream_tool_calls: None,
                laziness_detector: LazinessDetectorPerModelConfig::default(),
            };
            (key, config)
        })
        .collect()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntryConfig {
    /// Stable unique identifier for this catalog entry. When present,
    /// used as the catalog map key. Falls back to `model` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The routing slug sent in API requests.
    pub model: String,
    /// The base URL of the model. e.g. "https://api.atelier/v1"
    pub base_url: String,
    /// Human-readable display name of the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// The API key for this model's provider.
    /// If not set, falls back only to the configured `env_key`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Environment variable name(s) that hold the provider API key.
    /// Accepts a string or an array (first set, non-empty value wins).
    /// No implicit global provider credential is consulted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_key: Option<EnvKeys>,
    /// Which API backend to use for this model.
    /// Values: "chat_completions" (default), "responses"
    #[serde(default)]
    pub api_backend: ApiBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<AuthScheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_reasoning_effort: bool,
    /// Per-model reasoning-effort menu (source of truth). The two legacy fields
    /// above are derived from this list when it is non-empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<ReasoningEffortOption>,
    /// Extra headers to send with requests to this model's endpoint.
    /// Useful for BYOK (Bring Your Own Key) scenarios.
    /// Example: { "x-anthropic-api-key" = "sk-ant-..." }
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub extra_headers: IndexMap<String, String>,
    /// The total context window size in tokens for this model.
    /// Used for auto-compact threshold calculations.
    /// Required — BYOK users must explicitly set this in config.toml.
    pub context_window: NonZeroU64,
    /// Per-model auto-compact threshold (0-100). When the session's token
    /// usage exceeds this percentage of `context_window`, the conversation
    /// is summarized. Resolver precedence:
    /// requirements > env > user (per-model > global) > managed (per-model > global)
    /// > remote per-model (this field) > remote global > 85.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold_percent: Option<u8>,
    /// Per-model system-prompt identity label (not UI `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_label: Option<String>,
    /// The base URL to use when authenticating with an API key (non-session auth).
    /// When set, `base_url` is used for session-based auth and `api_base_url` for API key auth.
    /// When not set, `base_url` is used for all auth methods (e.g. BYOK / third-party models).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    /// When true, this model uses concise mode (compact system prompt,
    /// concise tool output, concise user message prefix, reduced toolset).
    /// Defaults to false — when omitted or false, nothing changes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub use_concise: bool,
    /// The type of system prompt to use for this model.
    /// e.g. "atelier-build", "codex".
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    /// Maximum seconds to wait between SSE chunks during inference streaming.
    /// When no chunk is received within this duration, the request fails with
    /// a non-retryable `IdleTimeout` error. This is a per-chunk deadline that
    /// resets on every received chunk — NOT a total-turn timeout.
    /// Default: 300 seconds (5 minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_idle_timeout_secs: Option<u64>,
    /// Maximum number of retries for transient API errors (429, 500, 502, etc.)
    /// during a single inference request. Default: 5.
    /// Can also be set via the `ATELIER_MAX_RETRIES` environment variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// Exclude from the client model picker; still usable internally (web_search, etc.).
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// When false, only OAuth users see this in the picker.
    #[serde(default = "default_true")]
    pub supported_in_api: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_backend_search: bool,
    /// Per-model config for the `x-compactions-remaining` header; `None` disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compactions_remaining: Option<CompactionsRemaining>,
    /// Per-model config for the `x-compaction-at` header; `None` disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_at_tokens: Option<CompactionAtTokens>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub show_model_fingerprint: bool,
    /// Inject `stream_tool_calls: true` into the request body
    /// so the upstream emits per-chunk `function_call_arguments.delta`
    /// Without this set, xAI API models send args as one delta
    /// event, defeating the purpose of streaming.
    ///
    /// Per-model opt-in -- BYOK endpoints that don't understand the
    /// flag should leave this unset to avoid request errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_tool_calls: Option<bool>,
    /// Per-model Layer-3 LazinessDetector configuration. Defaults to
    /// the all-disabled state via `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "is_default_laziness_detector")]
    pub laziness_detector: LazinessDetectorPerModelConfig,
}
/// True when `cfg` equals the all-disabled default. Derives `PartialEq`
/// on `f32`, which is fine for the current shape because both `f32`
/// fields default to `None` — there's no parsed-vs-literal `0.7` float
/// equality footgun. If a future default introduces `Some(0.7)`, this
/// helper must be reworked (e.g. compare on tolerance, or switch to a
/// bit-pattern compare) so `skip_serializing_if` doesn't start emitting
/// `[laziness_detector]` blocks for every model in `config.toml`.
fn is_default_laziness_detector(cfg: &LazinessDetectorPerModelConfig) -> bool {
    cfg == &LazinessDetectorPerModelConfig::default()
}
/// A `[model.foo]` entry from config.toml, parsed directly from raw TOML
/// (bypassing deep merge). Scalar fields are `Option` so absent means "inherit
/// from defaults/prefetched"; the collection fields (`extra_headers`,
/// `reasoning_efforts`) merge only when non-empty and so cannot express
/// "override to empty."
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ConfigModelOverride {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub api_key: Option<String>,
    /// Env var name(s) for the provider key — string or array in config.toml.
    pub env_key: Option<EnvKeys>,
    pub api_base_url: Option<String>,
    pub max_completion_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub api_backend: Option<ApiBackend>,
    #[serde(default)]
    pub extra_headers: IndexMap<String, String>,
    pub context_window: Option<u64>,
    /// Per-model auto-compact threshold override (0-100) from `[model.<id>]`.
    /// Read directly by `resolve_auto_compact_threshold_percent`; intentionally
    /// NOT merged into `ModelInfo.auto_compact_threshold_percent` so the
    /// resolver can keep user-per-model distinct from GB-per-model.
    pub auto_compact_threshold_percent: Option<u8>,
    /// Per-model system-prompt identity; not merged into `ModelInfo` (tiered resolve).
    pub system_prompt_label: Option<String>,
    pub use_concise: Option<bool>,
    pub agent_type: Option<String>,
    pub inference_idle_timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    pub hidden: Option<bool>,
    pub supported_in_api: Option<bool>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub supports_reasoning_effort: Option<bool>,
    pub reasoning_efforts: Vec<ReasoningEffortOption>,
    pub supports_backend_search: Option<bool>,
    /// Aliases must be registered in `config_model_override_parse::ALIASES`;
    /// serde rejects a table that contains both spellings otherwise.
    #[serde(alias = "send_compactions_remaining")]
    pub compactions_remaining: Option<CompactionsRemaining>,
    pub compaction_at_tokens: Option<CompactionAtTokens>,
    pub show_model_fingerprint: Option<bool>,
    pub stream_tool_calls: Option<bool>,
}
impl ConfigModelOverride {
    pub(crate) fn apply(
        &self,
        key: &str,
        base: Option<ModelEntry>,
        endpoints: &EndpointsConfig,
    ) -> ModelEntry {
        let mut entry = base.unwrap_or_else(|| ModelEntry::fallback(key, endpoints));
        if let Some(ref v) = self.model {
            entry.info.model = v.clone();
        }
        if let Some(ref v) = self.base_url {
            entry.info.base_url = v.clone();
            if self.api_base_url.is_none() {
                entry.api_base_url = None;
            }
        }
        if self.name.is_some() {
            entry.info.name.clone_from(&self.name);
        }
        if self.description.is_some() {
            entry.info.description.clone_from(&self.description);
        }
        if self.max_completion_tokens.is_some() {
            entry.info.max_completion_tokens = self.max_completion_tokens;
        }
        if self.temperature.is_some() {
            entry.info.temperature = self.temperature;
        }
        if self.top_p.is_some() {
            entry.info.top_p = self.top_p;
        }
        if let Some(ref v) = self.api_backend {
            entry.info.api_backend = v.clone();
        }
        if !self.extra_headers.is_empty() {
            entry.info.extra_headers = self.extra_headers.clone();
        }
        if let Some(cw) = self.context_window.and_then(NonZeroU64::new) {
            entry.info.context_window = cw;
        }
        if let Some(v) = self.use_concise {
            entry.info.use_concise = v;
        }
        if let Some(ref at) = self.agent_type {
            entry.info.agent_type.clone_from(at);
        }
        if self.inference_idle_timeout_secs.is_some() {
            entry.info.inference_idle_timeout_secs = self.inference_idle_timeout_secs;
        }
        if self.max_retries.is_some() {
            entry.info.max_retries = self.max_retries;
        }
        if let Some(v) = self.hidden {
            entry.info.hidden = v;
        }
        if let Some(v) = self.supported_in_api {
            entry.info.supported_in_api = v;
        }
        if self.reasoning_effort.is_some() {
            entry.info.reasoning_effort = self.reasoning_effort;
        }
        if let Some(v) = self.supports_reasoning_effort {
            entry.info.supports_reasoning_effort = v;
        } else if !entry.info.supports_reasoning_effort
            && matches!(entry.info.api_backend, ApiBackend::Messages)
        {
            entry.info.supports_reasoning_effort = true;
        }
        if !self.reasoning_efforts.is_empty() {
            entry.info.reasoning_efforts = self.reasoning_efforts.clone();
        }
        if let Some(v) = self.supports_backend_search {
            entry.info.supports_backend_search = v;
        }
        if self.compactions_remaining.is_some() {
            entry.info.compactions_remaining = self.compactions_remaining;
        }
        if self.compaction_at_tokens.is_some() {
            entry.info.compaction_at_tokens = self.compaction_at_tokens;
        }
        if let Some(v) = self.show_model_fingerprint {
            entry.info.show_model_fingerprint = v;
        }
        if self.stream_tool_calls.is_some() {
            entry.info.stream_tool_calls = self.stream_tool_calls;
        }
        if self.api_key.is_some() {
            entry.api_key.clone_from(&self.api_key);
        }
        if self.env_key.is_some() {
            entry.env_key.clone_from(&self.env_key);
        }
        if self.api_base_url.is_some() {
            entry.api_base_url.clone_from(&self.api_base_url);
        }
        if self.supported_in_api.is_none() && (self.api_key.is_some() || self.env_key.is_some()) {
            entry.info.supported_in_api = true;
        }
        entry
    }
}
/// Shared model metadata — the common fields across all model sources.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    /// Stable unique identifier for this catalog entry.
    /// Falls back to `model` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The routing slug sent in API requests.
    pub model: String,
    /// The base URL of the model (session endpoint). e.g. "https://cli-chat-proxy.atelier.com/v1"
    pub base_url: String,
    /// Human-readable name of the model. Honored by both the picker
    /// (`/model`) and `/session-info` -- when set, that's the label shown
    /// to users in either consumer.
    pub name: Option<String>,
    pub description: Option<String>,
    pub max_completion_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub api_backend: ApiBackend,
    pub auth_scheme: AuthScheme,
    pub extra_headers: IndexMap<String, String>,
    pub context_window: NonZeroU64,
    /// Per-model auto-compact threshold (0-100). `None` defers to the
    /// global / default tiers in `resolve_auto_compact_threshold_percent`.
    pub auto_compact_threshold_percent: Option<u8>,
    /// Per-model system-prompt identity (not UI picker `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_label: Option<String>,
    /// When true, this model uses concise mode (compact system prompt,
    /// concise tool output, concise user message prefix, reduced toolset).
    pub use_concise: bool,
    /// The type of agent configuration to use for this model.
    /// Always has a value; defaults to `"atelier-build-plan"` when the server
    /// or user config doesn't specify one.
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    /// Per-chunk idle timeout for inference streaming (see `ModelEntryConfig`).
    pub inference_idle_timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    /// Never show in picker (any auth). See also `supported_in_api`.
    pub hidden: bool,
    /// May the user select this model for normal chat? Derived from
    /// `allowed_models` in `resolve_model_catalog`; never persisted.
    #[serde(skip_serializing, default = "default_true")]
    pub user_selectable: bool,
    /// When false, only OAuth users see this in the picker.
    #[serde(default = "default_true")]
    pub supported_in_api: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// When true, the UI shows effort controls for this model.
    pub supports_reasoning_effort: bool,
    /// Per-model reasoning-effort menu (source of truth); legacy fields derived from it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<ReasoningEffortOption>,
    /// Whether image input is explicitly supported by this model.
    #[serde(default)]
    pub accepts_images: bool,
    /// Whether the Provider/model profile exposes a fast-mode switch.
    #[serde(default)]
    pub supports_fast_mode: bool,
    pub supports_backend_search: bool,
    /// Per-model config for the `x-compactions-remaining` header; `None` disables it.
    pub compactions_remaining: Option<CompactionsRemaining>,
    /// Per-model config for the `x-compaction-at` header; `None` disables it.
    pub compaction_at_tokens: Option<CompactionAtTokens>,
    pub show_model_fingerprint: bool,
    /// When `Some(true)`, the sampler injects `stream_tool_calls: true`
    pub stream_tool_calls: Option<bool>,
    /// Per-model Layer-3 LazinessDetector configuration. Defaults to
    /// the all-disabled state — the feature is per-model opt-in with a
    /// second-step `max_nudges_per_session > 0` opt-in for actually
    /// injecting nudges. See [`LazinessDetectorPerModelConfig`].
    #[serde(default)]
    pub laziness_detector: LazinessDetectorPerModelConfig,
}
impl ModelInfo {
    /// Minimal fallback descriptor for an unknown model slug.
    /// Used when a configured model ID isn't found in presets or remote models.
    pub fn fallback(slug: &str) -> Self {
        ModelInfo {
            user_selectable: true,
            id: None,
            model: slug.to_owned(),
            base_url: String::new(),
            name: None,
            description: None,
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: ApiBackend::default(),
            auth_scheme: Default::default(),
            extra_headers: IndexMap::new(),
            context_window: NonZeroU64::new(200_000).unwrap(),
            auto_compact_threshold_percent: None,
            system_prompt_label: None,
            use_concise: false,
            agent_type: default_agent_type(),
            inference_idle_timeout_secs: None,
            max_retries: None,
            hidden: false,
            supported_in_api: true,
            reasoning_effort: None,
            supports_reasoning_effort: false,
            reasoning_efforts: Vec::new(),
            accepts_images: false,
            supports_fast_mode: false,
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            show_model_fingerprint: false,
            stream_tool_calls: None,
            laziness_detector: LazinessDetectorPerModelConfig::default(),
        }
    }
    /// Extract shared model metadata from a flat config entry.
    pub fn from_config(entry: &ModelEntryConfig) -> Self {
        ModelInfo {
            user_selectable: true,
            id: entry.id.clone(),
            model: entry.model.clone(),
            base_url: entry.base_url.clone(),
            name: entry.name.clone(),
            description: entry.description.clone(),
            max_completion_tokens: entry.max_completion_tokens,
            temperature: entry.temperature,
            top_p: entry.top_p,
            api_backend: entry.api_backend.clone(),
            auth_scheme: entry.auth_scheme.clone().unwrap_or_default(),
            extra_headers: entry.extra_headers.clone(),
            context_window: entry.context_window,
            auto_compact_threshold_percent: entry.auto_compact_threshold_percent,
            system_prompt_label: entry.system_prompt_label.clone(),
            use_concise: entry.use_concise,
            agent_type: entry.agent_type.clone(),
            inference_idle_timeout_secs: entry.inference_idle_timeout_secs,
            max_retries: entry.max_retries,
            hidden: entry.hidden,
            supported_in_api: entry.supported_in_api,
            reasoning_effort: entry.reasoning_effort,
            supports_reasoning_effort: entry.supports_reasoning_effort,
            reasoning_efforts: entry.reasoning_efforts.clone(),
            accepts_images: false,
            supports_fast_mode: false,
            supports_backend_search: entry.supports_backend_search,
            compactions_remaining: entry.compactions_remaining,
            compaction_at_tokens: entry.compaction_at_tokens,
            show_model_fingerprint: entry.show_model_fingerprint,
            stream_tool_calls: entry.stream_tool_calls,
            laziness_detector: entry.laziness_detector.clone(),
        }
    }
    /// Derive the legacy effort gate/default from `reasoning_efforts` so the
    /// shell's internal reads (support gate, wire default, session modes) treat
    /// a menu-only model as supported. The single derive site; `to_acp_model_info`
    /// then just reads these fields. Idempotent (the remote/CCP path already sets
    /// them); the empty-list path leaves both legacy fields untouched.
    fn derive_reasoning_effort_fields(&mut self) {
        if self.reasoning_efforts.is_empty() {
            return;
        }
        self.supports_reasoning_effort = true;
        if self.reasoning_effort.is_none() {
            let default = self
                .reasoning_efforts
                .iter()
                .find(|opt| opt.default)
                .or_else(|| self.reasoning_efforts.first())
                .map(|opt| opt.value);
            self.reasoning_effort = default;
        }
    }
    /// Whether this model appears in the picker for the given auth mode.
    ///
    /// | `hidden` | `supported_in_api` | OAuth user | API-key user |
    /// |----------|--------------------|------------|--------------|
    /// | true     | _                  | hidden     | hidden       |
    /// | false    | true               | visible    | visible      |
    /// | false    | false              | visible    | **hidden**   |
    pub fn visible_for_auth(&self, is_session_auth: bool) -> bool {
        !self.hidden && (is_session_auth || self.supported_in_api)
    }
}
/// Flat struct so credential and endpoint fields coexist after deep-merge.
/// Routing reads fields, not provenance.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelEntry {
    pub info: ModelInfo,
    pub api_key: Option<String>,
    pub env_key: Option<EnvKeys>,
    /// When set, `base_url` is used for session auth, `api_base_url` for API-key auth.
    pub api_base_url: Option<String>,
    #[serde(default)]
    pub request_payload: serde_json::Map<String, serde_json::Value>,
    /// Provider/model-exact transport route for experimental remote
    /// compaction. Never merged into ordinary request JSON.
    #[serde(default)]
    pub remote_compaction_endpoint: Option<String>,
    /// Provider/model-exact transport route for experimental OpenAI
    /// Images-compatible generation. Never inherited from common defaults.
    #[serde(default)]
    pub image_generation_endpoint: Option<String>,
}
impl ModelEntry {
    /// Minimal fallback entry for an unknown model slug.
    pub fn fallback(slug: &str, endpoints: &EndpointsConfig) -> Self {
        let mut info = ModelInfo::fallback(slug);
        info.base_url = endpoints.resolve_inference_base_url();
        Self {
            info,
            api_key: None,
            env_key: None,
            api_base_url: None,
            request_payload: serde_json::Map::new(),
            remote_compaction_endpoint: None,
            image_generation_endpoint: None,
        }
    }
    pub fn info(&self) -> &ModelInfo {
        &self.info
    }
    pub fn from_config_entry(entry: &ModelEntryConfig) -> Self {
        Self {
            info: ModelInfo::from_config(entry),
            api_key: entry.api_key.clone(),
            env_key: entry.env_key.clone(),
            api_base_url: entry.api_base_url.clone(),
            request_payload: serde_json::Map::new(),
            remote_compaction_endpoint: None,
            image_generation_endpoint: None,
        }
    }
    /// The model's own (BYOK) credential: a non-empty `api_key`, else the first
    /// set, non-empty `env_key` value. `None` means the model has no usable own
    /// credential and resolution should fall through to the session / global key.
    fn own_credential(&self) -> Option<String> {
        first_own_credential(self.api_key.as_deref(), self.env_key.as_ref())
    }
    /// `true` when the model has a non-empty `api_key` or an `env_key` that
    /// resolves to a non-empty value.
    /// Probes `std::env::var` at call time — result is not stable across env changes.
    pub fn has_own_credentials(&self) -> bool {
        self.own_credential().is_some()
    }
}
impl std::ops::Deref for ModelEntry {
    type Target = ModelInfo;
    fn deref(&self) -> &ModelInfo {
        &self.info
    }
}
fn is_false(v: &bool) -> bool {
    !v
}
fn default_true() -> bool {
    true
}
/// Codebase indexing setting for `[features] codebase_indexing`.
///
/// Patterns are matched against the git root when available, otherwise the cwd,
/// which allows explicitly indexing non-git directories.
///
/// ```toml
/// codebase_indexing = false                                          # disable
/// codebase_indexing = true                                           # any git repo (default)
/// codebase_indexing = ["/Users/*/xai*", "!/Users/*/old-*"]           # globs, ! to exclude
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodebaseIndexingSetting {
    Enabled(bool),
    Patterns(Vec<String>),
}
impl Default for CodebaseIndexingSetting {
    fn default() -> Self {
        Self::Enabled(true)
    }
}
impl CodebaseIndexingSetting {
    /// Should `path` be indexed? For `Enabled(true)`, always yes (caller gates on git-root).
    /// For `Patterns`, path must match an include and not match any `!exclude`.
    pub fn should_index(&self, path: &std::path::Path) -> bool {
        match self {
            Self::Enabled(b) => *b,
            Self::Patterns(patterns) => {
                let path_str = path.to_string_lossy();
                let matches_any = |pats: &[&str]| {
                    pats.iter()
                        .any(|p| glob::Pattern::new(p).is_ok_and(|pat| pat.matches(&path_str)))
                };
                let (excludes, includes): (Vec<_>, Vec<_>) =
                    patterns.iter().partition(|p| p.starts_with('!'));
                let excludes: Vec<&str> = excludes
                    .iter()
                    .map(|p| p.strip_prefix('!').unwrap_or(p.as_str()))
                    .collect();
                let includes: Vec<&str> = includes.iter().map(|p| p.as_str()).collect();
                let included = includes.is_empty() || matches_any(&includes);
                let excluded = matches_any(&excludes);
                included && !excluded
            }
        }
    }
}
/// Optional role pair that drops a malformed value to `None` (with a warn)
/// instead of failing the whole config parse — one typo must not wipe the
/// config. Mirrors the remote tolerance in `util::config::remote`.
fn de_tolerant_goal_role_model<'de, D>(
    deserializer: D,
) -> Result<Option<crate::util::config::GoalRoleModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<toml::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| {
        v.try_into()
            .map_err(|e| {
                tracing::warn!(
                    error = % e, "[goal] role model: dropped malformed value"
                )
            })
            .ok()
    }))
}
/// Skeptic pool variant of [`de_tolerant_goal_role_model`]: a non-array yields
/// an empty pool; malformed entries are dropped, survivor order preserved (the
/// skeptic round-robin depends on it).
fn de_tolerant_goal_role_models<'de, D>(
    deserializer: D,
) -> Result<Vec<crate::util::config::GoalRoleModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<toml::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(toml::Value::Array(arr)) => arr
            .into_iter()
            .filter_map(|v| {
                v.try_into()
                    .map_err(|e| {
                        tracing::warn!(
                            error = % e, "[goal] skeptic model: dropped malformed entry"
                        );
                    })
                    .ok()
            })
            .collect(),
        _ => Vec::new(),
    })
}
/// `[goal]` section: the canonical home for `/goal` configuration. Field names
/// mirror the remote `goal_*` keys with the prefix dropped, so config and remote
/// stay 1:1. Per-key precedence is env > this config > remote > default.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GoalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planner_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_current_model_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifier_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_max_runs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategist_every: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverify_after: Option<u32>,
    #[serde(
        default,
        deserialize_with = "de_tolerant_goal_role_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub planner_model: Option<crate::util::config::GoalRoleModel>,
    #[serde(
        default,
        deserialize_with = "de_tolerant_goal_role_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub strategist_model: Option<crate::util::config::GoalRoleModel>,
    #[serde(
        default,
        deserialize_with = "de_tolerant_goal_role_models",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub skeptic_models: Vec<crate::util::config::GoalRoleModel>,
}
/// `[auto_mode]` section: server-side configuration for Auto permission mode.
/// ONE struct serves both the local `[auto_mode]` TOML table and the remote
/// remote settings `auto_mode` JSON object (coerced via `serde_json::from_value`), so
/// the two stay 1:1. All fields are plain scalars/enums, so they deserialize
/// cleanly from both formats (no custom tolerant deser needed). Unset fields stay
/// `None` here; the wire fn applies the built-in defaults once auto mode is
/// enabled (current model, `low` effort if the model supports it, `just_command`
/// prompt). Precedence: local config > remote > those built-in defaults.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoModeConfig {
    /// The Auto-mode gate. Lowest-precedence layer of the gate chain (env and
    /// local `[auto_mode] enabled` config win over this remote value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// How much context the classifier prompt includes. `None` ⇒ the wire fn's
    /// built-in default (`just_command`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_type: Option<atelier_workspace::permission::ClassifierPromptType>,
    /// Routing slug for a dedicated classifier model. `None` ⇒ inherit the
    /// session model. Resolved via `resolve_aux_model_sampling_config`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_model: Option<String>,
    /// Classifier reasoning effort. Applies on BOTH the routed-model path and the
    /// inherited session-model path; `None` ⇒ the wire fn's built-in default
    /// (`low` if the effective model supports reasoning effort, else unset).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Features {
    /// when set, the agent may ask permission for tool executions
    #[serde(default)]
    pub support_permission: bool,
    /// `None` = defer to remote settings / default (off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryMode>,
    /// Codebase graph indexing for go-to-definition/references.
    /// Accepts: true | false | ["glob", "!negative-glob", ...]
    /// Default: true (index any git repo). Patterns can explicitly match non-git directories.
    #[serde(default)]
    pub codebase_indexing: CodebaseIndexingSetting,
    /// Show a blocking warning when Atelier starts outside a Git repository.
    /// Default: false. Used as the local fallback when the `non_git_warning` remote settings
    /// flag in `atelier_build_settings` is absent. When the remote flag is present it takes
    /// precedence — `Some(false)` from remote settings overrides `true` here.
    #[serde(default)]
    pub non_git_warning: bool,
    /// Feedback system (heuristic popups + `/feedback` slash command).
    /// `None` = defer to remote settings / default (false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<bool>,
    /// Managed config fetching (managed_config.toml + requirements.toml).
    /// `None` = defer to env / default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_config: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsp_tools: Option<bool>,
    /// MCP tool search/discovery. `None` = defer to remote settings / env / default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_search: Option<bool>,
    /// Web fetch tool. `None` = defer to remote settings / env / default (false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch: Option<bool>,
    /// Ask-user-question tool. `None` = defer to remote settings / env / default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_user_question: Option<bool>,
    /// Session recap (`/recap` + automatic return-from-away recap).
    /// `None` = defer to remote settings / env / default (`true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_recap: Option<bool>,
    /// Voice dictation (STT). `None` = env / remote / default on.
    /// Set `false` in requirements or managed config to force off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_mode: Option<bool>,
    /// Two-pass (prefire) compaction: speculatively summarize the history
    /// prefix in the background, then summarize NOTE₁ + recent tail at
    /// compaction. `None` = defer to remote settings / env / default (`false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub two_pass_compaction: Option<bool>,
    /// Video generation tool. `None` = defer to remote settings / env / default (false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_gen: Option<bool>,
    /// Write file tool. `None` = defer to remote settings / env / default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_file: Option<bool>,
    /// Cancel-rewind: Ctrl+C before first activity restores the prompt.
    /// `None` = defer to remote settings / env / default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_rewind: Option<bool>,
    /// Auto-wake: immediately inject a synthetic prompt when a background
    /// task or subagent completes, instead of waiting for the idle drain.
    /// `None` = defer to remote settings / env / default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_wake: Option<bool>,
    /// Backend-executed tools (web_search, x_search run server-side).
    /// `None` = defer to env / default (true). Set `false` to force
    /// client-side tool execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_tools: Option<bool>,
    /// `summary` (default) | `transcript` | `segments`. `None` = defer to CLI /
    /// env (`ATELIER_COMPACTION_MODE`). Parsed via `CompactionMode::parse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_mode: Option<String>,
    /// `none` | `minimal` | `balanced` | `verbose` (default). `None` = defer to
    /// env (`ATELIER_COMPACTION_DETAIL`). The `segments` verbatim detail level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_detail: Option<String>,
    /// Feed the summarizer the verbatim conversation instead of the lossy rewrite; `None` = defer to env/remote settings/default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_verbatim_input: Option<bool>,
    /// Snapshot a completed subagent's isolated worktree into a durable git ref
    /// and delete its directory (resume rehydrates from the ref). This is the
    /// per-deployment rollout lever (set in managed_config.toml `[features]`).
    /// `None` = defer to remote settings / default (false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_worktree_snapshot: Option<bool>,
    /// Per-`Ready`-client transport-liveness pollers + the
    /// session-actor `StatusDispatcher`.
    ///
    /// When `true` (default), each successfully-handshaken MCP
    /// client gets a poller that detects rmcp service-loop
    /// termination and pushes `atelier/mcp/server_status` updates to
    /// the client. When `false`, neither watchers nor the
    /// dispatcher are spawned — useful as an emergency kill switch
    /// for the rollout. `None` = defer to env / default (true).
    ///
    /// Resolved via [`Config::resolve_mcp_liveness_watchers`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_liveness_watchers: Option<bool>,
    /// Bounded stdio auto-restart task.
    ///
    /// When `true`, the session-actor `StatusDispatcher` reacts to
    /// `TransportClosed` / `HandshakeFailed` events on stdio MCP
    /// servers by scheduling up to 3 respawn attempts with
    /// `[1s, 4s, 16s]` backoff. HTTP / HttpAuth servers are NOT
    /// auto-restarted (their existing `reset_transport` path
    /// covers the recovery). `None` = defer to env / default
    /// (recovery is on by default; set `false` here / via
    /// `ATELIER_MCP_AUTO_RESTART` to opt out).
    ///
    /// Resolved via [`Config::resolve_mcp_auto_restart`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_auto_restart: Option<bool>,
    /// Pager-side subscription to the `atelier/mcp/server_status` push.
    ///
    /// When `true` (default), the pager subscribes to the per-server
    /// status delta the shell emits via the dispatcher and
    /// patches the MCP servers modal in-place (no re-fetch round
    /// trip). When `false`, the pager ignores the push and falls
    /// back to the legacy `atelier/mcp/tools_changed` debounced refetch
    /// path. `None` = defer to env / default (true).
    ///
    /// The pager-side gate
    /// (`acp_handler::push_server_status_enabled`) uses an
    /// **env-only** OnceLock cache via
    /// [`crate::util::config::resolve_mcp_push_server_status(None, None, None)`].
    /// That function consults `BoolFlag::env` and the default `true`
    /// — it does NOT read this `Features` field. The shell-side
    /// `Config::resolve_mcp_push_server_status` does delegate
    /// through this field, but the pager never holds a `Config`.
    ///
    /// Practical consequence: setting
    /// `[features] mcp_push_server_status = false` in
    /// `~/.atelier/config.toml` will NOT disable the pager's
    /// subscription on a freshly-launched process. To disable the
    /// pager subscription, set `ATELIER_MCP_PUSH_SERVER_STATUS=0` in
    /// the env before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_push_server_status: Option<bool>,
    /// Whether the leader's `ConfigFileWatcher` adds the two narrow
    /// non-recursive watches for `<cwd>/` and `<cwd>/.atelier/`.
    ///
    /// When `true` (default), edits to `<cwd>/.mcp.json`,
    /// `<cwd>/.atelier/config.toml`, or `<cwd>/.claude.json` flow
    /// through the watcher → reloader → `ConfigUpdate::
    /// ProjectMcpServersChanged { cwd }` → `app.rs` ACP-injection
    /// pipeline and the affected sessions reload their MCP servers
    /// within the debounce window (~ 1 s). When `false`, the leader
    /// skips the cwd watches entirely and the only way to pick up a
    /// project-config edit is the user-triggered refresh button.
    ///
    /// The watches are **always non-recursive** — the name follows
    /// the convention for the rollout-gate flag. See
    /// `crate::config::watcher::ConfigFileWatcher::watch_path` for
    /// the inotify-quota rationale.
    ///
    /// The name is a documented misnomer — it gates
    /// the existence of the **cwd** watches, NOT their recursion
    /// mode. A future rename to `mcp_cwd_config_watch` would align
    /// name and behavior; deferred to a follow-up to avoid widening
    /// the config surface across requirements.toml / managed configs.
    ///
    /// Resolved via [`Config::resolve_mcp_recursive_config_watch`].
    /// `None` = defer to env / default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_recursive_config_watch: Option<bool>,
}
/// Resolved credentials for a model session.
pub struct ResolvedCredentials {
    pub api_key: Option<String>,
    pub base_url: String,
    pub auth_type: atelier_chat_state::AuthType,
    pub auth_scheme: AuthScheme,
}
/// First usable BYOK credential: a non-empty (trimmed) api_key, else the first
/// set, non-empty env_key value. Single source of truth for has_own_credentials,
/// resolve_credentials, and the JWT-reload path.
pub(crate) fn first_own_credential(
    api_key: Option<&str>,
    env_key: Option<&EnvKeys>,
) -> Option<String> {
    api_key
        .filter(|k| !k.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| env_key.and_then(EnvKeys::resolve_value))
}

fn is_local_provider_model(model: &ModelEntry) -> bool {
    model
        .info()
        .id
        .as_deref()
        .is_some_and(|id| id.split_once('/').is_some())
}

/// Resolve credentials for a model.
/// Priority: model api_key/env_key > session token.
///
/// When `env_key` lists multiple names, the first set non-empty value is used.
pub fn resolve_credentials(model: &ModelEntry, session_key: Option<&str>) -> ResolvedCredentials {
    let info = model.info();
    let local_provider_model = is_local_provider_model(model);
    let (api_key, base_url, auth_type) = if let Some(key) = model.own_credential() {
        (
            Some(key),
            info.base_url.clone(),
            atelier_chat_state::AuthType::ApiKey,
        )
    } else if !local_provider_model && let Some(key) = session_key {
        (
            Some(key.to_owned()),
            info.base_url.clone(),
            atelier_chat_state::AuthType::SessionToken,
        )
    } else {
        if let Some(ref env_keys) = model.env_key
            && !env_keys.is_empty()
        {
            tracing::warn!(
                model = % info.model, env_key = % env_keys,
                "model has env_key configured but none of the environment variables are set — \
                 requests will have no API key",
            );
        }
        (
            None,
            info.base_url.clone(),
            atelier_chat_state::AuthType::ApiKey,
        )
    };
    let auth_scheme = info.auth_scheme.clone();
    tracing::debug!(
        model = % info.model, auth_type = ? auth_type, "resolved credentials"
    );
    ResolvedCredentials {
        api_key,
        base_url,
        auth_type,
        auth_scheme,
    }
}
/// `disable_api_key_auth` at the credential seam: replace any Provider API-key
/// credential with the configured session token. If no session token exists,
/// the request fails closed instead of using the Provider key.
pub fn enforce_disable_api_key_auth(
    creds: &mut ResolvedCredentials,
    disable_api_key_auth: bool,
    session_key: Option<&str>,
) {
    if disable_api_key_auth && creds.auth_type == atelier_chat_state::AuthType::ApiKey {
        creds.auth_type = atelier_chat_state::AuthType::SessionToken;
        creds.api_key = session_key.map(str::to_owned);
        atelier_telemetry::unified_log::debug(
            "auth: kill switch blocked a Provider API key at the credential seam",
            None,
            Some(serde_json::json!(
                { "replaced_with_session" : session_key.is_some(), "base_url" : creds
                .base_url, }
            )),
        );
    }
}
/// Resolve credentials for an auxiliary sampling path (web search, image
/// description) with the first-party API-key kill switch applied, so these
/// paths honor `disable_api_key_auth` exactly like the main chat path.
fn resolve_credentials_enforced(
    entry: &ModelEntry,
    session_key: Option<&str>,
    disable_api_key_auth: bool,
) -> ResolvedCredentials {
    let mut credentials = resolve_credentials(entry, session_key);
    enforce_disable_api_key_auth(&mut credentials, disable_api_key_auth, session_key);
    credentials
}
pub use atelier_telemetry::config::deployment_id_from_key;
/// Try to resolve credentials for a model by loading the effective config.
/// Returns `None` (with a warning) if config loading, parsing, or model
/// lookup fails. `session_key` should only be passed when `auth_type` is
/// `SessionToken` — callers must guard this.
pub fn try_resolve_model_credentials(
    model_id: &str,
    session_key: Option<&str>,
) -> Option<ResolvedCredentials> {
    let raw = crate::config::load_effective_config()
        .map_err(|e| tracing::warn!(error = % e, "config load failed for credential resolution"))
        .ok()?;
    let cfg = Config::new_from_toml_cfg(&raw)
        .map_err(|e| tracing::warn!(error = % e, "config parse failed for credential resolution"))
        .ok()?;
    let models = resolve_model_list(&cfg, None);
    let entry = find_model_by_id(&models, model_id)?;
    let mut credentials = resolve_credentials(entry, session_key);
    enforce_disable_api_key_auth(
        &mut credentials,
        cfg.atelier_com_config.api_key_auth_disabled(),
        session_key,
    );
    Some(credentials)
}
/// Per-model auth facts (BYOK status + auth scheme) from one effective-config
/// load, memoized by the session actor.
#[derive(Clone)]
pub struct ModelAuthFacts {
    pub byok: ModelByok,
    pub auth_scheme: AuthScheme,
}
/// Resolve `model_id` to its auth facts from one effective-config load.
/// Load/parse failure → `byok = Unknown`; model absent from the catalog →
/// `NotByok`. An empty `model_id` (no sampling config yet) → `Unknown`, not
/// `NotByok`, so the gate isn't activated for an unidentified model.
pub fn resolve_model_auth_facts(model_id: &str) -> ModelAuthFacts {
    if model_id.is_empty() {
        return ModelAuthFacts {
            byok: ModelByok::Unknown,
            auth_scheme: AuthScheme::default(),
        };
    }
    with_resolved_model(model_id, |lookup| ModelAuthFacts {
        byok: byok_from_lookup(&lookup),
        auth_scheme: match lookup {
            ModelLookup::Loaded(Some(e)) => e.info().auth_scheme.clone(),
            _ => AuthScheme::default(),
        },
    })
}
fn byok_from_lookup(lookup: &ModelLookup) -> ModelByok {
    match lookup {
        ModelLookup::ConfigUnavailable => ModelByok::Unknown,
        ModelLookup::Loaded(Some(e)) if e.has_own_credentials() => ModelByok::Byok,
        ModelLookup::Loaded(_) => ModelByok::NotByok,
    }
}
enum ModelLookup<'a> {
    /// `None` if `model_id` is absent from the catalog.
    Loaded(Option<&'a ModelEntry>),
    ConfigUnavailable,
}
/// Load + parse the effective config and hand the `model_id` lookup to `f`,
/// keeping "config unavailable" distinct from "model absent" so callers can
/// stay conservative on a transient config failure.
fn with_resolved_model<T>(model_id: &str, f: impl FnOnce(ModelLookup) -> T) -> T {
    let Some(raw) = crate::config::load_effective_config()
        .map_err(|e| tracing::warn!(error = % e, "config load failed for model auth lookup"))
        .ok()
    else {
        return f(ModelLookup::ConfigUnavailable);
    };
    let Some(cfg) = Config::new_from_toml_cfg(&raw)
        .map_err(|e| tracing::warn!(error = % e, "config parse failed for model auth lookup"))
        .ok()
    else {
        return f(ModelLookup::ConfigUnavailable);
    };
    let models = resolve_model_list(&cfg, None);
    f(ModelLookup::Loaded(find_model_by_id(&models, model_id)))
}
/// Resolve a standalone `SamplerConfig` for an auxiliary model slug (image
/// description, session summary, ...), resolved through the catalog so a
/// `[model.*]` override redirects it to its own endpoint, credentials, and
/// routing `model`. `None` → caller falls back to the active session's model.
pub fn resolve_aux_model_sampling_config(
    model_id: &str,
    models: &IndexMap<String, ModelEntry>,
    endpoints: &EndpointsConfig,
    session_key: Option<&str>,
    disable_api_key_auth: bool,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
) -> Option<SamplerConfig> {
    let catalog_entry = find_model_by_id(models, model_id).cloned();
    if let Some(entry) = &catalog_entry {
        let credentials = resolve_credentials_enforced(entry, session_key, disable_api_key_auth);
        let sampler = sampling_config_for_model(
            entry,
            credentials,
            alpha_test_key.clone(),
            client_version.clone(),
            None,
            None,
        );
        if sampler.api_key.is_some() {
            return Some(sampler);
        }
    }
    let configured_bearer = session_key
        .map(|s| s.to_owned())
        .or_else(|| endpoints.deployment_key.clone());
    if let Some(bearer) = configured_bearer {
        let entry = ModelEntry {
            info: ModelInfo {
                user_selectable: true,
                id: None,
                model: catalog_entry
                    .map(|e| e.info.model)
                    .unwrap_or_else(|| model_id.to_owned()),
                base_url: endpoints.resolve_inference_base_url(),
                name: None,
                description: None,
                max_completion_tokens: None,
                temperature: None,
                top_p: None,
                api_backend: ApiBackend::Responses,
                auth_scheme: Default::default(),
                extra_headers: IndexMap::new(),
                context_window: NonZeroU64::new(200_000).unwrap(),
                auto_compact_threshold_percent: None,
                system_prompt_label: None,
                use_concise: false,
                agent_type: default_agent_type(),
                inference_idle_timeout_secs: None,
                max_retries: None,
                hidden: true,
                supported_in_api: true,
                reasoning_effort: None,
                supports_reasoning_effort: false,
                reasoning_efforts: Vec::new(),
                accepts_images: false,
                supports_fast_mode: false,
                supports_backend_search: false,
                compactions_remaining: None,
                compaction_at_tokens: None,
                show_model_fingerprint: false,
                stream_tool_calls: None,
                laziness_detector: LazinessDetectorPerModelConfig::default(),
            },
            api_key: Some(bearer),
            env_key: None,
            api_base_url: None,
            request_payload: serde_json::Map::new(),
            remote_compaction_endpoint: None,
            image_generation_endpoint: None,
        };
        let credentials = resolve_credentials_enforced(&entry, session_key, disable_api_key_auth);
        let sampler = sampling_config_for_model(
            &entry,
            credentials,
            alpha_test_key,
            client_version,
            None,
            None,
        );
        return Some(sampler);
    }
    tracing::warn!(
        aux_model = % model_id,
        "no credentials for auxiliary model; falling back to active model",
    );
    None
}
/// Finalize image-describe model + sampler config for user attachments.
/// Shared so the aux resolve happy path and the
/// `None` fallback cannot diverge between those entry points.
///
/// On aux resolve `Some`, stamp session-local fields (client id, attribution, bearer,
/// retries) onto the helper config. On `None`, fall back to the active session model and
/// full config (not forcing `image_description_model` onto the agent endpoint, which 404s
/// on BYOK / non-proxy routes for internal slugs like `atelier-build`).
/// Stamp the session-local fields (client id, attribution, bearer resolver,
/// retries) from the active session onto a routed aux `SamplerConfig` so a
/// helper model keeps the session's auth/attribution. Shared by image-describe
/// and the auto-mode classifier so the two can't drift.
pub fn stamp_session_local_sampler_fields(
    cfg: &mut SamplerConfig,
    active_session_config: &SamplerConfig,
    client_identifier: Option<String>,
    max_retries: Option<u32>,
) {
    cfg.client_identifier = client_identifier;
    cfg.attribution_callback = active_session_config.attribution_callback.clone();
    cfg.bearer_resolver = active_session_config.bearer_resolver.clone();
    cfg.max_retries = max_retries;
}
pub fn finalize_image_describe_sampler_config(
    resolved_aux: Option<SamplerConfig>,
    active_session_config: &SamplerConfig,
    client_identifier: Option<String>,
    max_retries: Option<u32>,
) -> (String, SamplerConfig) {
    match resolved_aux {
        Some(mut describe_cfg) => {
            stamp_session_local_sampler_fields(
                &mut describe_cfg,
                active_session_config,
                client_identifier,
                max_retries,
            );
            let model = describe_cfg.model.clone();
            (model, describe_cfg)
        }
        None => {
            let model = active_session_config.model.clone();
            (model, active_session_config.clone())
        }
    }
}
/// Re-derive `auth_type` from the model's own credentials so BYOK env-key
/// models stay on `ApiKey` even when a session token is present. Falls
/// back to `fallback` when the model isn't in the on-disk catalog.
pub fn resolve_chat_state_auth_type(
    model_id: &str,
    session_key: Option<&str>,
    fallback: atelier_chat_state::AuthType,
) -> atelier_chat_state::AuthType {
    try_resolve_model_credentials(model_id, session_key)
        .map(|r| r.auth_type)
        .unwrap_or(fallback)
}
pub fn sampling_config_for_model(
    model: &ModelEntry,
    credentials: ResolvedCredentials,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
    deployment_id: Option<String>,
    user_id: Option<String>,
) -> SamplerConfig {
    let info = model.info();
    let provider_id = info
        .id
        .as_deref()
        .and_then(|id| atelier_provider::ModelKey::parse(id).ok())
        .map(|key| key.provider_id);
    let oauth_resolvers = provider_id
        .as_deref()
        .and_then(crate::extensions::provider::oauth_request_resolvers);
    let model_name = info.model.clone();
    let max_completion_tokens = info.max_completion_tokens;
    let temperature = info.temperature;
    let top_p = info.top_p;
    let mut extra_headers = info.extra_headers.clone();
    inject_url_derived_headers(
        &mut extra_headers,
        alpha_test_key.as_deref(),
        &credentials.base_url,
    );
    let api_backend = info.api_backend.clone();
    SamplerConfig {
        api_key: credentials.api_key,
        model: model_name,
        base_url: credentials.base_url,
        provider_id,
        max_completion_tokens,
        temperature,
        top_p,
        request_payload: model.request_payload.clone(),
        remote_compaction_endpoint: model.remote_compaction_endpoint.clone(),
        image_generation_endpoint: model.image_generation_endpoint.clone(),
        api_backend,
        auth_scheme: credentials.auth_scheme,
        extra_headers,
        context_window: info.context_window.get(),
        client_version,
        reasoning_effort: info.reasoning_effort,
        force_http1: false,
        max_retries: info.max_retries,
        stream_tool_calls: info.stream_tool_calls.unwrap_or(false),
        idle_timeout_secs: None,
        client_identifier: None,
        deployment_id,
        user_id,
        origin_client: None,
        attribution_callback: None,
        bearer_resolver: oauth_resolvers.as_ref().map(|(bearer, _)| bearer.clone()),
        supports_backend_search: info.supports_backend_search,
        compactions_remaining: info.compactions_remaining,
        compaction_at_tokens: info.compaction_at_tokens,
        doom_loop_recovery: None,
        header_injector: oauth_resolvers.map(|(_, headers)| headers),
    }
}
/// Fold URL-derived headers into `extra_headers`.
///
/// The sampler crate is intentionally URL-agnostic: it does not inspect
/// `base_url` to decide which auth or staging headers to add. Replicate the
/// URL-derived header logic at the shell boundary so callers downstream see a
/// single homogenous header bag.
///
/// * cli-chat-proxy bases get `X-XAI-Token-Auth` and
///   `x-authenticateresponse` headers (mirrors the inline match in the legacy
///   `sampling::Client::new` on `is_cli_chat_proxy_url`).
/// * With the optional non-production feature, matching first-party hosts may
///   get an extra access header from the corresponding key argument.
///
/// Existing entries are never overwritten so callers can pre-set a value.
pub fn inject_url_derived_headers(
    headers: &mut IndexMap<String, String>,
    alpha_test_key: Option<&str>,
    base_url: &str,
) {
    if crate::util::is_cli_chat_proxy_url(base_url) {
        headers
            .entry("X-XAI-Token-Auth".to_string())
            .or_insert_with(|| "atelier-cli".to_string());
        headers
            .entry("x-authenticateresponse".to_string())
            .or_insert_with(|| "authenticate-response".to_string());
        headers
            .entry(crate::http::CLIENT_MODE_HEADER.to_string())
            .or_insert_with(|| crate::http::process_client_mode().to_string());
    }
    let _ = (alpha_test_key, base_url);
}
pub fn resolve_model_to_sampling_config(
    model_id: &str,
    models: &IndexMap<String, ModelEntry>,
    session_key: Option<&str>,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
    fallback_entry: Option<ModelEntry>,
) -> Option<SamplerConfig> {
    let entry = find_model_by_id(models, model_id)
        .cloned()
        .or(fallback_entry)?;
    let credentials = resolve_credentials(&entry, session_key);
    Some(sampling_config_for_model(
        &entry,
        credentials,
        alpha_test_key,
        client_version,
        None,
        None,
    ))
}
fn resolve_hidden_default_web_search_sampling_config(
    model_id: &str,
    session_key: Option<&str>,
    disable_api_key_auth: bool,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
    endpoints: &EndpointsConfig,
) -> SamplerConfig {
    let entry = ModelEntry {
        info: ModelInfo {
            id: None,
            model: model_id.to_owned(),
            base_url: endpoints.resolve_inference_base_url(),
            name: None,
            description: None,
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: ApiBackend::Responses,
            auth_scheme: Default::default(),
            extra_headers: IndexMap::new(),
            context_window: NonZeroU64::new(200_000).unwrap(),
            auto_compact_threshold_percent: None,
            system_prompt_label: None,
            use_concise: false,
            agent_type: default_agent_type(),
            inference_idle_timeout_secs: None,
            max_retries: None,
            hidden: true,
            user_selectable: true,
            supported_in_api: true,
            reasoning_effort: None,
            supports_reasoning_effort: false,
            reasoning_efforts: Vec::new(),
            accepts_images: false,
            supports_fast_mode: false,
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            show_model_fingerprint: false,
            stream_tool_calls: None,
            laziness_detector: LazinessDetectorPerModelConfig::default(),
        },
        api_key: None,
        env_key: None,
        api_base_url: None,
        request_payload: serde_json::Map::new(),
        remote_compaction_endpoint: None,
        image_generation_endpoint: None,
    };
    let credentials = resolve_credentials_enforced(&entry, session_key, disable_api_key_auth);
    sampling_config_for_model(
        &entry,
        credentials,
        alpha_test_key,
        client_version,
        None,
        None,
    )
}
pub fn resolve_web_search_sampling_config(
    model_id: &str,
    models: &IndexMap<String, ModelEntry>,
    session_key: Option<&str>,
    disable_api_key_auth: bool,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
    endpoints: &EndpointsConfig,
) -> Option<SamplerConfig> {
    let resolved = if let Some(entry) = find_model_by_id(models, model_id).cloned() {
        let credentials = resolve_credentials_enforced(&entry, session_key, disable_api_key_auth);
        Some(sampling_config_for_model(
            &entry,
            credentials,
            alpha_test_key,
            client_version,
            None,
            None,
        ))
    } else if model_id == crate::models::default_web_search_model() {
        Some(resolve_hidden_default_web_search_sampling_config(
            model_id,
            session_key,
            disable_api_key_auth,
            alpha_test_key,
            client_version,
            endpoints,
        ))
    } else {
        None
    };
    if resolved.is_none() {
        tracing::warn!(
            web_search_model = % model_id,
            "configured web_search model not found; disabling web search"
        );
    }
    resolved.map(crate::tools::config::web_search_sampling_config)
}
pub fn to_acp_model_info(
    models: &IndexMap<String, ModelEntry>,
) -> IndexMap<acp::ModelId, acp::ModelInfo> {
    models
        .iter()
        .map(|(key, model)| {
            let info = model.info();
            let model_id = acp::ModelId::new(Arc::from(key.clone()));
            let total_context_tokens = info.context_window.get();
            let meta = {
                let mut map = serde_json::Map::new();
                map.insert(
                    "totalContextTokens".to_string(),
                    serde_json::Value::Number(total_context_tokens.into()),
                );
                map.insert(
                    "agentType".to_string(),
                    serde_json::Value::String(info.agent_type.clone()),
                );
                if let Some((provider_id, provider_model_id)) = key.split_once('/') {
                    map.insert(
                        "providerId".to_string(),
                        serde_json::Value::String(provider_id.to_owned()),
                    );
                    map.insert(
                        "modelId".to_string(),
                        serde_json::Value::String(provider_model_id.to_owned()),
                    );
                }
                map.insert(
                    "wireApi".to_string(),
                    serde_json::Value::String(
                        match &info.api_backend {
                            ApiBackend::ChatCompletions => "chat_completions",
                            ApiBackend::Responses => "responses",
                            ApiBackend::Messages => "messages",
                        }
                        .to_owned(),
                    ),
                );
                map.insert(
                    "acceptsImages".to_string(),
                    serde_json::Value::Bool(info.accepts_images),
                );
                map.insert(
                    "supportsFastMode".to_string(),
                    serde_json::Value::Bool(info.supports_fast_mode),
                );
                if info.supports_reasoning_effort {
                    map.insert(
                        "supportsReasoningEffort".to_string(),
                        serde_json::Value::Bool(true),
                    );
                    if let Some(effort) = info.reasoning_effort {
                        map.insert(
                            REASONING_EFFORT_META_KEY.to_string(),
                            reasoning_effort_meta_value(effort),
                        );
                    }
                }
                if !info.reasoning_efforts.is_empty() {
                    map.insert(
                        REASONING_EFFORTS_META_KEY.to_string(),
                        reasoning_efforts_meta_value(&info.reasoning_efforts),
                    );
                }
                if map.is_empty() { None } else { Some(map) }
            };
            (
                model_id.clone(),
                acp::ModelInfo::new(
                    model_id,
                    info.name.clone().unwrap_or_else(|| info.model.clone()),
                )
                .description(info.description.clone())
                .meta(meta),
            )
        })
        .collect()
}
/// Error code for model switch rejection due to agent type mismatch.
pub const MODEL_SWITCH_INCOMPATIBLE_AGENT: &str = "MODEL_SWITCH_INCOMPATIBLE_AGENT";
/// Error code for model switch failure during the zero-turn full harness
/// rebuild path. Emitted when `RebuildAgentForDefinition` fails (definition
/// could not be resolved at handler time, `AgentBuilder::build()` errored,
/// or a turn started racing the rebuild).
pub const MODEL_SWITCH_REBUILD_FAILED: &str = "MODEL_SWITCH_REBUILD_FAILED";
/// Structured error payload for model switch rejection due to agent type
/// incompatibility. Serialized into `acp::Error.data` by the shell and
/// deserialized by the TUI for user-friendly error rendering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelSwitchIncompatibleAgentError {
    /// Stable machine-readable error code (always `MODEL_SWITCH_INCOMPATIBLE_AGENT`).
    pub code: String,
    /// The agent type currently active in the session.
    pub active_agent_type: String,
    /// The agent type required by the target model.
    pub required_agent_type: String,
    /// The model ID that was requested.
    pub model_id: String,
    /// Remediation hint for the client.
    pub suggestion: String,
}
impl ModelSwitchIncompatibleAgentError {
    /// Build an `acp::Error` with this structured payload.
    pub fn into_acp_error(self) -> acp::Error {
        let message = format!(
            "Cannot switch to model '{}': it requires agent '{}' but the active agent is '{}'. \
             Start a new session to use this model.",
            self.model_id, self.required_agent_type, self.active_agent_type,
        );
        acp::Error::new(acp::ErrorCode::InvalidRequest.into(), message)
            .data(serde_json::to_value(&self).ok())
    }
    /// Try to parse from an `acp::Error.data` field.
    pub fn from_acp_error(err: &acp::Error) -> Option<Self> {
        let data = err.data.as_ref()?;
        let code = data.get("code")?.as_str()?;
        if code != MODEL_SWITCH_INCOMPATIBLE_AGENT {
            return None;
        }
        serde_json::from_value(data.clone()).ok()
    }
    /// Render a user-friendly error message for the TUI.
    pub fn user_message(&self) -> String {
        format!(
            "Cannot switch to '{}' — it requires agent '{}' but the active agent is '{}'. \
             Start /new to use this model.",
            self.model_id, self.required_agent_type, self.active_agent_type,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use atelier_test_support::EnvGuard;
    use serial_test::serial;
    #[test]
    fn main_cli_tools_override_preserves_profile_injection_policy() {
        let overrides = CliAgentOverrides {
            tools: Some(vec!["read_file".into()]),
            ..Default::default()
        };
        let mut cases = vec![(AgentDefinition::default_atelier_build(), true)];
        for (mut definition, expected_injection) in cases {
            overrides.apply_to_definition(&mut definition);
            assert_eq!(definition.tools, vec!["read_file".to_string()]);
            assert_eq!(definition.inject_default_tools, expected_injection);
        }
    }
    /// `AutoModeConfig` parses identically from a local `[auto_mode]` TOML table
    /// and an equivalent remote settings JSON object (serde is format-agnostic). The
    /// lean shape is all scalars/enums, so no custom tolerant deser is needed.
    #[test]
    fn auto_mode_config_parses_from_toml_and_json_equivalently() {
        use atelier_workspace::permission::ClassifierPromptType;
        let toml_src = r#"
enabled = true
prompt_type = "no_user_tool_prefix"
classifier_model = "atelier-4.5"
reasoning_effort = "low"
"#;
        let from_toml: AutoModeConfig = toml::from_str(toml_src).unwrap();
        let json = serde_json::json!(
            { "enabled" : true, "prompt_type" : "no_user_tool_prefix", "classifier_model"
            : "atelier-4.5", "reasoning_effort" : "low" }
        );
        let from_json: AutoModeConfig = serde_json::from_value(json).unwrap();
        for cfg in [&from_toml, &from_json] {
            assert_eq!(cfg.enabled, Some(true));
            assert_eq!(
                cfg.prompt_type,
                Some(ClassifierPromptType::NoUserToolPrefix)
            );
            assert_eq!(cfg.classifier_model.as_deref(), Some("atelier-4.5"));
            assert_eq!(cfg.reasoning_effort, Some(ReasoningEffort::Low));
        }
        let empty: AutoModeConfig = toml::from_str("").unwrap();
        assert!(empty.enabled.is_none() && empty.prompt_type.is_none());
        assert!(empty.classifier_model.is_none() && empty.reasoning_effort.is_none());
    }
    /// `prompt_type` wire values are the snake_case `ClassifierPromptType` names.
    #[test]
    fn auto_mode_prompt_type_parses_snake_case() {
        use atelier_workspace::permission::ClassifierPromptType;
        for (s, variant) in [
            ("full", ClassifierPromptType::Full),
            (
                "no_user_tool_prefix",
                ClassifierPromptType::NoUserToolPrefix,
            ),
            ("bare_instructions", ClassifierPromptType::BareInstructions),
            ("just_command", ClassifierPromptType::JustCommand),
        ] {
            let cfg: AutoModeConfig = toml::from_str(&format!("prompt_type = \"{s}\"")).unwrap();
            assert_eq!(cfg.prompt_type, Some(variant));
        }
    }
    #[test]
    fn laziness_detector_default_is_all_disabled() {
        let cfg = LazinessDetectorPerModelConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.max_nudges_per_session, 0);
        assert_eq!(cfg.idle_threshold_ms, None);
        assert_eq!(cfg.min_confidence, None);
        assert_eq!(
            cfg.include_reasoning, None,
            "include_reasoning defaults to None so the harness default applies",
        );
    }
    #[test]
    fn laziness_detector_absent_block_deserializes_to_default() {
        let json = serde_json::json!(
            { "model" : "test", "base_url" : "https://test.api/v1", "context_window" :
            200_000, }
        );
        let entry: ModelEntryConfig =
            serde_json::from_value(json).expect("ModelEntryConfig deserializes without detector");
        assert_eq!(
            entry.laziness_detector,
            LazinessDetectorPerModelConfig::default()
        );
        let info = ModelInfo::from_config(&entry);
        assert!(!info.laziness_detector.enabled);
    }
    #[test]
    fn laziness_detector_fallback_modelinfo_is_disabled() {
        let info = ModelInfo::fallback("unknown-model");
        assert_eq!(
            info.laziness_detector,
            LazinessDetectorPerModelConfig::default(),
        );
        assert!(!info.laziness_detector.enabled);
        assert_eq!(info.laziness_detector.max_nudges_per_session, 0);
    }
    #[test]
    fn laziness_detector_block_round_trips_through_serde() {
        let json = serde_json::json!(
            { "enabled" : true, "max_nudges_per_session" : 3, "idle_threshold_ms" :
            15_000, "min_confidence" : 0.8, "include_reasoning" : false, }
        );
        let cfg: LazinessDetectorPerModelConfig =
            serde_json::from_value(json).expect("deserialize populated block");
        assert!(cfg.enabled);
        assert_eq!(cfg.max_nudges_per_session, 3);
        assert_eq!(cfg.idle_threshold_ms, Some(15_000));
        assert_eq!(cfg.min_confidence, Some(0.8));
        assert_eq!(cfg.include_reasoning, Some(false));
    }
    /// Pins all three states of the per-model `include_reasoning`
    /// override (`Some(true)`, `Some(false)`, absent → `None`) so a
    /// future drift on the `#[serde(default)]` attribute or the field
    /// type fails the test rather than silently changing the resolved
    /// default.
    #[test]
    fn laziness_detector_include_reasoning_serde_states() {
        let some_true: LazinessDetectorPerModelConfig =
            serde_json::from_value(serde_json::json!({ "include_reasoning" : true }))
                .expect("Some(true)");
        assert_eq!(some_true.include_reasoning, Some(true));
        let some_false: LazinessDetectorPerModelConfig =
            serde_json::from_value(serde_json::json!({ "include_reasoning" : false }))
                .expect("Some(false)");
        assert_eq!(some_false.include_reasoning, Some(false));
        let absent: LazinessDetectorPerModelConfig =
            serde_json::from_value(serde_json::json!({})).expect("absent → None");
        assert_eq!(absent.include_reasoning, None);
    }
    #[test]
    fn subagent_permission_mode_precedence() {
        let own = PermissionMode::Plan;
        let cases = [
            (
                PermissionMode::BypassPermissions,
                PermissionMode::BypassPermissions,
            ),
            (PermissionMode::AcceptEdits, PermissionMode::AcceptEdits),
            (PermissionMode::Auto, PermissionMode::Auto),
            (PermissionMode::Default, own.clone()),
            (PermissionMode::DontAsk, own.clone()),
            (PermissionMode::Plan, own.clone()),
        ];
        for (parent, expected) in cases {
            assert_eq!(
                resolve_subagent_permission_mode(own.clone(), &parent),
                expected,
                "parent={parent:?}"
            );
        }
    }
    #[cfg(any())] // The vendor CLI proxy and its synthetic headers were removed from Atelier.
    #[test]
    fn inject_url_derived_headers_adds_proxy_headers_for_cli_chat_proxy_url() {
        let mut headers = IndexMap::new();
        inject_url_derived_headers(&mut headers, None, crate::env::PROD_CLI_CHAT_PROXY_BASE_URL);
        assert_eq!(
            headers.get("X-XAI-Token-Auth").map(String::as_str),
            Some("atelier-cli")
        );
        assert_eq!(
            headers.get("x-authenticateresponse").map(String::as_str),
            Some("authenticate-response")
        );
    }
    #[test]
    fn inject_url_derived_headers_skips_proxy_headers_for_external_url() {
        let mut headers = IndexMap::new();
        inject_url_derived_headers(&mut headers, None, "https://api.atelier/v1");
        assert!(headers.get("X-XAI-Token-Auth").is_none());
        assert!(headers.get("x-authenticateresponse").is_none());
    }
    #[cfg(any())] // The vendor CLI proxy and its synthetic headers were removed from Atelier.
    #[test]
    fn inject_url_derived_headers_preserves_caller_extra_headers() {
        let mut headers = IndexMap::new();
        headers.insert("x-custom-byok".to_string(), "value".to_string());
        inject_url_derived_headers(&mut headers, None, crate::env::PROD_CLI_CHAT_PROXY_BASE_URL);
        assert_eq!(
            headers.get("x-custom-byok").map(String::as_str),
            Some("value")
        );
        assert_eq!(
            headers.get("X-XAI-Token-Auth").map(String::as_str),
            Some("atelier-cli")
        );
    }
    #[test]
    fn inject_url_derived_headers_does_not_overwrite_existing_entries() {
        let mut headers = IndexMap::new();
        headers.insert("X-XAI-Token-Auth".to_string(), "caller-set".to_string());
        inject_url_derived_headers(&mut headers, None, crate::env::PROD_CLI_CHAT_PROXY_BASE_URL);
        assert_eq!(
            headers.get("X-XAI-Token-Auth").map(String::as_str),
            Some("caller-set"),
        );
    }

    #[test]
    fn new_from_toml_cfg_without_model_remains_unconfigured() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            context = "default"
            request_agent = "atelier"
            "#,
        )
        .unwrap();

        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");

        assert_eq!(cfg.model, None);
    }

    #[test]
    fn new_from_toml_cfg_preserves_top_level_scalar_model() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            model = "example/deepseek-v4-flash"

            [ui]
            compact_mode = true
            "#,
        )
        .unwrap();

        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");

        assert_eq!(
            cfg.model.as_deref(),
            Some("example/deepseek-v4-flash"),
            "the legacy [model.*] extraction pass must not discard top-level config.toml:model",
        );
        assert!(cfg.ui.compact_mode);
    }

    #[test]
    fn parses_toolset_overrides() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [toolset.bash]
            timeout_secs = 123

            [toolset.ask_user_question]
            timeout_enabled = false
            timeout_secs = 30
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        assert_eq!(cfg.toolset.bash.timeout_secs, Some(123.0));
        assert_eq!(cfg.toolset.ask_user_question.timeout_enabled, Some(false));
        assert_eq!(cfg.toolset.ask_user_question.timeout_secs, Some(30));
    }
    #[test]
    fn parses_toolset_bash_float_timeout() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [toolset.bash]
            timeout_secs = 30.5
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        assert_eq!(cfg.toolset.bash.timeout_secs, Some(30.5));
    }
    #[test]
    fn resolve_runtime_fields_propagates_disable_zdr_incompatible_tools() {
        fn ctx(raw: &toml::Value) -> RuntimeResolutionContext<'_> {
            RuntimeResolutionContext {
                raw_config: raw,
                local_runtime_settings: None,
                cwd: None,
                is_headless: false,
                cli_subagents: None,
                cli_web_search_model: None,
                cli_session_summary_model: None,
                cli_experimental_memory: false,
                cli_no_memory: false,
                disable_web_search: false,
                todo_gate: false,
                laziness_debug_log: None,
                storage_mode: None,
            }
        }
        let empty: toml::Value = toml::Value::Table(toml::map::Map::new());
        let mut cfg = Config::new_from_toml_cfg(&empty).unwrap();
        cfg.resolve_runtime_fields(&ctx(&empty));
        assert!(!cfg.disable_zdr_incompatible_tools);
        let zdr: toml::Value =
            toml::from_str("[tools]\ndisable_zdr_incompatible_tools = true").unwrap();
        let mut cfg = Config::new_from_toml_cfg(&zdr).unwrap();
        cfg.resolve_runtime_fields(&ctx(&zdr));
        assert!(cfg.disable_zdr_incompatible_tools);
    }
    #[test]
    fn resolve_runtime_fields_propagates_disable_web_search() {
        fn ctx(raw: &toml::Value, disable_web_search: bool) -> RuntimeResolutionContext<'_> {
            RuntimeResolutionContext {
                raw_config: raw,
                local_runtime_settings: None,
                cwd: None,
                is_headless: true,
                cli_subagents: None,
                cli_web_search_model: None,
                cli_session_summary_model: None,
                cli_experimental_memory: false,
                cli_no_memory: false,
                disable_web_search,
                todo_gate: false,
                laziness_debug_log: None,
                storage_mode: None,
            }
        }
        let empty: toml::Value = toml::Value::Table(toml::map::Map::new());
        let mut cfg = Config::new_from_toml_cfg(&empty).unwrap();
        cfg.resolve_runtime_fields(&ctx(&empty, false));
        assert!(!cfg.disable_web_search);
        let mut cfg = Config::new_from_toml_cfg(&empty).unwrap();
        cfg.resolve_runtime_fields(&ctx(&empty, true));
        assert!(cfg.disable_web_search);
        let toml_on: toml::Value = toml::from_str("disable_web_search = true").unwrap();
        let mut cfg = Config::new_from_toml_cfg(&toml_on).unwrap();
        cfg.resolve_runtime_fields(&ctx(&toml_on, false));
        assert!(cfg.disable_web_search);
    }
    #[test]
    fn new_from_toml_cfg_restores_web_search_and_session_summary_models() {
        let empty: toml::Value = toml::Value::Table(toml::map::Map::new());
        let cfg = Config::new_from_toml_cfg(&empty).expect("empty config should parse");
        assert_eq!(
            cfg.web_search_model,
            crate::models::default_web_search_model(),
            "empty config should produce the compiled-in default web_search model"
        );
        assert_eq!(
            cfg.session_summary_model,
            Some(crate::models::default_session_summary_model().to_owned()),
            "empty config should produce compiled default session_summary model"
        );
        assert_eq!(
            cfg.image_description_model,
            Some(crate::models::default_image_description_model().to_owned()),
            "empty config should produce compiled default image_description model"
        );
        let with_overrides: toml::Value = toml::from_str(
            r#"
            [models]
            web_search = "custom-ws-model"
            session_summary = "custom-ss-model"
            image_description = "custom-id-model"
            "#,
        )
        .unwrap();
        let cfg2 = Config::new_from_toml_cfg(&with_overrides).expect("config should parse");
        assert_eq!(cfg2.web_search_model, "custom-ws-model");
        assert_eq!(
            cfg2.session_summary_model,
            Some("custom-ss-model".to_owned())
        );
        assert_eq!(
            cfg2.image_description_model,
            Some("custom-id-model".to_owned())
        );
    }
    #[test]
    fn hidden_default_web_search_resolution_is_explicit_and_responses_only() {
        let endpoints = EndpointsConfig::default();
        let resolved = resolve_web_search_sampling_config(
            crate::models::default_web_search_model(),
            &IndexMap::new(),
            Some("session-token"),
            false,
            None,
            None,
            &endpoints,
        )
        .expect("hidden default web search model should resolve");
        assert_eq!(resolved.model, crate::models::default_web_search_model());
        assert_eq!(resolved.base_url, endpoints.proxy_url());
        assert_eq!(resolved.api_backend, ApiBackend::Responses);
        assert_eq!(
            resolved.api_key.as_deref(),
            Some("session-token"),
            "hidden default should still use normal credential resolution"
        );
    }
    #[test]
    fn finalize_image_describe_sampler_none_uses_active_session_model_not_forced_helper() {
        let active = SamplerConfig {
            model: "composer-session-model".into(),
            ..Default::default()
        };
        let (model, cfg) = finalize_image_describe_sampler_config(None, &active, None, Some(3));
        assert_eq!(model, "composer-session-model");
        assert_eq!(cfg.model, "composer-session-model");
        assert_ne!(cfg.model, "atelier-build");
    }
    #[test]
    fn finalize_image_describe_sampler_some_stamps_session_fields() {
        let active = SamplerConfig {
            model: "composer-session-model".into(),
            ..Default::default()
        };
        let aux = SamplerConfig {
            model: "atelier-build".into(),
            ..Default::default()
        };
        let (model, cfg) =
            finalize_image_describe_sampler_config(Some(aux), &active, Some("cli".into()), Some(7));
        assert_eq!(model, "atelier-build");
        assert_eq!(cfg.model, "atelier-build");
        assert_eq!(cfg.client_identifier.as_deref(), Some("cli"));
        assert_eq!(cfg.max_retries, Some(7));
    }
    #[test]
    fn resolve_aux_model_honors_atelier_build_override() {
        let endpoints = EndpointsConfig::default();
        let mut catalog = IndexMap::new();
        catalog.insert(
            "atelier-build".to_string(),
            test_model_entry(
                "v9m-rl-learnability-tp8",
                "https://vendor.example/v1",
                Some("vendor-key"),
                None,
                None,
            ),
        );
        let resolved = resolve_aux_model_sampling_config(
            "atelier-build",
            &catalog,
            &endpoints,
            None,
            false,
            None,
            None,
        )
        .expect("override entry has an API key, so resolution succeeds");
        assert_eq!(resolved.model, "v9m-rl-learnability-tp8");
        assert_eq!(resolved.base_url, "https://vendor.example/v1");
        assert_eq!(resolved.api_key.as_deref(), Some("vendor-key"));
    }
    #[cfg(any())] // Vendor session-token substitution was removed; Providers own credentials.
    #[test]
    fn web_search_disable_api_key_auth_swaps_first_party_key_for_session() {
        let endpoints = EndpointsConfig::default();
        let mut models = IndexMap::new();
        models.insert(
            "ws-model".to_string(),
            test_model_entry(
                "ws-model",
                "https://api.atelier/v1",
                Some("first-party-key"),
                None,
                None,
            ),
        );
        let resolved = resolve_web_search_sampling_config(
            "ws-model",
            &models,
            Some("session-token"),
            true,
            None,
            None,
            &endpoints,
        )
        .expect("web search model should resolve");
        assert_eq!(
            resolved.api_key.as_deref(),
            Some("session-token"),
            "first-party API key must be swapped for the session token when disabled"
        );
    }
    #[test]
    fn parses_model_api_key() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-custom-model]
            model = "atelier-4.5"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            api_key = "sk-test-key-12345"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-custom-model").expect("model should exist");
        assert_eq!(model.info.model, "atelier-4.5");
        assert_eq!(model.info.base_url, "https://api.example.com/v1");
        assert_eq!(model.api_key, Some("sk-test-key-12345".to_string()));
    }
    fn test_model_entry(
        model: &str,
        base_url: &str,
        api_key: Option<&str>,
        env_key: Option<&str>,
        api_base_url: Option<&str>,
    ) -> ModelEntry {
        ModelEntry {
            info: ModelInfo {
                user_selectable: true,
                id: None,
                model: model.to_string(),
                base_url: base_url.to_string(),
                name: None,
                description: None,
                max_completion_tokens: None,
                temperature: None,
                top_p: None,
                api_backend: ApiBackend::default(),
                auth_scheme: Default::default(),
                extra_headers: IndexMap::new(),
                context_window: NonZeroU64::new(200_000).unwrap(),
                auto_compact_threshold_percent: None,
                system_prompt_label: None,
                use_concise: false,
                agent_type: default_agent_type(),
                inference_idle_timeout_secs: None,
                max_retries: None,
                hidden: false,
                supported_in_api: true,
                reasoning_effort: None,
                supports_reasoning_effort: false,
                reasoning_efforts: Vec::new(),
                accepts_images: false,
                supports_fast_mode: false,
                supports_backend_search: false,
                compactions_remaining: None,
                compaction_at_tokens: None,
                show_model_fingerprint: false,
                stream_tool_calls: None,
                laziness_detector: LazinessDetectorPerModelConfig::default(),
            },
            api_key: api_key.map(|s| s.to_string()),
            env_key: env_key.map(EnvKeys::single),
            api_base_url: api_base_url.map(|s| s.to_string()),
            request_payload: serde_json::Map::new(),
            remote_compaction_endpoint: None,
            image_generation_endpoint: None,
        }
    }
    /// The effective-model RE-support lookup must use the model ACTUALLY used:
    /// the resolved aux model when present, else the session model (an
    /// unresolvable slug ⇒ aux `None` ⇒ session model's capability wins).
    #[test]
    fn effective_classifier_supports_re_uses_actually_used_model() {
        let mut re_model = test_model_entry("v9", "https://x/v1", None, None, None);
        re_model.info.supports_reasoning_effort = true;
        let no_re_model = test_model_entry("legacy", "https://x/v1", None, None, None);
        let mut models = IndexMap::new();
        models.insert("v9".to_string(), re_model);
        models.insert("legacy".to_string(), no_re_model);
        assert!(effective_classifier_supports_re(
            Some("v9"),
            "legacy",
            &models
        ));
        assert!(effective_classifier_supports_re(None, "v9", &models));
        assert!(!effective_classifier_supports_re(None, "legacy", &models));
        assert!(!effective_classifier_supports_re(
            Some("typo-slug"),
            "v9",
            &models
        ));
        assert!(!effective_classifier_supports_re(None, "missing", &models));
    }
    #[test]
    fn sampling_config_uses_model_api_key_over_fallback() {
        let model = test_model_entry(
            "test-model",
            "https://test.api/v1",
            Some("model-specific-key"),
            None,
            None,
        );
        let sampling_config = sampling_config_for_model(
            &model,
            resolve_credentials(&model, None),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            sampling_config.api_key,
            Some("model-specific-key".to_string())
        );
        assert_eq!(sampling_config.base_url, "https://test.api/v1");
    }
    #[test]
    fn sampling_config_uses_fallback_when_no_model_api_key() {
        let model = test_model_entry("test-model", "https://test.api/v1", None, None, None);
        let sampling_config = sampling_config_for_model(
            &model,
            ResolvedCredentials {
                api_key: Some("fallback-key".to_string()),
                base_url: model.info().base_url.clone(),
                auth_type: atelier_chat_state::AuthType::ApiKey,
                auth_scheme: AuthScheme::Bearer,
            },
            None,
            None,
            None,
            None,
        );
        assert_eq!(sampling_config.api_key, Some("fallback-key".to_string()));
    }
    #[test]
    fn default_models_have_no_api_key_endpoint_fallback() {
        let endpoints = EndpointsConfig::default();
        for (model_id, entry) in default_model_entries(&endpoints) {
            assert_eq!(
                entry.api_base_url, None,
                "{model_id}: embedded defaults must not carry a vendor API endpoint"
            );
        }
    }
    #[test]
    fn env_keys_deser_string_or_array() {
        let one: EnvKeys = serde_json::from_str(r#""ANTHROPIC_AUTH_TOKEN""#).unwrap();
        assert_eq!(one.names(), vec!["ANTHROPIC_AUTH_TOKEN"]);
        let many: EnvKeys =
            serde_json::from_str(r#"["ANTHROPIC_AUTH_TOKEN", "LC_ANTHROPIC_AUTH_TOKEN"]"#).unwrap();
        assert_eq!(
            many.names(),
            vec!["ANTHROPIC_AUTH_TOKEN", "LC_ANTHROPIC_AUTH_TOKEN"]
        );
        let ser = serde_json::to_value(&one).unwrap();
        assert_eq!(ser, serde_json::json!("ANTHROPIC_AUTH_TOKEN"));
        let ser_many = serde_json::to_value(&many).unwrap();
        assert_eq!(
            ser_many,
            serde_json::json!(["ANTHROPIC_AUTH_TOKEN", "LC_ANTHROPIC_AUTH_TOKEN"])
        );
    }
    #[test]
    fn env_keys_resolve_first_set_wins() {
        let keys = EnvKeys::new([
            "ATELIER_TEST_ENV_KEY_PRIMARY",
            "ATELIER_TEST_ENV_KEY_FALLBACK",
        ]);
        assert_eq!(keys.resolve_value_with(|_| None), None, "none set");
        assert_eq!(
            keys.resolve_value_with(
                |n| (n == "ATELIER_TEST_ENV_KEY_FALLBACK").then(|| "from-fallback".into())
            ),
            Some("from-fallback".into())
        );
        assert_eq!(
            keys.resolve_value_with(|n| match n {
                "ATELIER_TEST_ENV_KEY_PRIMARY" => Some("from-primary".into()),
                "ATELIER_TEST_ENV_KEY_FALLBACK" => Some("from-fallback".into()),
                _ => None,
            }),
            Some("from-primary".into()),
            "primary wins when both set"
        );
        assert_eq!(
            keys.resolve_value_with(|n| match n {
                "ATELIER_TEST_ENV_KEY_PRIMARY" => Some(String::new()),
                "ATELIER_TEST_ENV_KEY_FALLBACK" => Some("from-fallback".into()),
                _ => None,
            }),
            Some("from-fallback".into())
        );
    }
    #[test]
    fn env_keys_single_and_array_are_semantically_equal() {
        let from_array: EnvKeys = serde_json::from_str(r#"["X"]"#).unwrap();
        assert_eq!(EnvKeys::new(["X"]), from_array);
        let from_string: EnvKeys = serde_json::from_str(r#""X""#).unwrap();
        assert_eq!(EnvKeys::new(["X"]), from_string);
    }
    #[test]
    fn env_keys_resolve_skips_whitespace_only_value() {
        let keys = EnvKeys::new(["ATELIER_TEST_WS_PRIMARY", "ATELIER_TEST_WS_FALLBACK"]);
        assert_eq!(
            keys.resolve_value_with(|n| match n {
                "ATELIER_TEST_WS_PRIMARY" => Some("   ".into()),
                "ATELIER_TEST_WS_FALLBACK" => Some("real".into()),
                _ => None,
            }),
            Some("real".into())
        );
        assert_eq!(
            EnvKeys::single("ATELIER_TEST_WS_ONLY").resolve_value_with(|_| Some("   ".into())),
            None
        );
        assert_eq!(
            EnvKeys::single("ATELIER_TEST_WS_PAD").resolve_value_with(|_| Some("  tok  ".into())),
            Some("  tok  ".into())
        );
    }
    #[test]
    #[serial]
    fn first_own_credential_empty_api_key_falls_through_to_env_key() {
        use atelier_test_support::EnvGuard;
        let var = "ATELIER_TEST_FIRST_OWN_CRED_ENV";
        let _guard = EnvGuard::set(var, "env-token");
        let env_key = EnvKeys::single(var);
        assert_eq!(
            first_own_credential(Some("   "), Some(&env_key)).as_deref(),
            Some("env-token")
        );
        assert_eq!(
            first_own_credential(Some("real-key"), Some(&env_key)).as_deref(),
            Some("real-key")
        );
    }
    #[test]
    #[serial]
    fn resolve_credentials_multi_env_key_uses_lc_alias() {
        use atelier_chat_state::AuthType;
        let primary = "ATELIER_TEST_MULTI_ENV_PRIMARY";
        let alias = "ATELIER_TEST_MULTI_ENV_LC_ALIAS";
        unsafe {
            std::env::remove_var(primary);
            std::env::set_var(alias, "token-via-lc-alias");
        }
        let mut model = test_model_entry("m", "https://inference.example/v1", None, None, None);
        model.env_key = Some(EnvKeys::new([primary, alias]));
        assert!(
            model.has_own_credentials(),
            "alias alone should satisfy has_own_credentials"
        );
        let creds = resolve_credentials(&model, None);
        assert_eq!(creds.auth_type, AuthType::ApiKey);
        assert_eq!(creds.api_key.as_deref(), Some("token-via-lc-alias"));
        unsafe {
            std::env::remove_var(alias);
            std::env::set_var(primary, "token-via-primary");
            std::env::set_var(alias, "token-via-lc-alias");
        }
        let creds = resolve_credentials(&model, None);
        assert_eq!(
            creds.api_key.as_deref(),
            Some("token-via-primary"),
            "exact primary wins over LC alias when both set"
        );
        unsafe {
            std::env::remove_var(primary);
            std::env::remove_var(alias);
        }
    }
    #[test]
    #[serial]
    fn resolve_credentials_empty_env_key_falls_through_to_session() {
        use atelier_chat_state::AuthType;
        use atelier_test_support::EnvGuard;
        let primary = "ATELIER_TEST_EMPTY_ENV_PRIMARY";
        let alias = "ATELIER_TEST_EMPTY_ENV_LC_ALIAS";
        let _primary = EnvGuard::set(primary, "");
        let _alias = EnvGuard::set(alias, "");
        let mut model = test_model_entry("m", "https://inference.example/v1", None, None, None);
        model.env_key = Some(EnvKeys::new([primary, alias]));
        assert!(!model.has_own_credentials());
        let creds = resolve_credentials(&model, Some("session-jwt"));
        assert_eq!(creds.auth_type, AuthType::SessionToken);
        assert_eq!(creds.api_key.as_deref(), Some("session-jwt"));
    }
    #[test]
    fn resolve_credentials_empty_api_key_falls_through_to_session() {
        use atelier_chat_state::AuthType;
        let model = test_model_entry("m", "https://inference.example/v1", Some(""), None, None);
        assert!(!model.has_own_credentials());
        let creds = resolve_credentials(&model, Some("session-jwt"));
        assert_eq!(creds.auth_type, AuthType::SessionToken);
        assert_eq!(creds.api_key.as_deref(), Some("session-jwt"));
    }
    #[test]
    #[serial]
    fn config_toml_env_key_array_parses() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
            [model."{dm}"]
            model = "{dm}"
            base_url = "https://inference.example.com/v1"
            env_key = ["ANTHROPIC_AUTH_TOKEN", "LC_ANTHROPIC_AUTH_TOKEN"]
            "#,
            ),
            None,
        );
        let model = models.get(dm).expect("model should exist");
        assert_eq!(
            model.env_key.as_ref().map(|k| k.names()),
            Some(vec!["ANTHROPIC_AUTH_TOKEN", "LC_ANTHROPIC_AUTH_TOKEN"])
        );
    }
    #[test]
    fn resolve_credentials_sets_auth_type() {
        use atelier_chat_state::AuthType;
        let model = test_model_entry("m", "https://example.com/v1", None, None, None);
        let creds = resolve_credentials(&model, Some("tok"));
        assert_eq!(creds.auth_type, AuthType::SessionToken);
        let byok = test_model_entry("m", "https://example.com/v1", Some("key"), None, None);
        let creds = resolve_credentials(&byok, Some("tok"));
        assert_eq!(creds.auth_type, AuthType::ApiKey);
    }
    /// Regression: BYOK env-var auth must stay ApiKey even when signed in,
    /// otherwise the bearer resolver overwrites the BYOK key with a session JWT.
    #[test]
    #[serial_test::serial]
    fn resolve_credentials_env_key_byok_keeps_api_key_auth_with_session() {
        use atelier_chat_state::AuthType;
        let env_var = "REGRESSION_BYOK_TOKEN_FOR_AUTH_TYPE_TEST";
        unsafe {
            std::env::set_var(env_var, "sk-byok-test-value");
        }
        let model = test_model_entry(
            "byok-gpt-test",
            "https://llm.example.com/v1",
            None,
            Some(env_var),
            None,
        );
        assert!(model.has_own_credentials());
        let creds = resolve_credentials(&model, Some("session-jwt"));
        assert_eq!(
            creds.auth_type,
            AuthType::ApiKey,
            "BYOK env_key model must resolve to ApiKey even when a session token is available",
        );
        assert_eq!(
            creds.api_key.as_deref(),
            Some("sk-byok-test-value"),
            "api_key must be the env value, not the session JWT",
        );
        unsafe {
            std::env::remove_var(env_var);
        }
    }
    #[cfg(any())] // The vendor Messages proxy was removed from Atelier.
    #[test]
    fn proxy_messages_models_use_bearer_auth_scheme() {
        let mut model = test_model_entry(
            "atelier-4.5",
            crate::env::PROD_CLI_CHAT_PROXY_BASE_URL,
            None,
            None,
            None,
        );
        model.info.api_backend = ApiBackend::Messages;
        let config = sampling_config_for_model(
            &model,
            resolve_credentials(&model, Some("tok")),
            None,
            None,
            None,
            None,
        );
        assert_eq!(config.api_backend, ApiBackend::Messages);
        assert_eq!(config.auth_scheme, AuthScheme::Bearer);
        assert_eq!(config.api_key, Some("tok".to_string()));
        assert_eq!(config.base_url, crate::env::PROD_CLI_CHAT_PROXY_BASE_URL);
        assert_eq!(
            config
                .extra_headers
                .get("X-XAI-Token-Auth")
                .map(String::as_str),
            Some("atelier-cli")
        );
    }
    /// Regression: without a session key, `resolve_credentials` falls through
    /// to ApiKey. Session-based callers must override auth_type to SessionToken
    /// when their auth manager has only a buffered/expired token.
    #[test]
    fn resolve_credentials_no_session_key_returns_api_key() {
        let model = test_model_entry("m", "https://example.com/v1", None, None, None);
        let creds = resolve_credentials(&model, None);
        assert_eq!(creds.auth_type, atelier_chat_state::AuthType::ApiKey);
    }
    fn api_key_creds(base_url: &str) -> ResolvedCredentials {
        ResolvedCredentials {
            api_key: Some("provider-secret".to_string()),
            base_url: base_url.to_string(),
            auth_type: atelier_chat_state::AuthType::ApiKey,
            auth_scheme: Default::default(),
        }
    }
    /// `disable_api_key_auth` blocks every configured Provider API key.
    #[test]
    fn enforce_disable_api_key_auth_blocks_all_provider_keys() {
        use atelier_chat_state::AuthType;
        let mut creds = api_key_creds("https://provider.example/v1");
        enforce_disable_api_key_auth(&mut creds, false, Some("session-jwt"));
        assert_eq!(creds.auth_type, AuthType::ApiKey);
        assert_eq!(creds.api_key.as_deref(), Some("provider-secret"));
        let mut creds = api_key_creds("https://provider.example/v1");
        enforce_disable_api_key_auth(&mut creds, true, Some("session-jwt"));
        assert_eq!(creds.auth_type, AuthType::SessionToken);
        assert_eq!(creds.api_key.as_deref(), Some("session-jwt"));
        let mut creds = api_key_creds("https://provider.example/v1");
        enforce_disable_api_key_auth(&mut creds, true, None);
        assert_eq!(creds.auth_type, AuthType::SessionToken);
        assert_eq!(creds.api_key, None);
        let mut creds = api_key_creds("https://api.example.com/v1");
        enforce_disable_api_key_auth(&mut creds, true, Some("session-jwt"));
        assert_eq!(creds.auth_type, AuthType::SessionToken);
        assert_eq!(creds.api_key.as_deref(), Some("session-jwt"));
        let mut creds = ResolvedCredentials {
            auth_type: AuthType::SessionToken,
            ..api_key_creds("https://provider.example/v1")
        };
        enforce_disable_api_key_auth(&mut creds, true, Some("session-jwt"));
        assert_eq!(creds.auth_type, AuthType::SessionToken);
    }
    #[test]
    fn x_api_key_auth_scheme_flows_from_config_to_sampler() {
        let mut model = test_model_entry(
            "messages-compatible-model",
            "https://messages.example.com/v1",
            Some("sk-ant-test-key"),
            None,
            None,
        );
        model.info.api_backend = ApiBackend::Messages;
        model.info.auth_scheme = AuthScheme::XApiKey;
        let creds = resolve_credentials(&model, None);
        assert_eq!(creds.auth_scheme, AuthScheme::XApiKey);
        assert_eq!(creds.auth_type, atelier_chat_state::AuthType::ApiKey);
        assert_eq!(creds.api_key, Some("sk-ant-test-key".to_string()));
        let config = sampling_config_for_model(&model, creds, None, None, None, None);
        assert_eq!(config.auth_scheme, AuthScheme::XApiKey);
        assert_eq!(config.api_backend, ApiBackend::Messages);
        let client = atelier_sampler::SamplingClient::new(config).expect("client should build");
        let info = client.auth_info();
        assert_eq!(info.auth_type, "x-api-key");
    }
    #[test]
    fn auth_scheme_defaults_to_bearer_when_not_set_in_config() {
        let model = test_model_entry(
            "atelier-4.5",
            "https://api.example.com/v1",
            Some("sk-openai-test"),
            None,
            None,
        );
        assert_eq!(model.info.auth_scheme, AuthScheme::Bearer);
        let creds = resolve_credentials(&model, None);
        assert_eq!(creds.auth_scheme, AuthScheme::Bearer);
        let config = sampling_config_for_model(&model, creds, None, None, None, None);
        assert_eq!(config.auth_scheme, AuthScheme::Bearer);
        let client = atelier_sampler::SamplingClient::new(config).expect("client should build");
        let info = client.auth_info();
        assert_eq!(info.auth_type, "bearer");
    }
    #[test]
    fn has_own_credentials_guards_session_vs_external_key() {
        let endpoints = EndpointsConfig::default();
        for (model_id, entry) in default_model_entries(&endpoints) {
            assert!(
                !entry.has_own_credentials(),
                "{model_id}: Default model must not claim own credentials"
            );
        }
        let config_model = test_model_entry(
            "my-model",
            "https://api.example.com/v1",
            Some("sk-external"),
            None,
            None,
        );
        assert!(config_model.has_own_credentials());
    }
    /// The `ConfigUnavailable → Unknown` arm matters for safety: a transient
    /// config failure must not read as a definite `NotByok`, which would drive
    /// the live resolver and could overwrite a per-model BYOK key.
    #[test]
    fn byok_from_lookup_classifies_all_states() {
        assert_eq!(
            byok_from_lookup(&ModelLookup::ConfigUnavailable),
            ModelByok::Unknown,
        );
        assert_eq!(
            byok_from_lookup(&ModelLookup::Loaded(None)),
            ModelByok::NotByok,
        );
        let byok = test_model_entry(
            "m",
            "https://api.example.com/v1",
            Some("sk-ext"),
            None,
            None,
        );
        assert_eq!(
            byok_from_lookup(&ModelLookup::Loaded(Some(&byok))),
            ModelByok::Byok,
        );
        let session = test_model_entry("m", "https://api.atelier/v1", None, None, None);
        assert_eq!(
            byok_from_lookup(&ModelLookup::Loaded(Some(&session))),
            ModelByok::NotByok,
        );
    }
    #[test]
    fn resolve_model_auth_facts_empty_model_id_is_unknown() {
        assert_eq!(resolve_model_auth_facts("").byok, ModelByok::Unknown);
    }
    #[cfg(any())] // Atelier has no built-in vendor default model or proxy to inherit from.
    #[test]
    fn user_override_adds_api_key_to_default_model() {
        let dm = crate::models::default_model();
        let raw_config: toml::Value = toml::from_str(&format!(
            r#"
            [model."{dm}"]
            api_key = "user-custom-api-key"
            "#,
        ))
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get(dm).expect("model should exist");
        assert_eq!(model.api_key, Some("user-custom-api-key".to_string()));
        assert_eq!(model.info.model, dm);
        assert_eq!(
            model.info.base_url, "https://cli-chat-proxy.atelier.com/v1",
            "base_url should inherit from default, not be stale"
        );
    }
    #[test]
    fn config_override_applies_show_model_fingerprint() {
        let endpoints = EndpointsConfig::default();
        let override_on = ConfigModelOverride {
            show_model_fingerprint: Some(true),
            ..Default::default()
        };
        let entry = override_on.apply("some-model", None, &endpoints);
        assert!(
            entry.info.show_model_fingerprint,
            "Some(true) override should enable show_model_fingerprint"
        );
        let mut base = ModelEntry::fallback("some-model", &endpoints);
        base.info.show_model_fingerprint = true;
        let override_absent = ConfigModelOverride::default();
        let entry = override_absent.apply("some-model", Some(base), &endpoints);
        assert!(
            entry.info.show_model_fingerprint,
            "None override should preserve the base entry's show_model_fingerprint"
        );
        let mut base = ModelEntry::fallback("some-model", &endpoints);
        base.info.show_model_fingerprint = true;
        let override_off = ConfigModelOverride {
            show_model_fingerprint: Some(false),
            ..Default::default()
        };
        let entry = override_off.apply("some-model", Some(base), &endpoints);
        assert!(
            !entry.info.show_model_fingerprint,
            "Some(false) override should disable show_model_fingerprint over a true base"
        );
    }
    #[test]
    fn user_override_parses_compaction_at_tokens_from_toml() {
        use atelier_sampling_types::CompactionAtTokens;
        let dm = crate::models::default_model();
        let raw_config: toml::Value = toml::from_str(&format!(
            r#"
            [model."{dm}"]
            compaction_at_tokens = true
            "#,
        ))
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let model = resolve_model_list(&cfg, None)
            .get(dm)
            .expect("model should exist")
            .clone();
        assert_eq!(
            model.info.compaction_at_tokens,
            Some(CompactionAtTokens::Enabled(true)),
        );
        let raw_config: toml::Value = toml::from_str(&format!(
            r#"
            [model."{dm}"]
            compaction_at_tokens = 367000
            "#,
        ))
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let model = resolve_model_list(&cfg, None)
            .get(dm)
            .expect("model should exist")
            .clone();
        assert_eq!(
            model.info.compaction_at_tokens,
            Some(CompactionAtTokens::Fixed(367_000)),
        );
    }
    #[test]
    fn user_override_parses_compactions_remaining_from_toml() {
        use atelier_sampling_types::CompactionsRemaining;
        let dm = crate::models::default_model();
        let raw_config: toml::Value = toml::from_str(&format!(
            r#"
            [model."{dm}"]
            compactions_remaining = true
            "#,
        ))
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let model = resolve_model_list(&cfg, None)
            .get(dm)
            .expect("model should exist")
            .clone();
        assert_eq!(
            model.info.compactions_remaining,
            Some(CompactionsRemaining::Dynamic(true)),
        );
        let raw_config: toml::Value = toml::from_str(&format!(
            r#"
            [model."{dm}"]
            compactions_remaining = 1
            "#,
        ))
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let model = resolve_model_list(&cfg, None)
            .get(dm)
            .expect("model should exist")
            .clone();
        assert_eq!(
            model.info.compactions_remaining,
            Some(CompactionsRemaining::Fixed(1)),
        );
        let raw_config: toml::Value = toml::from_str(&format!(
            r#"
            [model."{dm}"]
            send_compactions_remaining = true
            "#,
        ))
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let model = resolve_model_list(&cfg, None)
            .get(dm)
            .expect("model should exist")
            .clone();
        assert_eq!(
            model.info.compactions_remaining,
            Some(CompactionsRemaining::Dynamic(true)),
        );
    }
    #[test]
    fn default_auto_compact_threshold_is_none() {
        let cfg = Config::default();
        assert_eq!(cfg.session.auto_compact_threshold_percent, None);
    }
    #[test]
    fn parses_auto_compact_threshold_percent() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [session]
            auto_compact_threshold_percent = 75
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        assert_eq!(cfg.session.auto_compact_threshold_percent, Some(75));
    }
    #[test]
    fn compaction_mode_precedence_env_over_config_over_remote_over_default() {
        use atelier_chat_state::CompactionMode;
        assert_eq!(
            resolve_compaction_mode_from(Some("transcript"), Some("segments"), Some("summary")),
            CompactionMode::Transcript
        );
        assert_eq!(
            resolve_compaction_mode_from(None, Some("segments"), Some("summary")),
            CompactionMode::Segments(atelier_chat_state::CompactionDetail::default())
        );
        assert_eq!(
            resolve_compaction_mode_from(None, None, Some("segments")),
            CompactionMode::Segments(atelier_chat_state::CompactionDetail::default())
        );
        assert_eq!(
            resolve_compaction_mode_from(Some("garbage"), None, Some("segments")),
            CompactionMode::Segments(atelier_chat_state::CompactionDetail::default())
        );
        assert_eq!(
            resolve_compaction_mode_from(None, None, None),
            CompactionMode::Summary
        );
    }
    /// Detail shares the env>config>remote>default combinator that the mode
    /// test exercises; the detail-specific facts are remote settings routing and the
    /// `Verbose` default (with unrecognized values falling through).
    #[test]
    fn compaction_detail_resolves_local_runtime_settings_and_verbose_default() {
        use atelier_chat_state::CompactionDetail;
        assert_eq!(
            resolve_compaction_detail_from(None, None, Some("minimal")),
            CompactionDetail::Minimal
        );
        assert_eq!(
            resolve_compaction_detail_from(Some("garbage"), None, None),
            CompactionDetail::Verbose
        );
    }
    #[test]
    fn auto_compact_threshold_percent_defaults_when_not_specified() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [toolset.bash]
            timeout_secs = 123
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        assert_eq!(cfg.session.auto_compact_threshold_percent, None);
    }
    #[test]
    fn parses_model_context_window() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-custom-model]
            model = "custom-llm"
            base_url = "https://api.example.com/v1"
            context_window = 256000
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-custom-model").expect("model should exist");
        assert_eq!(model.info.context_window, NonZeroU64::new(256_000).unwrap());
    }
    #[test]
    fn sampling_config_context_window_from_entry_or_default() {
        let model = test_model_entry("any-model", "https://api.atelier/v1", None, None, None);
        let config = sampling_config_for_model(
            &model,
            resolve_credentials(&model, None),
            None,
            None,
            None,
            None,
        );
        assert_eq!(config.context_window, 200_000);
        let mut model = test_model_entry("any-model", "https://api.atelier/v1", None, None, None);
        model.info.context_window = NonZeroU64::new(256_000).unwrap();
        let config = sampling_config_for_model(
            &model,
            resolve_credentials(&model, None),
            None,
            None,
            None,
            None,
        );
        assert_eq!(config.context_window, 256_000);
    }
    #[test]
    fn parses_model_api_backend_responses() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-responses-model]
            model = "atelier-4.5"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            api_backend = "responses"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved
            .get("my-responses-model")
            .expect("model should exist");
        assert_eq!(model.info.api_backend, ApiBackend::Responses);
    }
    #[test]
    fn parses_model_api_backend_chat_completions() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-chat-model]
            model = "atelier-4.5"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            api_backend = "chat_completions"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-chat-model").expect("model should exist");
        assert_eq!(model.info.api_backend, ApiBackend::ChatCompletions);
    }
    /// Messages backend (Anthropic) auto-defaults supports_reasoning_effort=true.
    /// Without this, `--reasoning-effort` is silently dropped in
    /// atelier-shell/src/agent/models.rs:857 for any BYOK Claude config.
    #[test]
    fn model_messages_backend_auto_defaults_supports_reasoning_effort() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-claude]
            model = "atelier-4.5"
            base_url = "https://messages.example.com"
            context_window = 200000
            api_backend = "messages"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-claude").expect("model should exist");
        assert!(
            model.info.supports_reasoning_effort,
            "Messages backend should auto-default supports_reasoning_effort=true",
        );
    }
    /// An explicit `supports_reasoning_effort = false` in config must override
    /// the Messages auto-default — config wins.
    #[test]
    fn model_messages_backend_respects_explicit_supports_reasoning_effort_false() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-claude]
            model = "atelier-4.5"
            base_url = "https://messages.example.com"
            context_window = 200000
            api_backend = "messages"
            supports_reasoning_effort = false
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-claude").expect("model should exist");
        assert!(
            !model.info.supports_reasoning_effort,
            "explicit supports_reasoning_effort=false in config must override the Messages auto-default",
        );
    }
    /// Non-Messages backends keep their existing default (false) since adaptive
    /// thinking is Anthropic-specific and other providers vary per upstream model.
    #[test]
    fn model_chat_completions_backend_does_not_auto_default_supports_reasoning_effort() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-openai]
            model = "atelier-4.5"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            api_backend = "chat_completions"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-openai").expect("model should exist");
        assert!(
            !model.info.supports_reasoning_effort,
            "ChatCompletions backend must not auto-default supports_reasoning_effort=true",
        );
    }
    #[test]
    fn model_api_backend_defaults_to_chat_completions() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-model]
            model = "atelier-4.5"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-model").expect("model should exist");
        assert_eq!(model.info.api_backend, ApiBackend::ChatCompletions);
    }
    #[test]
    fn sampling_config_uses_model_api_backend() {
        let mut model =
            test_model_entry("test-model", "https://api.example.com/v1", None, None, None);
        model.info.api_backend = ApiBackend::Responses;
        let sampling_config = sampling_config_for_model(
            &model,
            resolve_credentials(&model, None),
            None,
            None,
            None,
            None,
        );
        assert_eq!(sampling_config.api_backend, ApiBackend::Responses);
    }
    #[test]
    fn parses_model_use_concise_true() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-concise-model]
            model = "my-concise-model"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            use_concise = true
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved
            .get("my-concise-model")
            .expect("model should exist");
        assert!(model.info.use_concise);
    }
    #[test]
    fn model_use_concise_defaults_to_false() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-model]
            model = "my-model"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-model").expect("model should exist");
        assert!(!model.info.use_concise);
    }
    #[test]
    fn model_info_from_config_propagates_use_concise() {
        let entry = ModelEntryConfig {
            id: None,
            model: "test".to_string(),
            base_url: "https://test.api/v1".to_string(),
            name: None,
            description: None,
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_key: None,
            env_key: None,
            api_backend: ApiBackend::default(),
            auth_scheme: None,
            extra_headers: IndexMap::new(),
            context_window: NonZeroU64::new(200_000).unwrap(),
            auto_compact_threshold_percent: None,
            system_prompt_label: None,
            api_base_url: None,
            use_concise: true,
            agent_type: default_agent_type(),
            inference_idle_timeout_secs: None,
            max_retries: None,
            hidden: false,
            supported_in_api: true,
            reasoning_effort: None,
            supports_reasoning_effort: false,
            reasoning_efforts: Vec::new(),
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            show_model_fingerprint: false,
            stream_tool_calls: None,
            laziness_detector: LazinessDetectorPerModelConfig::default(),
        };
        let info = ModelInfo::from_config(&entry);
        assert!(info.use_concise);
    }
    #[test]
    fn deprecated_toolset_use_concise_is_ignored_in_model_config() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [toolset]
            use_concise = true

            [model.my-model]
            model = "my-model"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-model").expect("model should exist");
        assert!(
            !model.info.use_concise,
            "old [toolset] use_concise should not affect per-model use_concise"
        );
    }
    #[test]
    fn agent_selection_config_defaults_to_none() {
        let cfg = Config::default();
        assert!(cfg.agent.name.is_none());
        assert!(cfg.agent.definition.is_none());
    }
    #[test]
    fn parses_agent_selection_name() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [agent]
            name = "my-custom-agent"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        assert_eq!(cfg.agent.name.as_deref(), Some("my-custom-agent"));
        assert!(cfg.agent.definition.is_none());
    }
    #[test]
    fn parses_agent_selection_definition_path() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [agent]
            definition = "/path/to/my-agent.md"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        assert!(cfg.agent.name.is_none());
        assert_eq!(
            cfg.agent.definition.as_deref(),
            Some(std::path::Path::new("/path/to/my-agent.md"))
        );
    }
    #[test]
    fn parses_agent_selection_both_name_and_definition() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [agent]
            name = "fallback-agent"
            definition = "/path/to/primary-agent.md"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        assert_eq!(cfg.agent.name.as_deref(), Some("fallback-agent"));
        assert_eq!(
            cfg.agent.definition.as_deref(),
            Some(std::path::Path::new("/path/to/primary-agent.md"))
        );
    }
    #[test]
    fn agent_selection_not_specified_uses_defaults() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [toolset.bash]
            timeout_secs = 123
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        assert!(cfg.agent.name.is_none());
        assert!(cfg.agent.definition.is_none());
    }
    #[test]
    fn parses_model_with_agent_type() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-agent-model]
            model = "my-agent-model"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            agent_type = "codex"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-agent-model").expect("model should exist");
        assert_eq!(model.info.agent_type, "codex");
    }
    #[test]
    fn model_agent_type_defaults_to_atelier_build() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.my-model]
            model = "my-model"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("my-model").expect("model should exist");
        assert_eq!(model.info.agent_type, DEFAULT_AGENT_TYPE);
    }
    #[test]
    fn model_info_from_config_propagates_agent_type() {
        let entry = ModelEntryConfig {
            id: None,
            model: "test".to_string(),
            base_url: "https://test.api/v1".to_string(),
            name: None,
            description: None,
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_key: None,
            env_key: None,
            api_backend: ApiBackend::default(),
            auth_scheme: None,
            extra_headers: IndexMap::new(),
            context_window: NonZeroU64::new(200_000).unwrap(),
            auto_compact_threshold_percent: None,
            system_prompt_label: None,
            api_base_url: None,
            use_concise: false,
            agent_type: "codex".to_string(),
            inference_idle_timeout_secs: None,
            max_retries: None,
            hidden: false,
            supported_in_api: true,
            reasoning_effort: None,
            supports_reasoning_effort: false,
            reasoning_efforts: Vec::new(),
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            show_model_fingerprint: false,
            stream_tool_calls: None,
            laziness_detector: LazinessDetectorPerModelConfig::default(),
        };
        let info = ModelInfo::from_config(&entry);
        assert_eq!(info.agent_type, "codex");
    }
    #[test]
    fn acp_model_meta_includes_agent_type_when_present() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("test-model", "https://test.api/v1", None, None, None);
        entry.info.name = Some("Test Model".to_string());
        entry.info.context_window = NonZeroU64::new(256_000).unwrap();
        entry.info.agent_type = "codex".to_string();
        models.insert("test-model".to_string(), entry);
        let acp_models = to_acp_model_info(&models);
        let acp_model = acp_models.values().next().expect("should have one model");
        let meta = acp_model.meta.as_ref().expect("meta should be present");
        assert_eq!(meta["agentType"], "codex");
        assert_eq!(meta["totalContextTokens"], 256_000);
    }
    #[test]
    fn acp_model_meta_includes_provider_identity_and_wire_api() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("alpha", "https://test.api/v1", None, None, None);
        entry.info.name = Some("Alpha".to_string());
        entry.info.api_backend = ApiBackend::Responses;
        models.insert("example/alpha".to_string(), entry);

        let acp_models = to_acp_model_info(&models);
        let meta = acp_models
            .values()
            .next()
            .and_then(|model| model.meta.as_ref())
            .expect("Provider metadata");

        assert_eq!(meta["providerId"], "example");
        assert_eq!(meta["modelId"], "alpha");
        assert_eq!(meta["wireApi"], "responses");
    }

    #[test]
    fn acp_model_meta_always_includes_agent_type() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("plain-model", "https://test.api/v1", None, None, None);
        entry.info.name = Some("Plain Model".to_string());
        entry.info.context_window = NonZeroU64::new(256_000).unwrap();
        models.insert("plain-model".to_string(), entry);
        let acp_models = to_acp_model_info(&models);
        let acp_model = acp_models.values().next().expect("should have one model");
        let meta = acp_model.meta.as_ref().expect("meta should be present");
        assert_eq!(meta["totalContextTokens"], 256_000);
        assert_eq!(
            meta["agentType"], DEFAULT_AGENT_TYPE,
            "agentType should always be in meta, defaulting to DEFAULT_AGENT_TYPE"
        );
    }
    #[test]
    fn acp_model_meta_emits_reasoning_effort_when_supported() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("m", "https://test.api/v1", None, None, None);
        entry.info.supports_reasoning_effort = true;
        entry.info.reasoning_effort = Some(ReasoningEffort::High);
        models.insert("m".to_string(), entry);
        let meta = to_acp_model_info(&models)
            .values()
            .next()
            .unwrap()
            .meta
            .clone()
            .unwrap();
        assert_eq!(meta["supportsReasoningEffort"], true);
        assert_eq!(meta["reasoningEffort"], "high");
    }
    #[test]
    fn acp_model_meta_supports_without_default_effort() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("m", "https://test.api/v1", None, None, None);
        entry.info.supports_reasoning_effort = true;
        models.insert("m".to_string(), entry);
        let meta = to_acp_model_info(&models)
            .values()
            .next()
            .unwrap()
            .meta
            .clone()
            .unwrap();
        assert_eq!(meta["supportsReasoningEffort"], true);
        assert!(meta.get("reasoningEffort").is_none());
    }
    #[test]
    fn acp_model_meta_emits_reasoning_efforts_and_derives_legacy() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("m", "https://test.api/v1", None, None, None);
        entry.info.reasoning_efforts = vec![
            ReasoningEffortOption {
                id: "deep".to_string(),
                value: ReasoningEffort::Xhigh,
                label: "Deep".to_string(),
                description: None,
                default: false,
            },
            ReasoningEffortOption {
                id: "high".to_string(),
                value: ReasoningEffort::High,
                label: "High".to_string(),
                description: None,
                default: true,
            },
        ];
        entry.info.derive_reasoning_effort_fields();
        models.insert("m".to_string(), entry);
        let meta = to_acp_model_info(&models)
            .values()
            .next()
            .unwrap()
            .meta
            .clone()
            .unwrap();
        assert_eq!(meta[REASONING_EFFORTS_META_KEY][0]["id"], "deep");
        assert_eq!(meta[REASONING_EFFORTS_META_KEY][0]["value"], "xhigh");
        assert_eq!(meta["supportsReasoningEffort"], true);
        assert_eq!(meta["reasoningEffort"], "high");
    }
    #[test]
    fn acp_model_meta_omits_reasoning_efforts_when_list_empty() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("m", "https://test.api/v1", None, None, None);
        entry.info.supports_reasoning_effort = true;
        entry.info.reasoning_effort = Some(ReasoningEffort::Medium);
        models.insert("m".to_string(), entry);
        let meta = to_acp_model_info(&models)
            .values()
            .next()
            .unwrap()
            .meta
            .clone()
            .unwrap();
        assert!(meta.get(REASONING_EFFORTS_META_KEY).is_none());
        assert_eq!(meta["supportsReasoningEffort"], true);
        assert_eq!(meta["reasoningEffort"], "medium");
    }
    #[test]
    fn acp_model_meta_keeps_explicit_scalar_when_list_present() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("m", "https://test.api/v1", None, None, None);
        entry.info.reasoning_effort = Some(ReasoningEffort::Low);
        entry.info.reasoning_efforts = vec![ReasoningEffortOption {
            id: "high".to_string(),
            value: ReasoningEffort::High,
            label: "High".to_string(),
            description: None,
            default: true,
        }];
        entry.info.derive_reasoning_effort_fields();
        models.insert("m".to_string(), entry);
        let meta = to_acp_model_info(&models)
            .values()
            .next()
            .unwrap()
            .meta
            .clone()
            .unwrap();
        assert_eq!(meta["supportsReasoningEffort"], true);
        assert_eq!(meta["reasoningEffort"], "low");
    }
    #[test]
    fn acp_model_meta_derives_first_option_when_no_default() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("m", "https://test.api/v1", None, None, None);
        entry.info.reasoning_efforts = vec![
            ReasoningEffortOption {
                id: "balanced".to_string(),
                value: ReasoningEffort::Medium,
                label: "Balanced".to_string(),
                description: None,
                default: false,
            },
            ReasoningEffortOption {
                id: "deep".to_string(),
                value: ReasoningEffort::Xhigh,
                label: "Deep".to_string(),
                description: None,
                default: false,
            },
        ];
        entry.info.derive_reasoning_effort_fields();
        models.insert("m".to_string(), entry);
        let meta = to_acp_model_info(&models)
            .values()
            .next()
            .unwrap()
            .meta
            .clone()
            .unwrap();
        assert_eq!(meta["supportsReasoningEffort"], true);
        assert_eq!(meta["reasoningEffort"], "medium");
    }
    #[test]
    fn acp_model_meta_omits_reasoning_when_unsupported() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("m", "https://test.api/v1", None, None, None);
        entry.info.reasoning_effort = Some(ReasoningEffort::High);
        models.insert("m".to_string(), entry);
        let meta = to_acp_model_info(&models)
            .values()
            .next()
            .unwrap()
            .meta
            .clone();
        if let Some(meta) = meta {
            assert!(meta.get("supportsReasoningEffort").is_none());
            assert!(meta.get("reasoningEffort").is_none());
        }
    }
    #[test]
    fn acp_model_meta_always_has_context_window() {
        let mut models = IndexMap::new();
        let mut entry = test_model_entry("unknown-model", "https://test.api/v1", None, None, None);
        entry.info.name = Some("Unknown Model".to_string());
        models.insert("unknown-model".to_string(), entry);
        let acp_models = to_acp_model_info(&models);
        let meta = acp_models.values().next().unwrap().meta.as_ref().unwrap();
        assert_eq!(meta["totalContextTokens"], 200_000);
    }
    #[test]
    fn hidden_model_excluded_from_acp_but_kept_in_catalog() {
        use crate::agent::models::{available_models, resolve_model_catalog};
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.visible-model]
            model = "visible-model"
            base_url = "https://api.atelier/v1"
            context_window = 200000

            [model.hidden-model]
            model = "hidden-model"
            base_url = "https://api.atelier/v1"
            context_window = 200000
            hidden = true
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).unwrap();
        let catalog = resolve_model_catalog(&cfg, None);
        let available = available_models(&catalog, true);
        assert!(
            catalog.contains_key("visible-model"),
            "visible model missing from catalog"
        );
        assert!(
            catalog.contains_key("hidden-model"),
            "hidden model missing from catalog"
        );
        assert!(
            available.values().any(|m| m.name == "visible-model"),
            "visible model missing from ACP"
        );
        assert!(
            !available.values().any(|m| m.name == "hidden-model"),
            "hidden model should NOT appear in ACP"
        );
    }
    #[test]
    fn disabled_models_removed_from_catalog() {
        use crate::agent::models::resolve_model_catalog;
        let raw: toml::Value = toml::from_str(
            r#"
            [models]
            disabled_models = ["to-disable"]
            [model.to-disable]
            model = "to-disable"
            base_url = "https://api.atelier/v1"
            context_window = 200000
            "#,
        )
        .unwrap();
        let catalog = resolve_model_catalog(&Config::new_from_toml_cfg(&raw).unwrap(), None);
        assert!(!catalog.contains_key("to-disable"));
    }
    #[test]
    fn hidden_models_kept_in_catalog_but_not_in_acp() {
        use crate::agent::models::{available_models, resolve_model_catalog};
        let raw: toml::Value = toml::from_str(
            r#"
            [models]
            hidden_models = ["to-hide"]
            [model.to-hide]
            model = "to-hide"
            base_url = "https://api.atelier/v1"
            context_window = 200000
            "#,
        )
        .unwrap();
        let catalog = resolve_model_catalog(&Config::new_from_toml_cfg(&raw).unwrap(), None);
        let available = available_models(&catalog, true);
        assert!(catalog.contains_key("to-hide"));
        assert!(catalog["to-hide"].info.hidden);
        assert!(!available.values().any(|m| m.name == "to-hide"));
    }
    #[test]
    fn allowed_models_marks_selectable_by_wildcard_key_or_model() {
        use crate::agent::models::resolve_model_catalog;
        let raw: toml::Value = toml::from_str(
            r#"
            [models]
            allowed_models = ["keep-*", "explicit-key", "explicit-model-id"]
            [model.to-drop]
            model = "to-drop"
            base_url = "https://api.atelier/v1"
            context_window = 256000
            [model.keep-one]
            model = "keep-one"
            base_url = "https://api.atelier/v1"
            context_window = 256000
            [model.explicit-key]
            model = "explicit-model-id"
            base_url = "https://api.atelier/v1"
            context_window = 256000
            "#,
        )
        .unwrap();
        let catalog = resolve_model_catalog(&Config::new_from_toml_cfg(&raw).unwrap(), None);
        assert!(catalog["keep-one"].info.user_selectable, "wildcard match");
        assert!(
            catalog["explicit-key"].info.user_selectable,
            "matched by catalog key or model id"
        );
        assert!(
            !catalog["to-drop"].info.user_selectable,
            "kept but not selectable"
        );
    }
    #[test]
    fn allowed_models_empty_is_unrestricted() {
        use crate::agent::models::resolve_model_catalog;
        let raw: toml::Value = toml::from_str(
            r#"
            [models]
            allowed_models = []
            [model.foo]
            model = "foo"
            base_url = "https://api.atelier/v1"
            context_window = 256000
            "#,
        )
        .unwrap();
        let catalog = resolve_model_catalog(&Config::new_from_toml_cfg(&raw).unwrap(), None);
        assert!(
            catalog["foo"].info.user_selectable,
            "empty allowed_models must not restrict"
        );
    }
    #[test]
    fn invalid_glob_is_rejected_by_validation() {
        use crate::agent::models::ModelGlobSet;
        assert!(ModelGlobSet::compile(Some(&vec!["atelier[".to_string()])).is_err());
        let raw: toml::Value = toml::from_str(
            r#"
            [models]
            allowed_models = ["atelier["]
            "#,
        )
        .unwrap();
        let err = Config::new_from_toml_cfg(&raw)
            .unwrap()
            .validate_model_filters()
            .unwrap_err();
        assert!(
            err.contains("allowed_models"),
            "error should name the offending field: {err}"
        );
    }
    #[test]
    fn supported_in_api_false_hides_from_api_key_users() {
        use crate::agent::models::{available_models, resolve_model_catalog};
        let raw: toml::Value = toml::from_str(
            r#"
            [model.oauth-only-model]
            model = "oauth-only-model"
            base_url = "https://api.atelier/v1"
            context_window = 200000
            supported_in_api = false

            [model.public-model]
            model = "public-model"
            base_url = "https://api.atelier/v1"
            context_window = 200000
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).unwrap();
        let catalog = resolve_model_catalog(&cfg, None);
        assert!(catalog.contains_key("oauth-only-model"));
        assert!(catalog.contains_key("public-model"));
        let api_available = available_models(&catalog, false);
        assert!(!api_available.values().any(|m| m.name == "oauth-only-model"));
        assert!(api_available.values().any(|m| m.name == "public-model"));
        let oauth_available = available_models(&catalog, true);
        assert!(
            oauth_available
                .values()
                .any(|m| m.name == "oauth-only-model")
        );
        assert!(oauth_available.values().any(|m| m.name == "public-model"));
    }
    #[test]
    fn inference_idle_timeout_secs_round_trip() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.slow-model]
            model = "atelier-4.5"
            base_url = "https://api.atelier/v1"
            context_window = 200000
            inference_idle_timeout_secs = 600
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("slow-model").expect("model should exist");
        assert_eq!(model.info.inference_idle_timeout_secs, Some(600));
    }
    #[test]
    fn inference_idle_timeout_secs_absent_defaults_to_none() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.default-model]
            model = "atelier-fast"
            base_url = "https://api.atelier/v1"
            context_window = 200000
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let model = resolved.get("default-model").expect("model should exist");
        assert_eq!(model.info.inference_idle_timeout_secs, None);
    }
    #[test]
    fn inference_idle_timeout_propagates_to_model_info() {
        let entry = ModelEntryConfig {
            id: None,
            model: "test".to_string(),
            base_url: "https://test.api/v1".to_string(),
            name: None,
            description: None,
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_key: None,
            env_key: None,
            api_backend: ApiBackend::default(),
            auth_scheme: None,
            extra_headers: IndexMap::new(),
            context_window: NonZeroU64::new(200_000).unwrap(),
            auto_compact_threshold_percent: None,
            system_prompt_label: None,
            api_base_url: None,
            use_concise: false,
            agent_type: default_agent_type(),
            inference_idle_timeout_secs: Some(120),
            max_retries: None,
            hidden: false,
            supported_in_api: true,
            reasoning_effort: None,
            supports_reasoning_effort: false,
            reasoning_efforts: Vec::new(),
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            show_model_fingerprint: false,
            stream_tool_calls: None,
            laziness_detector: LazinessDetectorPerModelConfig::default(),
        };
        let info = ModelInfo::from_config(&entry);
        assert_eq!(info.inference_idle_timeout_secs, Some(120));
    }
    #[test]
    fn telemetry_config_parses_local_event_gate() {
        let raw: toml::Value = toml::from_str(
            r#"
            [telemetry]
            enabled = true
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("should parse");
        assert_eq!(cfg.telemetry.enabled, Some(true));
        assert_eq!(
            serde_json::to_value(&cfg.telemetry).unwrap(),
            serde_json::json!({ "enabled": true })
        );
    }
    #[test]
    fn telemetry_config_defaults_to_local_event_gate_disabled() {
        let raw = toml::Value::Table(Default::default());
        let cfg = Config::new_from_toml_cfg(&raw).expect("should parse");
        assert_eq!(cfg.telemetry.enabled, None);
        assert_eq!(
            serde_json::to_value(&cfg.telemetry).unwrap(),
            serde_json::json!({ "enabled": null })
        );
    }
    #[test]
    fn auth_alias_maps_to_atelier_com_config() {
        let raw: toml::Value = toml::from_str(
            r#"
            [auth.oidc]
            issuer = "https://example.okta.com"
            client_id = "test-id"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        let oidc = cfg.atelier_com_config.oidc.expect("oidc should be set");
        assert_eq!(oidc.issuer, "https://example.okta.com");
        assert_eq!(oidc.client_id, "test-id");
    }
    #[test]
    fn atelier_com_config_still_works() {
        let raw: toml::Value = toml::from_str(
            r#"
            [atelier_com_config.oidc]
            issuer = "https://example.okta.com"
            client_id = "test-id"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        let oidc = cfg.atelier_com_config.oidc.expect("oidc should be set");
        assert_eq!(oidc.issuer, "https://example.okta.com");
    }
    /// `disable_api_key_auth` plumbs through the `[auth]` alias, and absent
    /// means None (opt-in knob, zero impact by default).
    #[test]
    fn disable_api_key_auth_parses_from_auth_alias() {
        let absent = Config::new_from_toml_cfg(&toml::from_str("").unwrap()).unwrap();
        assert_eq!(absent.atelier_com_config.disable_api_key_auth, None);
        let raw: toml::Value = toml::from_str(
            r#"
            [auth]
            disable_api_key_auth = true
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        assert_eq!(cfg.atelier_com_config.disable_api_key_auth, Some(true));
    }
    /// `force_login_team_uuid` parses a string (pin), array (any-of), or `[]`
    /// (fail closed); absent => None.
    #[test]
    fn force_login_team_uuid_parses_string_and_array() {
        use crate::auth::ForceLoginTeam;
        let absent = Config::new_from_toml_cfg(&toml::from_str("").unwrap()).unwrap();
        assert_eq!(absent.atelier_com_config.force_login_team_uuid, None);
        let raw: toml::Value = toml::from_str(
            r#"
            [auth]
            force_login_team_uuid = "team-abc"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        assert_eq!(
            cfg.atelier_com_config.force_login_team_uuid,
            Some(ForceLoginTeam::Single("team-abc".into())),
        );
        let raw: toml::Value = toml::from_str(
            r#"
            [atelier_com_config]
            force_login_team_uuid = ["team-a", "team-b"]
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        assert_eq!(
            cfg.atelier_com_config.force_login_team_uuid,
            Some(ForceLoginTeam::AnyOf(vec![
                "team-a".into(),
                "team-b".into()
            ])),
        );
        let raw: toml::Value = toml::from_str(
            r#"
            [auth]
            force_login_team_uuid = []
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        assert_eq!(
            cfg.atelier_com_config.force_login_team_uuid,
            Some(ForceLoginTeam::AnyOf(vec![])),
        );
    }
    /// Pinning a team via `force_login_team_uuid` implies API-key auth is
    /// disabled even without an explicit `disable_api_key_auth` (team
    /// membership can't be verified from a bare API key, so it needs IdP login).
    #[test]
    fn force_login_team_uuid_implies_api_key_auth_disabled() {
        use crate::auth::{AtelierComConfig, ForceLoginTeam};
        let base = AtelierComConfig {
            disable_api_key_auth: None,
            force_login_team_uuid: None,
            ..AtelierComConfig::default()
        };
        assert!(!base.api_key_auth_disabled());
        assert!(
            AtelierComConfig {
                disable_api_key_auth: Some(true),
                ..base.clone()
            }
            .api_key_auth_disabled()
        );
        assert!(
            AtelierComConfig {
                force_login_team_uuid: Some(ForceLoginTeam::Single("team-x".into())),
                ..base
            }
            .api_key_auth_disabled()
        );
    }
    fn resolve_models_from_toml(
        toml_str: &str,
        prefetched: Option<IndexMap<String, ModelEntry>>,
    ) -> (Config, IndexMap<String, ModelEntry>) {
        let raw: toml::Value = toml::from_str(toml_str).expect("test TOML should parse");
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        let resolved = resolve_model_list(&cfg, prefetched);
        (cfg, resolved)
    }
    fn resolve_sampling(model: &ModelEntry, session_key: Option<&str>) -> SamplerConfig {
        let credentials = resolve_credentials(model, session_key);
        sampling_config_for_model(model, credentials, None, None, None, None)
    }

    #[test]
    #[serial]
    fn local_provider_catalog_is_authoritative_and_uses_composite_keys() {
        let snapshot = ProviderSnapshot {
            providers: vec![atelier_provider::ProviderConfig {
                id: "anthropic-local".into(),
                display_name: "Local Anthropic".into(),
                base_url: url::Url::parse("http://127.0.0.1:4317/v1").unwrap(),
                credential: atelier_provider::CredentialRef::Environment {
                    variable: "ATELIER_ANTHROPIC_KEY".into(),
                },
                auth: ProviderAuth::Header {
                    name: "x-api-key".into(),
                },
                discovery: atelier_provider::ProviderDiscovery::Static,
                extra_headers: std::collections::BTreeMap::new(),
                enabled: true,
            }],
            models: vec![atelier_provider::ModelDescriptor {
                key: atelier_provider::ModelKey::new("anthropic-local", "claude-test").unwrap(),
                display_name: "Claude Test".into(),
                description: Some("local test model".into()),
                wire_api: Some(WireApi::Messages),
                context_window: Some(64_000),
                capabilities: atelier_provider::ModelCapabilities {
                    tool_calls: true,
                    reasoning_effort: true,
                    ..Default::default()
                },
                reasoning_efforts: vec!["high".into(), "medium".into()],
                default_effort: None,
                fast_mode: false,
                source: atelier_provider::ModelSource::Static,
                enabled: true,
            }],
            model_provider_overrides: Default::default(),
            experimental_model_features: Default::default(),
        };
        let mut remote = IndexMap::new();
        remote.insert(
            "remote/model".into(),
            test_model_entry(
                "remote-model",
                "https://remote.example/v1",
                None,
                None,
                None,
            ),
        );

        let cfg = Config::default();
        let resolved =
            resolve_model_list_with_provider_snapshot(&cfg, Some(remote), Some(&snapshot));
        assert_eq!(resolved.len(), 1);
        let entry = resolved.get("anthropic-local/claude-test").unwrap();
        assert_eq!(entry.info.model, "claude-test");
        assert_eq!(entry.info.api_backend, ApiBackend::Messages);
        assert_eq!(entry.info.auth_scheme, AuthScheme::XApiKey);
        assert_eq!(entry.info.context_window.get(), 64_000);
        assert!(entry.info.supports_reasoning_effort);
        assert!(entry.info.reasoning_effort.is_some());
        assert!(!resolved.contains_key("remote/model"));

        let sampling = resolve_sampling(entry, None);
        assert_eq!(sampling.api_key, None);
        assert_eq!(sampling.base_url, "http://127.0.0.1:4317/v1");
        assert_eq!(sampling.api_backend, ApiBackend::Messages);
    }

    #[test]
    fn config_model_is_first_in_the_runtime_catalog() {
        let provider = atelier_provider::ProviderConfig {
            id: "example".into(),
            display_name: "Example".into(),
            base_url: url::Url::parse("http://127.0.0.1:4317/v1").unwrap(),
            credential: atelier_provider::CredentialRef::None,
            auth: ProviderAuth::None,
            discovery: atelier_provider::ProviderDiscovery::Static,
            extra_headers: std::collections::BTreeMap::new(),
            enabled: true,
        };
        let descriptor = |model_id: &str| atelier_provider::ModelDescriptor {
            key: atelier_provider::ModelKey::new("example", model_id).unwrap(),
            display_name: model_id.into(),
            description: None,
            wire_api: Some(WireApi::ChatCompletions),
            context_window: Some(128_000),
            capabilities: Default::default(),
            reasoning_efforts: Vec::new(),
            default_effort: None,
            fast_mode: false,
            source: atelier_provider::ModelSource::Static,
            enabled: true,
        };
        let snapshot = ProviderSnapshot {
            providers: vec![provider],
            models: vec![descriptor("other"), descriptor("deepseek-v4-flash")],
            model_provider_overrides: Default::default(),
            experimental_model_features: Default::default(),
        };

        let entries =
            model_entries_from_provider_snapshot(&snapshot, Some("example/deepseek-v4-flash"));

        assert_eq!(
            entries.first().map(|(key, _)| key.as_str()),
            Some("example/deepseek-v4-flash")
        );
    }

    #[test]
    fn remote_compaction_endpoint_propagates_only_for_responses_models() {
        let provider = |id: &str| atelier_provider::ProviderConfig {
            id: id.into(),
            display_name: id.into(),
            base_url: url::Url::parse(&format!("https://{id}.example.test/v1")).unwrap(),
            credential: atelier_provider::CredentialRef::None,
            auth: ProviderAuth::None,
            discovery: atelier_provider::ProviderDiscovery::Static,
            extra_headers: std::collections::BTreeMap::new(),
            enabled: true,
        };
        let descriptor = |provider_id: &str, wire_api| atelier_provider::ModelDescriptor {
            key: atelier_provider::ModelKey::new(provider_id, "shared").unwrap(),
            display_name: "shared".into(),
            description: None,
            wire_api: Some(wire_api),
            context_window: Some(128_000),
            capabilities: Default::default(),
            reasoning_efforts: Vec::new(),
            default_effort: None,
            fast_mode: false,
            source: atelier_provider::ModelSource::Static,
            enabled: true,
        };
        let feature = atelier_provider::ExperimentalModelFeatures {
            remote_compaction: Some(atelier_provider::ExperimentalEndpoint {
                enabled: true,
                endpoint: "responses/compact".into(),
            }),
            image_generation: None,
        };
        let snapshot = ProviderSnapshot {
            providers: vec![provider("responses"), provider("chat")],
            models: vec![
                descriptor("responses", WireApi::Responses),
                descriptor("chat", WireApi::ChatCompletions),
            ],
            model_provider_overrides: Default::default(),
            experimental_model_features: std::collections::BTreeMap::from([
                ("responses/shared".into(), feature.clone()),
                ("chat/shared".into(), feature),
            ]),
        };

        let entries = model_entries_from_provider_snapshot(&snapshot, None);
        let responses = entries.get("responses/shared").unwrap();
        let chat = entries.get("chat/shared").unwrap();
        assert_eq!(
            responses.remote_compaction_endpoint.as_deref(),
            Some("responses/compact")
        );
        assert_eq!(chat.remote_compaction_endpoint, None);
        assert_eq!(
            resolve_sampling(responses, None)
                .remote_compaction_endpoint
                .as_deref(),
            Some("responses/compact")
        );
        assert_eq!(
            resolve_sampling(chat, None).remote_compaction_endpoint,
            None
        );
        assert!(
            !responses
                .request_payload
                .contains_key("remote_compaction_endpoint"),
            "transport routing metadata must not leak into ordinary inference payload"
        );
    }

    #[test]
    fn provider_models_without_transport_metadata_use_safe_runtime_defaults() {
        let model_key = atelier_provider::ModelKey::new("example", "discovered-model").unwrap();
        let snapshot = ProviderSnapshot {
            providers: vec![atelier_provider::ProviderConfig {
                id: "example".into(),
                display_name: "Example".into(),
                base_url: url::Url::parse("https://models.example.test/v1").unwrap(),
                credential: atelier_provider::CredentialRef::None,
                auth: ProviderAuth::None,
                discovery: atelier_provider::ProviderDiscovery::Static,
                extra_headers: std::collections::BTreeMap::new(),
                enabled: true,
            }],
            models: vec![atelier_provider::ModelDescriptor {
                key: model_key,
                display_name: "Discovered Model".into(),
                description: None,
                wire_api: None,
                context_window: None,
                capabilities: Default::default(),
                reasoning_efforts: Vec::new(),
                default_effort: None,
                fast_mode: false,
                source: atelier_provider::ModelSource::Remote,
                enabled: true,
            }],
            model_provider_overrides: Default::default(),
            experimental_model_features: Default::default(),
        };

        let entries = model_entries_from_provider_snapshot(&snapshot, None);
        let entry = entries
            .get("example/discovered-model")
            .expect("models without optional metadata remain selectable");

        assert_eq!(entry.info.context_window.get(), 100_000);
        assert_eq!(entry.info.api_backend, ApiBackend::ChatCompletions);
    }

    #[test]
    fn exact_model_definition_sets_wire_api() {
        let model_key = atelier_provider::ModelKey::new("example", "deepseek-v4-flash").unwrap();
        let provider = atelier_provider::ProviderConfig {
            id: "example".into(),
            display_name: "Example".into(),
            base_url: url::Url::parse("http://127.0.0.1:4317/v1").unwrap(),
            credential: atelier_provider::CredentialRef::None,
            auth: ProviderAuth::None,
            discovery: atelier_provider::ProviderDiscovery::Static,
            extra_headers: std::collections::BTreeMap::new(),
            enabled: true,
        };
        let snapshot = ProviderSnapshot {
            providers: vec![provider],
            models: vec![atelier_provider::ModelDescriptor {
                key: model_key.clone(),
                display_name: "deepseek-v4-flash".into(),
                description: None,
                wire_api: Some(WireApi::Responses),
                context_window: Some(128_000),
                capabilities: Default::default(),
                reasoning_efforts: Vec::new(),
                default_effort: None,
                fast_mode: false,
                source: atelier_provider::ModelSource::Static,
                enabled: true,
            }],
            model_provider_overrides: Default::default(),
            experimental_model_features: Default::default(),
        };

        let resolved_wire_api = snapshot.resolve_wire_api(&model_key).unwrap();
        let entries = model_entries_from_provider_snapshot(&snapshot, None);
        let entry = entries.get("example/deepseek-v4-flash").unwrap();
        let sampling = resolve_sampling(entry, None);

        assert_eq!(
            resolved_wire_api.wire_api,
            atelier_provider::WireApi::Responses
        );
        assert_eq!(
            resolved_wire_api.source,
            atelier_provider::WireApiSource::ModelDefinition
        );
        assert_eq!(entry.info.api_backend, ApiBackend::Responses);
        assert_eq!(sampling.api_backend, ApiBackend::Responses);
    }

    #[test]
    fn composite_provider_model_does_not_inherit_vendor_session_token() {
        let mut model = test_model_entry(
            "provider-model",
            "http://127.0.0.1:4317/v1",
            None,
            None,
            None,
        );
        model.info.id = Some("local/provider-model".into());

        let credentials = resolve_credentials(&model, Some("vendor-session-token"));

        assert_eq!(credentials.api_key, None);
        assert_eq!(credentials.base_url, "http://127.0.0.1:4317/v1");
        assert_eq!(
            credentials.auth_type,
            atelier_chat_state::AuthType::ApiKey,
            "an unauthenticated local Provider must not inherit vendor auth"
        );
    }

    #[test]
    #[serial]
    fn empty_provider_registry_does_not_restore_vendor_models() {
        let snapshot = ProviderSnapshot {
            providers: Vec::new(),
            models: Vec::new(),
            model_provider_overrides: Default::default(),
            experimental_model_features: Default::default(),
        };
        let cfg = Config::default();
        let mut prefetched = IndexMap::new();
        prefetched.insert(
            "vendor/model".into(),
            test_model_entry(
                "vendor-model",
                "https://vendor.example/v1",
                None,
                None,
                None,
            ),
        );

        let resolved =
            resolve_model_list_with_provider_snapshot(&cfg, Some(prefetched), Some(&snapshot));

        assert!(resolved.is_empty());
    }
    #[test]
    #[serial]
    fn e2e_user_overrides_default_model_key_with_custom_endpoint() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
            [model."{dm}"]
            model = "{dm}"
            base_url = "https://inference.example.com/v1"
            context_window = 200000
            env_key = "ENTERPRISE_AUTH_TOKEN"
            "#,
            ),
            None,
        );
        let model = models.get(dm).expect("model should exist");
        assert_eq!(model.info.base_url, "https://inference.example.com/v1");
        assert_eq!(
            model.env_key.as_ref().and_then(|k| k.primary()),
            Some("ENTERPRISE_AUTH_TOKEN")
        );
        unsafe { std::env::set_var("ENTERPRISE_AUTH_TOKEN", "enterprise-secret-key") };
        let sampling = resolve_sampling(model, None);
        assert_eq!(
            sampling.api_key.as_deref(),
            Some("enterprise-secret-key"),
            "should use the user's env_key, not fall through to session/external"
        );
        assert_eq!(
            sampling.base_url, "https://inference.example.com/v1",
            "should route to the user's custom endpoint"
        );
        unsafe { std::env::remove_var("ENTERPRISE_AUTH_TOKEN") };
    }
    #[test]
    #[serial]
    fn e2e_config_toml_model_overrides_default() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
            [model."{dm}"]
            base_url = "https://inference.example.com/v1"
            "#,
            ),
            None,
        );
        let model = models.get(dm).expect("model should exist");
        let sampling = resolve_sampling(model, Some("session-tok"));
        assert_eq!(sampling.base_url, "https://inference.example.com/v1");
        let sampling = resolve_sampling(model, None);
        assert_eq!(sampling.base_url, "https://inference.example.com/v1");
    }
    #[test]
    fn e2e_user_overrides_default_model_with_api_key() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
            [model."{dm}"]
            model = "{dm}"
            base_url = "https://my-proxy.example.com/v1"
            context_window = 200000
            api_key = "my-custom-api-key"
            "#,
            ),
            None,
        );
        let model = models.get(dm).expect("model should exist");
        assert_eq!(model.info.base_url, "https://my-proxy.example.com/v1");
        assert_eq!(model.api_key.as_deref(), Some("my-custom-api-key"));
        assert!(model.env_key.is_none());
        let sampling = resolve_sampling(model, Some("session-token"));
        assert_eq!(
            sampling.api_key.as_deref(),
            Some("my-custom-api-key"),
            "model's own api_key must beat session token"
        );
        assert_eq!(
            sampling.base_url, "https://my-proxy.example.com/v1",
            "should route to user's custom endpoint"
        );
    }
    #[test]
    fn parsed_config_has_models_config() {
        let raw: toml::Value = toml::from_str(
            r#"
            [models]
            default = "my-enterprise-model"
            web_search = "enterprise-search"
            session_summary = "title-model"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        assert_eq!(cfg.models.default.as_deref(), Some("my-enterprise-model"));
        assert_eq!(cfg.models.web_search.as_deref(), Some("enterprise-search"));
        assert_eq!(cfg.models.session_summary.as_deref(), Some("title-model"));
    }
    #[test]
    fn config_models_default_is_not_overwritten_by_default_models_json() {
        let config_default = Some("custom-byok-model");
        let local_runtime_settings_default = Some("remote-settings-model");
        let resolved = resolve_string_flag(
            None,
            "ATELIER_DEFAULT_MODEL_TEST_NONEXISTENT",
            config_default,
            local_runtime_settings_default,
        );
        let resolved = resolved.expect("should resolve to a value");
        assert_eq!(resolved.value, "custom-byok-model");
        assert_eq!(
            resolved.source,
            ConfigSource::Config,
            "[models] default from config.toml must beat remote settings and compiled-in defaults"
        );
    }
    #[test]
    fn config_models_default_custom_model_is_in_resolved_model_list() {
        let (_, models) = resolve_models_from_toml(
            r#"
            [model.acme-atelier]
            model = "atelier-4.5"
            base_url = "https://inference.example.com/v1"
            context_window = 256000
            env_key = "ENTERPRISE_AUTH_TOKEN"
            "#,
            None,
        );
        assert!(
            models.contains_key("acme-atelier"),
            "user-defined model must be in the resolved model list"
        );
        let model = models.get("acme-atelier").unwrap();
        assert_eq!(model.info.model, "atelier-4.5");
        assert_eq!(model.info.base_url, "https://inference.example.com/v1");
    }
    #[cfg(any())] // Atelier has no built-in vendor session or CLI proxy model.
    #[test]
    fn e2e_default_model_with_session_routes_to_proxy() {
        let (_, models) = resolve_models_from_toml("", None);
        let model = models
            .get(crate::models::default_model())
            .expect("default model should exist");
        let sampling = resolve_sampling(model, Some("session-token-123"));
        assert_eq!(sampling.api_key.as_deref(), Some("session-token-123"));
        assert_eq!(
            sampling.base_url, "https://cli-chat-proxy.atelier.com/v1",
            "session auth should route to cli-chat-proxy, not api.x.ai"
        );
    }
    #[test]
    fn e2e_user_config_overrides_prefetched_model() {
        let dm = crate::models::default_model();
        let mut prefetched = IndexMap::new();
        prefetched.insert(
            dm.to_string(),
            test_model_entry(
                dm,
                "https://cli-chat-proxy.atelier.com/v1",
                None,
                None,
                None,
            ),
        );
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
            [model."{dm}"]
            model = "{dm}"
            base_url = "https://my-proxy.example.com/v1"
            context_window = 200000
            api_key = "my-api-key"
            "#,
            ),
            Some(prefetched),
        );
        let model = models.get(dm).unwrap();
        assert_eq!(
            model.info.base_url, "https://my-proxy.example.com/v1",
            "user TOML should override prefetched model"
        );
        let sampling = resolve_sampling(model, Some("session-token"));
        assert_eq!(
            sampling.api_key.as_deref(),
            Some("my-api-key"),
            "model's own api_key should win over session token"
        );
        assert_eq!(sampling.base_url, "https://my-proxy.example.com/v1");
    }
    #[cfg(any())] // Atelier has no duplicate built-in vendor model/proxy entry to preserve.
    #[test]
    fn e2e_duplicate_model_field_both_entries_survive() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
            [model.acme-atelier]
            model = "{dm}"
            base_url = "https://inference.example.com/v1"
            context_window = 200000
            api_key = "enterprise-key"
            "#,
            ),
            None,
        );
        assert!(models.contains_key(dm), "default entry should still exist");
        assert!(
            models.contains_key("acme-atelier"),
            "user entry with different key should also exist"
        );
        let default = models.get(dm).unwrap();
        let user = models.get("acme-atelier").unwrap();
        assert_eq!(default.info.model, user.info.model, "same model field");
        assert_ne!(
            default.info.base_url, user.info.base_url,
            "different base_urls"
        );
        let sampling = resolve_sampling(user, None);
        assert_eq!(sampling.api_key.as_deref(), Some("enterprise-key"));
        assert_eq!(sampling.base_url, "https://inference.example.com/v1");
        let sampling = resolve_sampling(default, Some("session-key"));
        assert_eq!(sampling.api_key.as_deref(), Some("session-key"));
        assert_eq!(sampling.base_url, "https://cli-chat-proxy.atelier.com/v1",);
    }
    #[test]
    fn e2e_enterprise_custom_endpoint_skips_xai_defaults() {
        let mut cfg = Config::default();
        cfg.endpoints.models_base_url = Some("https://enterprise.acme.com/v1".to_owned());
        let mut prefetched = IndexMap::new();
        prefetched.insert(
            "acme-model".to_string(),
            test_model_entry(
                "acme-model",
                "https://enterprise.acme.com/v1",
                None,
                None,
                None,
            ),
        );
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        assert!(
            resolved.contains_key("acme-model"),
            "enterprise model should be present"
        );
        assert!(
            !resolved.contains_key(crate::models::default_model()),
            "xAI default must not leak into enterprise model list"
        );
        assert_eq!(resolved.len(), 1, "only the prefetched enterprise model");
    }
    #[test]
    fn e2e_default_endpoint_still_injects_defaults() {
        let cfg = Config::default();
        let resolved = resolve_model_list(&cfg, None);
        assert!(
            resolved.contains_key(crate::models::default_model()),
            "default model should be present when using default endpoint"
        );
    }
    #[test]
    fn e2e_acp_model_info_no_dedup_on_model_field() {
        let mut models = IndexMap::new();
        models.insert(
            "default-atelier".to_string(),
            test_model_entry(
                crate::models::default_model(),
                "https://cli-chat-proxy.atelier.com/v1",
                None,
                None,
                Some("https://api.atelier/v1"),
            ),
        );
        models.insert(
            "acme-atelier".to_string(),
            test_model_entry(
                crate::models::default_model(),
                "https://inference.example.com/v1",
                Some("enterprise-key"),
                None,
                None,
            ),
        );
        let acp_models = to_acp_model_info(&models);
        assert_eq!(
            acp_models.len(),
            2,
            "both entries should survive in ACP model list"
        );
        assert!(
            acp_models.contains_key(&acp::ModelId::new("default-atelier")),
            "default entry should be addressable by map key"
        );
        assert!(
            acp_models.contains_key(&acp::ModelId::new("acme-atelier")),
            "user entry should be addressable by map key"
        );
    }
    #[test]
    fn e2e_enterprise_endpoints_plus_partial_model_override() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
            [endpoints]
            cli_chat_proxy_base_url = "https://enterprise-proxy.acme.com/v1"

            [model."{dm}"]
            api_key = "acme-api-key"
            api_base_url = "https://provider-api.acme.com/v1"
            "#,
            ),
            None,
        );
        let model = models.get(dm).expect("model should exist");
        assert_eq!(
            model.info.base_url, "https://enterprise-proxy.acme.com/v1",
            "base_url must inherit from [endpoints], not stale default"
        );
        assert_eq!(model.api_key.as_deref(), Some("acme-api-key"));
        assert_eq!(
            model.api_base_url.as_deref(),
            Some("https://provider-api.acme.com/v1"),
        );
        let sampling = resolve_sampling(model, Some("session-token"));
        assert_eq!(
            sampling.api_key.as_deref(),
            Some("acme-api-key"),
            "model's own api_key must beat session token"
        );
        assert_eq!(
            sampling.base_url, "https://enterprise-proxy.acme.com/v1",
            "sampling must route to enterprise proxy"
        );
    }
    #[test]
    fn e2e_enterprise_endpoints_only_no_model_override() {
        let (_, models) = resolve_models_from_toml(
            r#"
            [endpoints]
            cli_chat_proxy_base_url = "https://enterprise-proxy.acme.com/v1"
            "#,
            None,
        );
        let model = models
            .get(crate::models::default_model())
            .expect("model should exist");
        assert_eq!(
            model.info.base_url, "https://enterprise-proxy.acme.com/v1",
            "default model should use enterprise cli_chat_proxy_base_url"
        );
        assert_eq!(model.api_base_url, None);
    }
    /// Unset every env var that `EndpointsConfig::default()` reads for endpoints,
    /// so the cli-chat-proxy resolver tests below are deterministic regardless of
    /// the ambient environment. Gated behind `#[serial]`.
    fn unset_endpoint_env_vars() {
        for k in [
            "ATELIER_CLI_CHAT_PROXY_BASE_URL",
            "ATELIER_MANAGED_CONFIG_URL",
            "ATELIER_MODELS_BASE_URL",
            "ATELIER_MODELS_LIST_URL",
        ] {
            unsafe { std::env::remove_var(k) };
        }
    }
    /// INVARIANT: endpoint resolution uses only explicit generic configuration.
    #[test]
    #[serial]
    fn aux_endpoints_resolve_to_proxy_never_inference() {
        unset_endpoint_env_vars();
        let cfg = EndpointsConfig {
            cli_chat_proxy_base_url: None,
            ..Default::default()
        };
        assert_eq!(cfg.proxy_url(), "");
        assert_eq!(cfg.resolve_inference_base_url(), "");
        assert_eq!(cfg.resolve_models_list_url(), "");
        let overridden = EndpointsConfig {
            cli_chat_proxy_base_url: Some("https://proxy.enterprise.example/v1".to_string()),
            ..Default::default()
        };
        assert_eq!(
            overridden.proxy_url(),
            "https://proxy.enterprise.example/v1"
        );
    }
    #[test]
    fn e2e_user_override_explicit_base_url_wins_over_endpoints() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
            [endpoints]
            cli_chat_proxy_base_url = "https://enterprise-proxy.acme.com/v1"

            [model."{dm}"]
            base_url = "https://my-special-proxy.example.com/v1"
            "#,
            ),
            None,
        );
        let model = models.get(dm).expect("model should exist");
        assert_eq!(
            model.info.base_url, "https://my-special-proxy.example.com/v1",
            "explicit base_url in [model.*] must win over [endpoints]"
        );
    }
    #[test]
    fn e2e_models_endpoint_serde_alias_parses_as_models_list_url() {
        let raw: toml::Value = toml::from_str(
            r#"
            [endpoints]
            models_endpoint = "https://old-style.acme.com/v1/models"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        assert_eq!(
            cfg.endpoints.models_list_url.as_deref(),
            Some("https://old-style.acme.com/v1/models"),
            "models_endpoint alias should parse into models_list_url"
        );
        assert!(cfg.endpoints.has_custom_endpoint());
    }
    #[test]
    fn e2e_config_models_parsed_directly_not_via_deep_merge() {
        let raw: toml::Value = toml::from_str(
            r#"
            [model.custom-model]
            model = "my-custom-llm"
            api_key = "custom-key"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        assert!(cfg.config_models.contains_key("custom-model"));
        let model_override = cfg.config_models.get("custom-model").unwrap();
        assert_eq!(model_override.model.as_deref(), Some("my-custom-llm"));
        assert_eq!(model_override.api_key.as_deref(), Some("custom-key"));
        assert!(
            model_override.base_url.is_none(),
            "base_url should be None when user didn't set it"
        );
    }
    #[test]
    #[serial]
    fn resolve_feedback_is_permanently_disabled() {
        unsafe { std::env::remove_var("ATELIER_FEEDBACK_ENABLED") };
        unsafe { std::env::remove_var("ATELIER_TELEMETRY_ENABLED") };
        let cfg = Config::default();
        let r = cfg.resolve_feedback();
        assert!(!r.value, "vendor feedback must stay disabled");
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    #[serial]
    fn resolve_session_recap_defaults_to_true_when_unset() {
        unsafe { std::env::remove_var("ATELIER_SESSION_RECAP") };
        let cfg = Config::default();
        let r = cfg.resolve_session_recap();
        assert!(r.value, "session_recap should be true by default");
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    #[serial]
    fn resolve_session_recap_config_off_overrides_default() {
        unsafe { std::env::remove_var("ATELIER_SESSION_RECAP") };
        let cfg = Config {
            features: Features {
                session_recap: Some(false),
                ..Default::default()
            },
            ..Default::default()
        };
        let r = cfg.resolve_session_recap();
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
    }
    #[test]
    #[serial]
    fn resolve_session_recap_env_off_overrides_default() {
        unsafe { std::env::set_var("ATELIER_SESSION_RECAP", "0") };
        let cfg = Config::default();
        let r = cfg.resolve_session_recap();
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var("ATELIER_SESSION_RECAP") };
    }
    #[test]
    #[serial]
    fn resolve_session_recap_remote_off_overrides_default() {
        unsafe { std::env::remove_var("ATELIER_SESSION_RECAP") };
        let cfg = Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                session_recap: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = cfg.resolve_session_recap();
        assert!(
            !r.value,
            "remote settings/remote false must kill-switch default on"
        );
        assert_eq!(r.source, ConfigSource::Remote);
    }
    /// Precedence: env > config.toml > remote settings > default(false). One test
    /// covers the full ladder so we do not maintain a matrix of flag cases.
    #[test]
    #[serial]
    fn resolve_two_pass_compaction_precedence() {
        unsafe { std::env::remove_var("ATELIER_TWO_PASS_COMPACTION") };
        let default_cfg = Config::default();
        let r = default_cfg.resolve_two_pass_compaction();
        assert!(!r.value, "default is opt-in off");
        assert_eq!(r.source, ConfigSource::Default);
        let remote_on = Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                two_pass_compaction_enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = remote_on.resolve_two_pass_compaction();
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Remote);
        let config_over_remote = Config {
            features: Features {
                two_pass_compaction: Some(true),
                ..Default::default()
            },
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                two_pass_compaction_enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = config_over_remote.resolve_two_pass_compaction();
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Config);
        unsafe { std::env::set_var("ATELIER_TWO_PASS_COMPACTION", "0") };
        let r = config_over_remote.resolve_two_pass_compaction();
        assert!(!r.value, "env wins over config + remote");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var("ATELIER_TWO_PASS_COMPACTION") };
    }
    /// Gate precedence: env > `[doom_loop_recovery]` > remote settings >
    /// default(off), with the remote layer merged PER-FIELD from the nested
    /// `doom_loop_recovery` object. One test covers the full ladder (the
    /// `resolve_two_pass_compaction_precedence` pattern).
    #[test]
    #[serial]
    fn resolve_doom_loop_recovery_precedence() {
        use crate::util::config::DoomLoopRecoverySettings;
        unsafe { std::env::remove_var("ATELIER_DOOM_LOOP_RECOVERY") };
        let default_cfg = Config::default();
        assert!(
            default_cfg.resolve_doom_loop_recovery().is_none(),
            "default is opt-in off"
        );
        let remote_on = Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                doom_loop_recovery: Some(DoomLoopRecoverySettings {
                    enabled: Some(true),
                    max_threshold: Some(16),
                    max_retries: Some(1),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let p = remote_on.resolve_doom_loop_recovery().expect("remote on");
        assert_eq!(p.max_threshold, 16);
        assert_eq!(p.max_retries, 1);
        let partial_remote = Config {
            doom_loop_recovery: DoomLoopRecoverySettings {
                enabled: Some(true),
                ..Default::default()
            },
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                doom_loop_recovery: Some(DoomLoopRecoverySettings {
                    max_threshold: Some(16),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let p = partial_remote
            .resolve_doom_loop_recovery()
            .expect("gate from TOML despite remote object omitting enabled");
        assert_eq!(p.max_threshold, 16, "remote tunable applies");
        assert_eq!(p.max_retries, 2, "unset field falls to the default");
        let config_over_remote = Config {
            doom_loop_recovery: DoomLoopRecoverySettings {
                enabled: Some(true),
                max_threshold: Some(4),
                max_retries: Some(3),
            },
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                doom_loop_recovery: Some(DoomLoopRecoverySettings {
                    enabled: Some(false),
                    max_threshold: Some(16),
                    max_retries: Some(1),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let p = config_over_remote
            .resolve_doom_loop_recovery()
            .expect("config on beats remote kill-switch");
        assert_eq!(p.max_threshold, 4);
        assert_eq!(p.max_retries, 3);
        unsafe { std::env::set_var("ATELIER_DOOM_LOOP_RECOVERY", "0") };
        assert!(
            config_over_remote.resolve_doom_loop_recovery().is_none(),
            "env wins over config + remote"
        );
        unsafe { std::env::remove_var("ATELIER_DOOM_LOOP_RECOVERY") };
    }
    /// The `[doom_loop_recovery]` TOML section deserializes through the
    /// standard config path (no bespoke parser).
    #[test]
    #[serial]
    fn doom_loop_recovery_section_parses_from_toml() {
        unsafe { std::env::remove_var("ATELIER_DOOM_LOOP_RECOVERY") };
        let raw: toml::Value = toml::from_str(
            r#"
            [doom_loop_recovery]
            enabled = true
            max_threshold = 12
            max_retries = 1
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).unwrap();
        assert_eq!(cfg.doom_loop_recovery.enabled, Some(true));
        let p = cfg.resolve_doom_loop_recovery().expect("enabled via toml");
        assert_eq!(p.max_threshold, 12);
        assert_eq!(p.max_retries, 1);
    }
    /// Out-of-range tunables clamp instead of being honored or dropped.
    #[test]
    #[serial]
    fn resolve_doom_loop_recovery_clamps_tunables() {
        use crate::util::config::DoomLoopRecoverySettings;
        unsafe { std::env::remove_var("ATELIER_DOOM_LOOP_RECOVERY") };
        let cfg = Config {
            doom_loop_recovery: DoomLoopRecoverySettings {
                enabled: Some(true),
                max_threshold: Some(1_000),
                max_retries: Some(99),
            },
            ..Default::default()
        };
        let p = cfg.resolve_doom_loop_recovery().expect("enabled");
        assert_eq!(p.max_threshold, 64);
        assert_eq!(p.max_retries, 5);
        let cfg = Config {
            doom_loop_recovery: DoomLoopRecoverySettings {
                enabled: Some(true),
                max_threshold: Some(0),
                max_retries: Some(0),
            },
            ..Default::default()
        };
        let p = cfg.resolve_doom_loop_recovery().expect("enabled");
        assert_eq!(p.max_threshold, 2);
        assert_eq!(p.max_retries, 0, "0 retries is valid (observe-only)");
    }
    #[test]
    #[serial]
    fn resolve_feedback_env_overrides_all() {
        unsafe { std::env::set_var("ATELIER_FEEDBACK_ENABLED", "true") };
        let mut cfg = Config::default();
        cfg.features.feedback = Some(false);
        cfg.local_runtime_settings = Some(crate::util::config::LocalRuntimeSettings {
            feedback_enabled: Some(false),
            ..Default::default()
        });
        let r = cfg.resolve_feedback();
        assert_eq!(r.source, ConfigSource::Default);
        assert!(!r.value);
        unsafe { std::env::remove_var("ATELIER_FEEDBACK_ENABLED") };
    }
    #[test]
    #[serial]
    fn resolve_feedback_config_overrides_local_runtime_settings() {
        unsafe { std::env::remove_var("ATELIER_FEEDBACK_ENABLED") };
        let mut cfg = Config::default();
        cfg.features.feedback = Some(true);
        cfg.local_runtime_settings = Some(crate::util::config::LocalRuntimeSettings {
            feedback_enabled: Some(false),
            ..Default::default()
        });
        let r = cfg.resolve_feedback();
        assert_eq!(r.source, ConfigSource::Default);
        assert!(!r.value);
    }
    #[test]
    #[serial]
    fn resolve_feedback_local_runtime_settings_used_when_no_local() {
        unsafe { std::env::remove_var("ATELIER_FEEDBACK_ENABLED") };
        let cfg = Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                feedback_enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = cfg.resolve_feedback();
        assert_eq!(r.source, ConfigSource::Default);
        assert!(!r.value);
    }
    #[test]
    #[serial]
    fn resolve_goal_defaults_to_true_when_unset() {
        unsafe { std::env::remove_var("ATELIER_GOAL") };
        let cfg = Config::default();
        let r = cfg.resolve_goal();
        assert!(r.value, "goal should be on by default");
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    #[serial]
    fn resolve_goal_env_overrides_config() {
        unsafe { std::env::set_var("ATELIER_GOAL", "1") };
        let mut cfg = Config::default();
        cfg.goal.enabled = Some(false);
        cfg.local_runtime_settings = Some(crate::util::config::LocalRuntimeSettings {
            goal_enabled: Some(false),
            ..Default::default()
        });
        let r = cfg.resolve_goal();
        assert_eq!(r.source, ConfigSource::Env);
        assert!(r.value);
        unsafe { std::env::remove_var("ATELIER_GOAL") };
    }
    #[test]
    #[serial]
    fn resolve_goal_config_overrides_local_runtime_settings() {
        unsafe { std::env::remove_var("ATELIER_GOAL") };
        let mut cfg = Config::default();
        cfg.goal.enabled = Some(true);
        cfg.local_runtime_settings = Some(crate::util::config::LocalRuntimeSettings {
            goal_enabled: Some(false),
            ..Default::default()
        });
        let r = cfg.resolve_goal();
        assert_eq!(r.source, ConfigSource::Config);
        assert!(r.value);
    }
    #[test]
    #[serial]
    fn resolve_goal_local_runtime_settings_used_when_no_local() {
        unsafe { std::env::remove_var("ATELIER_GOAL") };
        let cfg = Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                goal_enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = cfg.resolve_goal();
        assert_eq!(r.source, ConfigSource::Remote);
        assert!(r.value);
    }
    /// The remote settings `goal_enabled: false` kill-switch must still win over
    /// the default-on fallback.
    #[test]
    #[serial]
    fn resolve_goal_local_runtime_settings_kill_switch_overrides_default_on() {
        unsafe { std::env::remove_var("ATELIER_GOAL") };
        let cfg = Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                goal_enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = cfg.resolve_goal();
        assert_eq!(r.source, ConfigSource::Remote);
        assert!(!r.value);
    }
    #[test]
    #[serial]
    fn resolve_ask_user_question_defaults_to_true_when_unset() {
        unsafe { std::env::remove_var("ATELIER_ASK_USER_QUESTION") };
        let cfg = Config::default();
        let r = cfg.resolve_ask_user_question();
        assert!(r.value, "ask_user_question should be on by default");
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    #[serial]
    fn resolve_ask_user_question_local_runtime_settings_enables() {
        unsafe { std::env::remove_var("ATELIER_ASK_USER_QUESTION") };
        let cfg = Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                ask_user_question_enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = cfg.resolve_ask_user_question();
        assert_eq!(r.source, ConfigSource::Remote);
        assert!(r.value);
    }
    #[test]
    #[serial]
    fn resolve_ask_user_question_env_overrides_local_runtime_settings() {
        unsafe { std::env::set_var("ATELIER_ASK_USER_QUESTION", "1") };
        let cfg = Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                ask_user_question_enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = cfg.resolve_ask_user_question();
        assert_eq!(r.source, ConfigSource::Env);
        assert!(r.value);
        unsafe { std::env::remove_var("ATELIER_ASK_USER_QUESTION") };
    }
    #[test]
    #[serial]
    fn resolve_ask_user_question_config_overrides_local_runtime_settings() {
        unsafe { std::env::remove_var("ATELIER_ASK_USER_QUESTION") };
        let mut cfg = Config::default();
        cfg.features.ask_user_question = Some(true);
        cfg.local_runtime_settings = Some(crate::util::config::LocalRuntimeSettings {
            ask_user_question_enabled: Some(false),
            ..Default::default()
        });
        let r = cfg.resolve_ask_user_question();
        assert_eq!(r.source, ConfigSource::Config);
        assert!(r.value);
    }
    /// remote settings `ask_user_question_enabled: false` is a kill-switch: it must
    /// win over the default-on fallback.
    #[test]
    #[serial]
    fn resolve_ask_user_question_local_runtime_settings_kill_switch_overrides_default_on() {
        unsafe { std::env::remove_var("ATELIER_ASK_USER_QUESTION") };
        let cfg = Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                ask_user_question_enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = cfg.resolve_ask_user_question();
        assert_eq!(r.source, ConfigSource::Remote);
        assert!(!r.value);
    }
    #[cfg(any())] // Vendor-hosted image editing was removed from Atelier.
    #[test]
    #[serial]
    fn imagine_tools_disabled_gates_image_edit() {
        unsafe { std::env::remove_var("ATELIER_IMAGE_EDIT") };
        let with_list = |tools: Vec<&str>| Config {
            local_runtime_settings: Some(crate::util::config::LocalRuntimeSettings {
                imagine_tools_disabled: Some(tools.into_iter().map(String::from).collect()),
                ..Default::default()
            }),
            ..Default::default()
        };
        unsafe { std::env::set_var("ATELIER_IMAGE_EDIT", "1") };
        let off = with_list(vec!["image_edit"]).resolve_image_edit();
        assert!(!off.value);
        assert_eq!(off.source, ConfigSource::Remote);
        unsafe { std::env::remove_var("ATELIER_IMAGE_EDIT") };
        assert!(with_list(vec!["image_to_video"]).resolve_image_edit().value);
        assert!(Config::default().resolve_image_edit().value);
    }
    /// Clear every env var the goal/companion resolvers read so tests
    /// start from a known baseline regardless of run order.
    fn clear_goal_envs() {
        unsafe {
            std::env::remove_var("ATELIER_GOAL");
            std::env::remove_var("ATELIER_GOAL_CLASSIFIER");
            std::env::remove_var("ATELIER_GOAL_PLANNER");
            std::env::remove_var("ATELIER_GOAL_SUMMARY");
            std::env::remove_var("ATELIER_GOAL_VERIFIER_N");
            std::env::remove_var("ATELIER_GOAL_CLASSIFIER_MAX");
            std::env::remove_var("ATELIER_GOAL_STRATEGIST_EVERY");
            std::env::remove_var("ATELIER_GOAL_REVERIFY_AFTER");
        }
    }
    fn cfg_with_goal(goal: bool) -> Config {
        Config {
            goal: GoalConfig {
                enabled: Some(goal),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    fn cfg_with_goal_and_remote(
        goal: bool,
        remote: crate::util::config::LocalRuntimeSettings,
    ) -> Config {
        Config {
            goal: GoalConfig {
                enabled: Some(goal),
                ..Default::default()
            },
            local_runtime_settings: Some(remote),
            ..Default::default()
        }
    }
    fn remote_classifier(v: bool) -> crate::util::config::LocalRuntimeSettings {
        crate::util::config::LocalRuntimeSettings {
            goal_classifier_enabled: Some(v),
            ..Default::default()
        }
    }
    fn remote_planner(v: bool) -> crate::util::config::LocalRuntimeSettings {
        crate::util::config::LocalRuntimeSettings {
            goal_planner_enabled: Some(v),
            ..Default::default()
        }
    }
    fn remote_summary(v: bool) -> crate::util::config::LocalRuntimeSettings {
        crate::util::config::LocalRuntimeSettings {
            goal_summary_enabled: Some(v),
            ..Default::default()
        }
    }
    fn cfg_with_goal_config(goal: GoalConfig) -> Config {
        Config {
            goal,
            ..Default::default()
        }
    }
    fn cfg_with_goal_config_and_remote(
        goal: GoalConfig,
        remote: crate::util::config::LocalRuntimeSettings,
    ) -> Config {
        Config {
            goal,
            local_runtime_settings: Some(remote),
            ..Default::default()
        }
    }
    #[test]
    #[serial]
    fn resolve_goal_classifier_default_tracks_goal_enabled() {
        clear_goal_envs();
        assert!(
            !cfg_with_goal(false)
                .resolve_goal_classifier_enabled(false)
                .value
        );
        let on = cfg_with_goal(true).resolve_goal_classifier_enabled(true);
        assert!(on.value);
        assert_eq!(on.source, ConfigSource::Default);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_classifier_remote_forces_either_way() {
        clear_goal_envs();
        let off = cfg_with_goal_and_remote(true, remote_classifier(false))
            .resolve_goal_classifier_enabled(true);
        assert!(!off.value);
        assert_eq!(off.source, ConfigSource::Remote);
        let on = cfg_with_goal_and_remote(false, remote_classifier(true))
            .resolve_goal_classifier_enabled(false);
        assert!(on.value);
        assert_eq!(on.source, ConfigSource::Remote);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_classifier_env_overrides_default_and_remote() {
        clear_goal_envs();
        unsafe { std::env::set_var("ATELIER_GOAL_CLASSIFIER", "0") };
        let r = cfg_with_goal_and_remote(true, remote_classifier(true))
            .resolve_goal_classifier_enabled(true);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::set_var("ATELIER_GOAL_CLASSIFIER", "1") };
        let r = cfg_with_goal_and_remote(false, remote_classifier(false))
            .resolve_goal_classifier_enabled(false);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Env);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_planner_default_tracks_goal_enabled() {
        clear_goal_envs();
        assert!(
            !cfg_with_goal(false)
                .resolve_goal_planner_enabled(false)
                .value
        );
        let on = cfg_with_goal(true).resolve_goal_planner_enabled(true);
        assert!(on.value);
        assert_eq!(on.source, ConfigSource::Default);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_planner_remote_forces_either_way() {
        clear_goal_envs();
        let off = cfg_with_goal_and_remote(true, remote_planner(false))
            .resolve_goal_planner_enabled(true);
        assert!(!off.value);
        assert_eq!(off.source, ConfigSource::Remote);
        let on = cfg_with_goal_and_remote(false, remote_planner(true))
            .resolve_goal_planner_enabled(false);
        assert!(on.value);
        assert_eq!(on.source, ConfigSource::Remote);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_planner_env_overrides_default_and_remote() {
        clear_goal_envs();
        unsafe { std::env::set_var("ATELIER_GOAL_PLANNER", "0") };
        let r =
            cfg_with_goal_and_remote(true, remote_planner(true)).resolve_goal_planner_enabled(true);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::set_var("ATELIER_GOAL_PLANNER", "1") };
        let r = cfg_with_goal_and_remote(false, remote_planner(false))
            .resolve_goal_planner_enabled(false);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Env);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_summary_default_tracks_goal_enabled() {
        clear_goal_envs();
        assert!(
            !cfg_with_goal(false)
                .resolve_goal_summary_enabled(false)
                .value
        );
        let on = cfg_with_goal(true).resolve_goal_summary_enabled(true);
        assert!(on.value);
        assert_eq!(on.source, ConfigSource::Default);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_summary_remote_forces_either_way() {
        clear_goal_envs();
        let off = cfg_with_goal_and_remote(true, remote_summary(false))
            .resolve_goal_summary_enabled(true);
        assert!(!off.value);
        assert_eq!(off.source, ConfigSource::Remote);
        let on = cfg_with_goal_and_remote(false, remote_summary(true))
            .resolve_goal_summary_enabled(false);
        assert!(on.value);
        assert_eq!(on.source, ConfigSource::Remote);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_summary_env_overrides_default_and_remote() {
        clear_goal_envs();
        unsafe { std::env::set_var("ATELIER_GOAL_SUMMARY", "0") };
        let r =
            cfg_with_goal_and_remote(true, remote_summary(true)).resolve_goal_summary_enabled(true);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Env);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_classifier_config_honored_when_env_unset() {
        clear_goal_envs();
        let r = cfg_with_goal_config(GoalConfig {
            classifier_enabled: Some(true),
            ..Default::default()
        })
        .resolve_goal_classifier_enabled(false);
        assert_eq!(r.source, ConfigSource::Config);
        assert!(r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_classifier_env_beats_config() {
        clear_goal_envs();
        unsafe { std::env::set_var("ATELIER_GOAL_CLASSIFIER", "0") };
        let r = cfg_with_goal_config(GoalConfig {
            classifier_enabled: Some(true),
            ..Default::default()
        })
        .resolve_goal_classifier_enabled(false);
        assert_eq!(r.source, ConfigSource::Env);
        assert!(!r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_classifier_config_beats_remote() {
        clear_goal_envs();
        let r = cfg_with_goal_config_and_remote(
            GoalConfig {
                classifier_enabled: Some(true),
                ..Default::default()
            },
            remote_classifier(false),
        )
        .resolve_goal_classifier_enabled(false);
        assert_eq!(r.source, ConfigSource::Config);
        assert!(r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_classifier_config_beats_default() {
        clear_goal_envs();
        let r = cfg_with_goal_config(GoalConfig {
            enabled: Some(true),
            classifier_enabled: Some(false),
            ..Default::default()
        })
        .resolve_goal_classifier_enabled(false);
        assert_eq!(r.source, ConfigSource::Config);
        assert!(!r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_planner_config_honored_when_env_unset() {
        clear_goal_envs();
        let r = cfg_with_goal_config(GoalConfig {
            planner_enabled: Some(true),
            ..Default::default()
        })
        .resolve_goal_planner_enabled(false);
        assert_eq!(r.source, ConfigSource::Config);
        assert!(r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_planner_env_beats_config() {
        clear_goal_envs();
        unsafe { std::env::set_var("ATELIER_GOAL_PLANNER", "0") };
        let r = cfg_with_goal_config(GoalConfig {
            planner_enabled: Some(true),
            ..Default::default()
        })
        .resolve_goal_planner_enabled(false);
        assert_eq!(r.source, ConfigSource::Env);
        assert!(!r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_planner_config_beats_remote() {
        clear_goal_envs();
        let r = cfg_with_goal_config_and_remote(
            GoalConfig {
                planner_enabled: Some(true),
                ..Default::default()
            },
            remote_planner(false),
        )
        .resolve_goal_planner_enabled(false);
        assert_eq!(r.source, ConfigSource::Config);
        assert!(r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_planner_config_beats_default() {
        clear_goal_envs();
        let r = cfg_with_goal_config(GoalConfig {
            enabled: Some(true),
            planner_enabled: Some(false),
            ..Default::default()
        })
        .resolve_goal_planner_enabled(false);
        assert_eq!(r.source, ConfigSource::Config);
        assert!(!r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_summary_config_honored_when_env_unset() {
        clear_goal_envs();
        let r = cfg_with_goal_config(GoalConfig {
            summary_enabled: Some(true),
            ..Default::default()
        })
        .resolve_goal_summary_enabled(false);
        assert_eq!(r.source, ConfigSource::Config);
        assert!(r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_summary_env_beats_config() {
        clear_goal_envs();
        unsafe { std::env::set_var("ATELIER_GOAL_SUMMARY", "0") };
        let r = cfg_with_goal_config(GoalConfig {
            summary_enabled: Some(true),
            ..Default::default()
        })
        .resolve_goal_summary_enabled(false);
        assert_eq!(r.source, ConfigSource::Env);
        assert!(!r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_summary_config_beats_remote() {
        clear_goal_envs();
        let r = cfg_with_goal_config_and_remote(
            GoalConfig {
                summary_enabled: Some(true),
                ..Default::default()
            },
            remote_summary(false),
        )
        .resolve_goal_summary_enabled(false);
        assert_eq!(r.source, ConfigSource::Config);
        assert!(r.value);
        clear_goal_envs();
    }
    #[test]
    #[serial]
    fn resolve_goal_summary_config_beats_default() {
        clear_goal_envs();
        let r = cfg_with_goal_config(GoalConfig {
            enabled: Some(true),
            summary_enabled: Some(false),
            ..Default::default()
        })
        .resolve_goal_summary_enabled(false);
        assert_eq!(r.source, ConfigSource::Config);
        assert!(!r.value);
        clear_goal_envs();
    }
    #[test]
    fn goal_keys_round_trip_from_toml() {
        let raw: toml::Value = toml::from_str(
            r#"
[goal]
enabled = true
classifier_enabled = true
planner_enabled = false
summary_enabled = true
verifier_count = 4
classifier_max_runs = 7
strategist_every = 3
reverify_after = 6
"#,
        )
        .expect("test TOML should parse");
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        assert_eq!(cfg.goal.enabled, Some(true));
        assert_eq!(cfg.goal.classifier_enabled, Some(true));
        assert_eq!(cfg.goal.planner_enabled, Some(false));
        assert_eq!(cfg.goal.summary_enabled, Some(true));
        assert_eq!(cfg.goal.verifier_count, Some(4));
        assert_eq!(cfg.goal.classifier_max_runs, Some(7));
        assert_eq!(cfg.goal.strategist_every, Some(3));
        assert_eq!(cfg.goal.reverify_after, Some(6));
        let empty = Config::new_from_toml_cfg(&toml::from_str("").unwrap()).unwrap();
        assert_eq!(empty.goal.classifier_enabled, None);
        assert_eq!(empty.goal.verifier_count, None);
    }
    const GOAL_USE_CURRENT_ENV: &str = "ATELIER_GOAL_USE_CURRENT_MODEL_ONLY";
    fn clear_goal_model_env() {
        unsafe { std::env::remove_var(GOAL_USE_CURRENT_ENV) };
    }
    fn planner_pair() -> crate::util::config::GoalRoleModel {
        crate::util::config::GoalRoleModel {
            model: "atelier-4".to_string(),
            agent_type: "general-purpose".to_string(),
        }
    }
    fn strategist_pair() -> crate::util::config::GoalRoleModel {
        crate::util::config::GoalRoleModel {
            model: "atelier-4.5".to_string(),
            agent_type: "cursor".to_string(),
        }
    }
    #[test]
    #[serial]
    fn goal_use_current_model_only_env_true() {
        clear_goal_model_env();
        unsafe { std::env::set_var(GOAL_USE_CURRENT_ENV, "1") };
        let r = Config::default().resolve_goal_use_current_model_only();
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Env);
        clear_goal_model_env();
    }
    #[test]
    #[serial]
    fn goal_use_current_model_only_config_true() {
        clear_goal_model_env();
        let cfg = cfg_with_goal_config(GoalConfig {
            use_current_model_only: Some(true),
            ..Default::default()
        });
        let r = cfg.resolve_goal_use_current_model_only();
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Config);
        clear_goal_model_env();
    }
    #[test]
    #[serial]
    fn goal_use_current_model_only_default_false() {
        clear_goal_model_env();
        let r = Config::default().resolve_goal_use_current_model_only();
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Default);
        clear_goal_model_env();
    }
    #[test]
    #[serial]
    fn goal_use_current_model_only_env_overrides_config_false() {
        clear_goal_model_env();
        unsafe { std::env::set_var(GOAL_USE_CURRENT_ENV, "1") };
        let cfg = cfg_with_goal_config(GoalConfig {
            use_current_model_only: Some(false),
            ..Default::default()
        });
        let r = cfg.resolve_goal_use_current_model_only();
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Env);
        clear_goal_model_env();
    }
    fn remote_planner_model(
        p: crate::util::config::GoalRoleModel,
    ) -> crate::util::config::LocalRuntimeSettings {
        crate::util::config::LocalRuntimeSettings {
            goal_planner_model: Some(p),
            ..Default::default()
        }
    }
    fn remote_strategist_model(
        p: crate::util::config::GoalRoleModel,
    ) -> crate::util::config::LocalRuntimeSettings {
        crate::util::config::LocalRuntimeSettings {
            goal_strategist_model: Some(p),
            ..Default::default()
        }
    }
    #[test]
    fn resolve_goal_planner_model_kill_switch_inherits() {
        let cfg = cfg_with_goal_config_and_remote(
            GoalConfig::default(),
            remote_planner_model(planner_pair()),
        );
        let r = cfg.resolve_goal_planner_model(true);
        assert_eq!(r.value, GoalRoleModelChoice::InheritCurrent);
        assert_eq!(r.source, ConfigSource::Config);
    }
    #[test]
    fn resolve_goal_planner_model_remote_pair_explicit() {
        let cfg = cfg_with_goal_config_and_remote(
            GoalConfig::default(),
            remote_planner_model(planner_pair()),
        );
        let r = cfg.resolve_goal_planner_model(false);
        assert_eq!(r.value, GoalRoleModelChoice::Explicit(planner_pair()));
        assert_eq!(r.source, ConfigSource::Remote);
    }
    #[test]
    fn resolve_goal_planner_model_config_overrides_remote() {
        let cfg = cfg_with_goal_config_and_remote(
            GoalConfig {
                planner_model: Some(planner_pair()),
                ..Default::default()
            },
            remote_planner_model(strategist_pair()),
        );
        let r = cfg.resolve_goal_planner_model(false);
        assert_eq!(r.value, GoalRoleModelChoice::Explicit(planner_pair()));
        assert_eq!(r.source, ConfigSource::Config);
    }
    #[test]
    fn resolve_goal_planner_model_default_inherits() {
        let r = Config::default().resolve_goal_planner_model(false);
        assert_eq!(r.value, GoalRoleModelChoice::InheritCurrent);
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    fn resolve_goal_planner_model_remote_present_but_field_absent_inherits() {
        let cfg = cfg_with_goal_config_and_remote(
            GoalConfig::default(),
            remote_strategist_model(strategist_pair()),
        );
        let r = cfg.resolve_goal_planner_model(false);
        assert_eq!(r.value, GoalRoleModelChoice::InheritCurrent);
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    fn resolve_goal_strategist_model_remote_pair_explicit() {
        let cfg = cfg_with_goal_config_and_remote(
            GoalConfig::default(),
            remote_strategist_model(strategist_pair()),
        );
        let r = cfg.resolve_goal_strategist_model(false);
        assert_eq!(r.value, GoalRoleModelChoice::Explicit(strategist_pair()));
        assert_eq!(r.source, ConfigSource::Remote);
    }
    #[test]
    fn resolve_goal_strategist_model_config_overrides_remote() {
        let cfg = cfg_with_goal_config_and_remote(
            GoalConfig {
                strategist_model: Some(strategist_pair()),
                ..Default::default()
            },
            remote_strategist_model(planner_pair()),
        );
        let r = cfg.resolve_goal_strategist_model(false);
        assert_eq!(r.value, GoalRoleModelChoice::Explicit(strategist_pair()));
        assert_eq!(r.source, ConfigSource::Config);
    }
    #[test]
    fn resolve_goal_skeptic_models_kill_switch_inherits() {
        let cfg = cfg_with_goal_config(GoalConfig {
            skeptic_models: vec![planner_pair(), strategist_pair()],
            ..Default::default()
        });
        let r = cfg.resolve_goal_skeptic_models(true);
        assert!(r.value.is_empty(), "kill-switch ⇒ all skeptics inherit");
        assert_eq!(r.source, ConfigSource::Config);
    }
    #[test]
    fn resolve_goal_skeptic_models_remote_pool_explicit() {
        let remote = crate::util::config::LocalRuntimeSettings {
            goal_skeptic_models: vec![planner_pair(), strategist_pair()],
            ..Default::default()
        };
        let r = cfg_with_goal_config_and_remote(GoalConfig::default(), remote)
            .resolve_goal_skeptic_models(false);
        assert_eq!(
            r.value,
            vec![
                GoalRoleModelChoice::Explicit(planner_pair()),
                GoalRoleModelChoice::Explicit(strategist_pair()),
            ]
        );
        assert_eq!(r.source, ConfigSource::Remote);
    }
    #[test]
    fn resolve_goal_skeptic_models_config_pool_overrides_remote_pool() {
        let remote = crate::util::config::LocalRuntimeSettings {
            goal_skeptic_models: vec![strategist_pair(), strategist_pair()],
            ..Default::default()
        };
        let cfg = cfg_with_goal_config_and_remote(
            GoalConfig {
                skeptic_models: vec![planner_pair(), strategist_pair()],
                ..Default::default()
            },
            remote,
        );
        let r = cfg.resolve_goal_skeptic_models(false);
        assert_eq!(
            r.value,
            vec![
                GoalRoleModelChoice::Explicit(planner_pair()),
                GoalRoleModelChoice::Explicit(strategist_pair()),
            ]
        );
        assert_eq!(r.source, ConfigSource::Config);
    }
    #[test]
    fn resolve_goal_skeptic_models_no_pool_inherits() {
        let r = Config::default().resolve_goal_skeptic_models(false);
        assert!(r.value.is_empty());
        assert_eq!(r.source, ConfigSource::Default);
    }
    /// `[goal]` model pins parse from both the inline-table and `[[...]]` array forms.
    #[test]
    fn goal_model_pins_parse_from_toml() {
        let toml_str = r#"
[goal]
enabled = true
planner_model = { model = "atelier-build", agent_type = "atelier-build-plan" }

[goal.strategist_model]
model = "atelier-composer-2.5-fast"
agent_type = "cursor"

[[goal.skeptic_models]]
model = "atelier-build"
agent_type = "atelier-build-plan"

[[goal.skeptic_models]]
model = "atelier-composer-2.5-fast"
agent_type = "cursor"
"#;
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).unwrap();
        assert_eq!(
            cfg.goal.planner_model.as_ref().unwrap().model,
            "atelier-build"
        );
        assert_eq!(
            cfg.goal.strategist_model.as_ref().unwrap().agent_type,
            "cursor"
        );
        assert_eq!(cfg.goal.skeptic_models.len(), 2);
        assert_eq!(cfg.goal.skeptic_models[0].model, "atelier-build");
        assert_eq!(
            cfg.resolve_goal_planner_model(false).source,
            ConfigSource::Config
        );
    }
    /// A malformed pin must drop to `None`, not fail the whole parse (which
    /// would silently wipe every other setting).
    #[test]
    fn goal_model_pin_malformed_is_dropped_not_fatal() {
        let toml_str = r#"
[goal]
enabled = true
classifier_max_runs = 6
planner_model = { agent_type = "atelier-build-plan" }
"#;
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        let cfg = Config::new_from_toml_cfg(&raw)
            .expect("malformed planner_model must not fail the whole parse");
        assert!(cfg.goal.planner_model.is_none());
        assert_eq!(cfg.goal.classifier_max_runs, Some(6));
    }
    #[test]
    fn goal_skeptic_models_drop_malformed_entry_keep_rest() {
        let toml_str = r#"
[goal]
enabled = true

[[goal.skeptic_models]]
model = "atelier-build"
agent_type = "atelier-build-plan"

[[goal.skeptic_models]]
agent_type = "cursor"

[[goal.skeptic_models]]
model = "atelier-composer-2.5-fast"
agent_type = "cursor"
"#;
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).unwrap();
        assert_eq!(cfg.goal.skeptic_models.len(), 2);
        assert_eq!(cfg.goal.skeptic_models[0].model, "atelier-build");
        assert_eq!(
            cfg.goal.skeptic_models[1].model,
            "atelier-composer-2.5-fast"
        );
    }
    /// Acceptance test: a full managed-config `[goal]` block resolves end-to-end,
    /// every value sourced from config (not remote/default).
    #[test]
    #[serial]
    fn full_goal_managed_config_resolves_end_to_end() {
        clear_goal_envs();
        clear_goal_model_env();
        let raw: toml::Value = toml::from_str(
            r#"
[goal]
enabled = true
classifier_enabled = true
planner_enabled = true
verifier_count = 3
classifier_max_runs = 6
planner_model = { model = "atelier-build", agent_type = "atelier-build-plan" }
strategist_model = { model = "atelier-composer-2.5-fast", agent_type = "cursor" }

[[goal.skeptic_models]]
model = "atelier-build"
agent_type = "atelier-build-plan"

[[goal.skeptic_models]]
model = "atelier-composer-2.5-fast"
agent_type = "cursor"
"#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("[goal] config must parse");
        let atelier_build = crate::util::config::GoalRoleModel {
            model: "atelier-build".into(),
            agent_type: "atelier-build-plan".into(),
        };
        let composer = crate::util::config::GoalRoleModel {
            model: "atelier-composer-2.5-fast".into(),
            agent_type: "cursor".into(),
        };
        let goal_enabled = cfg.resolve_goal().value;
        assert!(goal_enabled);
        assert!(cfg.resolve_goal_classifier_enabled(goal_enabled).value);
        assert!(cfg.resolve_goal_planner_enabled(goal_enabled).value);
        assert_eq!(cfg.resolve_goal_verifier_count().value, 3);
        assert_eq!(cfg.resolve_goal_classifier_max_runs().value, 6);
        let use_current = cfg.resolve_goal_use_current_model_only().value;
        assert!(!use_current);
        let planner = cfg.resolve_goal_planner_model(use_current);
        assert_eq!(
            planner.value,
            GoalRoleModelChoice::Explicit(atelier_build.clone())
        );
        assert_eq!(planner.source, ConfigSource::Config);
        assert_eq!(
            cfg.resolve_goal_strategist_model(use_current).value,
            GoalRoleModelChoice::Explicit(composer.clone())
        );
        assert_eq!(
            cfg.resolve_goal_skeptic_models(use_current).value,
            vec![
                GoalRoleModelChoice::Explicit(atelier_build),
                GoalRoleModelChoice::Explicit(composer),
            ]
        );
        clear_goal_envs();
        clear_goal_model_env();
    }
    /// Run the production scan (`deserialize_collecting_unrecognized`) on a
    /// TOML string, mirroring the [model] removal + default-merge in
    /// `new_from_toml_cfg`.
    fn unused_keys_from_toml(toml_str: &str) -> Vec<String> {
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        let raw_without_models = {
            let mut r = raw.clone();
            if let toml::Value::Table(ref mut t) = r {
                t.remove("model");
            }
            r
        };
        let mut base = toml::Value::try_from(Config::default()).unwrap();
        if let toml::Value::Table(ref mut t) = base {
            t.remove("model");
        }
        crate::config::deep_merge_toml(&mut base, &raw_without_models);
        let (_config, unused) =
            Config::deserialize_collecting_unrecognized(base, &raw_without_models)
                .expect("config should deserialize");
        unused
    }
    #[test]
    fn config_warns_on_section_typo() {
        let raw: toml::Value = toml::from_str(
            r#"
            [endpoint]
            deployment_key = "xai-token-test"
        "#,
        )
        .unwrap();
        let config = Config::new_from_toml_cfg(&raw).expect("should parse");
        assert!(config.endpoints.deployment_key.is_none());
        let unused = unused_keys_from_toml(
            r#"
            [endpoint]
            deployment_key = "xai-token-test"
        "#,
        );
        assert!(unused.iter().any(|k| k == "endpoint"), "got: {unused:?}");
    }
    #[test]
    fn config_warns_on_field_typos() {
        let unused = unused_keys_from_toml(
            r#"
            [endpoints]
            deplomyent_key = "test"
            [ui]
            yoloo = true
            [features]
            telmetry = true
        "#,
        );
        assert!(
            unused.iter().any(|k| k == "endpoints.deplomyent_key"),
            "got: {unused:?}"
        );
        assert!(unused.iter().any(|k| k == "ui.yoloo"), "got: {unused:?}");
        assert!(
            unused.iter().any(|k| k == "features.telmetry"),
            "got: {unused:?}"
        );
    }
    #[test]
    fn config_accepts_all_known_sections() {
        let unused = unused_keys_from_toml(
            r#"
            disabled_mcp_servers = ["old-server"]
            [cli]
            auto_update = false
            [features]
            feedback = true
            [endpoints]
            deployment_key = "test"
            management_api_key = "mgmt-key"
            [models]
            default = "atelier-3"
            [ui]
            yolo = true
            theme = "dark"
            approval_mode = "ask"
            [session]
            auto_compact_threshold_percent = 85
            [telemetry]
            enabled = true
            [agent]
            name = "custom"
            [skills]
            paths = ["~/skills"]
            [plugins]
            paths = ["~/plugins"]
            [subagents]
            enabled = true
            [memory]
            enabled = true
            [compaction]
            [compaction.pruning]
            enabled = true
            [harness]
            block_for_upload = true
            [relay]
            enabled = false
            [remote]
            secret = "value"
            [worktree_pool]
            pool_size = 4
            [managed_mcps]
            enabled = true
            [mcp_servers.test]
            url = "https://mcp.test.com"
            [toolset.bash]
            timeout_secs = 120
            persistent_shell = true
            [shortcuts]
            ctrl_k = "search"
            [atelier_com_config]
            token_header = "test"
            [auth.oidc]
            issuer = "https://sso.corp.com"
            client_id = "abc123"
            [storage]
            cleanup_ttl_days = 7
            [[marketplace.sources]]
            name = "Local Dev"
            path = "/tmp/plugins"
            [permission]
            [[permission.rules]]
            action = "allow"
            tool = "bash"
            [tools]
            respect_gitignore = false
            [desktop]
            some_key = "value"
        "#,
        );
        assert!(
            unused.is_empty(),
            "false positive on valid config: {unused:?}"
        );
    }
    #[test]
    fn removed_remote_telemetry_keys_are_rejected() {
        let unused = unused_keys_from_toml(
            r#"
            [telemetry]
            enabled = true
            trace_upload = true
        "#,
        );
        assert_eq!(unused, vec!["telemetry.trace_upload".to_string()]);
    }
    #[test]
    fn removed_global_vendor_endpoint_key_is_rejected() {
        let removed_key = ["xai", "_api_base_url"].concat();
        let config =
            format!("[endpoints]\n{removed_key} = \"https://removed-vendor.invalid/v1\"\n");
        let unused = unused_keys_from_toml(&config);
        assert_eq!(unused, vec![format!("endpoints.{removed_key}")]);
    }
    #[test]
    fn config_accepts_compact_permission_section() {
        let unused = unused_keys_from_toml(
            r#"
            [permission]
            allow = ["Read(//tmp/**)"]
            deny = ["Bash(rm *)"]
            ask = ["WebFetch"]
        "#,
        );
        assert!(
            unused.is_empty(),
            "false positive on [permission] keys: {unused:?}"
        );
    }
    /// `prompt_policy` is not consumed from any TOML permission section (the
    /// verbose loader keeps only `rules`; prompt policy comes from .claude
    /// settings `defaultMode`), so it must warn rather than be a silent no-op.
    #[test]
    fn permission_prompt_policy_warns_as_unconsumed() {
        let unused = unused_keys_from_toml(
            r#"
            [permission]
            deny = ["Bash(rm *)"]
            prompt_policy = "deny"
        "#,
        );
        assert_eq!(
            unused,
            vec!["permission.prompt_policy".to_string()],
            "an unconsumed key in a security section must be flagged"
        );
    }
    /// A typo'd `[permission]` sub-key must still warn — silently dropping a
    /// misspelled security rule would leave the user believing it's in force.
    #[test]
    fn permission_unknown_subkey_still_warns() {
        let unused = unused_keys_from_toml(
            r#"
            [permission]
            denny = ["Bash(rm *)"]
            ask = ["WebFetch"]
        "#,
        );
        assert_eq!(
            unused,
            vec!["permission.denny".to_string()],
            "exactly the typo'd sub-key must be flagged"
        );
    }
    /// Permission *values* are opaque: a malformed `[[permission.rules]]`
    /// entry neither warns nor fails Config load — the out-of-band loaders
    /// parse it tolerantly and warn per item.
    #[test]
    fn malformed_permission_rules_do_not_fail_config_load() {
        let toml_str = r#"
            [[permission.rules]]
            pattern = 5
        "#;
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        Config::new_from_toml_cfg(&raw)
            .expect("malformed rule values are the permission loaders' concern");
        let unused = unused_keys_from_toml(toml_str);
        assert!(unused.is_empty(), "got: {unused:?}");
    }
    /// A non-table `[permission]` value still fails Config load (pre-existing
    /// behavior): a fundamentally broken security section should be loud.
    #[test]
    fn non_table_permission_value_fails_config_load() {
        let raw: toml::Value = toml::from_str(r#"permission = "foo""#).unwrap();
        assert!(
            Config::new_from_toml_cfg(&raw).is_err(),
            "non-table [permission] must fail loudly"
        );
    }
    /// Wrong-typed values for the opaque passthrough keys must neither warn
    /// nor fail config load — an admin typo in a managed layer must not brick
    /// startup fleet-wide; the out-of-band consumers degrade gracefully.
    #[test]
    fn wrong_typed_passthrough_values_neither_warn_nor_fail() {
        let toml_str = r#"
            [marketplace]
            official_marketplace_auto_installed = "yes"
        "#;
        let unused = unused_keys_from_toml(toml_str);
        assert!(unused.is_empty(), "got: {unused:?}");
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        Config::new_from_toml_cfg(&raw)
            .expect("wrong-typed passthrough values must not fail config load");
    }
    /// Exempting `[permission]` and friends must not swallow warnings for
    /// genuinely unknown keys.
    #[test]
    fn unknown_key_still_warns_next_to_exempt_sections() {
        let unused = unused_keys_from_toml(
            r#"
            [permission]
            deny = ["Bash(rm *)"]
            [marketplace]
            official_marketplace_auto_installed = true
            [ui]
            yollo = true
        "#,
        );
        assert_eq!(
            unused,
            vec!["ui.yollo".to_string()],
            "exactly the typo'd key must be flagged"
        );
    }
    fn empty_config() -> toml::Value {
        toml::Value::Table(toml::map::Map::new())
    }
    fn clear_runtime_env_vars() {
        unsafe {
            std::env::remove_var("ATELIER_SUBAGENTS");
            std::env::remove_var("ATELIER_RESPECT_GITIGNORE");
            std::env::remove_var("ATELIER_WEB_SEARCH_MODEL");
            std::env::remove_var("ATELIER_SESSION_SUMMARY_MODEL");
            std::env::remove_var("ATELIER_CURSOR_SKILLS_ENABLED");
            std::env::remove_var("ATELIER_CURSOR_RULES_ENABLED");
            std::env::remove_var("ATELIER_CURSOR_AGENTS_ENABLED");
            std::env::remove_var("ATELIER_CLAUDE_SKILLS_ENABLED");
            std::env::remove_var("ATELIER_CLAUDE_RULES_ENABLED");
            std::env::remove_var("ATELIER_CLAUDE_AGENTS_ENABLED");
        }
    }
    fn clear_managed_mcp_env_vars() {
        unsafe {
            std::env::remove_var("ATELIER_MANAGED_MCPS_ENABLED");
            std::env::remove_var("ATELIER_MANAGED_MCP_GATEWAY_TOOLS_ENABLED");
        }
    }
    fn isolate_compat_env() -> Vec<EnvGuard> {
        COMPAT_CELLS
            .into_iter()
            .map(|cell| EnvGuard::unset(cell.env_var()))
            .collect()
    }
    fn parse_compat(source: &str) -> CompatConfigToml {
        let raw: toml::Value = toml::from_str(source).unwrap();
        raw.get("compat").unwrap().clone().try_into().unwrap()
    }
    fn assert_session_one_disabled(config: CompatConfig, expected: CompatVendor) {
        for cell in COMPAT_CELLS {
            if cell.surface() == CompatSurface::Sessions {
                assert_eq!(
                    config.value(cell),
                    cell.vendor() != expected,
                    "{}.sessions",
                    cell.vendor().as_str()
                );
            }
        }
    }
    fn local_runtime_settings_with(
        key: CompatRemoteKey,
        value: bool,
    ) -> crate::util::config::LocalRuntimeSettings {
        let mut remote = crate::util::config::LocalRuntimeSettings::default();
        match key {
            CompatRemoteKey::CursorSkills => remote.cursor_skills_enabled = Some(value),
            CompatRemoteKey::CursorRules => remote.cursor_rules_enabled = Some(value),
            CompatRemoteKey::CursorAgents => remote.cursor_agents_enabled = Some(value),
            CompatRemoteKey::CursorMcps => remote.cursor_mcps_enabled = Some(value),
            CompatRemoteKey::CursorHooks => remote.cursor_hooks_enabled = Some(value),
            CompatRemoteKey::CursorSessions => {
                remote.cursor_sessions_enabled = Some(value);
            }
            CompatRemoteKey::ClaudeSkills => remote.claude_skills_enabled = Some(value),
            CompatRemoteKey::ClaudeRules => remote.claude_rules_enabled = Some(value),
            CompatRemoteKey::ClaudeAgents => remote.claude_agents_enabled = Some(value),
            CompatRemoteKey::ClaudeMcps => remote.claude_mcps_enabled = Some(value),
            CompatRemoteKey::ClaudeHooks => remote.claude_hooks_enabled = Some(value),
            CompatRemoteKey::ClaudeSessions => {
                remote.claude_sessions_enabled = Some(value);
            }
            CompatRemoteKey::CodexSessions => remote.codex_sessions_enabled = Some(value),
        }
        remote
    }
    #[test]
    #[serial]
    fn resolve_compat_defaults_match_registry() {
        let _env = isolate_compat_env();
        assert_eq!(
            resolve_compat_config(&CompatConfigToml::default(), None),
            CompatConfig::default()
        );
    }
    #[test]
    #[serial]
    fn resolve_compat_toml_sessions_disable_independently() {
        let _env = isolate_compat_env();
        for (vendor, section) in [
            (CompatVendor::Cursor, "cursor"),
            (CompatVendor::Claude, "claude"),
            (CompatVendor::Codex, "codex"),
        ] {
            let config = parse_compat(&format!("[compat.{section}]\nsessions = false"));
            assert_session_one_disabled(resolve_compat_config(&config, None), vendor);
        }
    }
    #[test]
    #[serial]
    fn resolve_raw_compat_sessions_fails_closed_per_vendor() {
        let _env = isolate_compat_env();
        let raw: toml::Value = toml::from_str(
            r#"
[compat.cursor]
sessions = "malformed"
[compat.claude]
sessions = false
[compat.codex]
hooks = "unrelated malformed field"
"#,
        )
        .unwrap();
        let resolved = resolve_compat_sessions_from_raw(Ok(&raw), None);
        assert!(!resolved.cursor.sessions);
        assert!(!resolved.claude.sessions);
        assert!(resolved.codex.sessions);
    }
    #[test]
    #[serial]
    fn resolve_raw_compat_sessions_keeps_absent_and_valid_cells_independent() {
        let _env = isolate_compat_env();
        let raw: toml::Value = toml::from_str(
            r#"
[compat.cursor]
sessions = false
hooks = "malformed but irrelevant"
[compat.claude]
sessions = true
"#,
        )
        .unwrap();
        let remote = crate::util::config::LocalRuntimeSettings {
            codex_sessions_enabled: Some(false),
            ..Default::default()
        };
        let resolved = resolve_compat_sessions_from_raw(Ok(&raw), Some(&remote));
        assert!(!resolved.cursor.sessions);
        assert!(resolved.claude.sessions);
        assert!(!resolved.codex.sessions);
    }
    #[test]
    fn compat_config_cell_is_tolerant_and_fail_closed_per_cell() {
        let raw: toml::Value = toml::from_str(
            r#"
[compat.cursor]
skills = false
rules = "malformed"
[compat.claude]
hooks = true
"#,
        )
        .unwrap();
        let cell = |vendor, surface| {
            COMPAT_CELLS
                .into_iter()
                .find(|cell| cell.vendor() == vendor && cell.surface() == surface)
                .unwrap()
        };
        assert_eq!(
            compat_config_cell(Ok(&raw), cell(CompatVendor::Cursor, CompatSurface::Skills)),
            Ok(Some(false))
        );
        assert_eq!(
            compat_config_cell(Ok(&raw), cell(CompatVendor::Cursor, CompatSurface::Rules)),
            Err(CompatConfigCellError::Malformed)
        );
        assert_eq!(
            compat_config_cell(Ok(&raw), cell(CompatVendor::Claude, CompatSurface::Hooks)),
            Ok(Some(true))
        );
        assert_eq!(
            compat_config_cell(Ok(&raw), cell(CompatVendor::Codex, CompatSurface::Sessions)),
            Ok(None)
        );
        assert_eq!(
            compat_config_cell(Err(()), cell(CompatVendor::Claude, CompatSurface::Sessions)),
            Err(CompatConfigCellError::Unavailable)
        );
    }
    #[test]
    #[serial]
    fn resolve_raw_compat_sessions_load_failure_fails_closed() {
        let _env = isolate_compat_env();
        let resolved = resolve_compat_sessions_from_raw(Err(()), None);
        assert!(!resolved.cursor.sessions);
        assert!(!resolved.claude.sessions);
        assert!(!resolved.codex.sessions);
    }
    #[test]
    #[serial]
    fn resolve_raw_compat_sessions_load_failure_allows_env_override() {
        let _env = isolate_compat_env();
        let _codex = EnvGuard::set("ATELIER_CODEX_SESSIONS_ENABLED", "true");
        let resolved = resolve_compat_sessions_from_raw(Err(()), None);
        assert!(!resolved.cursor.sessions);
        assert!(!resolved.claude.sessions);
        assert!(resolved.codex.sessions);
    }
    #[test]
    #[serial]
    fn resolve_raw_compat_sessions_valid_empty_uses_remote_and_defaults() {
        let _env = isolate_compat_env();
        let raw = toml::Value::Table(Default::default());
        let remote = crate::util::config::LocalRuntimeSettings {
            claude_sessions_enabled: Some(false),
            ..Default::default()
        };
        let resolved = resolve_compat_sessions_from_raw(Ok(&raw), Some(&remote));
        assert!(resolved.cursor.sessions);
        assert!(!resolved.claude.sessions);
        assert!(resolved.codex.sessions);
    }
    #[test]
    #[serial]
    fn remote_keys_are_one_hot_and_false_overrides_default() {
        let _env = isolate_compat_env();
        for key in COMPAT_CELLS
            .into_iter()
            .filter_map(|cell| cell.remote_key())
        {
            let remote = local_runtime_settings_with(key, false);
            for cell in COMPAT_CELLS {
                assert_eq!(
                    remote_compat_value(Some(&remote), cell.remote_key()),
                    (cell.remote_key() == Some(key)).then_some(false),
                    "{key:?} mapped to {}.{}",
                    cell.vendor().as_str(),
                    cell.surface().as_str()
                );
            }
        }
        let remote = local_runtime_settings_with(CompatRemoteKey::CursorSkills, false);
        assert!(CompatConfig::default().cursor.skills);
        assert!(
            !resolve_compat_config(&CompatConfigToml::default(), Some(&remote))
                .cursor
                .skills
        );
    }
    #[test]
    #[serial]
    fn resolve_compat_env_sessions_disable_independently() {
        let _env = isolate_compat_env();
        for (vendor, env_var) in [
            (CompatVendor::Cursor, "ATELIER_CURSOR_SESSIONS_ENABLED"),
            (CompatVendor::Claude, "ATELIER_CLAUDE_SESSIONS_ENABLED"),
            (CompatVendor::Codex, "ATELIER_CODEX_SESSIONS_ENABLED"),
        ] {
            let _disabled = EnvGuard::set(env_var, "false");
            assert_session_one_disabled(
                resolve_compat_config(&CompatConfigToml::default(), None),
                vendor,
            );
        }
    }
    #[test]
    #[serial]
    fn resolve_compat_precedence_and_reserved_codex_hook() {
        let _env = isolate_compat_env();
        let config =
            parse_compat("[compat.cursor]\nsessions = false\n[compat.codex]\nhooks = false");
        let remote = crate::util::config::LocalRuntimeSettings {
            cursor_sessions_enabled: Some(true),
            ..Default::default()
        };
        let resolved = resolve_compat_config(&config, Some(&remote));
        assert!(!resolved.cursor.sessions);
        assert!(!resolved.codex.hooks);
        assert!(resolved.cursor.hooks);
        assert!(resolved.claude.hooks);
        let _session = EnvGuard::set("ATELIER_CURSOR_SESSIONS_ENABLED", "true");
        let _hook = EnvGuard::set("ATELIER_CODEX_HOOKS_ENABLED", "true");
        let resolved = resolve_compat_config(&config, Some(&remote));
        assert!(resolved.cursor.sessions);
        assert!(resolved.codex.hooks);
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_compat_asymmetric_sources() {
        let _env = isolate_compat_env();
        let _cursor = EnvGuard::set("ATELIER_CURSOR_SESSIONS_ENABLED", "false");
        let raw: toml::Value =
            toml::from_str("[compat.cursor]\nsessions = true\n[compat.claude]\nsessions = false")
                .unwrap();
        let remote = crate::util::config::LocalRuntimeSettings {
            cursor_sessions_enabled: Some(true),
            claude_sessions_enabled: Some(true),
            codex_sessions_enabled: Some(false),
            ..Default::default()
        };
        let mut config = Config::new_from_toml_cfg(&raw).unwrap();
        config.resolve_runtime_fields(&RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: Some(&remote),
            cwd: None,
            is_headless: false,
            cli_subagents: None,
            cli_web_search_model: None,
            cli_session_summary_model: None,
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        });
        assert!(!config.compat_resolved.cursor.sessions);
        assert!(!config.compat_resolved.claude.sessions);
        assert!(!config.compat_resolved.codex.sessions);
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_interactive_defaults() {
        clear_runtime_env_vars();
        clear_managed_mcp_env_vars();
        let raw = empty_config();
        let mut cfg = Config::new_from_toml_cfg(&raw).unwrap();
        cfg.resolve_runtime_fields(&RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: None,
            cwd: None,
            is_headless: false,
            cli_subagents: None,
            cli_web_search_model: None,
            cli_session_summary_model: None,
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        });
        assert!(cfg.subagents_enabled);
        assert!(!cfg.respect_gitignore);
        assert!(cfg.managed_mcps_enabled);
        assert!(!cfg.managed_mcp_gateway_tools_enabled);
        assert_eq!(
            cfg.web_search_model,
            crate::models::default_web_search_model()
        );
        assert_eq!(
            cfg.session_summary_model,
            Some(crate::models::default_session_summary_model().to_owned())
        );
        assert!(!cfg.path_not_found_hints);
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_headless_defaults() {
        clear_runtime_env_vars();
        clear_managed_mcp_env_vars();
        let raw = empty_config();
        let mut cfg = Config::new_from_toml_cfg(&raw).unwrap();
        cfg.resolve_runtime_fields(&RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: None,
            cwd: None,
            is_headless: true,
            cli_subagents: None,
            cli_web_search_model: None,
            cli_session_summary_model: None,
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        });
        assert!(
            !cfg.managed_mcps_enabled,
            "headless should default managed_mcps to false"
        );
        assert!(!cfg.managed_mcp_gateway_tools_enabled);
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_managed_gateway_tools_from_remote() {
        clear_runtime_env_vars();
        clear_managed_mcp_env_vars();
        let raw = empty_config();
        let remote = crate::util::config::LocalRuntimeSettings {
            managed_mcp_gateway_tools_enabled: Some(true),
            ..Default::default()
        };
        let mut cfg = Config::new_from_toml_cfg(&raw).unwrap();
        cfg.resolve_runtime_fields(&RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: Some(&remote),
            cwd: None,
            is_headless: false,
            cli_subagents: None,
            cli_web_search_model: None,
            cli_session_summary_model: None,
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        });
        assert!(cfg.managed_mcp_gateway_tools_enabled);
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_subagents_from_config() {
        clear_runtime_env_vars();
        let raw: toml::Value = toml::from_str("[subagents]\nenabled = true").unwrap();
        let mut cfg = Config::new_from_toml_cfg(&raw).unwrap();
        cfg.resolve_runtime_fields(&RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: None,
            cwd: None,
            is_headless: false,
            cli_subagents: None,
            cli_web_search_model: None,
            cli_session_summary_model: None,
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        });
        assert!(cfg.subagents_enabled);
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_cli_subagents_override() {
        clear_runtime_env_vars();
        let raw = empty_config();
        let mut cfg = Config::new_from_toml_cfg(&raw).unwrap();
        cfg.resolve_runtime_fields(&RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: None,
            cwd: None,
            is_headless: false,
            cli_subagents: Some(true),
            cli_web_search_model: None,
            cli_session_summary_model: None,
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        });
        assert!(cfg.subagents_enabled);
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_gitignore_from_env() {
        clear_runtime_env_vars();
        unsafe { std::env::set_var("ATELIER_RESPECT_GITIGNORE", "0") };
        let raw = empty_config();
        let mut cfg = Config::new_from_toml_cfg(&raw).unwrap();
        cfg.resolve_runtime_fields(&RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: None,
            cwd: None,
            is_headless: false,
            cli_subagents: None,
            cli_web_search_model: None,
            cli_session_summary_model: None,
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        });
        assert!(!cfg.respect_gitignore);
        clear_runtime_env_vars();
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_model_overrides_from_cli() {
        clear_runtime_env_vars();
        let raw = empty_config();
        let mut cfg = Config::new_from_toml_cfg(&raw).unwrap();
        cfg.resolve_runtime_fields(&RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: None,
            cwd: None,
            is_headless: false,
            cli_subagents: None,
            cli_web_search_model: Some("custom-ws"),
            cli_session_summary_model: Some("custom-ss"),
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        });
        assert_eq!(cfg.web_search_model, "custom-ws");
        assert_eq!(cfg.session_summary_model, Some("custom-ss".to_owned()));
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_path_hints_from_remote() {
        clear_runtime_env_vars();
        let raw = empty_config();
        let remote = crate::util::config::LocalRuntimeSettings {
            path_not_found_hints: Some(true),
            ..Default::default()
        };
        let mut cfg = Config::new_from_toml_cfg(&raw).unwrap();
        cfg.resolve_runtime_fields(&RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: Some(&remote),
            cwd: None,
            is_headless: false,
            cli_subagents: None,
            cli_web_search_model: None,
            cli_session_summary_model: None,
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        });
        assert!(cfg.path_not_found_hints);
    }
    #[test]
    #[serial]
    fn resolve_runtime_fields_idempotent() {
        clear_runtime_env_vars();
        let raw: toml::Value = toml::from_str("[subagents]\nenabled = true").unwrap();
        let mut cfg = Config::new_from_toml_cfg(&raw).unwrap();
        let ctx = RuntimeResolutionContext {
            raw_config: &raw,
            local_runtime_settings: None,
            cwd: None,
            is_headless: false,
            cli_subagents: None,
            cli_web_search_model: None,
            cli_session_summary_model: None,
            cli_experimental_memory: false,
            cli_no_memory: false,
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            storage_mode: None,
        };
        cfg.resolve_runtime_fields(&ctx);
        let first_subagents = cfg.subagents_enabled;
        let first_gitignore = cfg.respect_gitignore;
        let first_mcps = cfg.managed_mcps_enabled;
        let first_ws = cfg.web_search_model.clone();
        cfg.resolve_runtime_fields(&ctx);
        assert_eq!(cfg.subagents_enabled, first_subagents);
        assert_eq!(cfg.respect_gitignore, first_gitignore);
        assert_eq!(cfg.managed_mcps_enabled, first_mcps);
        assert_eq!(cfg.web_search_model, first_ws);
    }
    #[test]
    fn telemetry_mode_toml_roundtrip() {
        let cfg: Features = toml::from_str("telemetry = true").unwrap();
        assert_eq!(cfg.telemetry, Some(TelemetryMode::Enabled));
        let cfg: Features = toml::from_str("telemetry = false").unwrap();
        assert_eq!(cfg.telemetry, Some(TelemetryMode::Disabled));
        let cfg: Features = toml::from_str(r#"telemetry = "session_metrics""#).unwrap();
        assert_eq!(cfg.telemetry, Some(TelemetryMode::SessionMetrics));
        let cfg: Features =
            toml::from_str(r#"telemetry = "metrics_v3""#).expect("unknown string must not error");
        assert_eq!(cfg.telemetry, Some(TelemetryMode::Disabled));
        assert!(toml::from_str::<Features>("telemetry = 42").is_err());
    }
    #[test]
    fn telemetry_enabled_from_toml_recognizes_modes() {
        let on: toml::Value = toml::from_str("[features]\ntelemetry = true\n").unwrap();
        assert_eq!(telemetry_enabled_from_toml(&on), Some(true));
        let session: toml::Value = toml::from_str(
            r#"[features]
telemetry = "session_metrics"
"#,
        )
        .unwrap();
        assert_eq!(telemetry_enabled_from_toml(&session), Some(true));
        let unknown: toml::Value = toml::from_str(
            r#"[features]
telemetry = "garbage"
"#,
        )
        .unwrap();
        assert_eq!(telemetry_enabled_from_toml(&unknown), None);
    }
    #[test]
    #[serial]
    fn is_telemetry_explicitly_disabled_sync_env_signals() {
        unsafe { std::env::set_var("ATELIER_TELEMETRY_ENABLED", "0") };
        unsafe { std::env::remove_var("DISABLE_TELEMETRY") };
        assert!(is_telemetry_explicitly_disabled_sync());
        unsafe { std::env::set_var("ATELIER_TELEMETRY_ENABLED", "1") };
        assert!(!is_telemetry_explicitly_disabled_sync());
        unsafe { std::env::remove_var("ATELIER_TELEMETRY_ENABLED") };
        unsafe { std::env::set_var("DISABLE_TELEMETRY", "1") };
        assert!(is_telemetry_explicitly_disabled_sync());
        unsafe { std::env::remove_var("DISABLE_TELEMETRY") };
    }
    #[test]
    fn version_overrides_apply_into_typed_config() {
        let mut value: toml::Value = toml::from_str(
            r#"
[models]
default = "atelier-build"

[[version_overrides]]
minimum_version = "1.8.0"
[version_overrides.models]
default = "atelier-4.5"
"#,
        )
        .unwrap();
        let v = semver::Version::parse("1.8.0").unwrap();
        atelier_config::apply_version_overrides(&mut value, &v).unwrap();
        let cfg = Config::new_from_toml_cfg(&value).unwrap();
        assert_eq!(cfg.models.default.as_deref(), Some("atelier-4.5"));
    }
    /// Reproduce the enterprise managed config bug: [model.atelier-build] sets
    /// context_window=500k for model="atelier-4.5", but
    /// [models].default="atelier-4.5" resolves to the bare
    /// prefetched entry (256k) because Layer 3 only overrides key
    /// "atelier-build", not key "atelier-4.5".
    ///
    /// After the Layer 4 slug propagation fix, both keys should have 500k.
    #[test]
    fn slug_propagation_enterprise_managed_config_key_mismatch() {
        let default_cw = DEFAULT_CONTEXT_WINDOW;
        let raw: toml::Value = toml::from_str(
            r#"
            [models]
            default = "atelier-4.5"

            [model.atelier-build]
            model = "atelier-4.5"
            context_window = 500000
            base_url = "https://inference.example.com/v1"
            api_backend = "responses"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        let mut prefetched = IndexMap::new();
        let mut entry = test_model_entry(
            "atelier-4.5",
            "https://inference.example.com/v1",
            None,
            None,
            None,
        );
        entry.info.context_window = NonZeroU64::new(default_cw).unwrap();
        prefetched.insert("atelier-4.5".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let by_key = resolved
            .get("atelier-build")
            .expect("atelier-build key must exist");
        assert_eq!(by_key.info.context_window.get(), 500_000);
        assert_eq!(by_key.info.model, "atelier-4.5");
        let by_latest = resolved
            .get("atelier-4.5")
            .expect("atelier-4.5 key must exist");
        assert_eq!(
            by_latest.info.context_window.get(),
            500_000,
            "BUG: prefetched 'atelier-4.5' should inherit 500k from \
             sibling 'atelier-build' (same model slug), not stay at {default_cw}"
        );
    }
    /// Slug propagation should carry over api_backend but NOT agent_type.
    #[test]
    fn slug_propagation_inherits_api_backend_but_not_agent_type() {
        let default_cw = DEFAULT_CONTEXT_WINDOW;
        let raw: toml::Value = toml::from_str(
            r#"
            [model.atelier-build]
            model = "atelier-4.5"
            context_window = 500000
            base_url = "https://test.example.com/v1"
            api_backend = "responses"
            agent_type = "atelier-build"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        let mut prefetched = IndexMap::new();
        let mut entry = test_model_entry(
            "atelier-4.5",
            "https://test.example.com/v1",
            None,
            None,
            None,
        );
        entry.info.context_window = NonZeroU64::new(default_cw).unwrap();
        entry.info.agent_type = default_agent_type();
        entry.info.api_backend = ApiBackend::default();
        prefetched.insert("atelier-4.5".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let latest = resolved.get("atelier-4.5").unwrap();
        assert_eq!(
            latest.info.agent_type,
            default_agent_type(),
            "agent_type must NOT be inherited from sibling — each entry owns its own harness"
        );
        assert_eq!(
            latest.info.api_backend,
            ApiBackend::Responses,
            "api_backend should be inherited from sibling"
        );
    }
    /// When the prefetched entry has an explicitly-set context_window
    /// (not the 256k default), slug propagation must NOT overwrite it.
    #[test]
    fn slug_propagation_does_not_overwrite_explicit_context_window() {
        let raw: toml::Value = toml::from_str(
            r#"
            [model.atelier-build]
            model = "atelier-4.5"
            context_window = 500000
            base_url = "https://test.example.com/v1"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        let mut prefetched = IndexMap::new();
        let mut entry = test_model_entry(
            "atelier-4.5",
            "https://test.example.com/v1",
            None,
            None,
            None,
        );
        entry.info.context_window = NonZeroU64::new(65_536).unwrap();
        prefetched.insert("atelier-4.5".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let latest = resolved.get("atelier-4.5").unwrap();
        assert_eq!(
            latest.info.context_window.get(),
            65_536,
            "explicitly-set context_window must not be overwritten by slug propagation"
        );
    }
    /// When no sibling has a real context_window, slug propagation is a no-op.
    #[test]
    fn slug_propagation_noop_when_no_donor() {
        let default_cw = DEFAULT_CONTEXT_WINDOW;
        let cfg = Config::default();
        let mut prefetched = IndexMap::new();
        let mut entry = test_model_entry(
            "some-unknown-model",
            "https://test.example.com/v1",
            None,
            None,
            None,
        );
        entry.info.context_window = NonZeroU64::new(default_cw).unwrap();
        prefetched.insert("some-unknown-model".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let model = resolved.get("some-unknown-model").unwrap();
        assert_eq!(
            model.info.context_window.get(),
            default_cw,
            "no donor exists, context_window should stay at parser default"
        );
    }
    /// Build a minimal `ModelEntry` for testing resolve_model_list.
    fn prefetch_model_entry(
        slug: &str,
        context_window: u64,
        api_backend: ApiBackend,
    ) -> ModelEntry {
        ModelEntry {
            info: ModelInfo {
                user_selectable: true,
                id: None,
                model: slug.to_owned(),
                base_url: "https://test.example.com/v1".to_owned(),
                name: Some(slug.to_owned()),
                description: None,
                max_completion_tokens: None,
                temperature: None,
                top_p: None,
                api_backend,
                auth_scheme: Default::default(),
                extra_headers: IndexMap::new(),
                context_window: NonZeroU64::new(context_window).unwrap(),
                use_concise: false,
                agent_type: default_agent_type(),
                inference_idle_timeout_secs: None,
                max_retries: None,
                hidden: false,
                supported_in_api: true,
                reasoning_effort: None,
                supports_reasoning_effort: false,
                reasoning_efforts: Vec::new(),
                accepts_images: false,
                supports_fast_mode: false,
                supports_backend_search: false,
                compactions_remaining: None,
                compaction_at_tokens: None,
                show_model_fingerprint: false,
                stream_tool_calls: None,
                laziness_detector: LazinessDetectorPerModelConfig::default(),
                auto_compact_threshold_percent: None,
                system_prompt_label: None,
            },
            api_key: None,
            env_key: None,
            api_base_url: None,
            request_payload: serde_json::Map::new(),
            remote_compaction_endpoint: None,
            image_generation_endpoint: None,
        }
    }
    #[test]
    fn global_extra_headers_apply_to_model_without_override() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            r#"
            [models]
            extra_headers = { "X-Request-Tags" = "team=example,env=prod" }
            "#,
            None,
        );
        let model = models.get(dm).expect("default model should exist");
        assert_eq!(
            model
                .info
                .extra_headers
                .get("X-Request-Tags")
                .map(String::as_str),
            Some("team=example,env=prod"),
            "global [models].extra_headers must apply to a model with no per-model override"
        );
    }
    #[test]
    fn per_model_extra_headers_override_global_per_key() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
                [models]
                extra_headers = {{ "X-Request-Tags" = "team=example,env=staging", "X-Team" = "platform" }}

                [model."{dm}"]
                extra_headers = {{ "X-Request-Tags" = "team=example,env=prod" }}
                "#,
            ),
            None,
        );
        let model = models.get(dm).expect("default model should exist");
        assert_eq!(
            model
                .info
                .extra_headers
                .get("X-Request-Tags")
                .map(String::as_str),
            Some("team=example,env=prod"),
            "per-model extra_headers must override the global value for that key"
        );
        assert_eq!(
            model.info.extra_headers.get("X-Team").map(String::as_str),
            Some("platform"),
            "a global-only key must still be inherited when a model overrides a different key"
        );
    }
    #[test]
    fn per_model_extra_headers_override_global_case_insensitively() {
        let dm = crate::models::default_model();
        let (_, models) = resolve_models_from_toml(
            &format!(
                r#"
                [models]
                extra_headers = {{ "X-Request-Tags" = "global" }}

                [model."{dm}"]
                extra_headers = {{ "x-request-tags" = "permodel" }}
                "#,
            ),
            None,
        );
        let model = models.get(dm).expect("default model should exist");
        let cost_tags: Vec<&str> = model
            .info
            .extra_headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("x-request-tags"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            cost_tags,
            vec!["permodel"],
            "per-model value must win case-insensitively, with no global case-variant duplicate"
        );
        assert!(
            !model.info.extra_headers.contains_key("X-Request-Tags"),
            "global \"X-Request-Tags\" must not co-exist with per-model \"x-request-tags\""
        );
    }
    #[test]
    fn global_extra_headers_apply_to_prefetched_model() {
        let mut cfg = Config::default();
        cfg.models.extra_headers.insert(
            "X-Request-Tags".to_owned(),
            "team=example,env=prod".to_owned(),
        );
        let entry = prefetch_model_entry("remote-only-model", 200_000, ApiBackend::default());
        let mut prefetched = IndexMap::new();
        prefetched.insert("remote-only-model".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let model = resolved
            .get("remote-only-model")
            .expect("prefetched model should exist");
        assert_eq!(
            model
                .info
                .extra_headers
                .get("X-Request-Tags")
                .map(String::as_str),
            Some("team=example,env=prod"),
            "global [models].extra_headers must cover models from /v1/models"
        );
    }
    #[test]
    fn global_model_defaults_apply_to_model_without_override() {
        let mut cfg = Config::default();
        cfg.models.temperature = Some(0.5);
        cfg.models.top_p = Some(0.25);
        cfg.models.max_completion_tokens = Some(4096);
        cfg.models.max_retries = Some(9);
        cfg.models.inference_idle_timeout_secs = Some(600);
        cfg.models.stream_tool_calls = Some(true);
        let entry = prefetch_model_entry("remote-only-model", 200_000, ApiBackend::default());
        let mut prefetched = IndexMap::new();
        prefetched.insert("remote-only-model".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let info = &resolved
            .get("remote-only-model")
            .expect("prefetched model should exist")
            .info;
        assert_eq!(info.temperature, Some(0.5));
        assert_eq!(info.top_p, Some(0.25));
        assert_eq!(info.max_completion_tokens, Some(4096));
        assert_eq!(info.max_retries, Some(9));
        assert_eq!(info.inference_idle_timeout_secs, Some(600));
        assert_eq!(info.stream_tool_calls, Some(true));
    }
    #[test]
    fn per_model_value_overrides_global_model_default() {
        let mut cfg = Config::default();
        cfg.models.max_retries = Some(9);
        cfg.models.max_completion_tokens = Some(8192);
        cfg.config_models.insert(
            "remote-only-model".to_owned(),
            ConfigModelOverride {
                max_retries: Some(2),
                ..Default::default()
            },
        );
        let entry = prefetch_model_entry("remote-only-model", 200_000, ApiBackend::default());
        let mut prefetched = IndexMap::new();
        prefetched.insert("remote-only-model".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let model = resolved
            .get("remote-only-model")
            .expect("model should exist");
        assert_eq!(
            model.info.max_retries,
            Some(2),
            "per-model value must win over the [models] default"
        );
        assert_eq!(
            model.info.max_completion_tokens,
            Some(8192),
            "a global-only default must still be inherited"
        );
    }
    #[test]
    fn global_model_defaults_do_not_override_prefetched_value() {
        let mut cfg = Config::default();
        cfg.models.max_retries = Some(9);
        cfg.models.temperature = Some(0.5);
        let mut entry = prefetch_model_entry("remote-only-model", 200_000, ApiBackend::default());
        entry.info.max_retries = Some(3);
        let mut prefetched = IndexMap::new();
        prefetched.insert("remote-only-model".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let model = resolved
            .get("remote-only-model")
            .expect("prefetched model should exist");
        assert_eq!(
            model.info.max_retries,
            Some(3),
            "a prefetched value must beat the [models] default (fallback semantics)"
        );
        assert_eq!(
            model.info.temperature,
            Some(0.5),
            "a field the prefetch left unset must inherit the [models] default"
        );
    }
    #[test]
    fn config_model_reasoning_efforts_parses_inline_tables_and_bare_strings() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.custom]
            model = "custom"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            reasoning_efforts = [
                { value = "high", label = "High", default = true },
                { id = "deep", value = "xhigh", label = "Deep", description = "Max" },
            ]

            [model.shorthand]
            model = "shorthand"
            base_url = "https://api.example.com/v1"
            context_window = 200000
            reasoning_efforts = ["low", "high"]
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let custom = &resolved.get("custom").expect("custom model").info;
        assert_eq!(custom.reasoning_efforts.len(), 2);
        assert_eq!(custom.reasoning_efforts[0].label, "High");
        assert!(custom.reasoning_efforts[0].default);
        assert_eq!(custom.reasoning_efforts[1].id, "deep");
        assert_eq!(custom.reasoning_efforts[1].value, ReasoningEffort::Xhigh);
        let shorthand = &resolved.get("shorthand").expect("shorthand model").info;
        let ids: Vec<_> = shorthand
            .reasoning_efforts
            .iter()
            .map(|o| o.id.as_str())
            .collect();
        assert_eq!(ids, ["low", "high"]);
        assert_eq!(shorthand.reasoning_efforts[0].label, "Low");
    }
    #[test]
    fn resolve_model_list_config_reasoning_efforts_beats_remote() {
        let raw_config: toml::Value = toml::from_str(
            r#"
            [model.atelier-x]
            reasoning_efforts = ["low"]
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw_config).expect("config should parse");
        let mut entry = prefetch_model_entry("atelier-x", 200_000, ApiBackend::default());
        entry.info.reasoning_efforts = vec![ReasoningEffortOption {
            id: "high".to_string(),
            value: ReasoningEffort::High,
            label: "High".to_string(),
            description: None,
            default: false,
        }];
        let mut prefetched = IndexMap::new();
        prefetched.insert("atelier-x".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let efforts = &resolved
            .get("atelier-x")
            .expect("atelier-x")
            .info
            .reasoning_efforts;
        assert_eq!(efforts.len(), 1);
        assert_eq!(
            efforts[0].id, "low",
            "config.toml list must override remote"
        );
    }
    #[test]
    fn resolve_model_list_inherits_context_window_from_default_when_prefetched_has_fallback() {
        let cfg = Config::default();
        let default_cw = DEFAULT_CONTEXT_WINDOW;
        let entry = prefetch_model_entry("atelier-build", default_cw, ApiBackend::default());
        let mut prefetched = IndexMap::new();
        prefetched.insert("atelier-build".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let entry = resolved.get("atelier-build").expect("model must exist");
        assert_ne!(
            entry.info.context_window.get(),
            default_cw,
            "context_window should have been inherited from hardcoded default, not left at DEFAULT_CONTEXT_WINDOW"
        );
    }
    #[test]
    fn resolve_model_list_does_not_override_explicitly_set_context_window() {
        let cfg = Config::default();
        let explicit_cw = 65_536;
        let entry = prefetch_model_entry("atelier-build", explicit_cw, ApiBackend::default());
        let mut prefetched = IndexMap::new();
        prefetched.insert("atelier-build".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let entry = resolved.get("atelier-build").expect("model must exist");
        assert_eq!(
            entry.info.context_window.get(),
            explicit_cw,
            "explicitly-set context_window must not be overwritten by default"
        );
    }
    #[test]
    fn resolve_model_list_inherits_agent_type_and_api_backend() {
        let cfg = Config::default();
        let default_cw = DEFAULT_CONTEXT_WINDOW;
        let entry = prefetch_model_entry("atelier-build", default_cw, ApiBackend::default());
        let mut prefetched = IndexMap::new();
        prefetched.insert("atelier-build".to_owned(), entry);
        let resolved = resolve_model_list(&cfg, Some(prefetched));
        let entry = resolved.get("atelier-build").expect("model must exist");
        let defaults = default_model_entries(&EndpointsConfig::default());
        if let Some(default) = defaults.get("atelier-build") {
            if default.info.agent_type != DEFAULT_AGENT_TYPE {
                assert_eq!(
                    entry.info.agent_type, default.info.agent_type,
                    "agent_type should be inherited from default"
                );
            }
            if default.info.api_backend != ApiBackend::default() {
                assert_eq!(
                    entry.info.api_backend, default.info.api_backend,
                    "api_backend should be inherited from default"
                );
            }
        }
    }
    #[test]
    fn hub_config_default_has_no_url() {
        assert!(HubConfig::default().url.is_none());
        assert!(!HubConfig::default().is_enabled());
    }
    #[test]
    fn hub_config_is_enabled_only_for_nonempty_url() {
        assert!(
            HubConfig {
                url: Some("wss://hub.example/ws".into()),
            }
            .is_enabled()
        );
        assert!(
            !HubConfig {
                url: Some("   ".into()),
            }
            .is_enabled()
        );
    }
    #[test]
    fn resolve_model_list_prunes_bundled_entries_not_in_prefetch() {
        let cfg = Config::default();
        let mut defs = default_model_entries(&EndpointsConfig::default());
        let mut p = IndexMap::new();
        if let Some(e) = defs.shift_remove("atelier-build") {
            p.insert("atelier-build".to_string(), e);
        }
        let resolved = resolve_model_list(&cfg, Some(p));
        assert!(resolved.contains_key("atelier-build"));
        let no_p = resolve_model_list(&cfg, None);
        assert!(no_p.contains_key("atelier-build"));
    }
    #[test]
    fn resolve_model_list_prefetch_visibility_matches_auth_and_server_list() {
        let cfg = Config::default();
        let mut defs = default_model_entries(&EndpointsConfig::default());
        let mut p = IndexMap::new();
        if let Some(e) = defs.shift_remove("atelier-build") {
            p.insert("atelier-build".to_string(), e);
        }
        let resolved = resolve_model_list(&cfg, Some(p));
        let sess: Vec<_> = resolved
            .values()
            .filter(|e| e.visible_for_auth(true))
            .collect();
        let api: Vec<_> = resolved
            .values()
            .filter(|e| e.visible_for_auth(false))
            .collect();
        assert_eq!(sess.len(), 1);
        assert!(api.is_empty());
    }
    #[test]
    fn resolve_model_list_keeps_prefetch_only_entries_and_prunes_defaults() {
        let cfg = Config::default();
        let mut p = IndexMap::new();
        let e = prefetch_model_entry("secret-xyz", 200000, ApiBackend::default());
        p.insert("secret-xyz".to_string(), e);
        let resolved = resolve_model_list(&cfg, Some(p));
        assert!(resolved.contains_key("secret-xyz"));
        assert!(!resolved.contains_key("atelier-build"));
    }
    #[test]
    fn resolve_model_list_prefetch_replaces_bundled_entirely() {
        let cfg = Config::default();
        let mut p = IndexMap::new();
        let e = prefetch_model_entry("atelier-4.5", 500_000, ApiBackend::Responses);
        p.insert("atelier-4.5".to_string(), e);
        let resolved = resolve_model_list(&cfg, Some(p));
        assert!(resolved.contains_key("atelier-4.5"));
        assert!(!resolved.contains_key("atelier-build"));
    }
    #[test]
    fn resolve_model_list_empty_prefetch_yields_empty_base() {
        let cfg = Config::default();
        let resolved = resolve_model_list(&cfg, Some(IndexMap::new()));
        assert!(resolved.is_empty());
    }
    /// Regression: enterprise managed config aliases atelier-build to their own
    /// endpoint with env_key. The bundled atelier-build has supported_in_api=false.
    /// The config overlay must be visible to API-key users (env_key = BYOK).
    #[test]
    fn byok_config_overlay_visible_to_api_key_users() {
        let raw: toml::Value = toml::from_str(
            r#"
            [model.atelier-build]
            model = "atelier-4.5"
            base_url = "https://inference.company.com/v1"
            env_key = "COMPANY_TOKEN"
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let entry = resolved
            .get("atelier-build")
            .expect("atelier-build must exist");
        assert!(
            entry.visible_for_auth(false),
            "BYOK config entry must be visible to API-key users — \
             bundled supported_in_api=false must not leak into credentialed overlays"
        );
    }
    /// Guard: config overlay WITHOUT credentials must NOT override the
    /// bundled supported_in_api flag. Only BYOK triggers the override.
    #[test]
    fn plain_config_overlay_preserves_bundled_visibility() {
        let raw: toml::Value = toml::from_str(
            r#"
            [model.atelier-build]
            context_window = 300000
            "#,
        )
        .unwrap();
        let cfg = Config::new_from_toml_cfg(&raw).expect("config should parse");
        let resolved = resolve_model_list(&cfg, None);
        let entry = resolved
            .get("atelier-build")
            .expect("atelier-build must exist");
        assert!(
            !entry.visible_for_auth(false),
            "non-BYOK config overlay must preserve bundled supported_in_api=false"
        );
    }
    #[test]
    #[serial]
    fn mcp_liveness_watchers_default_is_true() {
        unsafe { std::env::remove_var("ATELIER_MCP_LIVENESS_WATCHERS") };
        let r = resolve_mcp_liveness_watchers(None, None, None, None, None);
        assert!(r.value, "default-on by spec");
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    #[serial]
    fn mcp_liveness_watchers_requirement_wins_over_everything() {
        unsafe { std::env::set_var("ATELIER_MCP_LIVENESS_WATCHERS", "true") };
        let r = resolve_mcp_liveness_watchers(
            Some(false),
            Some(true),
            Some(true),
            Some(true),
            Some(true),
        );
        unsafe { std::env::remove_var("ATELIER_MCP_LIVENESS_WATCHERS") };
        assert!(!r.value, "requirement overrides every other layer");
        assert_eq!(r.source, ConfigSource::Requirement);
    }
    #[test]
    #[serial]
    fn mcp_liveness_watchers_cli_wins_over_env_and_below() {
        unsafe { std::env::set_var("ATELIER_MCP_LIVENESS_WATCHERS", "true") };
        let r =
            resolve_mcp_liveness_watchers(None, Some(false), Some(true), Some(true), Some(true));
        unsafe { std::env::remove_var("ATELIER_MCP_LIVENESS_WATCHERS") };
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Cli);
    }
    #[test]
    #[serial]
    fn mcp_liveness_watchers_env_wins_over_config_and_below() {
        unsafe { std::env::set_var("ATELIER_MCP_LIVENESS_WATCHERS", "false") };
        let r = resolve_mcp_liveness_watchers(None, None, Some(true), Some(true), Some(true));
        unsafe { std::env::remove_var("ATELIER_MCP_LIVENESS_WATCHERS") };
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Env);
    }
    #[test]
    #[serial]
    fn mcp_liveness_watchers_config_wins_over_managed_and_feature_flag() {
        unsafe { std::env::remove_var("ATELIER_MCP_LIVENESS_WATCHERS") };
        let r = resolve_mcp_liveness_watchers(None, None, Some(false), Some(true), Some(true));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
    }
    #[test]
    #[serial]
    fn mcp_liveness_watchers_managed_wins_over_feature_flag() {
        unsafe { std::env::remove_var("ATELIER_MCP_LIVENESS_WATCHERS") };
        let r = resolve_mcp_liveness_watchers(None, None, None, Some(false), Some(true));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }
    #[test]
    #[serial]
    fn mcp_liveness_watchers_feature_flag_used_when_no_higher_layer() {
        unsafe { std::env::remove_var("ATELIER_MCP_LIVENESS_WATCHERS") };
        let r = resolve_mcp_liveness_watchers(None, None, None, None, Some(false));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }
    #[test]
    #[serial]
    fn mcp_auto_restart_default_is_true() {
        unsafe { std::env::remove_var("ATELIER_MCP_AUTO_RESTART") };
        let r = resolve_mcp_auto_restart(None, None, None, None, None);
        assert!(r.value, "recovery is on by default");
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    #[serial]
    fn mcp_auto_restart_requirement_wins_over_everything() {
        unsafe { std::env::set_var("ATELIER_MCP_AUTO_RESTART", "false") };
        let r = resolve_mcp_auto_restart(
            Some(true),
            Some(false),
            Some(false),
            Some(false),
            Some(false),
        );
        unsafe { std::env::remove_var("ATELIER_MCP_AUTO_RESTART") };
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Requirement);
    }
    #[test]
    #[serial]
    fn mcp_auto_restart_env_wins_over_config_and_below() {
        unsafe { std::env::set_var("ATELIER_MCP_AUTO_RESTART", "true") };
        let r = resolve_mcp_auto_restart(None, None, Some(false), Some(false), Some(false));
        unsafe { std::env::remove_var("ATELIER_MCP_AUTO_RESTART") };
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Env);
    }
    #[test]
    #[serial]
    fn mcp_push_server_status_default_is_true() {
        unsafe { std::env::remove_var("ATELIER_MCP_PUSH_SERVER_STATUS") };
        let r = resolve_mcp_push_server_status(None, None, None, None, None);
        assert!(r.value, "default-on by spec");
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    #[serial]
    fn mcp_push_server_status_requirement_wins_over_everything() {
        unsafe { std::env::set_var("ATELIER_MCP_PUSH_SERVER_STATUS", "true") };
        let r = resolve_mcp_push_server_status(
            Some(false),
            Some(true),
            Some(true),
            Some(true),
            Some(true),
        );
        unsafe { std::env::remove_var("ATELIER_MCP_PUSH_SERVER_STATUS") };
        assert!(!r.value, "requirement overrides every other layer");
        assert_eq!(r.source, ConfigSource::Requirement);
    }
    #[test]
    #[serial]
    fn mcp_push_server_status_cli_wins_over_env_and_below() {
        unsafe { std::env::set_var("ATELIER_MCP_PUSH_SERVER_STATUS", "true") };
        let r =
            resolve_mcp_push_server_status(None, Some(false), Some(true), Some(true), Some(true));
        unsafe { std::env::remove_var("ATELIER_MCP_PUSH_SERVER_STATUS") };
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Cli);
    }
    #[test]
    #[serial]
    fn mcp_push_server_status_env_wins_over_config_and_below() {
        unsafe { std::env::set_var("ATELIER_MCP_PUSH_SERVER_STATUS", "false") };
        let r = resolve_mcp_push_server_status(None, None, Some(true), Some(true), Some(true));
        unsafe { std::env::remove_var("ATELIER_MCP_PUSH_SERVER_STATUS") };
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Env);
    }
    #[test]
    #[serial]
    fn mcp_push_server_status_config_wins_over_managed_and_feature_flag() {
        unsafe { std::env::remove_var("ATELIER_MCP_PUSH_SERVER_STATUS") };
        let r = resolve_mcp_push_server_status(None, None, Some(false), Some(true), Some(true));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
    }
    #[test]
    #[serial]
    fn mcp_push_server_status_managed_wins_over_feature_flag() {
        unsafe { std::env::remove_var("ATELIER_MCP_PUSH_SERVER_STATUS") };
        let r = resolve_mcp_push_server_status(None, None, None, Some(false), Some(true));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }
    #[test]
    #[serial]
    fn mcp_push_server_status_feature_flag_used_when_no_higher_layer() {
        unsafe { std::env::remove_var("ATELIER_MCP_PUSH_SERVER_STATUS") };
        let r = resolve_mcp_push_server_status(None, None, None, None, Some(false));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }
    #[test]
    #[serial]
    fn mcp_recursive_config_watch_default_is_true() {
        unsafe { std::env::remove_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH") };
        let r = resolve_mcp_recursive_config_watch(None, None, None, None, None);
        assert!(r.value, "default-on by spec");
        assert_eq!(r.source, ConfigSource::Default);
    }
    #[test]
    #[serial]
    fn mcp_recursive_config_watch_requirement_wins_over_everything() {
        unsafe { std::env::set_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH", "true") };
        let r = resolve_mcp_recursive_config_watch(
            Some(false),
            Some(true),
            Some(true),
            Some(true),
            Some(true),
        );
        unsafe { std::env::remove_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH") };
        assert!(!r.value, "requirement overrides every other layer");
        assert_eq!(r.source, ConfigSource::Requirement);
    }
    #[test]
    #[serial]
    fn mcp_recursive_config_watch_cli_wins_over_env_and_below() {
        unsafe { std::env::set_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH", "true") };
        let r = resolve_mcp_recursive_config_watch(
            None,
            Some(false),
            Some(true),
            Some(true),
            Some(true),
        );
        unsafe { std::env::remove_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH") };
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Cli);
    }
    #[test]
    #[serial]
    fn mcp_recursive_config_watch_env_wins_over_config_and_below() {
        unsafe { std::env::set_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH", "false") };
        let r = resolve_mcp_recursive_config_watch(None, None, Some(true), Some(true), Some(true));
        unsafe { std::env::remove_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH") };
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Env);
    }
    #[test]
    #[serial]
    fn mcp_recursive_config_watch_config_wins_over_managed_and_feature_flag() {
        unsafe { std::env::remove_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH") };
        let r = resolve_mcp_recursive_config_watch(None, None, Some(false), Some(true), Some(true));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
    }
    #[test]
    #[serial]
    fn mcp_recursive_config_watch_managed_wins_over_feature_flag() {
        unsafe { std::env::remove_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH") };
        let r = resolve_mcp_recursive_config_watch(None, None, None, Some(false), Some(true));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }
    #[test]
    #[serial]
    fn mcp_recursive_config_watch_feature_flag_used_when_no_higher_layer() {
        unsafe { std::env::remove_var("ATELIER_MCP_RECURSIVE_CONFIG_WATCH") };
        let r = resolve_mcp_recursive_config_watch(None, None, None, None, Some(false));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }
}
