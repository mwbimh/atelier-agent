use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// The fixed role identifiers supported by the Provider runtime.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RoleId {
    Main,
    Explore,
    Implement,
    Review,
    Test,
    Compact,
    Summary,
    Title,
}

impl RoleId {
    /// The supported roles in their stable public order.
    pub const ALL: [Self; 8] = [
        Self::Main,
        Self::Explore,
        Self::Implement,
        Self::Review,
        Self::Test,
        Self::Compact,
        Self::Summary,
        Self::Title,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Explore => "explore",
            Self::Implement => "implement",
            Self::Review => "review",
            Self::Test => "test",
            Self::Compact => "compact",
            Self::Summary => "summary",
            Self::Title => "title",
        }
    }
}

impl fmt::Debug for RoleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Display for RoleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RoleId {
    type Err = RoleError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "main" => Ok(Self::Main),
            "explore" => Ok(Self::Explore),
            "implement" => Ok(Self::Implement),
            "review" => Ok(Self::Review),
            "test" => Ok(Self::Test),
            "compact" => Ok(Self::Compact),
            "summary" => Ok(Self::Summary),
            "title" => Ok(Self::Title),
            value => Err(RoleError::UnknownRole(value.into())),
        }
    }
}

impl Serialize for RoleId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RoleId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

/// Errors returned while validating or resolving a Provider role.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoleError {
    #[error("role provider must not be empty")]
    EmptyProvider,
    #[error("role model must not be empty")]
    EmptyModel,
    #[error("role payload contains credential-like key: {0}")]
    SensitivePayloadKey(String),
    #[error("unknown role: {0}")]
    UnknownRole(String),
    #[error("role not found: {0}")]
    NotFound(RoleId),
}

/// Configuration applied to one fixed role.
///
/// The payload is intentionally kept as JSON data so Provider adapters can
/// interpret it without making this crate depend on a protocol-specific
/// request type. Its `Debug` implementation redacts all payload values.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RoleConfigWire")]
pub struct RoleConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub fast_mode: bool,
    #[serde(default)]
    pub payload: Map<String, Value>,
}

impl RoleConfig {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Result<Self, RoleError> {
        let config = Self {
            provider: provider.into(),
            model: model.into(),
            effort: None,
            fast_mode: false,
            payload: Map::new(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), RoleError> {
        if self.provider.trim().is_empty() {
            return Err(RoleError::EmptyProvider);
        }
        if self.model.trim().is_empty() {
            return Err(RoleError::EmptyModel);
        }
        if let Some(key) = find_sensitive_payload_key(&self.payload) {
            return Err(RoleError::SensitivePayloadKey(key));
        }
        Ok(())
    }

    /// Return Provider defaults overlaid by this role's payload.
    ///
    /// This is a shallow merge. Role keys win on collisions, and the result
    /// is rebuilt in sorted key order to make serialization deterministic.
    pub fn merged_payload(&self, provider_defaults: &Map<String, Value>) -> Map<String, Value> {
        let role_payload = self.effective_payload();
        merge_payloads(provider_defaults, &role_payload)
    }

    /// Return the payload that should be sent for this role.
    ///
    /// `fast_mode` is kept as a first-class role setting for the management
    /// surface, but is transported as a provider-specific JSON field so the
    /// sampler remains protocol-neutral.
    pub fn effective_payload(&self) -> Map<String, Value> {
        let mut payload = self.payload.clone();
        if self.fast_mode {
            payload.insert("fast_mode".into(), Value::Bool(true));
        }
        payload
    }

    fn default_for(_role_id: RoleId) -> Self {
        Self {
            provider: "default".into(),
            model: "default".into(),
            effort: None,
            fast_mode: false,
            payload: Map::new(),
        }
    }
}

fn find_sensitive_payload_key(payload: &Map<String, Value>) -> Option<String> {
    payload.iter().find_map(|(key, value)| {
        if is_sensitive_payload_key(key) {
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

fn is_sensitive_payload_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect();

    matches!(
        normalized.as_str(),
        "apikey"
            | "authorization"
            | "authtoken"
            | "bearertoken"
            | "cookie"
            | "credential"
            | "idtoken"
            | "password"
            | "passwd"
            | "privatekey"
            | "refreshtoken"
            | "secret"
            | "sessiontoken"
            | "token"
    ) || normalized.ends_with("apikey")
        || normalized.contains("credential")
        || normalized.contains("secret")
}

impl fmt::Debug for RoleConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RoleConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .field("fast_mode", &self.fast_mode)
            .field("payload", &RedactedPayload(self.payload.len()))
            .finish()
    }
}

#[derive(Deserialize)]
struct RoleConfigWire {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    fast_mode: bool,
    #[serde(default)]
    payload: Map<String, Value>,
}

impl TryFrom<RoleConfigWire> for RoleConfig {
    type Error = RoleError;

    fn try_from(value: RoleConfigWire) -> Result<Self, Self::Error> {
        let config = Self {
            provider: value.provider,
            model: value.model,
            effort: value.effort,
            fast_mode: value.fast_mode,
            payload: value.payload,
        };
        config.validate()?;
        Ok(config)
    }
}

struct RedactedPayload(usize);

impl fmt::Debug for RedactedPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("REDACTED").field(&self.0).finish()
    }
}

/// A registry for the fixed set of Provider roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoleRegistry {
    roles: BTreeMap<RoleId, RoleConfig>,
}

impl Default for RoleRegistry {
    fn default() -> Self {
        let roles = RoleId::ALL
            .into_iter()
            .map(|role_id| (role_id, RoleConfig::default_for(role_id)))
            .collect();
        Self { roles }
    }
}

impl RoleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn defaults() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.roles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }

    pub fn find(&self, role_id: RoleId) -> Option<&RoleConfig> {
        self.roles.get(&role_id)
    }

    pub fn get(&self, role_id: RoleId) -> Option<&RoleConfig> {
        self.find(role_id)
    }

    pub fn find_by_name(&self, name: &str) -> Option<&RoleConfig> {
        name.parse().ok().and_then(|role_id| self.find(role_id))
    }

    pub fn get_mut(&mut self, role_id: RoleId) -> Option<&mut RoleConfig> {
        self.roles.get_mut(&role_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (RoleId, &RoleConfig)> + '_ {
        RoleId::ALL
            .into_iter()
            .filter_map(|role_id| self.roles.get(&role_id).map(|config| (role_id, config)))
    }

    pub fn ids(&self) -> impl Iterator<Item = RoleId> + '_ {
        RoleId::ALL
            .into_iter()
            .filter(|role_id| self.roles.contains_key(role_id))
    }

    pub fn update(&mut self, role_id: RoleId, config: RoleConfig) -> Result<(), RoleError> {
        config.validate()?;
        self.roles.insert(role_id, config);
        Ok(())
    }

    pub fn upsert(&mut self, role_id: RoleId, config: RoleConfig) -> Result<(), RoleError> {
        self.update(role_id, config)
    }

    pub fn delete(&mut self, role_id: RoleId) -> Option<RoleConfig> {
        self.roles.remove(&role_id)
    }

    pub fn remove(&mut self, role_id: RoleId) -> Option<RoleConfig> {
        self.delete(role_id)
    }

    pub fn validate(&self) -> Result<(), RoleError> {
        for config in self.roles.values() {
            config.validate()?;
        }
        Ok(())
    }

    pub fn merged_payload(
        &self,
        role_id: RoleId,
        provider_defaults: &Map<String, Value>,
    ) -> Result<Map<String, Value>, RoleError> {
        let role = self.find(role_id).ok_or(RoleError::NotFound(role_id))?;
        Ok(role.merged_payload(provider_defaults))
    }
}

/// Deterministically merge a Provider payload with a role payload.
pub fn merge_payloads(
    provider_defaults: &Map<String, Value>,
    role_payload: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = BTreeMap::new();
    merged.extend(
        provider_defaults
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    merged.extend(
        role_payload
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    merged.into_iter().collect()
}
