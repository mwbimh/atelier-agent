//! Fixed-role Provider assignment ACP methods.

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use agent_client_protocol as acp;
use atelier_provider::{ModelKey, ProviderRegistry, RoleConfig, RoleError, RoleId};
use serde::Deserialize;

pub const ROLE_LIST: &str = "_atelier/role/list";
pub const ROLE_GET: &str = "_atelier/role/get";
pub const ROLE_UPDATE: &str = "_atelier/role/update";
pub const ROLE_DELETE: &str = "_atelier/role/delete";
pub const ROLE_UPDATE_PAYLOAD: &str = "_atelier/role/update_payload";
pub const ROLE_SET_FAST_MODE: &str = "_atelier/role/set_fast_mode";
pub const ROLE_TEST: &str = "_atelier/role/test";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoleParams {
    role_id: RoleId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoleUpdateParams {
    role_id: RoleId,
    config: RoleConfig,
    #[serde(default)]
    preserve_payload: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RolePayloadParams {
    role_id: RoleId,
    payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoleFastModeParams {
    role_id: RoleId,
    #[serde(default)]
    session_id: Option<String>,
    enabled: bool,
}

fn registry() -> Result<ProviderRegistry, acp::Error> {
    ProviderRegistry::load_or_create(atelier_config::atelier_home().join("providers.toml"))
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        ROLE_LIST | "atelier/role/list" => list(),
        ROLE_GET | "atelier/role/get" => get(args),
        ROLE_UPDATE | "atelier/role/update" => update(args),
        ROLE_DELETE | "atelier/role/delete" => delete(args),
        ROLE_UPDATE_PAYLOAD | "atelier/role/update_payload" => update_payload(args),
        ROLE_SET_FAST_MODE | "atelier/role/set_fast_mode" => set_fast_mode(agent, args).await,
        ROLE_TEST | "atelier/role/test" => test(args),
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn set_fast_mode(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: RoleFastModeParams = parse_params(args)?;
    if params.role_id == RoleId::Main {
        let raw_session_id = params.session_id.as_deref().ok_or_else(|| {
            acp::Error::invalid_params().data("sessionId is required for MAIN fast mode")
        })?;
        let session_id = acp::SessionId::new(raw_session_id.to_owned());
        let handle = agent.get_session_handle(&session_id).ok_or_else(|| {
            acp::Error::invalid_params().data(format!("session not found: {raw_session_id}"))
        })?;
        let (responds_to, response) = tokio::sync::oneshot::channel();
        handle
            .cmd_tx
            .send(crate::session::SessionCommand::SetRoleFastMode {
                enabled: params.enabled,
                responds_to,
            })
            .map_err(|_| acp::Error::internal_error().data("session command channel closed"))?;
        tokio::time::timeout(std::time::Duration::from_secs(5), response)
            .await
            .map_err(|_| acp::Error::internal_error().data("session fast-mode update timed out"))?
            .map_err(|_| acp::Error::internal_error().data("session fast-mode response dropped"))?;
    } else {
        let mut registry = registry()?;
        let mut config = role_config_for_fast_mode(&registry, params.role_id);
        config.set_fast_mode(params.enabled);
        registry
            .update_role(params.role_id, config)
            .map_err(role_error)?;
        registry
            .save()
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    }

    to_raw_response(&serde_json::json!({
        "roleId": params.role_id,
        "sessionId": params.session_id,
        "enabled": params.enabled,
    }))
}

fn role_config_for_fast_mode(registry: &ProviderRegistry, role_id: RoleId) -> RoleConfig {
    registry
        .role(role_id)
        .cloned()
        .unwrap_or_else(|| RoleConfig::with_fast_mode(false))
}

fn list() -> ExtResult {
    let registry = registry()?;
    let main = runtime_main_role()?;
    let roles: Vec<_> = RoleId::ALL
        .into_iter()
        .map(|role| {
            let context_source = runtime_role_context_source(role)?;
            Ok(if role == RoleId::Main {
                main_role_list_entry(main.as_ref(), context_source)
            } else {
                let (effective_source, effective, field_sources) =
                    resolve_effective_role(&registry, role, main.as_ref());
                role_list_entry(
                    role,
                    registry.role(role),
                    Some(effective_source),
                    effective.as_ref(),
                    field_sources.as_ref(),
                    context_source,
                )
            })
        })
        .collect::<Result<_, acp::Error>>()?;
    to_raw_response(&serde_json::json!({ "roles": roles }))
}

fn get(args: &acp::ExtRequest) -> ExtResult {
    let params: RoleParams = parse_params(args)?;
    let registry = registry()?;
    let main = runtime_main_role()?;
    let context_source = runtime_role_context_source(params.role_id)?;
    if params.role_id == RoleId::Main {
        return to_raw_response(&main_role_list_entry(main.as_ref(), context_source));
    }
    let (effective_source, effective, field_sources) =
        resolve_effective_role(&registry, params.role_id, main.as_ref());
    to_raw_response(&serde_json::json!({
        "roleId": params.role_id,
        "configured": registry.role(params.role_id).is_some(),
        "inherited": registry.role(params.role_id).is_none(),
        "effectiveSource": effective_source,
        "config": registry.role(params.role_id).map(redacted_role_config),
        "effectiveConfig": effective.as_ref().map(redacted_role_config),
        "fieldSources": field_sources,
        "contextSource": context_source,
    }))
}

fn update(args: &acp::ExtRequest) -> ExtResult {
    let mut params: RoleUpdateParams = parse_params(args)?;
    if params.role_id == RoleId::Main {
        let provider = params.config.provider_override().ok_or_else(|| {
            acp::Error::invalid_params().data("MAIN update requires an explicit provider")
        })?;
        let model_id = params.config.model_override().ok_or_else(|| {
            acp::Error::invalid_params().data("MAIN update requires an explicit model")
        })?;
        if params.config.effort.is_some()
            || params.config.fast_mode_override() == Some(true)
            || !params.config.payload.is_empty()
        {
            return Err(acp::Error::invalid_params().data(
                "MAIN update only persists provider/model; use the active Session controls for effort and fast mode",
            ));
        }
        let model = format!("{provider}/{model_id}");
        atelier_config::runtime_defaults::update_default_model_at(
            &atelier_config::atelier_home(),
            &model,
        )
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
        return to_raw_response(&serde_json::json!({
            "roleId": RoleId::Main,
            "source": "config.toml",
            "configured": true,
            "config": redacted_role_config(&params.config),
        }));
    }
    let mut registry = registry()?;
    if let Some(existing) = registry.role(params.role_id) {
        params.config = existing.patched_with(&params.config, params.preserve_payload);
    }
    registry
        .update_role(params.role_id, params.config)
        .map_err(role_error)?;
    registry
        .save()
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    get_from_registry(&registry, params.role_id)
}

fn delete(args: &acp::ExtRequest) -> ExtResult {
    let params: RoleParams = parse_params(args)?;
    if params.role_id == RoleId::Main {
        return Err(acp::Error::invalid_params()
            .data("MAIN is managed by config.toml; select its model with /model"));
    }
    let mut registry = registry()?;
    let removed = registry.remove_role(params.role_id).is_some();
    registry
        .save()
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    to_raw_response(&serde_json::json!({
        "roleId": params.role_id,
        "deleted": removed,
        "message": if removed {
            "Role override removed; fixed-parent inheritance restored"
        } else {
            "Role had no exact override; fixed-parent inheritance is unchanged"
        },
    }))
}

fn update_payload(args: &acp::ExtRequest) -> ExtResult {
    let params: RolePayloadParams = parse_params(args)?;
    if params.role_id == RoleId::Main {
        return Err(
            acp::Error::invalid_params().data("MAIN request payload is not stored in roles.toml")
        );
    }
    let mut registry = registry()?;
    let mut config = registry.role(params.role_id).cloned().ok_or_else(|| {
        acp::Error::invalid_params().data(format!("role is not configured: {}", params.role_id))
    })?;
    config.payload = params.payload;
    registry
        .update_role(params.role_id, config)
        .map_err(role_error)?;
    registry
        .save()
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    get_from_registry(&registry, params.role_id)
}

fn test(args: &acp::ExtRequest) -> ExtResult {
    let params: RoleParams = parse_params(args)?;
    let registry = registry()?;
    let main = runtime_main_role()?;
    to_raw_response(&role_test_response(
        &registry,
        params.role_id,
        main.as_ref(),
    ))
}

fn role_test_response(
    registry: &ProviderRegistry,
    role_id: RoleId,
    main: Option<&RoleConfig>,
) -> serde_json::Value {
    let (_, effective, _) = resolve_effective_role(registry, role_id, main);
    let Some(config) = effective else {
        return serde_json::json!({
            "roleId": role_id,
            "configured": false,
            "explicitlyConfigured": registry.role(role_id).is_some(),
            "providerExists": false,
            "modelExists": false,
            "credentialAvailable": false,
            "message": "role has no effective model; select MAIN or configure a model override",
        });
    };
    let provider_exists = registry.provider(&config.provider).is_some();
    let model_exists = ModelKey::new(&config.provider, &config.model)
        .ok()
        .and_then(|key| registry.model(&key))
        .is_some();
    let credential_available = registry
        .provider(&config.provider)
        .is_some_and(|provider| provider.credential.is_available());
    let message = if !provider_exists {
        "provider is not configured"
    } else if !model_exists {
        "model is not present in the local catalog"
    } else if !credential_available {
        "provider credential is not available"
    } else {
        "role configuration is ready"
    };
    serde_json::json!({
        "roleId": role_id,
        "configured": true,
        "explicitlyConfigured": registry.role(role_id).is_some(),
        "providerExists": provider_exists,
        "modelExists": model_exists,
        "credentialAvailable": credential_available,
        "message": message,
    })
}

fn runtime_main_role() -> Result<Option<RoleConfig>, acp::Error> {
    let model = atelier_config::runtime_defaults::resolve_runtime_defaults_at(
        &atelier_config::atelier_home(),
    )
    .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?
    .model;
    model
        .map(|model| {
            let (provider, model) = model.split_once('/').ok_or_else(|| {
                acp::Error::invalid_params().data("MAIN model must use provider/model format")
            })?;
            RoleConfig::new(provider, model)
                .map_err(|error| acp::Error::invalid_params().data(error.to_string()))
        })
        .transpose()
}

fn runtime_role_context_source(role_id: RoleId) -> Result<Option<serde_json::Value>, acp::Error> {
    let home = atelier_config::atelier_home();
    let defaults = atelier_config::runtime_defaults::resolve_runtime_defaults_at(&home)
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
    let resolved = atelier_config::runtime_defaults::load_resolved_context_role_prompt_source_at(
        &home,
        &defaults.context,
        role_id,
    )
    .map_err(|error| {
        acp::Error::invalid_params()
            .data(format!("failed to resolve {role_id} role Context: {error}"))
    })?;
    Ok(resolved.map(|resolved| {
        serde_json::json!({
            "package": resolved.package,
            "role": resolved.role,
            "empty": resolved.prompt.is_empty(),
        })
    }))
}

fn resolve_effective_role(
    registry: &ProviderRegistry,
    role_id: RoleId,
    main: Option<&RoleConfig>,
) -> (
    RoleId,
    Option<RoleConfig>,
    Option<atelier_provider::RoleFieldSources>,
) {
    const MISSING: &str = "__atelier_unconfigured_main__";
    let placeholder = RoleConfig::new(MISSING, MISSING).expect("placeholder is valid");
    let resolved = registry
        .roles()
        .resolve_inherited_details(role_id, main.unwrap_or(&placeholder));
    if resolved.config.provider == MISSING || resolved.config.model == MISSING {
        (resolved.source, None, None)
    } else {
        (
            resolved.source,
            Some(resolved.config),
            Some(resolved.field_sources),
        )
    }
}

fn main_role_list_entry(
    config: Option<&RoleConfig>,
    context_source: Option<serde_json::Value>,
) -> serde_json::Value {
    let model = config.map(|config| format!("{}/{}", config.provider, config.model));
    serde_json::json!({
        "roleId": RoleId::Main,
        "displayName": "MAIN",
        "source": "config.toml",
        "configured": config.is_some(),
        "model": model,
        "config": config.map(redacted_role_config),
        "effectiveConfig": config.map(redacted_role_config),
        "contextSource": context_source,
    })
}

fn role_list_entry(
    role_id: RoleId,
    config: Option<&RoleConfig>,
    effective_source: Option<RoleId>,
    effective_config: Option<&RoleConfig>,
    field_sources: Option<&atelier_provider::RoleFieldSources>,
    context_source: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "roleId": role_id,
        "configured": config.is_some(),
        "inherited": config.is_none(),
        "effectiveSource": effective_source,
        "config": config.map(redacted_role_config),
        "effectiveConfig": effective_config.map(redacted_role_config),
        "fieldSources": field_sources,
        "contextSource": context_source,
    })
}

fn get_from_registry(registry: &ProviderRegistry, role_id: RoleId) -> ExtResult {
    let config = registry
        .role(role_id)
        .ok_or_else(|| acp::Error::internal_error().data("role disappeared after update"))?;
    let main = runtime_main_role()?;
    let (effective_source, effective, field_sources) =
        resolve_effective_role(registry, role_id, main.as_ref());
    let context_source = runtime_role_context_source(role_id)?;
    to_raw_response(&serde_json::json!({
        "roleId": role_id,
        "configured": true,
        "inherited": false,
        "effectiveSource": effective_source,
        "config": redacted_role_config(config),
        "effectiveConfig": effective.as_ref().map(redacted_role_config),
        "fieldSources": field_sources,
        "contextSource": context_source,
    }))
}

fn redacted_role_config(config: &RoleConfig) -> serde_json::Value {
    let mut value = serde_json::to_value(config).expect("RoleConfig is serializable");
    if let Some(payload) = value.get_mut("payload") {
        *payload = atelier_acp_runtime::redact_payload(payload);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{
        redacted_role_config, resolve_effective_role, role_config_for_fast_mode, role_list_entry,
        role_test_response,
    };
    use atelier_provider::{ProviderRegistry, RoleConfig, RoleId};
    use serde_json::json;

    #[test]
    fn role_rpc_config_redacts_secret_payload_values() {
        let mut config = RoleConfig::new("proxy", "model").unwrap();
        config.payload = serde_json::from_value(json!({
            "temperature": 0.2,
            "api_key": "secret-key",
            "nested": {"authorization": "Bearer secret"},
        }))
        .unwrap();

        let value = redacted_role_config(&config);

        assert_eq!(value["payload"]["temperature"], 0.2);
        assert_eq!(value["payload"]["api_key"], "[REDACTED]");
        assert_eq!(value["payload"]["nested"]["authorization"], "[REDACTED]");
    }

    #[test]
    fn role_list_reports_missing_assignment_as_unconfigured() {
        let registry = ProviderRegistry::in_memory();

        let value = role_list_entry(
            RoleId::General,
            registry.role(RoleId::General),
            None,
            None,
            None,
            None,
        );

        assert_eq!(value["roleId"], "general");
        assert_eq!(value["configured"], false);
        assert!(value["config"].is_null());
    }

    #[test]
    fn missing_assignment_resolves_to_main_when_main_is_selected() {
        let registry = ProviderRegistry::in_memory();
        let main = RoleConfig::new("provider", "main-model").unwrap();

        let (source, effective, _) =
            resolve_effective_role(&registry, RoleId::General, Some(&main));

        assert_eq!(source, RoleId::Main);
        let effective = effective.unwrap();
        assert_eq!(effective.provider, "provider");
        assert_eq!(effective.model, "main-model");
    }

    #[test]
    fn role_test_reports_default_placeholder_as_unconfigured() {
        let registry = ProviderRegistry::in_memory();

        let value = role_test_response(&registry, RoleId::Main, None);

        assert_eq!(value["roleId"], "main");
        assert_eq!(value["configured"], false);
        assert_eq!(value["providerExists"], false);
        assert_eq!(value["modelExists"], false);
        assert_eq!(value["credentialAvailable"], false);
        assert_eq!(
            value["message"],
            "role has no effective model; select MAIN or configure a model override"
        );
    }

    #[test]
    fn non_main_fast_mode_creates_a_sparse_override_without_copying_main() {
        let registry = ProviderRegistry::in_memory();

        let mut config = role_config_for_fast_mode(&registry, RoleId::Compact);
        config.set_fast_mode(true);
        let serialized = serde_json::to_value(config).unwrap();

        assert_eq!(serialized, json!({"fast_mode": true}));
    }

    #[test]
    fn non_main_fast_mode_preserves_existing_exact_overrides() {
        let mut registry = ProviderRegistry::in_memory();
        registry
            .update_role(
                RoleId::Compact,
                RoleConfig::new("provider", "compact-model").unwrap(),
            )
            .unwrap();

        let config = role_config_for_fast_mode(&registry, RoleId::Compact);

        assert_eq!(config.provider, "provider");
        assert_eq!(config.model, "compact-model");
    }
}

fn role_error(error: atelier_provider::ProviderError) -> acp::Error {
    match error {
        atelier_provider::ProviderError::InvalidProvider(message)
        | atelier_provider::ProviderError::InvalidModelKey(message) => {
            acp::Error::invalid_params().data(message)
        }
        other => acp::Error::internal_error().data(other.to_string()),
    }
}
