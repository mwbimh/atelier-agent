use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const ROLES_SCHEMA_VERSION: u32 = 1;
const PROVIDER_MODELS_SCHEMA_VERSION: u32 = 1;
const COMMON_MODELS_SCHEMA_VERSION: u32 = 2;
const CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub(crate) struct RegistryPaths {
    pub providers: PathBuf,
    pub roles: PathBuf,
    pub default_models: PathBuf,
    pub provider_models: PathBuf,
    pub provider_cache: PathBuf,
}

impl RegistryPaths {
    pub(crate) fn from_provider_path(path: PathBuf) -> Self {
        let home = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Self {
            providers: path,
            roles: home.join("roles.toml"),
            default_models: home.join("models/default"),
            provider_models: home.join("models/providers"),
            provider_cache: home.join("cache/providers"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvidersFile {
    schema_version: u32,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfigDisk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConfigDisk {
    display_name: String,
    base_url: Url,
    #[serde(default)]
    credential: CredentialRef,
    auth: ProviderAuth,
    #[serde(default)]
    discovery: ProviderDiscovery,
    #[serde(default)]
    extra_headers: BTreeMap<String, String>,
    #[serde(default = "disk_default_enabled")]
    enabled: bool,
}

fn disk_default_enabled() -> bool {
    true
}

impl ProviderConfigDisk {
    fn into_runtime(self, id: String) -> ProviderConfig {
        ProviderConfig {
            id,
            display_name: self.display_name,
            base_url: self.base_url,
            credential: self.credential,
            auth: self.auth,
            discovery: self.discovery,
            extra_headers: self.extra_headers,
            enabled: self.enabled,
        }
    }

    fn from_runtime(config: &ProviderConfig) -> Self {
        Self {
            display_name: config.display_name.clone(),
            base_url: config.base_url.clone(),
            credential: config.credential.clone(),
            auth: config.auth.clone(),
            discovery: config.discovery.clone(),
            extra_headers: config.extra_headers.clone(),
            enabled: config.enabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RolesFile {
    schema_version: u32,
    #[serde(default)]
    roles: RoleRegistry,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ModelProfileDisk {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub wire_api: Option<WireApi>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    #[serde(default)]
    pub default_effort: Option<String>,
    #[serde(default)]
    pub fast_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_tiers: Vec<String>,
    #[serde(default)]
    pub capabilities: CapabilityOverrides,
    #[serde(default)]
    pub payload: Map<String, Value>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub experimental: ExperimentalModelFeatures,
}

impl ModelProfileDisk {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    fn validate(&self, label: &str) -> Result<(), ProviderError> {
        if self.context_window == Some(0) {
            return Err(ProviderError::InvalidProvider(format!(
                "model profile {label} has a zero context_window"
            )));
        }

        let mut seen = BTreeSet::new();
        for effort in &self.reasoning_efforts {
            if !matches!(
                effort.as_str(),
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            ) {
                return Err(ProviderError::InvalidProvider(format!(
                    "model profile {label} has unsupported reasoning effort {effort:?}"
                )));
            }
            if !seen.insert(effort.as_str()) {
                return Err(ProviderError::InvalidProvider(format!(
                    "model profile {label} repeats reasoning effort {effort:?}"
                )));
            }
        }

        if let Some(default_effort) = self.default_effort.as_deref()
            && !seen.contains(default_effort)
        {
            return Err(ProviderError::InvalidProvider(format!(
                "model profile {label} default reasoning effort {default_effort:?} is not present in reasoning_efforts"
            )));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderModelsFile {
    schema_version: u32,
    #[serde(default)]
    models: BTreeMap<String, ModelProfileDisk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommonModelsFile {
    schema_version: u32,
    #[serde(default)]
    models: BTreeMap<String, ModelProfileDisk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderModelCache {
    schema_version: u32,
    provider_id: String,
    #[serde(default)]
    models: Vec<ModelDescriptor>,
}

pub(crate) fn load(paths: &RegistryPaths) -> Result<RegistryFile, ProviderError> {
    let providers = if paths.providers.exists() {
        let source = std::fs::read_to_string(&paths.providers)?;
        let file: ProvidersFile = toml::from_str(&source)?;
        require_schema(
            "providers.toml",
            file.schema_version,
            CURRENT_SCHEMA_VERSION,
        )?;
        file.providers
            .into_iter()
            .map(|(id, config)| {
                let config = config.into_runtime(id.clone());
                config.validate()?;
                Ok((id, config))
            })
            .collect::<Result<BTreeMap<_, _>, ProviderError>>()?
    } else {
        BTreeMap::new()
    };

    let roles = if paths.roles.exists() {
        let source = std::fs::read_to_string(&paths.roles)?;
        let file: RolesFile = toml::from_str(&source)?;
        require_schema("roles.toml", file.schema_version, ROLES_SCHEMA_VERSION)?;
        file.roles.validate()?;
        file.roles
    } else {
        RoleRegistry::default()
    };

    let presets = load_common_profiles(&paths.default_models)?;
    let profiles = load_provider_profiles(&paths.provider_models, providers.keys())?;
    let caches = load_provider_caches(&paths.provider_cache, providers.keys())?;

    let mut models = BTreeMap::new();
    let mut model_provider_overrides = BTreeMap::new();
    let mut model_profiles = BTreeMap::new();
    for provider_id in providers.keys() {
        let exact = profiles.get(provider_id).cloned().unwrap_or_default();
        let remote = caches.get(provider_id).cloned().unwrap_or_default();
        let mut ids = exact.keys().cloned().collect::<BTreeSet<_>>();
        ids.extend(remote.keys().cloned());
        for model_id in ids {
            let key = ModelKey::new(provider_id.clone(), model_id.clone())?;
            let exact_profile = exact.get(&model_id).cloned().unwrap_or_default();
            let common = presets.get(&model_id).cloned();
            let remote_descriptor = remote.get(&model_id).cloned();
            let descriptor = resolve_descriptor(
                key.clone(),
                remote_descriptor,
                common.as_ref(),
                &exact_profile,
            );
            let base_capabilities = descriptor.capabilities.clone();
            let base_source = descriptor.source.clone();
            models.insert(
                key.to_string(),
                StoredModel {
                    descriptor,
                    overrides: exact_profile.capabilities.clone(),
                    base_capabilities: Some(base_capabilities),
                    base_source: Some(base_source),
                },
            );
            if exact_profile.wire_api.is_some() || !exact_profile.payload.is_empty() {
                model_provider_overrides.insert(
                    key.to_string(),
                    ProviderModelOverride {
                        wire_api: exact_profile.wire_api,
                        payload: exact_profile.payload.clone(),
                    },
                );
            }
            model_profiles.insert(key.to_string(), exact_profile);
        }
    }

    Ok(RegistryFile {
        schema_version: CURRENT_SCHEMA_VERSION,
        providers,
        models,
        roles,
        model_provider_overrides,
        model_profiles,
    })
}

pub(crate) fn save(paths: &RegistryPaths, state: &RegistryFile) -> Result<(), ProviderError> {
    let providers = ProvidersFile {
        schema_version: CURRENT_SCHEMA_VERSION,
        providers: state
            .providers
            .iter()
            .map(|(id, config)| (id.clone(), ProviderConfigDisk::from_runtime(config)))
            .collect(),
    };
    write_toml(&paths.providers, &providers)?;

    let roles = RolesFile {
        schema_version: ROLES_SCHEMA_VERSION,
        roles: state.roles.clone(),
    };
    write_toml(&paths.roles, &roles)?;

    let mut profiles_by_provider: BTreeMap<String, BTreeMap<String, ModelProfileDisk>> =
        BTreeMap::new();
    for (key, profile) in &state.model_profiles {
        if profile.is_empty() {
            continue;
        }
        let key = ModelKey::parse(key)?;
        profiles_by_provider
            .entry(key.provider_id)
            .or_default()
            .insert(key.model_id, profile.clone());
    }
    for provider_id in state.providers.keys() {
        let path = paths.provider_models.join(provider_id).join("models.toml");
        let file = ProviderModelsFile {
            schema_version: PROVIDER_MODELS_SCHEMA_VERSION,
            models: profiles_by_provider.remove(provider_id).unwrap_or_default(),
        };
        write_toml(&path, &file)?;
    }

    let mut cache_by_provider: BTreeMap<String, Vec<ModelDescriptor>> = BTreeMap::new();
    for stored in state.models.values() {
        if stored.base_source == Some(ModelSource::Remote)
            || stored.descriptor.source == ModelSource::Remote
        {
            cache_by_provider
                .entry(stored.descriptor.key.provider_id.clone())
                .or_default()
                .push(stored.descriptor.clone());
        }
    }
    for (provider_id, mut models) in cache_by_provider {
        models.sort_by(|left, right| left.key.model_id.cmp(&right.key.model_id));
        let file = ProviderModelCache {
            schema_version: CACHE_SCHEMA_VERSION,
            provider_id: provider_id.clone(),
            models,
        };
        write_json(
            &paths.provider_cache.join(provider_id).join("models.json"),
            &file,
        )?;
    }
    Ok(())
}

fn load_common_profiles(
    directory: &Path,
) -> Result<BTreeMap<String, ModelProfileDisk>, ProviderError> {
    if !directory.exists() {
        return Ok(BTreeMap::new());
    }
    let mut paths = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("toml"))
        .collect::<Vec<_>>();
    paths.sort();
    let mut profiles = BTreeMap::new();
    for path in paths {
        let source = std::fs::read_to_string(&path)?;
        let file: CommonModelsFile = toml::from_str(&source)?;
        require_schema(
            &path.display().to_string(),
            file.schema_version,
            COMMON_MODELS_SCHEMA_VERSION,
        )?;
        for (model_id, profile) in file.models {
            validate_identifier(&model_id, "default model id")?;
            if model_id.contains('*') {
                return Err(ProviderError::InvalidProvider(format!(
                    "default model profiles must use exact model ids, found {model_id:?} in {}",
                    path.display()
                )));
            }
            profile.validate(&format!("{model_id:?} in {}", path.display()))?;
            if !profile.experimental.is_empty() {
                return Err(ProviderError::InvalidProvider(format!(
                    "experimental model endpoints are provider-specific and cannot be set in {}",
                    path.display()
                )));
            }
            if profiles.insert(model_id.clone(), profile).is_some() {
                return Err(ProviderError::InvalidProvider(format!(
                    "duplicate default model profile {model_id:?} in {}",
                    path.display()
                )));
            }
        }
    }
    Ok(profiles)
}

fn load_provider_profiles<'a>(
    directory: &Path,
    providers: impl Iterator<Item = &'a String>,
) -> Result<BTreeMap<String, BTreeMap<String, ModelProfileDisk>>, ProviderError> {
    let mut result = BTreeMap::new();
    for provider_id in providers {
        let path = directory.join(provider_id).join("models.toml");
        if !path.exists() {
            continue;
        }
        let source = std::fs::read_to_string(&path)?;
        let file: ProviderModelsFile = toml::from_str(&source)?;
        require_schema(
            &path.display().to_string(),
            file.schema_version,
            PROVIDER_MODELS_SCHEMA_VERSION,
        )?;
        for (model_id, profile) in &file.models {
            validate_identifier(model_id, "provider model id")?;
            if model_id.contains('*') {
                return Err(ProviderError::InvalidProvider(format!(
                    "provider model profiles must use exact model ids, found {model_id:?} in {}",
                    path.display()
                )));
            }
            profile.validate(&format!("{provider_id}/{model_id} in {}", path.display()))?;
            if let Some(key) = find_sensitive_payload_key(&profile.payload) {
                return Err(ProviderError::InvalidProvider(format!(
                    "model profile payload contains credential-like key: {key}"
                )));
            }
            profile.experimental.validate()?;
        }
        result.insert(provider_id.clone(), file.models);
    }
    Ok(result)
}

fn load_provider_caches<'a>(
    directory: &Path,
    providers: impl Iterator<Item = &'a String>,
) -> Result<BTreeMap<String, BTreeMap<String, ModelDescriptor>>, ProviderError> {
    let mut result = BTreeMap::new();
    for provider_id in providers {
        let path = directory.join(provider_id).join("models.json");
        if !path.exists() {
            continue;
        }
        let source = std::fs::read_to_string(&path)?;
        let file: ProviderModelCache = serde_json::from_str(&source)?;
        require_schema(
            &path.display().to_string(),
            file.schema_version,
            CACHE_SCHEMA_VERSION,
        )?;
        if file.provider_id != *provider_id {
            return Err(ProviderError::InvalidProvider(format!(
                "model cache {} belongs to provider {} instead of {provider_id}",
                path.display(),
                file.provider_id
            )));
        }
        let mut models = BTreeMap::new();
        for mut descriptor in file.models {
            if descriptor.key.provider_id != *provider_id {
                return Err(ProviderError::InvalidModelKey(format!(
                    "cached model {} belongs to the wrong provider",
                    descriptor.key
                )));
            }
            descriptor.source = ModelSource::Remote;
            models.insert(descriptor.key.model_id.clone(), descriptor);
        }
        result.insert(provider_id.clone(), models);
    }
    Ok(result)
}

fn resolve_descriptor(
    key: ModelKey,
    remote: Option<ModelDescriptor>,
    common: Option<&ModelProfileDisk>,
    exact: &ModelProfileDisk,
) -> ModelDescriptor {
    let apply_common_capabilities = remote
        .as_ref()
        .is_none_or(|descriptor| descriptor.capabilities == ModelCapabilities::default());
    let mut descriptor = remote.unwrap_or_else(|| ModelDescriptor {
        key: key.clone(),
        display_name: key.model_id.clone(),
        description: None,
        wire_api: None,
        context_window: None,
        capabilities: ModelCapabilities::default(),
        reasoning_efforts: Vec::new(),
        default_effort: None,
        fast_mode: false,
        source: ModelSource::Static,
        enabled: true,
    });
    descriptor.key = key;
    if let Some(common) = common {
        apply_missing_profile(&mut descriptor, common, apply_common_capabilities);
    }
    apply_exact_profile(&mut descriptor, exact);
    descriptor
}

fn profile_fast_mode(profile: &ModelProfileDisk) -> Option<bool> {
    if profile
        .service_tiers
        .iter()
        .any(|tier| tier.eq_ignore_ascii_case("priority"))
    {
        Some(true)
    } else {
        profile.fast_mode
    }
}

fn apply_missing_profile(
    descriptor: &mut ModelDescriptor,
    profile: &ModelProfileDisk,
    apply_capabilities: bool,
) {
    if descriptor.wire_api.is_none() {
        descriptor.wire_api = profile.wire_api;
    }
    if descriptor.context_window.is_none() {
        descriptor.context_window = profile.context_window;
    }
    if descriptor.reasoning_efforts.is_empty() {
        descriptor.reasoning_efforts = profile.reasoning_efforts.clone();
    }
    if descriptor.default_effort.is_none() {
        descriptor.default_effort = profile.default_effort.clone();
    }
    if !descriptor.fast_mode {
        descriptor.fast_mode = profile_fast_mode(profile).unwrap_or(false);
    }
    if apply_capabilities {
        descriptor.capabilities = profile
            .capabilities
            .apply_to(descriptor.capabilities.clone());
    }
    if !descriptor.reasoning_efforts.is_empty() {
        descriptor.capabilities.reasoning_effort = true;
    }
}

fn apply_exact_profile(descriptor: &mut ModelDescriptor, profile: &ModelProfileDisk) {
    if let Some(display_name) = profile.display_name.as_ref() {
        descriptor.display_name = display_name.clone();
    }
    if profile.description.is_some() {
        descriptor.description = profile.description.clone();
    }
    if profile.wire_api.is_some() {
        descriptor.wire_api = profile.wire_api;
    }
    if profile.context_window.is_some() {
        descriptor.context_window = profile.context_window;
    }
    if !profile.reasoning_efforts.is_empty() {
        descriptor.reasoning_efforts = profile.reasoning_efforts.clone();
    }
    if profile.default_effort.is_some() {
        descriptor.default_effort = profile.default_effort.clone();
    }
    if let Some(fast_mode) = profile_fast_mode(profile) {
        descriptor.fast_mode = fast_mode;
    }
    descriptor.capabilities = profile
        .capabilities
        .apply_to(descriptor.capabilities.clone());
    if !descriptor.reasoning_efforts.is_empty() {
        descriptor.capabilities.reasoning_effort = true;
    }
    if let Some(enabled) = profile.enabled {
        descriptor.enabled = enabled;
    }
    if !profile.is_empty() {
        descriptor.source = ModelSource::UserOverride;
    }
}

fn require_schema(label: &str, actual: u32, expected: u32) -> Result<(), ProviderError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ProviderError::InvalidProvider(format!(
            "unsupported {label} schema version {actual}; expected {expected}"
        )))
    }
}

fn write_toml(path: &Path, value: &impl Serialize) -> Result<(), ProviderError> {
    let content = toml::to_string_pretty(value)?;
    write_atomic(path, content.as_bytes())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), ProviderError> {
    let content = serde_json::to_vec_pretty(value)?;
    write_atomic(path, &content)
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<(), ProviderError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content)?;
    temp.as_file().sync_all()?;
    persist_provider_temp_file(temp, path)
}
