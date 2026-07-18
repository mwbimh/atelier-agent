//! Fixed-role Provider assignment ACP methods.

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use agent_client_protocol as acp;
use atelier_provider::{ModelKey, ProviderRegistry, RoleConfig, RoleError, RoleId};
use serde::Deserialize;

pub const ROLE_LIST: &str = "_atelier/role/list";
pub const ROLE_GET: &str = "_atelier/role/get";
pub const ROLE_UPDATE: &str = "_atelier/role/update";
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
}

fn registry() -> Result<ProviderRegistry, acp::Error> {
    ProviderRegistry::load_or_create(atelier_config::atelier_home().join("providers.toml"))
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))
}

pub async fn handle(_agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        ROLE_LIST | "atelier/role/list" => list(),
        ROLE_GET | "atelier/role/get" => get(args),
        ROLE_UPDATE | "atelier/role/update" => update(args),
        ROLE_TEST | "atelier/role/test" => test(args),
        _ => Err(acp::Error::method_not_found()),
    }
}

fn list() -> ExtResult {
    let registry = registry()?;
    let roles: Vec<_> = registry
        .roles()
        .iter()
        .map(|(role, config)| {
            serde_json::json!({
                "roleId": role,
                "config": redacted_role_config(config),
            })
        })
        .collect();
    to_raw_response(&serde_json::json!({ "roles": roles }))
}

fn get(args: &acp::ExtRequest) -> ExtResult {
    let params: RoleParams = parse_params(args)?;
    let registry = registry()?;
    let config = registry.role(params.role_id).ok_or_else(|| {
        acp::Error::invalid_params().data(format!("role is not configured: {}", params.role_id))
    })?;
    to_raw_response(&serde_json::json!({
        "roleId": params.role_id,
        "config": redacted_role_config(config),
    }))
}

fn update(args: &acp::ExtRequest) -> ExtResult {
    let params: RoleUpdateParams = parse_params(args)?;
    let mut registry = registry()?;
    registry
        .update_role(params.role_id, params.config)
        .map_err(role_error)?;
    registry
        .save()
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    get_from_registry(&registry, params.role_id)
}

fn test(args: &acp::ExtRequest) -> ExtResult {
    let params: RoleParams = parse_params(args)?;
    let registry = registry()?;
    let Some(config) = registry.role(params.role_id) else {
        return to_raw_response(&serde_json::json!({
            "roleId": params.role_id,
            "configured": false,
            "providerExists": false,
            "modelExists": false,
            "credentialAvailable": false,
            "message": "role is not configured",
        }));
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
    to_raw_response(&serde_json::json!({
        "roleId": params.role_id,
        "configured": true,
        "providerExists": provider_exists,
        "modelExists": model_exists,
        "credentialAvailable": credential_available,
        "message": message,
    }))
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
        *payload = xai_acp_lib::redact_payload(payload);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::redacted_role_config;
    use atelier_provider::RoleConfig;
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
