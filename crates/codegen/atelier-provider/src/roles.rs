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
    General,
    Explore,
    Implement,
    Review,
    Test,
    Compact,
    Summary,
    Title,
    Planner,
    Strategist,
    Skeptic,
}

impl RoleId {
    /// The supported roles in their stable public order.
    pub const ALL: [Self; 12] = [
        Self::Main,
        Self::General,
        Self::Explore,
        Self::Implement,
        Self::Review,
        Self::Test,
        Self::Compact,
        Self::Summary,
        Self::Title,
        Self::Planner,
        Self::Strategist,
        Self::Skeptic,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::General => "general",
            Self::Explore => "explore",
            Self::Implement => "implement",
            Self::Review => "review",
            Self::Test => "test",
            Self::Compact => "compact",
            Self::Summary => "summary",
            Self::Title => "title",
            Self::Planner => "planner",
            Self::Strategist => "strategist",
            Self::Skeptic => "skeptic",
        }
    }

    /// Fixed execution-settings parent. Main is represented by config.toml
    /// and is never persisted in roles.toml.
    pub const fn settings_parent(self) -> Option<Self> {
        match self {
            Self::Main => None,
            Self::General => Some(Self::Main),
            Self::Explore | Self::Implement | Self::Review | Self::Test => Some(Self::General),
            Self::Compact
            | Self::Summary
            | Self::Title
            | Self::Planner
            | Self::Strategist
            | Self::Skeptic => Some(Self::Main),
        }
    }

    /// Context inheritance is intentionally stricter than execution-settings
    /// inheritance: no non-main Role can acquire MAIN's custom context.
    pub const fn context_ancestry(self) -> &'static [Self] {
        match self {
            Self::Main => &[Self::Main],
            Self::General => &[Self::General],
            Self::Explore => &[Self::Explore, Self::General],
            Self::Implement => &[Self::Implement, Self::General],
            Self::Review => &[Self::Review, Self::General],
            Self::Test => &[Self::Test, Self::General],
            Self::Compact => &[Self::Compact],
            Self::Summary => &[Self::Summary],
            Self::Title => &[Self::Title],
            Self::Planner => &[Self::Planner],
            Self::Strategist => &[Self::Strategist],
            Self::Skeptic => &[Self::Skeptic],
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
            "general" => Ok(Self::General),
            "explore" => Ok(Self::Explore),
            "implement" => Ok(Self::Implement),
            "review" => Ok(Self::Review),
            "test" => Ok(Self::Test),
            "compact" => Ok(Self::Compact),
            "summary" => Ok(Self::Summary),
            "title" => Ok(Self::Title),
            "planner" => Ok(Self::Planner),
            "strategist" => Ok(Self::Strategist),
            "skeptic" => Ok(Self::Skeptic),
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
    #[error(
        "invalid role reasoning effort: {0} (expected one of: none, minimal, low, medium, high, xhigh, max)"
    )]
    InvalidEffort(String),
    #[error("role payload contains credential-like key: {0}")]
    SensitivePayloadKey(String),
    #[error("unknown role: {0}")]
    UnknownRole(String),
    #[error("role not found: {0}")]
    NotFound(RoleId),
    #[error("role override must configure at least one execution field")]
    EmptyOverride,
    #[error("MAIN is managed by config.toml and cannot be persisted in roles.toml")]
    MainManagedByConfig,
}

/// Configuration applied to one fixed role.
///
/// The payload is intentionally kept as JSON data so Provider adapters can
/// interpret it without making this crate depend on a protocol-specific
/// request type. Its `Debug` implementation redacts all payload values.
#[derive(Clone, PartialEq, Eq)]
pub struct RoleConfig {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub fast_mode: bool,
    pub payload: Map<String, Value>,
    provider_explicit: bool,
    model_explicit: bool,
    fast_mode_explicit: bool,
}

impl RoleConfig {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Result<Self, RoleError> {
        let config = Self {
            provider: provider.into(),
            model: model.into(),
            effort: None,
            fast_mode: false,
            payload: Map::new(),
            provider_explicit: true,
            model_explicit: true,
            fast_mode_explicit: true,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_fast_mode(enabled: bool) -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            effort: None,
            fast_mode: enabled,
            payload: Map::new(),
            provider_explicit: false,
            model_explicit: false,
            fast_mode_explicit: true,
        }
    }

    pub fn validate(&self) -> Result<(), RoleError> {
        if self.provider_explicit && self.provider.trim().is_empty() {
            return Err(RoleError::EmptyProvider);
        }
        if self.model_explicit && self.model.trim().is_empty() {
            return Err(RoleError::EmptyModel);
        }
        if let Some(effort) = self.effort.as_deref()
            && !is_valid_reasoning_effort(effort)
        {
            return Err(RoleError::InvalidEffort(effort.into()));
        }
        if let Some(key) = find_sensitive_payload_key(&self.payload) {
            return Err(RoleError::SensitivePayloadKey(key));
        }
        if !self.is_configured() {
            return Err(RoleError::EmptyOverride);
        }
        Ok(())
    }

    /// Every persisted role assignment is configured. Unconfigured roles are
    /// represented by the absence of an entry in [`RoleRegistry`].
    pub fn is_configured(&self) -> bool {
        self.provider_explicit
            || self.model_explicit
            || self.effort.is_some()
            || self.fast_mode_explicit
            || !self.payload.is_empty()
    }

    pub fn provider_override(&self) -> Option<&str> {
        self.provider_explicit.then_some(self.provider.as_str())
    }

    pub fn model_override(&self) -> Option<&str> {
        self.model_explicit.then_some(self.model.as_str())
    }

    pub fn fast_mode_override(&self) -> Option<bool> {
        self.fast_mode_explicit.then_some(self.fast_mode)
    }

    pub fn set_fast_mode(&mut self, enabled: bool) {
        self.fast_mode = enabled;
        self.fast_mode_explicit = true;
    }

    /// Apply a sparse management patch without dropping exact overrides that
    /// were omitted by the patch. Payload is either preserved or replaced as
    /// a whole; runtime parent/child payload inheritance remains a separate
    /// operation performed by [`RoleRegistry::resolve_inherited`].
    pub fn patched_with(&self, patch: &Self, preserve_payload: bool) -> Self {
        let mut merged = self.clone();
        if patch.provider_explicit {
            merged.provider.clone_from(&patch.provider);
            merged.provider_explicit = true;
        }
        if patch.model_explicit {
            merged.model.clone_from(&patch.model);
            merged.model_explicit = true;
        }
        if let Some(effort) = &patch.effort {
            merged.effort = Some(effort.clone());
        }
        if patch.fast_mode_explicit {
            merged.fast_mode = patch.fast_mode;
            merged.fast_mode_explicit = true;
        }
        if !preserve_payload {
            merged.payload.clone_from(&patch.payload);
        }
        merged
    }

    fn apply_to(&self, effective: &mut Self) {
        if self.provider_explicit {
            effective.provider.clone_from(&self.provider);
        }
        if self.model_explicit {
            effective.model.clone_from(&self.model);
        }
        if let Some(effort) = &self.effort {
            effective.effort = Some(effort.clone());
        }
        if self.fast_mode_explicit {
            effective.fast_mode = self.fast_mode;
        }
        effective.payload = merge_payloads(&effective.payload, &self.payload);
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
        if self.fast_mode_explicit {
            payload.insert("fast_mode".into(), Value::Bool(self.fast_mode));
        }
        payload
    }
}

fn is_valid_reasoning_effort(effort: &str) -> bool {
    matches!(
        effort.to_ascii_lowercase().as_str(),
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
    )
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

impl Serialize for RoleConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;
        let mut fields = 0;
        fields += usize::from(self.provider_explicit);
        fields += usize::from(self.model_explicit);
        fields += usize::from(self.effort.is_some());
        fields += usize::from(self.fast_mode_explicit);
        fields += usize::from(!self.payload.is_empty());
        let mut map = serializer.serialize_map(Some(fields))?;
        if self.provider_explicit {
            map.serialize_entry("provider", &self.provider)?;
        }
        if self.model_explicit {
            map.serialize_entry("model", &self.model)?;
        }
        if let Some(effort) = &self.effort {
            map.serialize_entry("effort", effort)?;
        }
        if self.fast_mode_explicit {
            map.serialize_entry("fast_mode", &self.fast_mode)?;
        }
        if !self.payload.is_empty() {
            map.serialize_entry("payload", &self.payload)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for RoleConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        RoleConfig::try_from(RoleConfigWire::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleConfigWire {
    provider: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    fast_mode: Option<bool>,
    payload: Option<Map<String, Value>>,
}

impl TryFrom<RoleConfigWire> for RoleConfig {
    type Error = RoleError;

    fn try_from(value: RoleConfigWire) -> Result<Self, Self::Error> {
        let provider_explicit = value.provider.is_some();
        let model_explicit = value.model.is_some();
        let fast_mode_explicit = value.fast_mode.is_some();
        let config = Self {
            provider: value.provider.unwrap_or_default(),
            model: value.model.unwrap_or_default(),
            effort: value.effort,
            fast_mode: value.fast_mode.unwrap_or(false),
            payload: value.payload.unwrap_or_default(),
            provider_explicit,
            model_explicit,
            fast_mode_explicit,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleFieldSources {
    pub provider: RoleId,
    pub model: RoleId,
    pub effort: Option<RoleId>,
    pub fast_mode: Option<RoleId>,
    pub payload: BTreeMap<String, RoleId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoleConfig {
    pub source: RoleId,
    pub config: RoleConfig,
    pub field_sources: RoleFieldSources,
}

/// A registry for the fixed set of Provider roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoleRegistry {
    roles: BTreeMap<RoleId, RoleConfig>,
}

impl Default for RoleRegistry {
    fn default() -> Self {
        Self {
            roles: BTreeMap::new(),
        }
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
        if role_id == RoleId::Main {
            return Err(RoleError::MainManagedByConfig);
        }
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
        if self.roles.contains_key(&RoleId::Main) {
            return Err(RoleError::MainManagedByConfig);
        }
        for config in self.roles.values() {
            config.validate()?;
        }
        Ok(())
    }

    /// Resolve a persisted Role assignment through fixed non-main parents.
    /// Returning `None` means execution settings must inherit MAIN/config.toml.
    pub fn find_inherited(&self, role_id: RoleId) -> Option<(RoleId, &RoleConfig)> {
        let mut current = Some(role_id);
        while let Some(candidate) = current {
            if candidate == RoleId::Main {
                return None;
            }
            if let Some(config) = self.find(candidate) {
                return Some((candidate, config));
            }
            current = candidate.settings_parent();
        }
        None
    }

    /// Resolve every execution field independently through the fixed settings
    /// chain. `main` is the synthesized config.toml execution profile and is
    /// never read from this registry. Parent payload keys are merged before
    /// child payload keys, while scalar fields use the nearest explicit value.
    pub fn resolve_inherited(&self, role_id: RoleId, main: &RoleConfig) -> (RoleId, RoleConfig) {
        let resolved = self.resolve_inherited_details(role_id, main);
        (resolved.source, resolved.config)
    }

    pub fn resolve_inherited_details(
        &self,
        role_id: RoleId,
        main: &RoleConfig,
    ) -> ResolvedRoleConfig {
        let mut chain = Vec::new();
        let mut current = Some(role_id);
        while let Some(candidate) = current {
            if candidate == RoleId::Main {
                break;
            }
            chain.push(candidate);
            current = candidate.settings_parent();
        }
        chain.reverse();

        let mut effective = main.clone();
        let mut source = RoleId::Main;
        let mut field_sources = RoleFieldSources {
            provider: RoleId::Main,
            model: RoleId::Main,
            effort: main.effort.as_ref().map(|_| RoleId::Main),
            fast_mode: main.fast_mode_explicit.then_some(RoleId::Main),
            payload: main
                .payload
                .keys()
                .map(|key| (key.clone(), RoleId::Main))
                .collect(),
        };
        for candidate in chain {
            if let Some(config) = self.find(candidate) {
                if config.provider_explicit {
                    field_sources.provider = candidate;
                }
                if config.model_explicit {
                    field_sources.model = candidate;
                }
                if config.effort.is_some() {
                    field_sources.effort = Some(candidate);
                }
                if config.fast_mode_explicit {
                    field_sources.fast_mode = Some(candidate);
                }
                for key in config.payload.keys() {
                    field_sources.payload.insert(key.clone(), candidate);
                }
                config.apply_to(&mut effective);
                source = candidate;
            }
        }
        ResolvedRoleConfig {
            source,
            config: effective,
            field_sources,
        }
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
