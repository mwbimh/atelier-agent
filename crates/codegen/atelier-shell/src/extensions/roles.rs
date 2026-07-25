//! Fixed-role Provider assignment ACP methods.

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use agent_client_protocol as acp;
use atelier_provider::{ModelKey, ProviderRegistry, RoleConfig, RoleError, RoleId};
use serde::Deserialize;

pub const ROLE_LIST: &str = "_atelier/role/list";
pub const ROLE_GET: &str = "_atelier/role/get";
pub const ROLE_UPDATE: &str = "_atelier/role/update";
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
    session_id: String,
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
        ROLE_UPDATE_PAYLOAD | "atelier/role/update_payload" => update_payload(args),
        ROLE_SET_FAST_MODE | "atelier/role/set_fast_mode" => set_fast_mode(agent, args).await,
        ROLE_TEST | "atelier/role/test" => test(args),
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn set_fast_mode(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: RoleFastModeParams = parse_params(args)?;
    if params.role_id != RoleId::Main {
        return Err(acp::Error::invalid_params().data("fast mode can only update the main role"));
    }
    let mut registry = registry()?;
    let mut config = configured_role(&registry, params.role_id)?.clone();
    config.fast_mode = params.enabled;
    registry
        .update_role(params.role_id, config)
        .map_err(role_error)?;
    registry
        .save()
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;

    let session_id = acp::SessionId::new(params.session_id.clone());
    let handle = agent.get_session_handle(&session_id).ok_or_else(|| {
        acp::Error::invalid_params().data(format!("session not found: {}", params.session_id))
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

    to_raw_response(&serde_json::json!({
        "roleId": params.role_id,
        "sessionId": params.session_id,
        "enabled": params.enabled,
    }))
}

fn list() -> ExtResult {
    let registry = registry()?;
    let roles: Vec<_> = RoleId::ALL
        .into_iter()
        .map(|role| role_list_entry(role, registry.role(role)))
        .collect();
    to_raw_response(&serde_json::json!({ "roles": roles }))
}

fn get(args: &acp::ExtRequest) -> ExtResult {
    let params: RoleParams = parse_params(args)?;
    let registry = registry()?;
    let config = configured_role(&registry, params.role_id)?;
    to_raw_response(&serde_json::json!({
        "roleId": params.role_id,
        "config": redacted_role_config(config),
    }))
}

fn update(args: &acp::ExtRequest) -> ExtResult {
    let mut params: RoleUpdateParams = parse_params(args)?;
    let mut registry = registry()?;
    if params.preserve_payload
        && let Some(existing) = registry.role(params.role_id)
    {
        params.config.payload = existing.payload.clone();
    }
    registry
        .update_role(params.role_id, params.config)
        .map_err(role_error)?;
    registry
        .save()
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    get_from_registry(&registry, params.role_id)
}

fn update_payload(args: &acp::ExtRequest) -> ExtResult {
    let params: RolePayloadParams = parse_params(args)?;
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
    to_raw_response(&role_test_response(&registry, params.role_id))
}

fn role_test_response(registry: &ProviderRegistry, role_id: RoleId) -> serde_json::Value {
    let Some(config) = registry
        .role(role_id)
        .filter(|config| config.is_configured())
    else {
        return serde_json::json!({
            "roleId": role_id,
            "configured": false,
            "providerExists": false,
            "modelExists": false,
            "credentialAvailable": false,
            "message": "role is not configured",
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
        "providerExists": provider_exists,
        "modelExists": model_exists,
        "credentialAvailable": credential_available,
        "message": message,
    })
}

fn configured_role(
    registry: &ProviderRegistry,
    role_id: RoleId,
) -> Result<&RoleConfig, acp::Error> {
    registry
        .role(role_id)
        .filter(|config| config.is_configured())
        .ok_or_else(|| {
            acp::Error::invalid_params().data(format!("role is not configured: {role_id}"))
        })
}

fn role_list_entry(role_id: RoleId, config: Option<&RoleConfig>) -> serde_json::Value {
    serde_json::json!({
        "roleId": role_id,
        "configured": config.is_some(),
        "config": config.map(redacted_role_config),
    })
}

fn get_from_registry(registry: &ProviderRegistry, role_id: RoleId) -> ExtResult {
    let config = registry
        .role(role_id)
        .ok_or_else(|| acp::Error::internal_error().data("role disappeared after update"))?;
    to_raw_response(&serde_json::json!({
        "roleId": role_id,
        "config": redacted_role_config(config),
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
    use super::{configured_role, redacted_role_config, role_list_entry, role_test_response};
    use agent_client_protocol as acp;
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

        let value = role_list_entry(RoleId::Main, registry.role(RoleId::Main));

        assert_eq!(value["roleId"], "main");
        assert_eq!(value["configured"], false);
        assert!(value["config"].is_null());
    }

    #[test]
    fn role_get_rejects_missing_assignment_as_unconfigured() {
        let registry = ProviderRegistry::in_memory();

        let error = configured_role(&registry, RoleId::Main).unwrap_err();

        assert_eq!(error.code, acp::ErrorCode::InvalidParams);
        assert_eq!(error.data, Some(json!("role is not configured: main")));
    }

    #[test]
    fn role_test_reports_default_placeholder_as_unconfigured() {
        let registry = ProviderRegistry::in_memory();

        let value = role_test_response(&registry, RoleId::Main);

        assert_eq!(value["roleId"], "main");
        assert_eq!(value["configured"], false);
        assert_eq!(value["providerExists"], false);
        assert_eq!(value["modelExists"], false);
        assert_eq!(value["credentialAvailable"], false);
        assert_eq!(value["message"], "role is not configured");
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
