//! User-owned Atelier configuration management.

use agent_client_protocol as acp;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;

pub const CONFIG_GET: &str = "_atelier/config/get";
pub const CONFIG_UPDATE: &str = "_atelier/config/update";
pub const RESET_DEFAULTS: &str = "_atelier/config/reset_defaults";

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateConfigParams {
    model: String,
    #[serde(default)]
    switch: bool,
    #[serde(default)]
    effort: Option<String>,
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        CONFIG_GET | "atelier/config/get" => get_config(),
        CONFIG_UPDATE | "atelier/config/update" => update_config(args),
        RESET_DEFAULTS | "atelier/config/reset_defaults" => reset_defaults(agent).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

fn get_config() -> ExtResult {
    let home = atelier_config::atelier_home();
    let defaults = atelier_config::runtime_defaults::resolve_runtime_defaults_at(&home)
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
    to_raw_response(&defaults)
}

fn update_config(args: &acp::ExtRequest) -> ExtResult {
    let params: UpdateConfigParams = parse_params(args)?;
    let key = atelier_provider::ModelKey::parse(&params.model)
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
    let registry = atelier_provider::ProviderRegistry::load_or_create(
        atelier_config::atelier_home().join("providers.toml"),
    )
    .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    let provider = registry.provider(&key.provider_id).ok_or_else(|| {
        acp::Error::invalid_params().data(format!("configured model is unavailable: {key}"))
    })?;
    let model = registry.model(&key).ok_or_else(|| {
        acp::Error::invalid_params().data(format!("configured model is unavailable: {key}"))
    })?;
    if !provider.enabled || !model.enabled {
        return Err(
            acp::Error::invalid_params().data(format!("configured model is unavailable: {key}"))
        );
    }
    atelier_config::runtime_defaults::update_default_model_at(
        &atelier_config::atelier_home(),
        &key.to_string(),
    )
    .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    to_raw_response(&serde_json::json!({
        "model": key.to_string(),
        "persisted": true,
        "switch": params.switch,
        "effort": params.effort,
        "message": "Default model updated",
    }))
}

async fn reset_defaults(agent: &MvpAgent) -> ExtResult {
    let home = atelier_config::atelier_home();
    atelier_config::defaults::reset_user_defaults(&home)
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    agent
        .reload_local_provider_catalog_and_reconcile_sessions()
        .await
        .map_err(|error| acp::Error::internal_error().data(error))?;
    to_raw_response(&serde_json::json!({
        "reset": true,
        "home": home,
        "restartRecommended": true,
    }))
}
