//! Local Provider and model registry ACP methods.
//!
//! These methods manage the local provider registry, model catalog, and
//! credential references exposed to ACP clients. Secret values are written
//! only to the configured OS credential store and never to `providers.toml`.

use agent_client_protocol as acp;
use atelier_provider::{
    CapabilityOverrides, CredentialRef, ModelDescriptor, ModelKey, ProviderConfig,
    ProviderDiscovery, ProviderError, ProviderModelOverride, ProviderProtocol, ProviderRegistry,
    ProviderSnapshot, WireApi,
};
use serde::Deserialize;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;

pub const PROVIDER_LIST: &str = "_atelier/provider/list";
pub const PROVIDER_CREATE: &str = "_atelier/provider/create";
pub const PROVIDER_UPDATE: &str = "_atelier/provider/update";
pub const PROVIDER_DELETE: &str = "_atelier/provider/delete";
pub const PROVIDER_TEST: &str = "_atelier/provider/test";
pub const PROVIDER_REFRESH_MODELS: &str = "_atelier/provider/refresh_models";
pub const PROVIDER_ENABLE: &str = "_atelier/provider/enable";
pub const MODEL_LIST: &str = "_atelier/model/list";
pub const MODEL_GET: &str = "_atelier/model/get";
pub const MODEL_UPDATE: &str = "_atelier/model/update";
pub const MODEL_UPDATE_WIRE_API: &str = "_atelier/model/update_wire_api";
pub const MODEL_PROVIDER_OVERRIDE_LIST: &str = "_atelier/model_provider_override/list";
pub const MODEL_PROVIDER_OVERRIDE_SET: &str = "_atelier/model_provider_override/set";
pub const MODEL_PROVIDER_OVERRIDE_DELETE: &str = "_atelier/model_provider_override/delete";
pub const MODEL_PROVIDER_OVERRIDE_TEST: &str = "_atelier/model_provider_override/test";
pub const MODEL_SET_DEFAULT: &str = "_atelier/model/set_default";
pub const MODEL_SET_CAPABILITIES: &str = "_atelier/model/set_capabilities";
pub const CREDENTIAL_STATUS: &str = "_atelier/credential/status";
pub const CREDENTIAL_SET: &str = "_atelier/credential/set";
pub const CREDENTIAL_DELETE: &str = "_atelier/credential/delete";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderParams {
    provider: ProviderConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderIdParams {
    provider_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderEnabledParams {
    provider_id: String,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialParams {
    provider_id: String,
    credential: CredentialRef,
    #[serde(default)]
    secret: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelParams {
    model: ModelDescriptor,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelKeyParams {
    model_key: Option<String>,
    provider_id: Option<String>,
    model_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CapabilityParams {
    model_key: String,
    overrides: CapabilityOverrides,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireApiParams {
    model_key: String,
    wire_api: Option<WireApi>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelOverrideParams {
    model_key: String,
    #[serde(default)]
    wire_api: Option<WireApi>,
    #[serde(default)]
    payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelOverrideDeleteParams {
    model_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelOverrideTestParams {
    model_key: String,
    #[serde(default)]
    execute: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderStatus {
    provider_id: String,
    configured: bool,
    network_probe: bool,
    http_status: Option<u16>,
    message: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshResult {
    provider_id: String,
    refreshed: bool,
    models: Vec<ModelDescriptor>,
    message: String,
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        PROVIDER_LIST | "atelier/provider/list" | MODEL_LIST | "atelier/model/list" => list(args),
        PROVIDER_CREATE
        | "atelier/provider/create"
        | PROVIDER_UPDATE
        | "atelier/provider/update" => upsert_provider(agent, args),
        PROVIDER_DELETE | "atelier/provider/delete" => delete_provider(agent, args),
        PROVIDER_TEST | "atelier/provider/test" => provider_status(args).await,
        PROVIDER_REFRESH_MODELS | "atelier/provider/refresh_models" => {
            refresh_models(agent, args).await
        }
        PROVIDER_ENABLE | "atelier/provider/enable" => set_provider_enabled(agent, args),
        MODEL_GET | "atelier/model/get" => get_model(args),
        MODEL_UPDATE | "atelier/model/update" => upsert_model(agent, args),
        MODEL_UPDATE_WIRE_API | "atelier/model/update_wire_api" => {
            update_model_wire_api(agent, args)
        }
        MODEL_PROVIDER_OVERRIDE_LIST | "atelier/model_provider_override/list" => {
            list_model_provider_overrides(args)
        }
        MODEL_PROVIDER_OVERRIDE_SET | "atelier/model_provider_override/set" => {
            set_model_provider_override(agent, args)
        }
        MODEL_PROVIDER_OVERRIDE_DELETE | "atelier/model_provider_override/delete" => {
            delete_model_provider_override(agent, args)
        }
        MODEL_PROVIDER_OVERRIDE_TEST | "atelier/model_provider_override/test" => {
            test_model_provider_override(args).await
        }
        MODEL_SET_DEFAULT | "atelier/model/set_default" => set_default_model(agent, args),
        MODEL_SET_CAPABILITIES | "atelier/model/set_capabilities" => set_capabilities(agent, args),
        CREDENTIAL_STATUS | "atelier/credential/status" => credential_status(),
        CREDENTIAL_SET | "atelier/credential/set" => set_credential(agent, args),
        CREDENTIAL_DELETE | "atelier/credential/delete" => delete_credential(agent, args),
        _ => Err(acp::Error::method_not_found()),
    }
}

fn registry() -> Result<ProviderRegistry, ProviderError> {
    ProviderRegistry::load_or_create(atelier_config::atelier_home().join("providers.toml"))
}

fn persist(registry: &ProviderRegistry) -> Result<(), ProviderError> {
    registry.save()
}

fn list(args: &acp::ExtRequest) -> ExtResult {
    let registry = registry().map_err(to_acp_error)?;
    if args.method.as_ref() == MODEL_LIST || args.method.as_ref() == "atelier/model/list" {
        return to_raw_response(&serde_json::json!({
            "models": registry.snapshot().models,
            "defaultModel": registry.default_model(),
        }));
    }
    to_raw_response(&registry.snapshot())
}

fn get_model(args: &acp::ExtRequest) -> ExtResult {
    let params: ModelKeyParams = parse_params(args)?;
    let registry = registry().map_err(to_acp_error)?;
    let key = parse_model_key(params)?.ok_or_else(|| {
        acp::Error::invalid_params().data("modelKey or providerId + modelId is required")
    })?;
    let model = registry
        .model(&key)
        .ok_or_else(|| ProviderError::ModelNotFound(key.to_string()))
        .map_err(to_acp_error)?;
    let resolved = registry.resolve_wire_api(&key).map_err(to_acp_error)?;
    to_raw_response(&serde_json::json!({
        "model": model,
        "wireApi": resolved.wire_api,
        "wireApiSource": format!("{:?}", resolved.source),
        "providerModelOverride": registry.model_provider_override(&key),
    }))
}

fn update_model_wire_api(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: WireApiParams = parse_params(args)?;
    let key = ModelKey::parse(&params.model_key).map_err(to_acp_error)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry
        .set_model_wire_api(&key, params.wire_api)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    get_model_from_registry(&registry, &key)
}

fn list_model_provider_overrides(args: &acp::ExtRequest) -> ExtResult {
    let params: ModelKeyParams = parse_params(args)?;
    let registry = registry().map_err(to_acp_error)?;
    if let Some(key) = parse_model_key(params)? {
        return to_raw_response(&serde_json::json!({
            "modelKey": key.to_string(),
            "override": registry.model_provider_override(&key),
            "resolved": registry.resolve_wire_api(&key).map_err(to_acp_error)?,
        }));
    }
    let overrides: Vec<_> = registry
        .snapshot()
        .model_provider_overrides
        .into_iter()
        .map(|(model_key, override_config)| {
            serde_json::json!({
                "modelKey": model_key,
                "override": override_config,
            })
        })
        .collect();
    to_raw_response(&serde_json::json!({ "overrides": overrides }))
}

fn set_model_provider_override(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ModelOverrideParams = parse_params(args)?;
    let key = ModelKey::parse(&params.model_key).map_err(to_acp_error)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry
        .set_model_provider_override(
            &key,
            ProviderModelOverride {
                wire_api: params.wire_api,
                payload: params.payload,
            },
        )
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    get_model_from_registry(&registry, &key)
}

fn delete_model_provider_override(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ModelOverrideDeleteParams = parse_params(args)?;
    let key = ModelKey::parse(&params.model_key).map_err(to_acp_error)?;
    let mut registry = registry().map_err(to_acp_error)?;
    let removed = registry
        .remove_model_provider_override(&key)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    to_raw_response(&serde_json::json!({
        "modelKey": key.to_string(),
        "removed": removed,
        "resolved": registry.resolve_wire_api(&key).map_err(to_acp_error)?,
    }))
}

async fn test_model_provider_override(args: &acp::ExtRequest) -> ExtResult {
    let params: ModelOverrideTestParams = parse_params(args)?;
    let key = ModelKey::parse(&params.model_key).map_err(to_acp_error)?;
    let registry = registry().map_err(to_acp_error)?;
    let resolved = registry.resolve_wire_api(&key).map_err(to_acp_error)?;
    let provider = registry
        .provider(&key.provider_id)
        .cloned()
        .ok_or_else(|| ProviderError::ProviderNotFound(key.provider_id.clone()))
        .map_err(to_acp_error)?;
    let endpoint = provider_endpoint(&provider, resolved.wire_api);
    if !params.execute {
        return to_raw_response(&serde_json::json!({
            "modelKey": key.to_string(),
            "providerId": provider.id,
            "model": key.model_id,
            "wireApi": resolved.wire_api,
            "endpoint": endpoint,
            "executed": false,
            "message": "pair test preview; set execute=true to send a minimal request",
        }));
    }
    let request = build_provider_post_request(&provider, endpoint.clone(), resolved.wire_api)
        .map_err(to_acp_error)?
        .json(&minimal_pair_test_payload(&key.model_id, resolved.wire_api));
    let response = request.send().await.map_err(|error| {
        acp::Error::internal_error().data(format!("model pair test failed: {error}"))
    })?;
    let status = response.status();
    to_raw_response(&serde_json::json!({
        "modelKey": key.to_string(),
        "providerId": provider.id,
        "wireApi": resolved.wire_api,
        "endpoint": endpoint,
        "executed": true,
        "ok": status.is_success(),
        "httpStatus": status.as_u16(),
    }))
}

fn get_model_from_registry(registry: &ProviderRegistry, key: &ModelKey) -> ExtResult {
    let model = registry
        .model(key)
        .ok_or_else(|| ProviderError::ModelNotFound(key.to_string()))
        .map_err(to_acp_error)?;
    let resolved = registry.resolve_wire_api(key).map_err(to_acp_error)?;
    to_raw_response(&serde_json::json!({
        "model": model,
        "wireApi": resolved.wire_api,
        "wireApiSource": format!("{:?}", resolved.source),
        "providerModelOverride": registry.model_provider_override(key),
    }))
}

fn upsert_provider(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ProviderParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry
        .upsert_provider(params.provider)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    to_raw_response(&registry.snapshot())
}

fn delete_provider(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ProviderIdParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry
        .remove_provider(&params.provider_id)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    to_raw_response(&registry.snapshot())
}

fn set_provider_enabled(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ProviderEnabledParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry
        .set_provider_enabled(&params.provider_id, params.enabled)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    to_raw_response(&registry.snapshot())
}

async fn provider_status(args: &acp::ExtRequest) -> ExtResult {
    let params: ProviderIdParams = parse_params(args)?;
    let registry = registry().map_err(to_acp_error)?;
    let provider = registry
        .providers()
        .find(|provider| provider.id == params.provider_id)
        .cloned()
        .ok_or_else(|| ProviderError::ProviderNotFound(params.provider_id.clone()))
        .map_err(to_acp_error)?;
    let url = provider_probe_url(&provider).map_err(to_acp_error)?;
    let response = build_provider_request(&provider, url.clone())
        .map_err(to_acp_error)?
        .send()
        .await
        .map_err(|error| {
            acp::Error::internal_error().data(format!("provider probe failed: {error}"))
        })?;
    let status = response.status();
    to_raw_response(&ProviderStatus {
        provider_id: provider.id,
        configured: true,
        network_probe: true,
        http_status: Some(status.as_u16()),
        message: if status.is_success() {
            "provider probe succeeded".to_owned()
        } else {
            format!("provider probe returned HTTP {status}")
        },
    })
}

async fn refresh_models(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ProviderIdParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    let provider = registry
        .providers()
        .find(|provider| provider.id == params.provider_id)
        .cloned()
        .ok_or_else(|| ProviderError::ProviderNotFound(params.provider_id.clone()))
        .map_err(to_acp_error)?;
    let (refreshed, message) = match &provider.discovery {
        ProviderDiscovery::Disabled | ProviderDiscovery::Static => (
            false,
            "model discovery is disabled; returning the local catalog".to_owned(),
        ),
        ProviderDiscovery::OpenAiModels { path } => {
            let url = provider_discovery_url(&provider, path).map_err(to_acp_error)?;
            let response = build_provider_request(&provider, url)
                .map_err(to_acp_error)?
                .send()
                .await
                .map_err(|error| {
                    acp::Error::internal_error().data(format!("model discovery failed: {error}"))
                })?;
            let status = response.status();
            if !status.is_success() {
                return Err(acp::Error::internal_error()
                    .data(format!("model discovery returned HTTP {status}")));
            }
            let body = response.text().await.map_err(|error| {
                acp::Error::internal_error().data(format!("model discovery body failed: {error}"))
            })?;
            let models = atelier_provider::parse_openai_models_response(&provider.id, &body)
                .map_err(to_acp_error)?;
            let count = models.len();
            registry
                .merge_discovered_models(&provider.id, models)
                .map_err(to_acp_error)?;
            persist(&registry).map_err(to_acp_error)?;
            (true, format!("refreshed {count} models from the provider"))
        }
    };
    reload_live_catalog(agent)?;
    let models: Vec<_> = registry
        .models()
        .filter(|model| model.key.provider_id == provider.id)
        .collect();
    to_raw_response(&RefreshResult {
        provider_id: provider.id,
        refreshed,
        models,
        message,
    })
}

fn upsert_model(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ModelParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry.upsert_model(params.model).map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    to_raw_response(&registry.snapshot())
}

fn set_default_model(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ModelKeyParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    let key = parse_model_key(params)?;
    registry.set_default_model(key).map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    to_raw_response(&registry.snapshot())
}

fn set_capabilities(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: CapabilityParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    let (provider_id, model_id) = params.model_key.split_once('/').ok_or_else(|| {
        acp::Error::invalid_params().data("modelKey must use provider/model format")
    })?;
    let key = ModelKey::new(provider_id, model_id).map_err(to_acp_error)?;
    registry
        .set_capability_overrides(&key, params.overrides)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    to_raw_response(&registry.snapshot())
}

fn credential_status() -> ExtResult {
    let registry = registry().map_err(to_acp_error)?;
    let credentials: Vec<_> = registry
        .providers()
        .map(|provider| {
            let (configured, available, error_code) = match &provider.credential {
                CredentialRef::None => (false, false, None),
                credential => match credential.resolve() {
                    Ok(Some(_)) => (true, true, None),
                    Ok(None) => (true, false, None),
                    Err(error) => (true, false, Some(format!("{:?}", error.code()))),
                },
            };
            serde_json::json!({
                "providerId": provider.id,
                "configured": configured,
                "available": available,
                "errorCode": error_code,
            })
        })
        .collect();
    to_raw_response(&serde_json::json!({ "credentials": credentials }))
}

fn set_credential(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: CredentialParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    let mut provider = registry
        .provider(&params.provider_id)
        .cloned()
        .ok_or_else(|| ProviderError::ProviderNotFound(params.provider_id.clone()))
        .map_err(to_acp_error)?;
    apply_credential_update(&mut provider, params.credential, params.secret)?;
    registry.upsert_provider(provider).map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    to_raw_response(&serde_json::json!({
        "providerId": params.provider_id,
        "configured": true,
    }))
}

fn delete_credential(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ProviderIdParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    let mut provider = registry
        .provider(&params.provider_id)
        .cloned()
        .ok_or_else(|| ProviderError::ProviderNotFound(params.provider_id.clone()))
        .map_err(to_acp_error)?;
    if matches!(provider.credential, CredentialRef::SecretStore { .. }) {
        provider
            .credential
            .delete_secret()
            .map_err(ProviderError::from)
            .map_err(to_acp_error)?;
    }
    provider.credential = CredentialRef::None;
    registry.upsert_provider(provider).map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent)?;
    to_raw_response(&serde_json::json!({
        "providerId": params.provider_id,
        "configured": false,
    }))
}

fn apply_credential_update(
    provider: &mut ProviderConfig,
    credential: CredentialRef,
    secret: Option<String>,
) -> Result<(), acp::Error> {
    credential
        .validate()
        .map_err(ProviderError::from)
        .map_err(to_acp_error)?;
    if let Some(secret) = secret {
        if !matches!(credential, CredentialRef::SecretStore { .. }) {
            return Err(acp::Error::invalid_params()
                .data("secret can only be supplied with a SecretStore credential"));
        }
        credential
            .set_secret(secret.into())
            .map_err(ProviderError::from)
            .map_err(to_acp_error)?;
    }
    provider.credential = credential;
    Ok(())
}

fn provider_discovery_url(
    provider: &ProviderConfig,
    path: &str,
) -> Result<url::Url, ProviderError> {
    let path = if path.trim().is_empty() {
        "models"
    } else {
        path
    };
    let mut url = provider.base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/{}", path.trim_start_matches('/')));
    Ok(url)
}

fn provider_probe_url(provider: &ProviderConfig) -> Result<url::Url, ProviderError> {
    match &provider.discovery {
        ProviderDiscovery::OpenAiModels { path } => provider_discovery_url(provider, path),
        ProviderDiscovery::Disabled | ProviderDiscovery::Static => Ok(provider.base_url.clone()),
    }
}

fn build_provider_request(
    provider: &ProviderConfig,
    url: url::Url,
) -> Result<reqwest::RequestBuilder, ProviderError> {
    Ok(crate::http::shared_client()
        .get(url)
        .timeout(std::time::Duration::from_secs(15))
        .headers(provider_headers(provider)?))
}

fn build_provider_post_request(
    provider: &ProviderConfig,
    url: url::Url,
    wire_api: WireApi,
) -> Result<reqwest::RequestBuilder, ProviderError> {
    Ok(crate::http::shared_client()
        .post(url)
        .timeout(std::time::Duration::from_secs(15))
        .headers(provider_headers_for_wire_api(provider, Some(wire_api))?))
}

fn provider_headers(
    provider: &ProviderConfig,
) -> Result<reqwest::header::HeaderMap, ProviderError> {
    provider_headers_for_wire_api(provider, None)
}

fn provider_headers_for_wire_api(
    provider: &ProviderConfig,
    wire_api: Option<WireApi>,
) -> Result<reqwest::header::HeaderMap, ProviderError> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    let mut headers = HeaderMap::new();
    if let Some(secret) = provider.credential.resolve()? {
        let use_messages_auth = wire_api
            .map(|wire_api| wire_api == WireApi::Messages)
            .unwrap_or(matches!(
                provider.protocol,
                ProviderProtocol::AnthropicMessages
            ));
        let (name, value) = if use_messages_auth {
            ("x-api-key", secret.expose_secret().to_owned())
        } else {
            (
                "authorization",
                format!("Bearer {}", secret.expose_secret()),
            )
        };
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_str(&value).map_err(|error| {
                ProviderError::InvalidProvider(format!("invalid credential header: {error}"))
            })?,
        );
    }
    for (name, value) in &provider.extra_headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            ProviderError::InvalidProvider(format!("invalid extra header name: {error}"))
        })?;
        let value = HeaderValue::from_str(value).map_err(|error| {
            ProviderError::InvalidProvider(format!("invalid extra header value: {error}"))
        })?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn minimal_pair_test_payload(model: &str, wire_api: WireApi) -> serde_json::Value {
    match wire_api {
        WireApi::ChatCompletions => serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1,
            "stream": false,
        }),
        WireApi::Responses => serde_json::json!({
            "model": model,
            "input": "ping",
            "max_output_tokens": 1,
            "stream": false,
        }),
        WireApi::Messages => serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1,
            "stream": false,
        }),
    }
}

fn provider_endpoint(provider: &ProviderConfig, wire_api: WireApi) -> url::Url {
    let mut url = provider.base_url.clone();
    let path = url.path().trim_end_matches('/');
    url.set_path(&format!("{path}/{}", wire_api.endpoint_suffix()));
    url
}

fn parse_model_key(params: ModelKeyParams) -> Result<Option<ModelKey>, acp::Error> {
    let Some(raw) = params.model_key else {
        return match (params.provider_id, params.model_id) {
            (Some(provider_id), Some(model_id)) => ModelKey::new(provider_id, model_id)
                .map(Some)
                .map_err(to_acp_error),
            (None, None) => Ok(None),
            _ => Err(acp::Error::invalid_params()
                .data("providerId and modelId must be provided together")),
        };
    };
    let (provider_id, model_id) = raw.split_once('/').ok_or_else(|| {
        acp::Error::invalid_params().data("modelKey must use provider/model format")
    })?;
    ModelKey::new(provider_id, model_id)
        .map(Some)
        .map_err(to_acp_error)
}

fn reload_live_catalog(agent: &MvpAgent) -> Result<(), acp::Error> {
    agent
        .models_manager
        .reload_local_provider_catalog()
        .map_err(|error| acp::Error::internal_error().data(error))
}

fn to_acp_error(error: ProviderError) -> acp::Error {
    acp::Error::invalid_params().data(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{apply_credential_update, minimal_pair_test_payload, parse_model_key};
    use atelier_provider::{CredentialRef, ProviderConfig, ProviderProtocol, WireApi};
    use std::collections::BTreeMap;
    use url::Url;

    #[test]
    fn model_key_parser_accepts_composite_key() {
        let key = parse_model_key(super::ModelKeyParams {
            model_key: Some("proxy/model".into()),
            provider_id: None,
            model_id: None,
        })
        .unwrap()
        .unwrap();
        assert_eq!(key.to_string(), "proxy/model");
    }

    #[test]
    fn model_key_parser_rejects_unscoped_id() {
        assert!(
            parse_model_key(super::ModelKeyParams {
                model_key: Some("model".into()),
                provider_id: None,
                model_id: None,
            })
            .is_err()
        );
    }

    #[test]
    fn credential_update_rejects_plain_secret_for_non_secret_store() {
        let mut provider = ProviderConfig {
            id: "test".into(),
            display_name: "Test".into(),
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: Url::parse("https://provider.example/v1").unwrap(),
            credential: CredentialRef::None,
            discovery: atelier_provider::ProviderDiscovery::Disabled,
            extra_headers: BTreeMap::new(),
            enabled: true,
        };
        let error = apply_credential_update(
            &mut provider,
            CredentialRef::Environment {
                variable: "TEST_TOKEN".into(),
            },
            Some("must-not-persist".into()),
        )
        .expect_err("plain secrets must be restricted to SecretStore references");
        assert_eq!(
            error.data,
            Some(serde_json::json!(
                "secret can only be supplied with a SecretStore credential"
            ))
        );
        assert!(matches!(provider.credential, CredentialRef::None));
    }

    #[test]
    fn pair_test_payload_matches_each_wire_api() {
        let chat = minimal_pair_test_payload("model", WireApi::ChatCompletions);
        assert!(chat.get("messages").is_some());
        assert!(chat.get("input").is_none());

        let responses = minimal_pair_test_payload("model", WireApi::Responses);
        assert_eq!(responses["input"], "ping");
        assert_eq!(responses["max_output_tokens"], 1);
        assert!(responses.get("messages").is_none());

        let messages = minimal_pair_test_payload("model", WireApi::Messages);
        assert!(messages.get("messages").is_some());
        assert_eq!(messages["max_tokens"], 1);
    }
}
