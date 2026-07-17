//! Provider and model management for the private Atelier runtime.
//!
//! This crate deliberately owns configuration and catalog state only. Network
//! adapters, UI code, and credential backends depend on these types instead of
//! making the runtime guess capabilities from a model name.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::str::FromStr;
use thiserror::Error;
use url::Url;

#[cfg(windows)]
mod windows_credentials;

const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for SecretString {
    fn as_ref(&self) -> &str {
        self.expose_secret()
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString(REDACTED)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialErrorCode {
    NotConfigured,
    InvalidReference,
    EnvironmentVariableMissing,
    EnvironmentVariableNotUnicode,
    EnvironmentVariableEmpty,
    CommandInvalid,
    CommandSpawnFailed,
    CommandFailed,
    CommandOutputNotUtf8,
    CommandOutputEmpty,
    CommandOutputMultipleLines,
    SecretStoreUnsupported,
    SecretStoreOperationFailed,
    SecretStoreAccountMismatch,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("no credential is configured")]
    NotConfigured,
    #[error("invalid {kind} credential reference: {reason}")]
    InvalidReference { kind: &'static str, reason: String },
    #[error("credential environment variable is not set: {variable}")]
    EnvironmentVariableMissing { variable: String },
    #[error("credential environment variable is not valid UTF-8: {variable}")]
    EnvironmentVariableNotUnicode { variable: String },
    #[error("credential environment variable is empty: {variable}")]
    EnvironmentVariableEmpty { variable: String },
    #[error("credential command could not be started: {program}")]
    CommandSpawnFailed { program: String },
    #[error("credential command failed: {program} (status {status:?})")]
    CommandFailed {
        program: String,
        status: Option<i32>,
    },
    #[error("credential command output is not valid UTF-8: {program}")]
    CommandOutputNotUtf8 { program: String },
    #[error("credential command output is empty: {program}")]
    CommandOutputEmpty { program: String },
    #[error("credential command output must contain one line: {program}")]
    CommandOutputMultipleLines { program: String },
    #[error(
        "SecretStore credential references are unsupported: service={service}, account={account}"
    )]
    SecretStoreUnsupported { service: String, account: String },
    #[error(
        "SecretStore credential {operation} failed: service={service}, account={account}, Windows error={error_code}"
    )]
    SecretStoreOperationFailed {
        operation: &'static str,
        service: String,
        account: String,
        error_code: i32,
    },
    #[error("SecretStore credential account does not match: service={service}, account={account}")]
    SecretStoreAccountMismatch { service: String, account: String },
}

impl CredentialError {
    pub fn code(&self) -> CredentialErrorCode {
        match self {
            Self::NotConfigured => CredentialErrorCode::NotConfigured,
            Self::InvalidReference { kind, .. } if *kind == "command" => {
                CredentialErrorCode::CommandInvalid
            }
            Self::InvalidReference { .. } => CredentialErrorCode::InvalidReference,
            Self::EnvironmentVariableMissing { .. } => {
                CredentialErrorCode::EnvironmentVariableMissing
            }
            Self::EnvironmentVariableNotUnicode { .. } => {
                CredentialErrorCode::EnvironmentVariableNotUnicode
            }
            Self::EnvironmentVariableEmpty { .. } => CredentialErrorCode::EnvironmentVariableEmpty,
            Self::CommandSpawnFailed { .. } => CredentialErrorCode::CommandSpawnFailed,
            Self::CommandFailed { .. } => CredentialErrorCode::CommandFailed,
            Self::CommandOutputNotUtf8 { .. } => CredentialErrorCode::CommandOutputNotUtf8,
            Self::CommandOutputEmpty { .. } => CredentialErrorCode::CommandOutputEmpty,
            Self::CommandOutputMultipleLines { .. } => {
                CredentialErrorCode::CommandOutputMultipleLines
            }
            Self::SecretStoreUnsupported { .. } => CredentialErrorCode::SecretStoreUnsupported,
            Self::SecretStoreOperationFailed { .. } => {
                CredentialErrorCode::SecretStoreOperationFailed
            }
            Self::SecretStoreAccountMismatch { .. } => {
                CredentialErrorCode::SecretStoreAccountMismatch
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("invalid provider: {0}")]
    InvalidProvider(String),
    #[error("invalid model key: {0}")]
    InvalidModelKey(String),
    #[error("provider not found: {0}")]
    ProviderNotFound(String),
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("provider already exists: {0}")]
    ProviderAlreadyExists(String),
    #[error("credential resolution failed: {0}")]
    Credential(#[from] CredentialError),
    #[error("model discovery response is invalid: {0}")]
    DiscoveryInvalid(String),
    #[error("model discovery response parsing failed: {0}")]
    DiscoveryParse(#[from] serde_json::Error),
    #[error("configuration I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("configuration serialization failed: {0}")]
    Serialization(#[from] toml::de::Error),
    #[error("configuration encoding failed: {0}")]
    Encoding(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum CredentialRef {
    #[default]
    None,
    Environment {
        variable: String,
    },
    SecretStore {
        service: String,
        account: String,
    },
    Command {
        program: String,
        args: Vec<String>,
    },
}

impl CredentialRef {
    pub fn validate(&self) -> Result<(), CredentialError> {
        match self {
            Self::None => Ok(()),
            Self::Environment { variable } => {
                if variable.trim().is_empty() || variable.contains(['=', '\0']) {
                    return Err(CredentialError::InvalidReference {
                        kind: "environment",
                        reason: "variable name must not be empty or contain '=' or NUL".into(),
                    });
                }
                Ok(())
            }
            Self::SecretStore { service, account } => {
                if service.trim().is_empty()
                    || account.trim().is_empty()
                    || service.contains('\0')
                    || account.contains('\0')
                {
                    return Err(CredentialError::InvalidReference {
                        kind: "secret_store",
                        reason: "service and account must not be empty or contain NUL".into(),
                    });
                }
                Ok(())
            }
            Self::Command { program, args } => {
                if program.trim().is_empty() || program.contains('\0') {
                    return Err(CredentialError::InvalidReference {
                        kind: "command",
                        reason: "program must not be empty or contain NUL".into(),
                    });
                }
                if args.iter().any(|argument| argument.contains('\0')) {
                    return Err(CredentialError::InvalidReference {
                        kind: "command",
                        reason: "arguments must not contain NUL".into(),
                    });
                }
                Ok(())
            }
        }
    }

    pub fn resolve(&self) -> Result<Option<SecretString>, CredentialError> {
        self.validate()?;
        match self {
            Self::None => Ok(None),
            Self::Environment { variable } => match std::env::var(variable) {
                Ok(value) if value.is_empty() => Err(CredentialError::EnvironmentVariableEmpty {
                    variable: variable.clone(),
                }),
                Ok(value) => Ok(Some(SecretString::new(value))),
                Err(std::env::VarError::NotPresent) => {
                    Err(CredentialError::EnvironmentVariableMissing {
                        variable: variable.clone(),
                    })
                }
                Err(std::env::VarError::NotUnicode(_)) => {
                    Err(CredentialError::EnvironmentVariableNotUnicode {
                        variable: variable.clone(),
                    })
                }
            },
            Self::SecretStore { service, account } => {
                #[cfg(windows)]
                {
                    windows_credentials::read(service, account).map(Some)
                }
                #[cfg(not(windows))]
                {
                    Err(CredentialError::SecretStoreUnsupported {
                        service: service.clone(),
                        account: account.clone(),
                    })
                }
            }
            Self::Command { program, args } => {
                let output = ProcessCommand::new(program)
                    .args(args)
                    .output()
                    .map_err(|_| CredentialError::CommandSpawnFailed {
                        program: program.clone(),
                    })?;
                if !output.status.success() {
                    return Err(CredentialError::CommandFailed {
                        program: program.clone(),
                        status: output.status.code(),
                    });
                }
                let value = String::from_utf8(output.stdout).map_err(|_| {
                    CredentialError::CommandOutputNotUtf8 {
                        program: program.clone(),
                    }
                })?;
                let value = value.trim_end_matches(['\r', '\n']);
                if value.is_empty() {
                    return Err(CredentialError::CommandOutputEmpty {
                        program: program.clone(),
                    });
                }
                if value.contains(['\r', '\n']) {
                    return Err(CredentialError::CommandOutputMultipleLines {
                        program: program.clone(),
                    });
                }
                Ok(Some(SecretString::new(value)))
            }
        }
    }

    pub fn set_secret(&self, secret: SecretString) -> Result<(), CredentialError> {
        self.validate()?;
        let Self::SecretStore { service, account } = self else {
            return Err(CredentialError::InvalidReference {
                kind: "secret_store",
                reason: "secret storage requires a SecretStore credential reference".into(),
            });
        };

        #[cfg(windows)]
        {
            windows_credentials::write(service, account, secret.expose_secret())
        }
        #[cfg(not(windows))]
        {
            let _ = secret;
            Err(CredentialError::SecretStoreUnsupported {
                service: service.clone(),
                account: account.clone(),
            })
        }
    }

    pub fn delete_secret(&self) -> Result<(), CredentialError> {
        self.validate()?;
        let Self::SecretStore { service, account } = self else {
            return Err(CredentialError::InvalidReference {
                kind: "secret_store",
                reason: "secret deletion requires a SecretStore credential reference".into(),
            });
        };

        #[cfg(windows)]
        {
            windows_credentials::delete(service, account)
        }
        #[cfg(not(windows))]
        {
            Err(CredentialError::SecretStoreUnsupported {
                service: service.clone(),
                account: account.clone(),
            })
        }
    }

    pub fn resolve_required(&self) -> Result<SecretString, CredentialError> {
        self.resolve()?.ok_or(CredentialError::NotConfigured)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ProviderDiscovery {
    #[default]
    Disabled,
    Static,
    OpenAiModels {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    pub id: String,
    pub display_name: String,
    pub protocol: ProviderProtocol,
    pub base_url: Url,
    #[serde(default)]
    pub credential: CredentialRef,
    #[serde(default)]
    pub discovery: ProviderDiscovery,
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl ProviderConfig {
    pub fn validate(&self) -> Result<(), ProviderError> {
        validate_identifier(&self.id, "provider id")?;
        self.credential.validate()?;
        if self.display_name.trim().is_empty() {
            return Err(ProviderError::InvalidProvider(
                "display_name must not be empty".into(),
            ));
        }
        match self.base_url.scheme() {
            "http" | "https" => Ok(()),
            scheme => Err(ProviderError::InvalidProvider(format!(
                "unsupported base URL scheme: {scheme}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ModelKey {
    pub provider_id: String,
    pub model_id: String,
}

impl ModelKey {
    pub fn new(
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let key = Self {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        };
        validate_identifier(&key.provider_id, "provider id")?;
        if key.model_id.trim().is_empty() || key.model_id.contains(['\n', '\r']) {
            return Err(ProviderError::InvalidModelKey(
                "model id must not be empty or contain newlines".into(),
            ));
        }
        Ok(key)
    }

    pub fn parse(value: &str) -> Result<Self, ProviderError> {
        let (provider_id, model_id) = value.split_once('/').ok_or_else(|| {
            ProviderError::InvalidModelKey("model key must use provider/model format".into())
        })?;
        Self::new(provider_id, model_id)
    }
}

impl FromStr for ModelKey {
    type Err = ProviderError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for ModelKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider_id, self.model_id)
    }
}

pub fn parse_model_key(value: &str) -> Result<ModelKey, ProviderError> {
    ModelKey::parse(value)
}

pub fn parse_custom_model_id(provider_id: &str, model_id: &str) -> Result<ModelKey, ProviderError> {
    ModelKey::new(provider_id, model_id)
}

fn validate_identifier(value: &str, label: &str) -> Result<(), ProviderError> {
    if value.trim().is_empty() || value.contains(['/', '\\', '\n', '\r']) {
        return Err(ProviderError::InvalidModelKey(format!(
            "{label} must be non-empty and must not contain path separators"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub text_input: bool,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub tool_calls: bool,
    #[serde(default)]
    pub parallel_tool_calls: bool,
    #[serde(default)]
    pub reasoning_effort: bool,
    #[serde(default)]
    pub web_search: bool,
    #[serde(default)]
    pub image_generation: bool,
    #[serde(default)]
    pub server_compaction: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            text_input: true,
            image_input: false,
            tool_calls: false,
            parallel_tool_calls: false,
            reasoning_effort: false,
            web_search: false,
            image_generation: false,
            server_compaction: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityOverrides {
    pub text_input: Option<bool>,
    pub image_input: Option<bool>,
    pub tool_calls: Option<bool>,
    pub parallel_tool_calls: Option<bool>,
    pub reasoning_effort: Option<bool>,
    pub web_search: Option<bool>,
    pub image_generation: Option<bool>,
    pub server_compaction: Option<bool>,
}

impl CapabilityOverrides {
    fn apply_to(&self, mut capabilities: ModelCapabilities) -> ModelCapabilities {
        if let Some(value) = self.text_input {
            capabilities.text_input = value;
        }
        if let Some(value) = self.image_input {
            capabilities.image_input = value;
        }
        if let Some(value) = self.tool_calls {
            capabilities.tool_calls = value;
        }
        if let Some(value) = self.parallel_tool_calls {
            capabilities.parallel_tool_calls = value;
        }
        if let Some(value) = self.reasoning_effort {
            capabilities.reasoning_effort = value;
        }
        if let Some(value) = self.web_search {
            capabilities.web_search = value;
        }
        if let Some(value) = self.image_generation {
            capabilities.image_generation = value;
        }
        if let Some(value) = self.server_compaction {
            capabilities.server_compaction = value;
        }
        capabilities
    }

    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    #[default]
    Static,
    Remote,
    UserOverride,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelDescriptor {
    pub key: ModelKey,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    #[serde(default)]
    pub source: ModelSource,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

pub fn parse_openai_models_response(
    provider_id: &str,
    response: &str,
) -> Result<Vec<ModelDescriptor>, ProviderError> {
    let value: Value = serde_json::from_str(response)?;
    parse_openai_models_value(provider_id, &value)
}

pub fn parse_openai_models_value(
    provider_id: &str,
    response: &Value,
) -> Result<Vec<ModelDescriptor>, ProviderError> {
    validate_identifier(provider_id, "provider id")?;
    let object = response
        .as_object()
        .ok_or_else(|| ProviderError::DiscoveryInvalid("response must be a JSON object".into()))?;
    let data = object
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderError::DiscoveryInvalid("response data must be a JSON array".into())
        })?;

    data.iter()
        .enumerate()
        .map(|(index, value)| parse_openai_model_value(provider_id, index, value))
        .collect()
}

fn parse_openai_model_value(
    provider_id: &str,
    index: usize,
    value: &Value,
) -> Result<ModelDescriptor, ProviderError> {
    let object = value.as_object().ok_or_else(|| {
        ProviderError::DiscoveryInvalid(format!("model entry {index} must be a JSON object"))
    })?;
    let model_id = json_string(object, &["id", "model"])
        .ok_or_else(|| ProviderError::DiscoveryInvalid(format!("model entry {index} has no id")))?;
    let key = parse_custom_model_id(provider_id, &model_id)?;
    let display_name = json_string(object, &["name", "display_name", "displayName"])
        .unwrap_or_else(|| key.model_id.clone());
    let capabilities = parse_model_capabilities(object);

    Ok(ModelDescriptor {
        key,
        display_name,
        description: json_string(object, &["description"]),
        context_window: json_u64(
            object,
            &[
                "context_window",
                "contextWindow",
                "context_length",
                "contextLength",
                "max_context_tokens",
                "maxContextTokens",
            ],
        ),
        capabilities,
        reasoning_efforts: parse_reasoning_efforts(object),
        source: ModelSource::Remote,
        enabled: true,
    })
}

fn json_string(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        object
            .get(*name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    })
}

fn json_u64(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_u64))
}

fn json_bool(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<bool> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_bool))
}

fn parse_model_capabilities(object: &serde_json::Map<String, Value>) -> ModelCapabilities {
    let nested = object.get("capabilities").and_then(Value::as_object);
    let get = |names: &[&str]| {
        json_bool(object, names).or_else(|| nested.and_then(|value| json_bool(value, names)))
    };
    let mut capabilities = ModelCapabilities::default();
    if let Some(value) = get(&[
        "text_input",
        "textInput",
        "supports_text_input",
        "supportsTextInput",
    ]) {
        capabilities.text_input = value;
    }
    if let Some(value) = get(&[
        "image_input",
        "imageInput",
        "supports_image_input",
        "supportsImageInput",
        "supports_vision",
        "supportsVision",
    ]) {
        capabilities.image_input = value;
    }
    if let Some(value) = get(&[
        "tool_calls",
        "toolCalls",
        "supports_tool_calls",
        "supportsToolCalls",
        "supports_tools",
        "supportsTools",
    ]) {
        capabilities.tool_calls = value;
    }
    if let Some(value) = get(&[
        "parallel_tool_calls",
        "parallelToolCalls",
        "supports_parallel_tool_calls",
        "supportsParallelToolCalls",
    ]) {
        capabilities.parallel_tool_calls = value;
    }
    if let Some(value) = get(&[
        "reasoning_effort",
        "reasoningEffort",
        "supports_reasoning_effort",
        "supportsReasoningEffort",
    ]) {
        capabilities.reasoning_effort = value;
    }
    if let Some(value) = get(&[
        "web_search",
        "webSearch",
        "supports_web_search",
        "supportsWebSearch",
    ]) {
        capabilities.web_search = value;
    }
    if let Some(value) = get(&[
        "image_generation",
        "imageGeneration",
        "supports_image_generation",
        "supportsImageGeneration",
    ]) {
        capabilities.image_generation = value;
    }
    if let Some(value) = get(&[
        "server_compaction",
        "serverCompaction",
        "supports_server_compaction",
        "supportsServerCompaction",
    ]) {
        capabilities.server_compaction = value;
    }
    capabilities
}

fn parse_reasoning_efforts(object: &serde_json::Map<String, Value>) -> Vec<String> {
    let Some(values) = object
        .get("reasoning_efforts")
        .or_else(|| object.get("reasoningEfforts"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
            Value::Object(value) => json_string(value, &["id", "value"]),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredModel {
    descriptor: ModelDescriptor,
    #[serde(default)]
    overrides: CapabilityOverrides,
    #[serde(default)]
    base_capabilities: Option<ModelCapabilities>,
    #[serde(default)]
    base_source: Option<ModelSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryFile {
    schema_version: u32,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    models: BTreeMap<String, StoredModel>,
    #[serde(default)]
    default_model: Option<ModelKey>,
}

impl Default for RegistryFile {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
            default_model: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSnapshot {
    pub providers: Vec<ProviderConfig>,
    pub models: Vec<ModelDescriptor>,
    pub default_model: Option<ModelKey>,
}

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    path: Option<PathBuf>,
    state: RegistryFile,
}

impl ProviderRegistry {
    pub fn in_memory() -> Self {
        Self {
            path: None,
            state: RegistryFile::default(),
        }
    }

    pub fn load_or_create(path: impl Into<PathBuf>) -> Result<Self, ProviderError> {
        let path = path.into();
        if !path.exists() {
            return Ok(Self {
                path: Some(path),
                state: RegistryFile::default(),
            });
        }
        let source = std::fs::read_to_string(&path)?;
        let state: RegistryFile = toml::from_str(&source)?;
        if state.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ProviderError::InvalidProvider(format!(
                "unsupported provider registry schema version {}",
                state.schema_version
            )));
        }
        Ok(Self {
            path: Some(path),
            state,
        })
    }

    pub fn snapshot(&self) -> ProviderSnapshot {
        ProviderSnapshot {
            providers: self.state.providers.values().cloned().collect(),
            models: self
                .state
                .models
                .values()
                .map(|stored| stored.descriptor.clone())
                .collect(),
            default_model: self.state.default_model.clone(),
        }
    }

    pub fn providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.state.providers.values()
    }

    /// Look up one provider without exposing the registry's storage layout.
    pub fn provider(&self, provider_id: &str) -> Option<&ProviderConfig> {
        self.state.providers.get(provider_id)
    }

    /// Whether at least one configured provider is enabled.
    pub fn has_enabled_providers(&self) -> bool {
        self.state
            .providers
            .values()
            .any(|provider| provider.enabled)
    }

    /// Return enabled providers together with their locally configured models.
    ///
    /// The returned values are owned so callers can build an immutable runtime
    /// catalog without holding a borrow into the registry while resolving
    /// credentials or applying model capability policy.
    pub fn enabled_provider_models(&self) -> Vec<(ProviderConfig, ModelDescriptor)> {
        self.state
            .models
            .values()
            .filter_map(|stored| {
                let provider = self
                    .state
                    .providers
                    .get(&stored.descriptor.key.provider_id)?;
                provider
                    .enabled
                    .then(|| (provider.clone(), stored.descriptor.clone()))
            })
            .collect()
    }

    pub fn models(&self) -> impl Iterator<Item = ModelDescriptor> + '_ {
        self.state
            .models
            .values()
            .map(|stored| stored.descriptor.clone())
    }

    pub fn model(&self, key: &ModelKey) -> Option<ModelDescriptor> {
        self.state
            .models
            .get(&key.to_string())
            .map(|stored| stored.descriptor.clone())
    }

    pub fn default_model(&self) -> Option<&ModelKey> {
        self.state.default_model.as_ref()
    }

    pub fn upsert_provider(&mut self, config: ProviderConfig) -> Result<(), ProviderError> {
        config.validate()?;
        self.state.providers.insert(config.id.clone(), config);
        Ok(())
    }

    pub fn remove_provider(&mut self, provider_id: &str) -> Result<(), ProviderError> {
        if self.state.providers.remove(provider_id).is_none() {
            return Err(ProviderError::ProviderNotFound(provider_id.into()));
        }
        self.state
            .models
            .retain(|_, stored| stored.descriptor.key.provider_id != provider_id);
        // Deliberately preserve default_model. A deleted provider must surface
        // as unavailable instead of silently switching to another provider.
        Ok(())
    }

    pub fn set_provider_enabled(
        &mut self,
        provider_id: &str,
        enabled: bool,
    ) -> Result<(), ProviderError> {
        let provider = self
            .state
            .providers
            .get_mut(provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(provider_id.into()))?;
        provider.enabled = enabled;
        Ok(())
    }

    pub fn upsert_model(&mut self, descriptor: ModelDescriptor) -> Result<(), ProviderError> {
        if !self
            .state
            .providers
            .contains_key(&descriptor.key.provider_id)
        {
            return Err(ProviderError::ProviderNotFound(
                descriptor.key.provider_id.clone(),
            ));
        }
        ModelKey::new(
            descriptor.key.provider_id.clone(),
            descriptor.key.model_id.clone(),
        )?;
        if descriptor.display_name.trim().is_empty() {
            return Err(ProviderError::InvalidModelKey(
                "model display_name must not be empty".into(),
            ));
        }
        let id = descriptor.key.to_string();
        let overrides = self
            .state
            .models
            .get(&id)
            .map(|stored| stored.overrides.clone())
            .unwrap_or_default();
        let base_capabilities = descriptor.capabilities.clone();
        let base_source = descriptor.source.clone();
        let mut descriptor = descriptor;
        descriptor.capabilities = overrides.apply_to(descriptor.capabilities.clone());
        if !overrides.is_empty() {
            descriptor.source = ModelSource::UserOverride;
        }
        self.state.models.insert(
            id,
            StoredModel {
                descriptor,
                overrides,
                base_capabilities: Some(base_capabilities),
                base_source: Some(base_source),
            },
        );
        Ok(())
    }

    /// Merge already parsed discovery results. This method performs no network
    /// I/O, so callers cannot accidentally couple an HTTP response to registry
    /// mutation.
    pub fn merge_discovered_models(
        &mut self,
        provider_id: &str,
        descriptors: Vec<ModelDescriptor>,
    ) -> Result<(), ProviderError> {
        if !self.state.providers.contains_key(provider_id) {
            return Err(ProviderError::ProviderNotFound(provider_id.into()));
        }
        for descriptor in &descriptors {
            if descriptor.key.provider_id != provider_id {
                return Err(ProviderError::InvalidModelKey(format!(
                    "discovered model {} belongs to provider {} instead of {provider_id}",
                    descriptor.key, descriptor.key.provider_id
                )));
            }
            ModelKey::new(
                descriptor.key.provider_id.clone(),
                descriptor.key.model_id.clone(),
            )?;
            if descriptor.display_name.trim().is_empty() {
                return Err(ProviderError::InvalidModelKey(
                    "model display_name must not be empty".into(),
                ));
            }
        }
        for mut descriptor in descriptors {
            descriptor.source = ModelSource::Remote;
            self.upsert_model(descriptor)?;
        }
        Ok(())
    }

    pub fn set_capability_overrides(
        &mut self,
        key: &ModelKey,
        overrides: CapabilityOverrides,
    ) -> Result<(), ProviderError> {
        let stored = self
            .state
            .models
            .get_mut(&key.to_string())
            .ok_or_else(|| ProviderError::ModelNotFound(key.to_string()))?;
        let base_capabilities = stored
            .base_capabilities
            .clone()
            .unwrap_or_else(|| stored.descriptor.capabilities.clone());
        let base_source = stored
            .base_source
            .clone()
            .unwrap_or_else(|| stored.descriptor.source.clone());
        stored.overrides = overrides;
        stored.base_capabilities = Some(base_capabilities.clone());
        stored.descriptor.capabilities = stored.overrides.apply_to(base_capabilities);
        stored.descriptor.source = if stored.overrides.is_empty() {
            base_source
        } else {
            ModelSource::UserOverride
        };
        Ok(())
    }

    pub fn set_default_model(&mut self, key: Option<ModelKey>) -> Result<(), ProviderError> {
        if let Some(key) = &key
            && !self.state.models.contains_key(&key.to_string())
        {
            return Err(ProviderError::ModelNotFound(key.to_string()));
        }
        self.state.default_model = key;
        Ok(())
    }

    pub fn save(&self) -> Result<(), ProviderError> {
        let path = self.path.as_ref().ok_or_else(|| {
            ProviderError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "in-memory provider registry has no persistence path",
            ))
        })?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let content = toml::to_string_pretty(&self.state)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(content.as_bytes())?;
        temp.as_file().sync_all()?;
        match temp.persist(path) {
            Ok(_) => Ok(()),
            Err(error) => {
                // Windows cannot rename over an existing file. The fallback is
                // intentionally explicit and still writes only the complete
                // serialized document, never a partially written config.
                std::fs::write(path, content).map_err(|fallback| {
                    ProviderError::Io(std::io::Error::new(
                        fallback.kind(),
                        format!("atomic provider config replacement failed: {error}; fallback failed: {fallback}"),
                    ))
                })
            }
        }
    }
}

pub mod rpc {
    pub const PROVIDER_LIST: &str = "_atelier/provider/list";
    pub const PROVIDER_CREATE: &str = "_atelier/provider/create";
    pub const PROVIDER_UPDATE: &str = "_atelier/provider/update";
    pub const PROVIDER_DELETE: &str = "_atelier/provider/delete";
    pub const PROVIDER_TEST: &str = "_atelier/provider/test";
    pub const PROVIDER_REFRESH_MODELS: &str = "_atelier/provider/refresh_models";
    pub const MODEL_LIST: &str = "_atelier/model/list";
    pub const MODEL_UPDATE: &str = "_atelier/model/update";
    pub const MODEL_SET_DEFAULT: &str = "_atelier/model/set_default";
    pub const CREDENTIAL_STATUS: &str = "_atelier/credential/status";
    pub const CREDENTIAL_SET: &str = "_atelier/credential/set";
    pub const CREDENTIAL_DELETE: &str = "_atelier/credential/delete";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_validation_rejects_path_like_ids() {
        let config = ProviderConfig {
            id: "bad/id".into(),
            display_name: "Bad".into(),
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: Url::parse("https://example.test").unwrap(),
            credential: CredentialRef::None,
            discovery: ProviderDiscovery::Disabled,
            extra_headers: BTreeMap::new(),
            enabled: true,
        };
        assert!(config.validate().is_err());
    }
}
