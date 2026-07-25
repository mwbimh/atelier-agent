//! Local Provider and model registry ACP methods.
//!
//! These methods manage the local provider registry, model catalog, and
//! credential references exposed to ACP clients. Secret values are written
//! only to the configured OS credential store and never to `providers.toml`.

use agent_client_protocol as acp;
use atelier_provider::auth::{
    AuthorizationCodeSession, DeviceCodePoll, DeviceCodeSession, OAuthError, OAuthHttpClient,
    OAuthHttpResponse, ProviderOAuthCredentialStore, ProviderOAuthMethod,
};
use atelier_provider::{
    CapabilityOverrides, CredentialRef, ModelDescriptor, ModelKey, ProviderConfig,
    ProviderDiscovery, ProviderError, ProviderModelOverride, ProviderProtocol, ProviderRegistry,
    ProviderSnapshot, WireApi,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;

pub const PROVIDER_LIST: &str = "_atelier/provider/list";
pub const PROVIDER_CREATE: &str = "_atelier/provider/create";
pub const PROVIDER_UPDATE: &str = "_atelier/provider/update";
pub const PROVIDER_DELETE: &str = "_atelier/provider/delete";
pub const PROVIDER_TEST: &str = "_atelier/provider/test";
pub const PROVIDER_REFRESH_MODELS: &str = "_atelier/provider/refresh_models";
pub const PROVIDER_ENABLE: &str = "_atelier/provider/enable";
pub const PROVIDER_OAUTH_BEGIN: &str = "_atelier/provider/oauth_begin";
pub const PROVIDER_OAUTH_COMPLETE: &str = "_atelier/provider/oauth_complete";
pub const PROVIDER_OAUTH_LOGOUT: &str = "_atelier/provider/oauth_logout";
pub const MODEL_LIST: &str = "_atelier/model/list";
pub const MODEL_GET: &str = "_atelier/model/get";
pub const MODEL_UPDATE: &str = "_atelier/model/update";
pub const MODEL_UPDATE_WIRE_API: &str = "_atelier/model/update_wire_api";
pub const MODEL_PROVIDER_OVERRIDE_LIST: &str = "_atelier/model_provider_override/list";
pub const MODEL_PROVIDER_OVERRIDE_SET: &str = "_atelier/model_provider_override/set";
pub const MODEL_PROVIDER_OVERRIDE_DELETE: &str = "_atelier/model_provider_override/delete";
pub const MODEL_PROVIDER_OVERRIDE_TEST: &str = "_atelier/model_provider_override/test";
pub const MODEL_SET_CAPABILITIES: &str = "_atelier/model/set_capabilities";
pub const CREDENTIAL_STATUS: &str = "_atelier/credential/status";
pub const CREDENTIAL_SET: &str = "_atelier/credential/set";
pub const CREDENTIAL_DELETE: &str = "_atelier/credential/delete";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderParams {
    provider: ProviderConfig,
    #[serde(default)]
    preserve_existing: bool,
    #[serde(default)]
    preserve_existing_credential: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderIdParams {
    provider_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthBeginParams {
    provider_id: String,
    #[serde(default)]
    flow: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthCompleteParams {
    login_id: String,
}

enum PendingOAuthLogin {
    AuthorizationCode {
        provider_id: String,
        session: AuthorizationCodeSession,
    },
    DeviceCode {
        provider_id: String,
        session: DeviceCodeSession,
    },
}

fn pending_oauth_logins() -> &'static Mutex<HashMap<String, PendingOAuthLogin>> {
    static PENDING: OnceLock<Mutex<HashMap<String, PendingOAuthLogin>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
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
        | "atelier/provider/update" => upsert_provider(agent, args).await,
        PROVIDER_DELETE | "atelier/provider/delete" => delete_provider(agent, args).await,
        PROVIDER_TEST | "atelier/provider/test" => provider_status(args).await,
        PROVIDER_REFRESH_MODELS | "atelier/provider/refresh_models" => {
            refresh_models(agent, args).await
        }
        PROVIDER_ENABLE | "atelier/provider/enable" => set_provider_enabled(agent, args).await,
        PROVIDER_OAUTH_BEGIN | "atelier/provider/oauth_begin" => begin_oauth_login(args).await,
        PROVIDER_OAUTH_COMPLETE | "atelier/provider/oauth_complete" => {
            complete_oauth_login(agent, args).await
        }
        PROVIDER_OAUTH_LOGOUT | "atelier/provider/oauth_logout" => logout_oauth(agent, args).await,
        MODEL_GET | "atelier/model/get" => get_model(args),
        MODEL_UPDATE | "atelier/model/update" => upsert_model(agent, args).await,
        MODEL_UPDATE_WIRE_API | "atelier/model/update_wire_api" => {
            update_model_wire_api(agent, args).await
        }
        MODEL_PROVIDER_OVERRIDE_LIST | "atelier/model_provider_override/list" => {
            list_model_provider_overrides(args)
        }
        MODEL_PROVIDER_OVERRIDE_SET | "atelier/model_provider_override/set" => {
            set_model_provider_override(agent, args).await
        }
        MODEL_PROVIDER_OVERRIDE_DELETE | "atelier/model_provider_override/delete" => {
            delete_model_provider_override(agent, args).await
        }
        MODEL_PROVIDER_OVERRIDE_TEST | "atelier/model_provider_override/test" => {
            test_model_provider_override(args).await
        }
        MODEL_SET_CAPABILITIES | "atelier/model/set_capabilities" => {
            set_capabilities(agent, args).await
        }
        CREDENTIAL_STATUS | "atelier/credential/status" => credential_status(),
        CREDENTIAL_SET | "atelier/credential/set" => set_credential(agent, args).await,
        CREDENTIAL_DELETE | "atelier/credential/delete" => delete_credential(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

fn registry() -> Result<ProviderRegistry, ProviderError> {
    ProviderRegistry::load_or_create(atelier_config::atelier_home().join("providers.toml"))
}

fn persist(registry: &ProviderRegistry) -> Result<(), ProviderError> {
    registry.save()
}

fn select_oauth_method(
    provider: &ProviderConfig,
    requested_flow: Option<&str>,
) -> Result<ProviderOAuthMethod, ProviderError> {
    let CredentialRef::OAuth {
        provider_id,
        methods,
    } = &provider.credential
    else {
        return Err(ProviderError::InvalidProvider(format!(
            "Provider {} does not declare OAuth methods; configure client_id and OAuth endpoints in providers.toml",
            provider.id
        )));
    };
    if provider_id != &provider.id {
        return Err(ProviderError::InvalidProvider(format!(
            "Provider {} OAuth namespace is configured as {provider_id}",
            provider.id
        )));
    }
    let method = match requested_flow {
        Some(flow) => methods
            .iter()
            .find(|method| method.flow_name() == flow)
            .cloned()
            .ok_or_else(|| {
                ProviderError::InvalidProvider(format!(
                    "Provider {} does not configure OAuth flow {flow}",
                    provider.id
                ))
            })?,
        None => methods.first().cloned().ok_or_else(|| {
            ProviderError::InvalidProvider(format!(
                "Provider {} does not declare OAuth methods; configure client_id and OAuth endpoints in providers.toml",
                provider.id
            ))
        })?,
    };
    method
        .validate(&provider.id)
        .map_err(|error| ProviderError::InvalidProvider(error.to_string()))?;
    Ok(method)
}

async fn begin_oauth_login(args: &acp::ExtRequest) -> ExtResult {
    let params: OAuthBeginParams = parse_params(args)?;
    let registry = registry().map_err(to_acp_error)?;
    let provider = registry
        .provider(&params.provider_id)
        .cloned()
        .ok_or_else(|| ProviderError::ProviderNotFound(params.provider_id.clone()))
        .map_err(to_acp_error)?;
    let method = select_oauth_method(&provider, params.flow.as_deref()).map_err(to_acp_error)?;
    let provider_id = provider.id.clone();
    let login_id = uuid::Uuid::new_v4().to_string();
    let login_id_for_task = login_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let client = RuntimeOAuthHttpClient;
        match method {
            method @ ProviderOAuthMethod::AuthorizationCode { .. } => {
                let session = AuthorizationCodeSession::begin(
                    method.authorization_code_config(&provider_id)?,
                )?;
                let url = session.authorization_url().to_string();
                let browser_opened = webbrowser::open(&url).is_ok();
                pending_oauth_logins()
                    .lock()
                    .map_err(|_| OAuthError::transport("OAuth login state lock is poisoned"))?
                    .insert(
                        login_id_for_task.clone(),
                        PendingOAuthLogin::AuthorizationCode {
                            provider_id: provider_id.clone(),
                            session,
                        },
                    );
                Ok::<_, OAuthError>(serde_json::json!({
                    "providerId": provider_id,
                    "loginId": login_id_for_task,
                    "flow": "authorization-code",
                    "verificationUrl": url,
                    "userCode": null,
                    "browserOpened": browser_opened,
                    "message": "Complete OAuth authorization in the browser",
                }))
            }
            method @ ProviderOAuthMethod::DeviceCode { .. } => {
                let session =
                    DeviceCodeSession::begin(&client, method.device_code_config(&provider_id)?)?;
                let url = session
                    .verification_uri_complete()
                    .unwrap_or_else(|| session.verification_uri())
                    .to_string();
                let user_code = session.user_code().to_owned();
                let browser_opened = webbrowser::open(&url).is_ok();
                pending_oauth_logins()
                    .lock()
                    .map_err(|_| OAuthError::transport("OAuth login state lock is poisoned"))?
                    .insert(
                        login_id_for_task.clone(),
                        PendingOAuthLogin::DeviceCode {
                            provider_id: provider_id.clone(),
                            session,
                        },
                    );
                Ok(serde_json::json!({
                    "providerId": provider_id,
                    "loginId": login_id_for_task,
                    "flow": "device-code",
                    "verificationUrl": url,
                    "userCode": user_code,
                    "browserOpened": browser_opened,
                    "message": "Enter the device code, then return to Atelier",
                }))
            }
        }
    })
    .await
    .map_err(|error| {
        acp::Error::internal_error().data(format!("Provider OAuth setup task failed: {error}"))
    })?
    .map_err(to_oauth_acp_error)?;
    to_raw_response(&result)
}

async fn complete_oauth_login(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: OAuthCompleteParams = parse_params(args)?;
    let pending = pending_oauth_logins()
        .lock()
        .map_err(|_| acp::Error::internal_error().data("OAuth login state lock is poisoned"))?
        .remove(&params.login_id)
        .ok_or_else(|| {
            acp::Error::invalid_params()
                .data("OAuth login is missing, expired, or already completed")
        })?;
    let home = atelier_config::atelier_home();
    let provider_id = tokio::task::spawn_blocking(move || {
        let client = RuntimeOAuthHttpClient;
        let (provider_id, credential) = match pending {
            PendingOAuthLogin::AuthorizationCode {
                provider_id,
                session,
            } => {
                let credential = session.complete(&client, Duration::from_secs(600))?;
                (provider_id, credential)
            }
            PendingOAuthLogin::DeviceCode {
                provider_id,
                mut session,
            } => {
                let credential = loop {
                    std::thread::sleep(session.poll_interval());
                    match session.poll_once(&client)? {
                        DeviceCodePoll::Pending | DeviceCodePoll::SlowDown => continue,
                        DeviceCodePoll::Complete(credential) => break credential,
                    }
                };
                (provider_id, credential)
            }
        };
        ProviderOAuthCredentialStore::system(home).save(&provider_id, &credential)?;
        Ok::<_, OAuthError>(provider_id)
    })
    .await
    .map_err(|error| {
        acp::Error::internal_error().data(format!("Provider OAuth completion task failed: {error}"))
    })?
    .map_err(to_oauth_acp_error)?;
    reload_live_catalog(agent).await?;
    to_raw_response(&serde_json::json!({
        "providerId": provider_id,
        "loggedIn": true,
        "message": "Provider OAuth login completed",
    }))
}

async fn logout_oauth(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ProviderIdParams = parse_params(args)?;
    let registry = registry().map_err(to_acp_error)?;
    let provider = registry
        .provider(&params.provider_id)
        .ok_or_else(|| ProviderError::ProviderNotFound(params.provider_id.clone()))
        .map_err(to_acp_error)?;
    if !matches!(provider.credential, CredentialRef::OAuth { .. }) {
        return Err(acp::Error::invalid_params().data(format!(
            "Provider {} is not configured for OAuth",
            params.provider_id
        )));
    }
    let removed = ProviderOAuthCredentialStore::system(atelier_config::atelier_home())
        .delete(&params.provider_id)
        .map_err(to_oauth_acp_error)?;
    reload_live_catalog(agent).await?;
    to_raw_response(&serde_json::json!({
        "providerId": params.provider_id,
        "loggedOut": removed,
        "message": if removed {
            "Provider OAuth credential removed"
        } else {
            "Provider OAuth credential was not present"
        },
    }))
}

struct RuntimeOAuthHttpClient;

impl OAuthHttpClient for RuntimeOAuthHttpClient {
    fn post_form(
        &self,
        url: &url::Url,
        form: &[(String, String)],
    ) -> Result<OAuthHttpResponse, OAuthError> {
        let response = crate::http::shared_blocking_client()
            .post(url.clone())
            .form(form)
            .send()
            .map_err(|error| OAuthError::transport(error.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .map_err(|error| OAuthError::transport(error.to_string()))?
            .to_vec();
        Ok(OAuthHttpResponse { status, body })
    }
}

fn to_oauth_acp_error(error: OAuthError) -> acp::Error {
    if matches!(
        &error,
        OAuthError::InvalidConfig(_) | OAuthError::InvalidProviderId(_)
    ) {
        acp::Error::invalid_params().data(error.to_string())
    } else {
        acp::Error::internal_error().data(error.to_string())
    }
}

fn redacted_model_override(registry: &ProviderRegistry, key: &ModelKey) -> serde_json::Value {
    let value = serde_json::to_value(registry.model_provider_override(key))
        .unwrap_or(serde_json::Value::Null);
    atelier_acp_runtime::redact_payload(&value)
}

fn list(args: &acp::ExtRequest) -> ExtResult {
    let registry = registry().map_err(to_acp_error)?;
    if args.method.as_ref() == MODEL_LIST || args.method.as_ref() == "atelier/model/list" {
        return to_raw_response(&serde_json::json!({
            "models": registry.snapshot().models,
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
    let experimental = registry
        .experimental_model_features(&key)
        .map_err(to_acp_error)?;
    to_raw_response(&serde_json::json!({
        "model": model,
        "wireApi": resolved.wire_api,
        "wireApiSource": format!("{:?}", resolved.source),
        "providerModelOverride": redacted_model_override(&registry, &key),
        "experimental": experimental,
    }))
}

async fn update_model_wire_api(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: WireApiParams = parse_params(args)?;
    let key = ModelKey::parse(&params.model_key).map_err(to_acp_error)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry
        .set_model_wire_api(&key, params.wire_api)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent).await?;
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

async fn set_model_provider_override(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
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
    reload_live_catalog(agent).await?;
    get_model_from_registry(&registry, &key)
}

async fn delete_model_provider_override(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ModelOverrideDeleteParams = parse_params(args)?;
    let key = ModelKey::parse(&params.model_key).map_err(to_acp_error)?;
    let mut registry = registry().map_err(to_acp_error)?;
    let removed = registry
        .remove_model_provider_override(&key)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent).await?;
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
    let (config, request) = pair_test_runtime_request(&registry, &key).map_err(to_acp_error)?;
    let client = crate::sampling::Client::new(config).map_err(|error| {
        acp::Error::internal_error().data(format!("model pair test setup failed: {error}"))
    })?;
    client
        .conversation_collect(request)
        .await
        .map_err(|error| {
            acp::Error::internal_error().data(format!("model pair test failed: {error}"))
        })?;
    to_raw_response(&serde_json::json!({
        "modelKey": key.to_string(),
        "providerId": provider.id,
        "wireApi": resolved.wire_api,
        "endpoint": endpoint,
        "executed": true,
        "ok": true,
        "message": "pair test completed through the runtime sampler",
    }))
}

fn pair_test_runtime_request(
    registry: &ProviderRegistry,
    key: &ModelKey,
) -> Result<
    (
        atelier_sampler::SamplerConfig,
        crate::sampling::ConversationRequest,
    ),
    ProviderError,
> {
    let models =
        crate::agent::config::model_entries_from_provider_snapshot(&registry.snapshot(), None);
    let config = crate::agent::config::resolve_model_to_sampling_config(
        &key.to_string(),
        &models,
        None,
        None,
        None,
        None,
    )
    .ok_or_else(|| ProviderError::ModelNotFound(key.to_string()))?;
    let request = crate::sampling::ConversationRequest::from_items(vec![
        crate::sampling::ConversationItem::user("Reply with OK."),
    ])
    .with_model(&config.model);
    Ok((config, request))
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
        "providerModelOverride": redacted_model_override(registry, key),
    }))
}

async fn upsert_provider(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let mut params: ProviderParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    let existing_credential = registry
        .provider(&params.provider.id)
        .map(|provider| provider.credential.clone());
    if params.preserve_existing {
        preserve_existing_provider_fields(
            &registry,
            &mut params.provider,
            params.preserve_existing_credential,
        )
        .map_err(to_acp_error)?;
    }
    let replacement_credential = params.provider.credential.clone();
    registry
        .upsert_provider(params.provider)
        .map_err(to_acp_error)?;
    delete_replaced_secret_store_credential(existing_credential.as_ref(), &replacement_credential)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent).await?;
    to_raw_response(&registry.snapshot())
}

fn preserve_existing_provider_fields(
    registry: &ProviderRegistry,
    provider: &mut ProviderConfig,
    preserve_credential: bool,
) -> Result<(), ProviderError> {
    let existing = registry
        .providers()
        .find(|existing| existing.id == provider.id)
        .cloned()
        .ok_or_else(|| ProviderError::ProviderNotFound(provider.id.clone()))?;
    provider.display_name = existing.display_name;
    provider.discovery = existing.discovery;
    provider.extra_headers = existing.extra_headers;
    provider.enabled = existing.enabled;
    if preserve_credential {
        provider.credential = existing.credential;
    }
    Ok(())
}

fn delete_replaced_secret_store_credential(
    existing: Option<&CredentialRef>,
    replacement: &CredentialRef,
) -> Result<(), ProviderError> {
    let Some(existing @ CredentialRef::SecretStore { .. }) = existing else {
        return Ok(());
    };
    if existing == replacement {
        return Ok(());
    }
    existing.delete_secret().map_err(ProviderError::from)
}

async fn delete_provider(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ProviderIdParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry
        .remove_provider(&params.provider_id)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent).await?;
    to_raw_response(&registry.snapshot())
}

async fn set_provider_enabled(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ProviderEnabledParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry
        .set_provider_enabled(&params.provider_id, params.enabled)
        .map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent).await?;
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
    to_raw_response(&provider_probe_outcome(&provider.id, status)?)
}

fn provider_probe_outcome(
    provider_id: &str,
    status: reqwest::StatusCode,
) -> Result<ProviderStatus, acp::Error> {
    if !status.is_success() {
        return Err(
            acp::Error::internal_error().data(format!("provider probe returned HTTP {status}"))
        );
    }
    Ok(ProviderStatus {
        provider_id: provider_id.to_owned(),
        configured: true,
        network_probe: true,
        http_status: Some(status.as_u16()),
        message: "provider probe succeeded".to_owned(),
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
    reload_live_catalog(agent).await?;
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

async fn upsert_model(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: ModelParams = parse_params(args)?;
    let mut registry = registry().map_err(to_acp_error)?;
    registry.upsert_model(params.model).map_err(to_acp_error)?;
    persist(&registry).map_err(to_acp_error)?;
    reload_live_catalog(agent).await?;
    to_raw_response(&registry.snapshot())
}

async fn set_capabilities(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
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
    reload_live_catalog(agent).await?;
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

async fn set_credential(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
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
    reload_live_catalog(agent).await?;
    to_raw_response(&serde_json::json!({
        "providerId": params.provider_id,
        "configured": true,
    }))
}

async fn delete_credential(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
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
    reload_live_catalog(agent).await?;
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

async fn reload_live_catalog(agent: &MvpAgent) -> Result<(), acp::Error> {
    agent
        .reload_local_provider_catalog_and_reconcile_sessions()
        .await
        .map_err(|error| acp::Error::internal_error().data(error))
}

fn to_acp_error(error: ProviderError) -> acp::Error {
    acp::Error::invalid_params().data(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        apply_credential_update, delete_replaced_secret_store_credential,
        pair_test_runtime_request, parse_model_key, preserve_existing_provider_fields,
        provider_probe_outcome, redacted_model_override, select_oauth_method,
    };
    use atelier_provider::{
        CredentialRef, ModelCapabilities, ModelDescriptor, ModelKey, ModelSource, ProviderConfig,
        ProviderModelOverride, ProviderProtocol, ProviderRegistry, WireApi,
    };
    use std::collections::BTreeMap;
    use url::Url;

    fn oauth_test_provider() -> ProviderConfig {
        ProviderConfig {
            id: "allm".into(),
            display_name: "AllM".into(),
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: Url::parse("https://provider.example/v1").unwrap(),
            credential: CredentialRef::None,
            discovery: atelier_provider::ProviderDiscovery::Disabled,
            extra_headers: BTreeMap::new(),
            enabled: true,
        }
    }

    #[test]
    fn oauth_method_selection_requires_declared_provider_configuration() {
        let provider = oauth_test_provider();
        let error = select_oauth_method(&provider, None).unwrap_err();
        assert!(error.to_string().contains("does not declare OAuth methods"));
    }

    #[test]
    fn oauth_method_selection_supports_explicit_and_single_method_flows() {
        let mut provider = oauth_test_provider();
        provider.credential = CredentialRef::OAuth {
            provider_id: provider.id.clone(),
            methods: vec![atelier_provider::auth::ProviderOAuthMethod::device_code(
                "desktop-client",
                Url::parse("https://login.example.test/device").unwrap(),
                Url::parse("https://login.example.test/token").unwrap(),
            )],
        };
        assert_eq!(
            select_oauth_method(&provider, None).unwrap().flow_name(),
            "device-code"
        );
        assert_eq!(
            select_oauth_method(&provider, Some("device-code"))
                .unwrap()
                .flow_name(),
            "device-code"
        );
        assert!(select_oauth_method(&provider, Some("authorization-code")).is_err());
    }

    #[test]
    fn provider_probe_rejects_non_success_http_statuses() {
        for status in [
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let error = provider_probe_outcome("allm", status)
                .expect_err("non-success Provider probes must fail the RPC");
            let message = error.data.expect("error detail").to_string();
            assert!(message.contains(status.as_str()));
        }
    }

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
    fn provider_edit_preserves_fields_not_exposed_by_the_slash_command() {
        let mut registry = ProviderRegistry::in_memory();
        registry
            .upsert_provider(ProviderConfig {
                id: "proxy".into(),
                display_name: "Company Proxy".into(),
                protocol: ProviderProtocol::OpenAiResponses,
                base_url: Url::parse("https://old.example/v1").unwrap(),
                credential: CredentialRef::None,
                discovery: atelier_provider::ProviderDiscovery::Static,
                extra_headers: BTreeMap::from([("x-tenant".into(), "kept".into())]),
                enabled: false,
            })
            .unwrap();
        let mut edited = ProviderConfig {
            id: "proxy".into(),
            display_name: "proxy".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: Url::parse("https://new.example/v1").unwrap(),
            credential: CredentialRef::Environment {
                variable: "PROXY_KEY".into(),
            },
            discovery: atelier_provider::ProviderDiscovery::OpenAiModels {
                path: "models".into(),
            },
            extra_headers: BTreeMap::new(),
            enabled: true,
        };

        preserve_existing_provider_fields(&registry, &mut edited, false).unwrap();

        assert_eq!(edited.display_name, "Company Proxy");
        assert!(matches!(
            edited.discovery,
            atelier_provider::ProviderDiscovery::Static
        ));
        assert_eq!(edited.extra_headers["x-tenant"], "kept");
        assert!(!edited.enabled);
        assert_eq!(edited.protocol, ProviderProtocol::OpenAiChatCompletions);
        assert_eq!(edited.base_url.as_str(), "https://new.example/v1");
    }

    #[test]
    fn provider_edit_preserves_credential_when_the_slash_command_omits_it() {
        let mut registry = ProviderRegistry::in_memory();
        registry
            .upsert_provider(ProviderConfig {
                id: "proxy".into(),
                display_name: "Proxy".into(),
                protocol: ProviderProtocol::OpenAiResponses,
                base_url: Url::parse("https://old.example/v1").unwrap(),
                credential: CredentialRef::Environment {
                    variable: "PROXY_API_KEY".into(),
                },
                discovery: atelier_provider::ProviderDiscovery::Static,
                extra_headers: BTreeMap::new(),
                enabled: true,
            })
            .unwrap();
        let mut edited = ProviderConfig {
            id: "proxy".into(),
            display_name: "proxy".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: Url::parse("https://new.example/v1").unwrap(),
            credential: CredentialRef::None,
            discovery: atelier_provider::ProviderDiscovery::OpenAiModels {
                path: "models".into(),
            },
            extra_headers: BTreeMap::new(),
            enabled: true,
        };

        preserve_existing_provider_fields(&registry, &mut edited, true).unwrap();

        assert_eq!(
            edited.credential,
            CredentialRef::Environment {
                variable: "PROXY_API_KEY".into(),
            }
        );
        assert_eq!(edited.protocol, ProviderProtocol::OpenAiChatCompletions);
        assert_eq!(edited.base_url.as_str(), "https://new.example/v1");
    }

    #[cfg(windows)]
    #[test]
    fn provider_edit_explicit_none_deletes_existing_secret_store_secret() {
        let credential = CredentialRef::SecretStore {
            service: format!(
                "atelier-shell-provider-edit-none-test-{}",
                std::process::id()
            ),
            account: "test-account".into(),
        };
        let _cleanup = CredentialCleanup(credential.clone());
        let _ = credential.delete_secret();
        credential.set_secret("secret-to-delete".into()).unwrap();

        let mut registry = ProviderRegistry::in_memory();
        registry
            .upsert_provider(ProviderConfig {
                id: "proxy".into(),
                display_name: "Proxy".into(),
                protocol: ProviderProtocol::OpenAiResponses,
                base_url: Url::parse("https://old.example/v1").unwrap(),
                credential: credential.clone(),
                discovery: atelier_provider::ProviderDiscovery::Static,
                extra_headers: BTreeMap::new(),
                enabled: true,
            })
            .unwrap();
        let edited = ProviderConfig {
            id: "proxy".into(),
            display_name: "Proxy".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: Url::parse("https://new.example/v1").unwrap(),
            credential: CredentialRef::None,
            discovery: atelier_provider::ProviderDiscovery::Static,
            extra_headers: BTreeMap::new(),
            enabled: true,
        };
        let existing_credential = registry
            .provider("proxy")
            .map(|provider| provider.credential.clone());
        let replacement_credential = edited.credential.clone();

        registry.upsert_provider(edited).unwrap();
        delete_replaced_secret_store_credential(
            existing_credential.as_ref(),
            &replacement_credential,
        )
        .unwrap();

        assert!(matches!(
            &registry.provider("proxy").unwrap().credential,
            CredentialRef::None
        ));
        assert!(credential.resolve().is_err());
    }

    #[cfg(windows)]
    #[test]
    fn provider_edit_omitted_credential_keeps_existing_secret_store_secret() {
        let credential = CredentialRef::SecretStore {
            service: format!(
                "atelier-shell-provider-edit-preserve-test-{}",
                std::process::id()
            ),
            account: "test-account".into(),
        };
        let _cleanup = CredentialCleanup(credential.clone());
        let _ = credential.delete_secret();
        credential.set_secret("secret-to-preserve".into()).unwrap();

        let mut registry = ProviderRegistry::in_memory();
        registry
            .upsert_provider(ProviderConfig {
                id: "proxy".into(),
                display_name: "Proxy".into(),
                protocol: ProviderProtocol::OpenAiResponses,
                base_url: Url::parse("https://old.example/v1").unwrap(),
                credential: credential.clone(),
                discovery: atelier_provider::ProviderDiscovery::Static,
                extra_headers: BTreeMap::new(),
                enabled: true,
            })
            .unwrap();
        let mut edited = ProviderConfig {
            id: "proxy".into(),
            display_name: "proxy".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: Url::parse("https://new.example/v1").unwrap(),
            credential: CredentialRef::None,
            discovery: atelier_provider::ProviderDiscovery::OpenAiModels {
                path: "models".into(),
            },
            extra_headers: BTreeMap::new(),
            enabled: true,
        };

        preserve_existing_provider_fields(&registry, &mut edited, true).unwrap();
        delete_replaced_secret_store_credential(
            registry
                .provider("proxy")
                .map(|provider| &provider.credential),
            &edited.credential,
        )
        .unwrap();

        assert_eq!(edited.credential, credential);
        assert!(credential.resolve().unwrap().is_some());
    }

    #[cfg(windows)]
    struct CredentialCleanup(CredentialRef);

    #[cfg(windows)]
    impl Drop for CredentialCleanup {
        fn drop(&mut self) {
            let _ = self.0.delete_secret();
        }
    }

    #[test]
    fn pair_test_runtime_request_uses_model_override_config() {
        let key = ModelKey::new("proxy", "model").unwrap();
        let mut registry = ProviderRegistry::in_memory();
        registry
            .upsert_provider(ProviderConfig {
                id: "proxy".into(),
                display_name: "Proxy".into(),
                protocol: ProviderProtocol::OpenAiResponses,
                base_url: Url::parse("https://provider.example/v1").unwrap(),
                credential: CredentialRef::None,
                discovery: atelier_provider::ProviderDiscovery::Disabled,
                extra_headers: BTreeMap::from([("x-provider".into(), "enabled".into())]),
                enabled: true,
            })
            .unwrap();
        registry
            .upsert_model(ModelDescriptor {
                key: key.clone(),
                display_name: "Model".into(),
                description: None,
                wire_api: None,
                context_window: Some(32_000),
                capabilities: ModelCapabilities::default(),
                reasoning_efforts: Vec::new(),
                default_effort: None,
                fast_mode: false,
                source: ModelSource::Static,
                enabled: true,
            })
            .unwrap();
        registry
            .set_model_provider_override(
                &key,
                ProviderModelOverride {
                    wire_api: Some(WireApi::ChatCompletions),
                    payload: serde_json::from_value(serde_json::json!({
                        "temperature": 0.2,
                        "provider_option": { "budget": 123 }
                    }))
                    .unwrap(),
                },
            )
            .unwrap();

        let (config, request) = pair_test_runtime_request(&registry, &key).unwrap();

        assert_eq!(config.model, "model");
        assert_eq!(
            config.api_backend,
            atelier_sampler::ApiBackend::ChatCompletions
        );
        assert_eq!(config.request_payload["temperature"], 0.2);
        assert_eq!(config.request_payload["provider_option"]["budget"], 123);
        assert_eq!(config.extra_headers["x-provider"], "enabled");
        assert_eq!(request.max_output_tokens, None);
        assert_eq!(request.items.len(), 1);
    }

    #[test]
    fn model_override_rpc_view_redacts_sensitive_payload_values() {
        let key = ModelKey::new("allm", "deepseek-v4-flash").unwrap();
        let mut registry = ProviderRegistry::in_memory();
        registry
            .upsert_provider(ProviderConfig {
                id: "allm".into(),
                display_name: "allm".into(),
                protocol: ProviderProtocol::OpenAiChatCompletions,
                base_url: Url::parse("https://provider.example/v1").unwrap(),
                credential: CredentialRef::None,
                discovery: atelier_provider::ProviderDiscovery::Disabled,
                extra_headers: BTreeMap::new(),
                enabled: true,
            })
            .unwrap();
        registry
            .upsert_model(ModelDescriptor {
                key: key.clone(),
                display_name: "deepseek-v4-flash".into(),
                description: None,
                wire_api: None,
                context_window: None,
                capabilities: ModelCapabilities::default(),
                reasoning_efforts: Vec::new(),
                default_effort: None,
                fast_mode: false,
                source: ModelSource::Static,
                enabled: true,
            })
            .unwrap();
        registry
            .set_model_provider_override(
                &key,
                ProviderModelOverride {
                    wire_api: Some(WireApi::ChatCompletions),
                    payload: serde_json::from_value(serde_json::json!({
                        "diagnostic_note": "Authorization: Bearer audit-secret",
                        "temperature": 0.2
                    }))
                    .unwrap(),
                },
            )
            .unwrap();

        let redacted = redacted_model_override(&registry, &key);
        let encoded = redacted.to_string();
        assert!(!encoded.contains("audit-secret"));
        assert_eq!(redacted["payload"]["temperature"], 0.2);
    }
}
