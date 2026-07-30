//! Provider and model management for the private Atelier runtime.
//!
//! This crate deliberately owns configuration and catalog state only. Network
//! adapters, UI code, and credential backends depend on these types instead of
//! making the runtime guess capabilities from a model name.

use serde::{Deserialize, Serialize};
pub mod auth;
pub mod roles;
mod storage_v2;

pub use roles::{
    ResolvedRoleConfig, RoleConfig, RoleError, RoleFieldSources, RoleId, RoleRegistry,
    fast_mode_from_payload, merge_payloads,
};
use serde_json::{Map, Value};
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

const CURRENT_SCHEMA_VERSION: u32 = 3;

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
    OAuthCredentialUnavailable,
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
    #[error("OAuth credential is unavailable for Provider {provider_id}: {reason}")]
    OAuthCredentialUnavailable { provider_id: String, reason: String },
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
            Self::OAuthCredentialUnavailable { .. } => {
                CredentialErrorCode::OAuthCredentialUnavailable
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
    #[error("invalid role: {0}")]
    InvalidRole(#[from] RoleError),
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

/// How a Provider credential is injected into HTTP requests.
///
/// Authentication belongs to the Provider connection. It is deliberately
/// independent from [`WireApi`]: a proxy can expose any model wire format while
/// still requiring either Bearer authentication or a custom credential header.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ProviderAuth {
    None,
    Bearer,
    Header { name: String },
}

/// Wire protocol used for one exact Provider/model request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WireApi {
    ChatCompletions,
    Responses,
    Messages,
}

impl WireApi {
    pub const fn endpoint_suffix(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat/completions",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }
}

/// Provider/model-specific request configuration.
///
/// The payload is deliberately limited to non-credential JSON fields. API
/// credentials and headers remain owned by [`ProviderConfig`].
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderModelOverride {
    #[serde(default)]
    pub wire_api: Option<WireApi>,
    #[serde(default)]
    pub payload: Map<String, Value>,
}

/// Provider-independent model settings.  The catalog still keeps
/// `ModelKey` entries for selecting a concrete provider/model pair, while
/// these settings hold the model-wide Wire API and pair-specific overrides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelConfig {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub wire_api: Option<WireApi>,
    #[serde(default)]
    pub provider_overrides: BTreeMap<String, ProviderModelOverride>,
}

impl ModelConfig {
    fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            wire_api: None,
            provider_overrides: BTreeMap::new(),
        }
    }
}

impl ProviderModelOverride {
    pub fn empty() -> Self {
        Self {
            wire_api: None,
            payload: Map::new(),
        }
    }

    pub fn validate(&self) -> Result<(), ProviderError> {
        if let Some(key) = find_sensitive_payload_key(&self.payload) {
            return Err(ProviderError::InvalidProvider(format!(
                "model override payload contains credential-like key: {key}"
            )));
        }
        Ok(())
    }
}

impl fmt::Debug for ProviderModelOverride {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderModelOverride")
            .field("wire_api", &self.wire_api)
            .field("payload", &RedactedPayload(self.payload.len()))
            .finish()
    }
}

struct RedactedPayload(usize);

impl fmt::Debug for RedactedPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("REDACTED").field(&self.0).finish()
    }
}

fn find_sensitive_payload_key(payload: &Map<String, Value>) -> Option<String> {
    payload.iter().find_map(|(key, value)| {
        let normalized: String = key
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .map(|character| character.to_ascii_lowercase())
            .collect();
        let sensitive = matches!(
            normalized.as_str(),
            "apikey"
                | "authorization"
                | "authtoken"
                | "bearertoken"
                | "cookie"
                | "credential"
                | "password"
                | "passwd"
                | "privatekey"
                | "refreshtoken"
                | "secret"
                | "sessiontoken"
                | "token"
        ) || normalized.ends_with("apikey")
            || normalized.contains("credential")
            || normalized.contains("secret");
        if sensitive {
            return Some(key.clone());
        }
        match value {
            Value::Object(object) => find_sensitive_payload_key(object),
            Value::Array(values) => values.iter().find_map(|value| match value {
                Value::Object(object) => find_sensitive_payload_key(object),
                _ => None,
            }),
            _ => None,
        }
    })
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
    OAuth {
        provider_id: String,
        methods: Vec<auth::ProviderOAuthMethod>,
    },
}

impl CredentialRef {
    /// Return whether this reference can supply credentials when a Provider
    /// request is made. Providers that intentionally require no secret use
    /// `CredentialRef::None` and are therefore considered available.
    pub fn is_available(&self) -> bool {
        match self {
            Self::None => true,
            _ => self.resolve().ok().flatten().is_some(),
        }
    }

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
            Self::OAuth {
                provider_id,
                methods,
            } => {
                if methods.is_empty() {
                    return Err(CredentialError::InvalidReference {
                        kind: "oauth",
                        reason: "at least one OAuth method must be configured".into(),
                    });
                }
                let mut flows = std::collections::BTreeSet::new();
                for method in methods {
                    method.validate(provider_id).map_err(|error| {
                        CredentialError::InvalidReference {
                            kind: "oauth",
                            reason: error.to_string(),
                        }
                    })?;
                    if !flows.insert(method.flow_name()) {
                        return Err(CredentialError::InvalidReference {
                            kind: "oauth",
                            reason: format!(
                                "OAuth flow {} is configured more than once",
                                method.flow_name()
                            ),
                        });
                    }
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
            Self::OAuth { provider_id, .. } => auth::resolve_system_access_token(provider_id)
                .map_err(|error| CredentialError::OAuthCredentialUnavailable {
                    provider_id: provider_id.clone(),
                    reason: error.to_string(),
                }),
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

#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    pub id: String,
    pub display_name: String,
    pub base_url: Url,
    #[serde(default)]
    pub credential: CredentialRef,
    pub auth: ProviderAuth,
    #[serde(default)]
    pub discovery: ProviderDiscovery,
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

const REDACTED: &str = "REDACTED";

#[derive(Serialize)]
struct ProviderConfigWire<'a> {
    id: &'a str,
    display_name: &'a str,
    base_url: &'a Url,
    credential: &'a CredentialRef,
    auth: &'a ProviderAuth,
    discovery: &'a ProviderDiscovery,
    extra_headers: BTreeMap<String, String>,
    enabled: bool,
}

impl Serialize for ProviderConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let extra_headers = self
            .extra_headers
            .iter()
            .map(|(name, value)| {
                let value = if is_sensitive_header_name(name) {
                    REDACTED.to_owned()
                } else {
                    value.clone()
                };
                (name.clone(), value)
            })
            .collect();
        ProviderConfigWire {
            id: &self.id,
            display_name: &self.display_name,
            base_url: &self.base_url,
            credential: &self.credential,
            auth: &self.auth,
            discovery: &self.discovery,
            extra_headers,
            enabled: self.enabled,
        }
        .serialize(serializer)
    }
}

struct RedactedHeaders<'a>(&'a BTreeMap<String, String>);

impl fmt::Debug for RedactedHeaders<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = formatter.debug_map();
        for name in self.0.keys() {
            map.entry(name, &REDACTED);
        }
        map.finish()
    }
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("id", &self.id)
            .field("display_name", &self.display_name)
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .field("auth", &self.auth)
            .field("discovery", &self.discovery)
            .field("extra_headers", &RedactedHeaders(&self.extra_headers))
            .field("enabled", &self.enabled)
            .finish()
    }
}

fn is_sensitive_header_name(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect();

    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "apikey"
            | "xapikey"
            | "accesskey"
            | "authtoken"
            | "accesstoken"
            | "refreshtoken"
            | "password"
            | "passwd"
    ) || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("credential")
}

fn is_valid_http_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn is_reserved_transport_header_name(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect();
    normalized == "useragent"
}

fn redacted_snapshot_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .keys()
        .map(|name| (name.clone(), REDACTED.to_owned()))
        .collect()
}

fn default_enabled() -> bool {
    true
}

impl ProviderConfig {
    pub fn validate(&self) -> Result<(), ProviderError> {
        validate_identifier(&self.id, "provider id")?;
        self.credential.validate()?;
        if let ProviderAuth::Header { name } = &self.auth {
            if !is_valid_http_header_name(name) {
                return Err(ProviderError::InvalidProvider(
                    "credential header name must be a valid HTTP field name".into(),
                ));
            }
            if is_reserved_transport_header_name(name) {
                return Err(ProviderError::InvalidProvider(format!(
                    "credential header '{name}' cannot override the configured User-Agent"
                )));
            }
        }
        if !matches!(&self.credential, CredentialRef::None)
            && matches!(&self.auth, ProviderAuth::None)
        {
            return Err(ProviderError::InvalidProvider(
                "configured credential requires a Provider auth policy".into(),
            ));
        }
        if let CredentialRef::OAuth { provider_id, .. } = &self.credential
            && provider_id != &self.id
        {
            return Err(ProviderError::InvalidProvider(format!(
                "OAuth credential Provider id {provider_id} does not match {}",
                self.id
            )));
        }
        if let Some(name) = self
            .extra_headers
            .keys()
            .find(|name| is_sensitive_header_name(name))
        {
            return Err(ProviderError::InvalidProvider(format!(
                "extra header '{name}' contains credential material; use the provider credential reference instead"
            )));
        }
        if let Some(name) = self
            .extra_headers
            .keys()
            .find(|name| is_reserved_transport_header_name(name))
        {
            return Err(ProviderError::InvalidProvider(format!(
                "extra header '{name}' cannot override the configured User-Agent"
            )));
        }
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
    fn from_capabilities(capabilities: &ModelCapabilities) -> Self {
        Self {
            text_input: Some(capabilities.text_input),
            image_input: Some(capabilities.image_input),
            tool_calls: Some(capabilities.tool_calls),
            parallel_tool_calls: Some(capabilities.parallel_tool_calls),
            reasoning_effort: Some(capabilities.reasoning_effort),
            web_search: Some(capabilities.web_search),
            image_generation: Some(capabilities.image_generation),
            server_compaction: Some(capabilities.server_compaction),
        }
    }

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
    /// Model-wide wire API. A provider/model override takes precedence.
    #[serde(default)]
    pub wire_api: Option<WireApi>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    #[serde(default)]
    pub default_effort: Option<String>,
    #[serde(default)]
    pub fast_mode: bool,
    #[serde(default)]
    pub source: ModelSource,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Experimental Provider-specific endpoint. Endpoints are relative to the
/// Provider base URL; absolute URLs are rejected so a model profile cannot
/// silently redirect credentials to another origin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperimentalEndpoint {
    #[serde(default)]
    pub enabled: bool,
    pub endpoint: String,
}

impl ExperimentalEndpoint {
    fn validate(&self) -> Result<(), ProviderError> {
        let endpoint = self.endpoint.trim();
        let unsafe_segment = endpoint
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."));
        if endpoint.is_empty()
            || endpoint != self.endpoint
            || endpoint.starts_with(['/', '\\'])
            || endpoint.contains(['\\', '?', '#', '%'])
            || endpoint.chars().any(char::is_control)
            || Url::parse(endpoint).is_ok()
            || unsafe_segment
        {
            return Err(ProviderError::InvalidProvider(format!(
                "experimental endpoint must be a safe Provider-relative path: {}",
                self.endpoint
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperimentalModelFeatures {
    #[serde(default)]
    pub remote_compaction: Option<ExperimentalEndpoint>,
    #[serde(default)]
    pub image_generation: Option<ExperimentalEndpoint>,
}

impl ExperimentalModelFeatures {
    fn validate(&self) -> Result<(), ProviderError> {
        if let Some(endpoint) = &self.remote_compaction {
            endpoint.validate()?;
        }
        if let Some(endpoint) = &self.image_generation {
            endpoint.validate()?;
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
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
    let wire_api = parse_discovered_wire_api(object, index)?;

    Ok(ModelDescriptor {
        key,
        display_name,
        description: json_string(object, &["description"]),
        wire_api,
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
        default_effort: json_string(object, &["default_effort", "defaultEffort"]),
        fast_mode: parse_fast_mode_capability(object),
        source: ModelSource::Remote,
        enabled: true,
    })
}

fn parse_fast_mode_capability(object: &serde_json::Map<String, Value>) -> bool {
    let canonical_service_tier = object
        .get("service_tiers")
        .or_else(|| object.get("serviceTiers"))
        .and_then(Value::as_array)
        .is_some_and(|tiers| {
            tiers.iter().any(|tier| {
                tier.as_str()
                    .or_else(|| tier.as_object()?.get("id")?.as_str())
                    .is_some_and(|id| id.eq_ignore_ascii_case("priority"))
            })
        });
    if canonical_service_tier {
        return true;
    }

    let legacy_fast_tier = object
        .get("additional_speed_tiers")
        .or_else(|| object.get("additionalSpeedTiers"))
        .and_then(Value::as_array)
        .is_some_and(|tiers| {
            tiers.iter().filter_map(Value::as_str).any(|tier| {
                tier.eq_ignore_ascii_case("fast") || tier.eq_ignore_ascii_case("priority")
            })
        });
    legacy_fast_tier
        || json_bool(
            object,
            &[
                "fast_mode",
                "fastMode",
                "supports_fast_mode",
                "supportsFastMode",
            ],
        )
        .unwrap_or(false)
}

fn parse_discovered_wire_api(
    object: &serde_json::Map<String, Value>,
    index: usize,
) -> Result<Option<WireApi>, ProviderError> {
    let Some(value) = object.get("wire_api").or_else(|| object.get("wireApi")) else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err(ProviderError::DiscoveryInvalid(format!(
            "model entry {index} wire API must be a string"
        )));
    };
    let wire_api = match value {
        "responses" => WireApi::Responses,
        "chat_completions" => WireApi::ChatCompletions,
        "messages" => WireApi::Messages,
        _ => {
            return Err(ProviderError::DiscoveryInvalid(format!(
                "model entry {index} has unknown wire API {value:?}"
            )));
        }
    };
    Ok(Some(wire_api))
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
    roles: RoleRegistry,
    #[serde(default)]
    model_provider_overrides: BTreeMap<String, ProviderModelOverride>,
    #[serde(skip)]
    model_profiles: BTreeMap<String, storage_v2::ModelProfileDisk>,
}

impl Default for RegistryFile {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
            roles: RoleRegistry::default(),
            model_provider_overrides: BTreeMap::new(),
            model_profiles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ProviderSnapshot {
    pub providers: Vec<ProviderConfig>,
    pub models: Vec<ModelDescriptor>,
    pub model_provider_overrides: BTreeMap<String, ProviderModelOverride>,
    /// Provider/model-exact experimental controls. This map is populated only
    /// from `models/providers/<provider>/models.toml`; common presets and
    /// discovery metadata can never contribute entries.
    #[serde(default)]
    pub experimental_model_features: BTreeMap<String, ExperimentalModelFeatures>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WireApiSource {
    ProviderModelOverride,
    ModelDefinition,
    /// No exact transport metadata was available. Atelier uses the documented
    /// OpenAI-compatible baseline instead of hiding the discovered model.
    Default,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedWireApi {
    pub provider: String,
    pub model: String,
    pub wire_api: WireApi,
    pub source: WireApiSource,
}

#[derive(Serialize)]
struct ProviderSnapshotProvider<'a> {
    id: &'a str,
    display_name: &'a str,
    base_url: &'a Url,
    credential: &'a CredentialRef,
    auth: &'a ProviderAuth,
    discovery: &'a ProviderDiscovery,
    extra_headers: BTreeMap<String, String>,
    enabled: bool,
}

#[derive(Serialize)]
struct ProviderSnapshotWire<'a> {
    providers: Vec<ProviderSnapshotProvider<'a>>,
    models: &'a [ModelDescriptor],
    model_provider_overrides: &'a BTreeMap<String, ProviderModelOverride>,
    experimental_model_features: &'a BTreeMap<String, ExperimentalModelFeatures>,
}

impl Serialize for ProviderSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let providers = self
            .providers
            .iter()
            .map(|provider| ProviderSnapshotProvider {
                id: &provider.id,
                display_name: &provider.display_name,
                base_url: &provider.base_url,
                credential: &provider.credential,
                auth: &provider.auth,
                discovery: &provider.discovery,
                extra_headers: redacted_snapshot_headers(&provider.extra_headers),
                enabled: provider.enabled,
            })
            .collect();
        ProviderSnapshotWire {
            providers,
            models: &self.models,
            model_provider_overrides: &self.model_provider_overrides,
            experimental_model_features: &self.experimental_model_features,
        }
        .serialize(serializer)
    }
}

impl ProviderSnapshot {
    pub fn resolve_wire_api(&self, key: &ModelKey) -> Result<ResolvedWireApi, ProviderError> {
        let model = self
            .models
            .iter()
            .find(|model| model.key == *key)
            .ok_or_else(|| ProviderError::ModelNotFound(key.to_string()))?;
        self.providers
            .iter()
            .find(|provider| provider.id == key.provider_id)
            .ok_or_else(|| ProviderError::ProviderNotFound(key.provider_id.clone()))?;
        if let Some(override_config) = self.model_provider_overrides.get(&key.to_string())
            && let Some(wire_api) = override_config.wire_api
        {
            return Ok(ResolvedWireApi {
                provider: key.provider_id.clone(),
                model: key.model_id.clone(),
                wire_api,
                source: WireApiSource::ProviderModelOverride,
            });
        }
        if let Some(wire_api) = model.wire_api {
            return Ok(ResolvedWireApi {
                provider: key.provider_id.clone(),
                model: key.model_id.clone(),
                wire_api,
                source: WireApiSource::ModelDefinition,
            });
        }
        Ok(ResolvedWireApi {
            provider: key.provider_id.clone(),
            model: key.model_id.clone(),
            wire_api: WireApi::ChatCompletions,
            source: WireApiSource::Default,
        })
    }

    /// Resolve an active remote-compaction endpoint for one exact
    /// Provider/model pair. A configured endpoint is inert unless the exact
    /// profile enables it and the effective wire API is Responses.
    pub fn resolve_remote_compaction_endpoint(
        &self,
        key: &ModelKey,
    ) -> Result<Option<String>, ProviderError> {
        if self.resolve_wire_api(key)?.wire_api != WireApi::Responses {
            return Ok(None);
        }
        let Some(endpoint) = self
            .experimental_model_features
            .get(&key.to_string())
            .and_then(|features| features.remote_compaction.as_ref())
            .filter(|endpoint| endpoint.enabled)
        else {
            return Ok(None);
        };
        endpoint.validate()?;
        Ok(Some(endpoint.endpoint.clone()))
    }

    /// Resolve an active OpenAI Images-compatible endpoint for one exact
    /// Provider/model pair. Common presets and discovery metadata never enter
    /// `experimental_model_features`, so a same-named model under another
    /// Provider cannot inherit this route.
    pub fn resolve_image_generation_endpoint(
        &self,
        key: &ModelKey,
    ) -> Result<Option<String>, ProviderError> {
        if self.resolve_wire_api(key)?.wire_api == WireApi::Messages {
            return Ok(None);
        }
        let Some(endpoint) = self
            .experimental_model_features
            .get(&key.to_string())
            .and_then(|features| features.image_generation.as_ref())
            .filter(|endpoint| endpoint.enabled)
        else {
            return Ok(None);
        };
        endpoint.validate()?;
        Ok(Some(endpoint.endpoint.clone()))
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    paths: Option<storage_v2::RegistryPaths>,
    state: RegistryFile,
}

impl ProviderRegistry {
    pub fn in_memory() -> Self {
        Self {
            paths: None,
            state: RegistryFile::default(),
        }
    }

    pub fn load_or_create(path: impl Into<PathBuf>) -> Result<Self, ProviderError> {
        let paths = storage_v2::RegistryPaths::from_provider_path(path.into());
        let state = storage_v2::load(&paths)?;
        Ok(Self {
            paths: Some(paths),
            state,
        })
    }

    pub fn snapshot(&self) -> ProviderSnapshot {
        let model_provider_overrides = self.state.model_provider_overrides.clone();
        let experimental_model_features = self
            .state
            .model_profiles
            .iter()
            .filter_map(|(key, profile)| {
                (!profile.experimental.is_empty())
                    .then(|| (key.clone(), profile.experimental.clone()))
            })
            .collect();
        ProviderSnapshot {
            providers: self.state.providers.values().cloned().collect(),
            models: self
                .state
                .models
                .values()
                .map(|stored| stored.descriptor.clone())
                .collect(),
            model_provider_overrides,
            experimental_model_features,
        }
    }

    pub fn model_config(&self, model_id: &str) -> Option<ModelConfig> {
        self.state
            .models
            .values()
            .find(|stored| stored.descriptor.key.model_id == model_id)
            .map(|stored| ModelConfig {
                id: model_id.to_owned(),
                display_name: stored.descriptor.display_name.clone(),
                wire_api: stored.descriptor.wire_api,
                provider_overrides: BTreeMap::new(),
            })
    }

    pub fn providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.state.providers.values()
    }

    pub fn roles(&self) -> &RoleRegistry {
        &self.state.roles
    }

    pub fn role(&self, role_id: RoleId) -> Option<&RoleConfig> {
        self.state.roles.find(role_id)
    }

    pub fn update_role(
        &mut self,
        role_id: RoleId,
        config: RoleConfig,
    ) -> Result<(), ProviderError> {
        self.state.roles.update(role_id, config)?;
        Ok(())
    }

    pub fn remove_role(&mut self, role_id: RoleId) -> Option<RoleConfig> {
        self.state.roles.delete(role_id)
    }

    pub fn merged_role_payload(
        &self,
        role_id: RoleId,
        provider_defaults: &Map<String, Value>,
    ) -> Result<Map<String, Value>, ProviderError> {
        Ok(self
            .state
            .roles
            .merged_payload(role_id, provider_defaults)?)
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

    pub fn model_provider_override(&self, key: &ModelKey) -> Option<ProviderModelOverride> {
        self.state
            .model_provider_overrides
            .get(&key.to_string())
            .cloned()
    }

    pub fn set_model_wire_api(
        &mut self,
        key: &ModelKey,
        wire_api: Option<WireApi>,
    ) -> Result<(), ProviderError> {
        let stored = self
            .state
            .models
            .get_mut(&key.to_string())
            .ok_or_else(|| ProviderError::ModelNotFound(key.to_string()))?;
        stored.descriptor.wire_api = wire_api;
        let profile = self
            .state
            .model_profiles
            .entry(key.to_string())
            .or_default();
        profile.wire_api = wire_api;
        if let Some(wire_api) = wire_api {
            self.state
                .model_provider_overrides
                .entry(key.to_string())
                .or_insert_with(ProviderModelOverride::empty)
                .wire_api = Some(wire_api);
        } else if let Some(override_config) = self
            .state
            .model_provider_overrides
            .get_mut(&key.to_string())
        {
            override_config.wire_api = None;
            if override_config.payload.is_empty() {
                self.state.model_provider_overrides.remove(&key.to_string());
            }
        }
        Ok(())
    }

    pub fn set_model_provider_override(
        &mut self,
        key: &ModelKey,
        override_config: ProviderModelOverride,
    ) -> Result<(), ProviderError> {
        override_config.validate()?;
        if !self.state.models.contains_key(&key.to_string()) {
            return Err(ProviderError::ModelNotFound(key.to_string()));
        }
        if override_config.wire_api.is_none() && override_config.payload.is_empty() {
            self.state.model_provider_overrides.remove(&key.to_string());
            let profile = self
                .state
                .model_profiles
                .entry(key.to_string())
                .or_default();
            profile.wire_api = None;
            profile.payload.clear();
        } else {
            let profile = self
                .state
                .model_profiles
                .entry(key.to_string())
                .or_default();
            profile.wire_api = override_config.wire_api;
            profile.payload = override_config.payload.clone();
            self.state
                .model_provider_overrides
                .insert(key.to_string(), override_config);
        }
        Ok(())
    }

    pub fn remove_model_provider_override(
        &mut self,
        key: &ModelKey,
    ) -> Result<bool, ProviderError> {
        if !self.state.models.contains_key(&key.to_string()) {
            return Err(ProviderError::ModelNotFound(key.to_string()));
        }
        let removed = self
            .state
            .model_provider_overrides
            .remove(&key.to_string())
            .is_some();
        if let Some(profile) = self.state.model_profiles.get_mut(&key.to_string()) {
            profile.wire_api = None;
            profile.payload.clear();
        }
        Ok(removed)
    }

    pub fn resolve_wire_api(&self, key: &ModelKey) -> Result<ResolvedWireApi, ProviderError> {
        self.snapshot().resolve_wire_api(key)
    }

    pub fn experimental_model_features(
        &self,
        key: &ModelKey,
    ) -> Result<ExperimentalModelFeatures, ProviderError> {
        if !self.state.models.contains_key(&key.to_string()) {
            return Err(ProviderError::ModelNotFound(key.to_string()));
        }
        Ok(self
            .state
            .model_profiles
            .get(&key.to_string())
            .map(|profile| profile.experimental.clone())
            .unwrap_or_default())
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
        self.state
            .model_provider_overrides
            .retain(|key, _| !key.starts_with(&format!("{provider_id}/")));
        self.state
            .model_profiles
            .retain(|key, _| !key.starts_with(&format!("{provider_id}/")));
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
        let persist_profile = descriptor.source != ModelSource::Remote;
        let overrides = self
            .state
            .models
            .get(&id)
            .map(|stored| stored.overrides.clone())
            .unwrap_or_default();
        let base_capabilities = descriptor.capabilities.clone();
        let base_source = descriptor.source.clone();
        let mut descriptor = descriptor;
        if descriptor.wire_api.is_none()
            && let Some(existing) = self.state.models.get(&id)
        {
            descriptor.wire_api = existing.descriptor.wire_api;
        }
        descriptor.capabilities = overrides.apply_to(descriptor.capabilities.clone());
        if !overrides.is_empty() {
            descriptor.source = ModelSource::UserOverride;
        }
        self.state.models.insert(
            id.clone(),
            StoredModel {
                descriptor,
                overrides,
                base_capabilities: Some(base_capabilities),
                base_source: Some(base_source),
            },
        );
        if persist_profile {
            let stored = &self.state.models[&id].descriptor;
            self.state.model_profiles.insert(
                id,
                storage_v2::ModelProfileDisk {
                    display_name: Some(stored.display_name.clone()),
                    description: stored.description.clone(),
                    wire_api: stored.wire_api,
                    context_window: stored.context_window,
                    reasoning_efforts: stored.reasoning_efforts.clone(),
                    default_effort: stored.default_effort.clone(),
                    fast_mode: Some(stored.fast_mode),
                    service_tiers: Vec::new(),
                    capabilities: CapabilityOverrides::from_capabilities(&stored.capabilities),
                    payload: Map::new(),
                    enabled: Some(stored.enabled),
                    experimental: ExperimentalModelFeatures::default(),
                },
            );
        }
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
        let discovered_keys = descriptors
            .iter()
            .map(|descriptor| descriptor.key.to_string())
            .collect::<std::collections::HashSet<_>>();
        self.state.models.retain(|key, stored| {
            let belongs_to_provider = stored.descriptor.key.provider_id == provider_id;
            let remote_backed = stored.descriptor.source == ModelSource::Remote
                || stored.base_source == Some(ModelSource::Remote);
            !belongs_to_provider || !remote_backed || discovered_keys.contains(key)
        });
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
        self.state
            .model_profiles
            .entry(key.to_string())
            .or_default()
            .capabilities = stored.overrides.clone();
        stored.base_capabilities = Some(base_capabilities.clone());
        stored.descriptor.capabilities = stored.overrides.apply_to(base_capabilities);
        stored.descriptor.source = if stored.overrides.is_empty() {
            base_source
        } else {
            ModelSource::UserOverride
        };
        Ok(())
    }

    pub fn save(&self) -> Result<(), ProviderError> {
        for provider in self.state.providers.values() {
            provider.validate()?;
        }
        for override_config in self.state.model_provider_overrides.values() {
            override_config.validate()?;
        }
        self.state.roles.validate()?;
        let paths = self.paths.as_ref().ok_or_else(|| {
            ProviderError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "in-memory provider registry has no persistence path",
            ))
        })?;
        storage_v2::save(paths, &self.state)
    }
}

#[cfg(not(windows))]
fn persist_provider_temp_file(
    temp: tempfile::NamedTempFile,
    path: &Path,
) -> Result<(), ProviderError> {
    temp.persist(path)
        .map(|_| ())
        .map_err(|error| ProviderError::Io(error.error))
}

#[cfg(windows)]
fn persist_provider_temp_file(
    temp: tempfile::NamedTempFile,
    path: &Path,
) -> Result<(), ProviderError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let temp_path = temp.into_temp_path();
    let source = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| {
        ProviderError::Io(std::io::Error::other(format!(
            "atomic provider config replacement failed: {error}"
        )))
    })?;
    Ok(())
}

pub mod rpc {
    pub const PROVIDER_LIST: &str = "_atelier/provider/list";
    pub const PROVIDER_CREATE: &str = "_atelier/provider/create";
    pub const PROVIDER_UPDATE: &str = "_atelier/provider/update";
    pub const PROVIDER_DELETE: &str = "_atelier/provider/delete";
    pub const PROVIDER_TEST: &str = "_atelier/provider/test";
    pub const PROVIDER_REFRESH_MODELS: &str = "_atelier/provider/refresh_models";
    pub const PROVIDER_OAUTH_BEGIN: &str = "_atelier/provider/oauth_begin";
    pub const PROVIDER_OAUTH_COMPLETE: &str = "_atelier/provider/oauth_complete";
    pub const PROVIDER_OAUTH_LOGOUT: &str = "_atelier/provider/oauth_logout";
    pub const MODEL_LIST: &str = "_atelier/model/list";
    pub const MODEL_UPDATE: &str = "_atelier/model/update";
    pub const MODEL_GET: &str = "_atelier/model/get";
    pub const MODEL_UPDATE_WIRE_API: &str = "_atelier/model/update_wire_api";
    pub const MODEL_PROVIDER_OVERRIDE_LIST: &str = "_atelier/model_provider_override/list";
    pub const MODEL_PROVIDER_OVERRIDE_SET: &str = "_atelier/model_provider_override/set";
    pub const MODEL_PROVIDER_OVERRIDE_DELETE: &str = "_atelier/model_provider_override/delete";
    pub const MODEL_PROVIDER_OVERRIDE_TEST: &str = "_atelier/model_provider_override/test";
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
            base_url: Url::parse("https://example.test").unwrap(),
            credential: CredentialRef::None,
            auth: ProviderAuth::None,
            discovery: ProviderDiscovery::Disabled,
            extra_headers: BTreeMap::new(),
            enabled: true,
        };
        assert!(config.validate().is_err());
    }
}
