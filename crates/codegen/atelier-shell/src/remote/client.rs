//! HTTP client for backend CRUD operations.
use crate::auth::{AtelierAuth, AtelierComConfig};
use crate::session::export::{ExportedMessage, ExportedMetadata, ExportedSession};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::time::Duration;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Build a share URL from a permission ID
pub fn share_url(permission_id: &str) -> String {
    format!("atelier://share/{}", permission_id)
}
fn add_cli_chat_proxy_headers_blocking(
    builder: reqwest::blocking::RequestBuilder,
    auth: &AtelierAuth,
    alpha_test_key: Option<&str>,
    url: &str,
) -> reqwest::blocking::RequestBuilder {
    let mut builder = builder
        .header("Authorization", format!("Bearer {}", &auth.key))
        .header("X-XAI-Token-Auth", AtelierComConfig::default().token_header)
        .header("x-userid", &auth.user_id)
        .header("x-atelier-client-version", atelier_version::VERSION);
    if let Some(email) = &auth.email {
        builder = builder.header("x-email", email);
    }
    let _ = (alpha_test_key, url);
    builder
        .header(
            "x-atelier-client-identifier",
            crate::http::process_client_identifier(),
        )
        .header(
            crate::http::CLIENT_MODE_HEADER,
            crate::http::process_client_mode(),
        )
}
async fn parse_json_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, BackendError> {
    let bytes = response.bytes().await?;
    serde_json::from_slice(&bytes).map_err(BackendError::from)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareResponse {
    pub permission_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadDataResponse {
    pub messages: Option<Vec<LoadedMessage>>,
    pub session: Option<SessionInfo>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedMessage {
    pub id: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveDataRequest {
    pub messages: Vec<ExportedMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertSessionRequest {
    pub session: SessionUpdate,
    pub agent_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("vendor backend is disabled in the private build")]
    Disabled,
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Request failed: {status} - {body}")]
    RequestFailed { status: u16, body: String },
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Session not found: {session_id}")]
    SessionNotFound { session_id: String },
    #[error("Hydration I/O error at {path}: {source}")]
    Hydration {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("Auth error: {0}")]
    Auth(String),
}
pub struct BackendClient {
    reqwest_client: reqwest::Client,
    client: reqwest_middleware::ClientWithMiddleware,
    base_url: String,
    pub(crate) auth_manager: Option<std::sync::Arc<crate::auth::AuthManager>>,
}
impl Default for BackendClient {
    fn default() -> Self {
        Self::new()
    }
}
impl BackendClient {
    fn build_default_client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .expect("failed to build HTTP client")
    }
    pub fn new() -> Self {
        let reqwest_client = Self::build_default_client();
        Self {
            client: reqwest_middleware::ClientBuilder::new(reqwest_client.clone()).build(),
            reqwest_client,
            base_url: String::new(),
            auth_manager: None,
        }
    }
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let reqwest_client = Self::build_default_client();
        Self {
            client: reqwest_middleware::ClientBuilder::new(reqwest_client.clone()).build(),
            reqwest_client,
            base_url: base_url.into(),
            auth_manager: None,
        }
    }
    /// Attach a live `AuthManager` so every request resolves a fresh token
    /// instead of requiring the caller to pass `&AtelierAuth`.
    pub fn with_auth_manager(mut self, manager: std::sync::Arc<crate::auth::AuthManager>) -> Self {
        let credentials: std::sync::Arc<dyn atelier_auth::AuthCredentialProvider> =
            std::sync::Arc::new(
                crate::auth::credential_provider::ShellAuthCredentialProvider::new(
                    manager.clone(),
                    None,
                    None,
                ),
            );
        self.client = crate::http::with_auth_retry(self.reqwest_client.clone(), credentials);
        self.auth_manager = Some(manager);
        self
    }
    /// Resolve auth from the attached `AuthManager`.
    async fn resolve_auth(&self) -> Result<AtelierAuth, BackendError> {
        let manager = self
            .auth_manager
            .as_ref()
            .ok_or_else(|| BackendError::Auth("No AuthManager configured".into()))?;
        manager
            .auth()
            .await
            .map_err(|e| BackendError::Auth(format!("{e}")))
    }
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
    /// Upload session and create share link.
    ///
    /// The session data (`save_session_data`) is sent inline to the backend.
    /// If the backend responds with 413 (payload too large), the error is
    /// logged as a warning and the share continues — the caller is expected
    /// to have already uploaded the data to GCS via a signed URL as a
    /// fallback.
    pub async fn share_session(
        &self,
        session: &ExportedSession,
        agent_id: &str,
    ) -> Result<String, BackendError> {
        self.upsert_session(&session.session_id, &session.metadata, agent_id)
            .await?;
        match self
            .save_session_data(
                &session.session_id,
                &session.messages,
                Some(&session.metadata),
            )
            .await
        {
            Ok(()) => {}
            Err(BackendError::RequestFailed { status: 413, .. }) => {
                tracing::warn!(
                    session_id = % session.session_id,
                    "Backend returned 413 for save_session_data; \
                     session data should already be in GCS via signed URL"
                );
            }
            Err(e) => return Err(e),
        }
        let share_response = self.create_share_link(&session.session_id).await?;
        Ok(share_url(&share_response.permission_id))
    }
    /// Sync session to backend without creating a share link.
    pub async fn sync_session(
        &self,
        session: &ExportedSession,
        agent_id: &str,
    ) -> Result<(), BackendError> {
        self.upsert_session(&session.session_id, &session.metadata, agent_id)
            .await?;
        self.save_session_data(
            &session.session_id,
            &session.messages,
            Some(&session.metadata),
        )
        .await?;
        Ok(())
    }
    /// Build auth + identity headers.
    /// Must include X-XAI-Token-Auth so nginx auth subrequest routes to authenticate_atelier_cli_token.
    /// See: crates/codegen/atelier-shell/src/agent/app.rs:run_headless
    async fn auth_header_map(&self) -> Result<reqwest::header::HeaderMap, BackendError> {
        use reqwest::header::{HeaderMap, HeaderValue};
        let auth = self.resolve_auth().await?;
        let mut headers = HeaderMap::new();
        let required = |value: &str, name: &str| -> Result<HeaderValue, BackendError> {
            HeaderValue::from_str(value)
                .map_err(|e| BackendError::Auth(format!("invalid {name} header: {e}")))
        };
        headers.insert(
            "X-XAI-Token-Auth",
            required(
                &AtelierComConfig::default().token_header,
                "X-XAI-Token-Auth",
            )?,
        );
        headers.insert("x-userid", required(&auth.user_id, "x-userid")?);
        if let Some(email) = &auth.email
            && let Ok(v) = HeaderValue::from_str(email)
        {
            headers.insert("x-email", v);
        }
        if let Ok(v) = HeaderValue::from_str(&crate::http::process_client_identifier()) {
            headers.insert("x-atelier-client-identifier", v);
        }
        headers.insert(
            crate::http::CLIENT_MODE_HEADER,
            HeaderValue::from_static(crate::http::process_client_mode()),
        );
        headers.insert(
            "x-atelier-client-version",
            HeaderValue::from_static(atelier_version::VERSION),
        );
        Ok(headers)
    }
    async fn send_with_auth(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, BackendError> {
        let headers = self.auth_header_map().await?;
        let builder = xai_file_utils::trace_context::inject_trace_context_into_request(
            builder.headers(headers),
        );
        let request = builder.build()?;
        self.client.execute(request).await.map_err(|e| match e {
            reqwest_middleware::Error::Reqwest(e) => BackendError::Network(e),
            reqwest_middleware::Error::Middleware(e) => BackendError::Auth(e.to_string()),
        })
    }
    pub async fn upsert_session(
        &self,
        session_id: &str,
        metadata: &ExportedMetadata,
        agent_id: &str,
    ) -> Result<(), BackendError> {
        let url = format!("{}/sessions/{}", self.base_url, session_id);
        let request = UpsertSessionRequest {
            session: SessionUpdate {
                title: metadata.title.clone(),
                cwd: Some(metadata.cwd.clone()),
                status: Some("active".to_string()),
                metadata: serde_json::to_value(metadata).ok(),
            },
            agent_id: agent_id.to_string(),
        };
        let response = self
            .send_with_auth(self.reqwest_client.put(&url).json(&request))
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(BackendError::RequestFailed { status, body });
        }
        Ok(())
    }
    pub async fn save_session_data(
        &self,
        session_id: &str,
        messages: &[ExportedMessage],
        metadata: Option<&ExportedMetadata>,
    ) -> Result<(), BackendError> {
        let url = format!("{}/sessions/{}/data", self.base_url, session_id);
        let request = SaveDataRequest {
            messages: messages.to_vec(),
            metadata: metadata.and_then(|m| serde_json::to_value(m).ok()),
        };
        let response = self
            .send_with_auth(self.reqwest_client.post(&url).json(&request))
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(BackendError::RequestFailed { status, body });
        }
        Ok(())
    }
    /// List all sessions for the authenticated user. `GET /sessions`
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, BackendError> {
        let url = format!("{}/sessions", self.base_url);
        let response = self.send_with_auth(self.reqwest_client.get(&url)).await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(BackendError::RequestFailed { status, body });
        }
        #[derive(Deserialize)]
        struct ListResponse {
            sessions: Vec<SessionInfo>,
        }
        let data: ListResponse = response.json().await?;
        Ok(data.sessions)
    }
    pub async fn load_session_data(
        &self,
        session_id: &str,
    ) -> Result<LoadDataResponse, BackendError> {
        let url = format!("{}/sessions/{}/data", self.base_url, session_id);
        let response = self.send_with_auth(self.reqwest_client.get(&url)).await?;
        if response.status().as_u16() == 404 {
            return Err(BackendError::SessionNotFound {
                session_id: session_id.to_string(),
            });
        }
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(BackendError::RequestFailed { status, body });
        }
        let data: LoadDataResponse = response.json().await?;
        Ok(data)
    }
    pub async fn create_share_link(&self, session_id: &str) -> Result<ShareResponse, BackendError> {
        let url = format!("{}/sessions/{}/share", self.base_url, session_id);
        let response = self.send_with_auth(self.reqwest_client.post(&url)).await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(BackendError::RequestFailed { status, body });
        }
        let share_response: ShareResponse = response.json().await?;
        Ok(share_response)
    }
    pub async fn delete_session_data(&self, session_id: &str) -> Result<(), BackendError> {
        let url = format!("{}/sessions/{}/data", self.base_url, session_id);
        let response = self
            .send_with_auth(self.reqwest_client.delete(&url))
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(BackendError::RequestFailed { status, body });
        }
        Ok(())
    }
}
/// Default context window (256k) when the remote endpoint doesn't provide one.
pub(crate) const DEFAULT_CONTEXT_WINDOW: u64 = 256_000;
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<serde_json::Value>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointAuth {
    ApiKey,
    Session,
}
struct ListModelsEndpoint {
    url: String,
    auth: EndpointAuth,
}
/// The `/v1/models` URL [`fetch_models_blocking`] hits for this
/// endpoints/auth shape. Doubles as the models disk-cache origin key: cached
/// entries embed absolute `base_url`s from the backend that served them, so a
/// catalog fetched from one backend (env override, another deployment, a
/// test's mock server) must be a cache miss for any other backend.
pub(crate) fn models_list_url(
    endpoints: &crate::agent::config::EndpointsConfig,
    fetch_auth: crate::agent::models::ModelFetchAuth,
) -> String {
    ListModelsEndpoint::from_endpoints(endpoints, fetch_auth).url
}
impl ListModelsEndpoint {
    fn from_endpoints(
        endpoints: &crate::agent::config::EndpointsConfig,
        fetch_auth: crate::agent::models::ModelFetchAuth,
    ) -> Self {
        if endpoints.has_custom_endpoint() {
            Self {
                url: endpoints.resolve_models_list_url(),
                auth: EndpointAuth::ApiKey,
            }
        } else if fetch_auth == crate::agent::models::ModelFetchAuth::ApiKey {
            let base_url = endpoints.xai_api_base_url.trim_end_matches('/');
            Self {
                url: if base_url.is_empty() {
                    String::new()
                } else {
                    format!("{base_url}/models")
                },
                auth: EndpointAuth::ApiKey,
            }
        } else {
            Self {
                url: endpoints.resolve_models_list_url(),
                auth: EndpointAuth::Session,
            }
        }
    }
}
/// Fetch models from an OpenAI-compatible `/v1/models` endpoint.
/// Fetch result: model entries + optional etag from response.
pub struct FetchModelsResult {
    pub models: Vec<crate::agent::config::ModelEntryConfig>,
    pub etag: Option<String>,
}
pub(crate) fn fetch_models_blocking(
    endpoints: &crate::agent::config::EndpointsConfig,
    auth: Option<&AtelierAuth>,
    fetch_auth: crate::agent::models::ModelFetchAuth,
) -> Result<FetchModelsResult, BackendError> {
    let client = crate::http::shared_blocking_client();
    let source = ListModelsEndpoint::from_endpoints(endpoints, fetch_auth);
    if source.url.trim().is_empty() {
        return Err(BackendError::Disabled);
    }
    let inference_base_url = endpoints.resolve_inference_base_url();
    tracing::info!("Fetching models from {}", source.url);
    let mut request = client.get(&source.url);
    match source.auth {
        EndpointAuth::ApiKey => {
            let api_key = crate::agent::auth_method::read_xai_api_key_env()
                .or_else(|_| {
                    auth.map(|a| a.key.clone())
                        .ok_or(std::env::VarError::NotPresent)
                })
                .map_err(|_| {
                    BackendError::Auth(
                        "No API key for custom models endpoint. Set XAI_API_KEY.".into(),
                    )
                })?;
            request = request.header("Authorization", format!("Bearer {}", api_key));
        }
        EndpointAuth::Session => {
            let auth = auth.ok_or_else(|| {
                BackendError::Auth("No auth credentials for cli-chat-proxy".into())
            })?;
            request = request
                .header("Authorization", format!("Bearer {}", &auth.key))
                .header("X-XAI-Token-Auth", "atelier-cli")
                .header("x-userid", &auth.user_id)
                .header("x-atelier-client-version", atelier_version::VERSION)
                .header(
                    crate::http::CLIENT_MODE_HEADER,
                    crate::http::process_client_mode(),
                );
            if let Some(email) = &auth.email {
                request = request.header("x-email", email);
            }
        }
    }
    let response = request.send()?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().unwrap_or_default();
        tracing::warn!("Failed to fetch models: {} - {}", status, body);
        return Err(BackendError::RequestFailed { status, body });
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let models_response: ModelsResponse = response.json()?;
    tracing::info!(
        "Fetched {} models from {}",
        models_response.data.len(),
        source.url
    );
    let mut models = Vec::with_capacity(models_response.data.len());
    for (idx, value) in models_response.data.into_iter().enumerate() {
        match parse_remote_model_value(&value, &inference_base_url) {
            Some(model) => models.push(model),
            None => {
                tracing::warn!(
                    "Skipping model at index {}: missing required field ('model' or 'context_window') or invalid types",
                    idx
                )
            }
        }
    }
    Ok(FetchModelsResult { models, etag })
}
/// Parse a single model entry from the /models-v2 response.
/// Used by both initial model fetch and session-resume metadata refresh.
pub fn parse_remote_model_value(
    value: &serde_json::Value,
    default_base_url: &str,
) -> Option<crate::agent::config::ModelEntryConfig> {
    let obj = value.as_object()?;
    let meta = obj.get("_meta").and_then(|v| v.as_object());
    let id = get_string(obj, "id");
    let model = get_string(obj, "model")
        .or_else(|| get_string(obj, "modelId"))
        .or_else(|| id.clone())
        .or_else(|| meta.and_then(|m| get_string(m, "model")))
        .or_else(|| meta.and_then(|m| get_string(m, "modelId")))?;
    let base_url = get_string(obj, "baseUrl")
        .or_else(|| get_string(obj, "base_url"))
        .unwrap_or_else(|| default_base_url.to_owned());
    let name = get_string(obj, "name").or_else(|| Some(model.clone()));
    let context_window = get_u64(obj, "contextWindow")
        .or_else(|| get_u64(obj, "context_window"))
        .or_else(|| meta.and_then(|m| get_u64(m, "contextWindow")))
        .or_else(|| meta.and_then(|m| get_u64(m, "totalContextTokens")))
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let context_window = std::num::NonZeroU64::new(context_window)?;
    let agent_type = get_string(obj, "systemPromptType")
        .or_else(|| get_string(obj, "system_prompt_type"))
        .or_else(|| get_string(obj, "agent_type"))
        .or_else(|| get_string(obj, "agentType"))
        .or_else(|| meta.and_then(|m| get_string(m, "agentType")))
        .or_else(|| meta.and_then(|m| get_string(m, "agent_type")))
        .unwrap_or_else(crate::agent::config::default_agent_type);
    let api_backend = get_string(obj, "apiBackend")
        .or_else(|| get_string(obj, "api_backend"))
        .and_then(|s| match s.as_str() {
            "responses" => Some(crate::sampling::ApiBackend::Responses),
            "chat_completions" => Some(crate::sampling::ApiBackend::ChatCompletions),
            "messages" => Some(crate::sampling::ApiBackend::Messages),
            _ => None,
        })
        .unwrap_or_default();
    Some(crate::agent::config::ModelEntryConfig {
        id,
        model,
        base_url,
        name,
        description: get_string(obj, "description"),
        max_completion_tokens: get_u64(obj, "maxCompletionTokens")
            .or_else(|| get_u64(obj, "max_completion_tokens"))
            .and_then(|v| u32::try_from(v).ok()),
        temperature: get_f64(obj, "temperature").map(|v| v as f32),
        top_p: get_f64(obj, "topP").or_else(|| get_f64(obj, "top_p")).map(|v| v as f32),
        api_key: get_string(obj, "apiKey").or_else(|| get_string(obj, "api_key")),
        env_key: get_env_keys(obj, "envKey").or_else(|| get_env_keys(obj, "env_key")),
        api_backend,
        context_window,
        auto_compact_threshold_percent: get_u64(obj, "autoCompactThresholdPercent")
            .or_else(|| get_u64(obj, "auto_compact_threshold_percent"))
            .and_then(|v| u8::try_from(v).ok()),
        system_prompt_label: get_string(obj, "systemPromptLabel")
            .or_else(|| get_string(obj, "system_prompt_label"))
            .filter(|s| !s.trim().is_empty()),
        extra_headers: get_string_map(obj, "extraHeaders"),
        api_base_url: get_string(obj, "apiBaseUrl")
            .or_else(|| get_string(obj, "api_base_url")),
        use_concise: obj
            .get("useConcise")
            .or_else(|| obj.get("use_concise"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        agent_type,
        inference_idle_timeout_secs: get_u64(obj, "inferenceIdleTimeoutSecs")
            .or_else(|| get_u64(obj, "inference_idle_timeout_secs")),
        max_retries: get_u64(obj, "maxRetries")
            .or_else(|| get_u64(obj, "max_retries"))
            .and_then(|v| u32::try_from(v).ok()),
        hidden: obj
            .get("hidden")
            .or_else(|| meta.and_then(|m| m.get("hidden")))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        supported_in_api: obj
            .get("supportedInApi")
            .or_else(|| obj.get("supported_in_api"))
            .or_else(|| meta.and_then(|m| m.get("supportedInApi")))
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        auth_scheme: None,
        reasoning_effort: get_string(obj, "reasoningEffort")
            .or_else(|| get_string(obj, "reasoning_effort"))
            .or_else(|| meta.and_then(|m| get_string(m, "reasoningEffort")))
            .and_then(|s| s.parse().ok()),
        supports_reasoning_effort: obj
            .get("supportsReasoningEffort")
            .or_else(|| obj.get("supports_reasoning_effort"))
            .or_else(|| meta.and_then(|m| m.get("supportsReasoningEffort")))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        reasoning_efforts: obj
            .get("reasoningEfforts")
            .or_else(|| obj.get("reasoning_efforts"))
            .or_else(|| meta.and_then(|m| m.get("reasoningEfforts")))
            .and_then(|v| v.as_array())
            .map(|arr| atelier_sampling_types::parse_reasoning_effort_options(arr))
            .unwrap_or_default(),
        supports_backend_search: obj
            .get("supportsBackendSearch")
            .or_else(|| obj.get("supports_backend_search"))
            .or_else(|| meta.and_then(|m| m.get("supportsBackendSearch")))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        compactions_remaining: obj
            .get("compactionsRemaining")
            .or_else(|| obj.get("compactions_remaining"))
            .or_else(|| meta.and_then(|m| m.get("compactionsRemaining")))
            .and_then(parse_compactions_remaining)
            .or_else(|| {
                obj
                    .get("sendCompactionsRemaining")
                    .or_else(|| obj.get("send_compactions_remaining"))
                    .or_else(|| meta.and_then(|m| m.get("sendCompactionsRemaining")))
                    .and_then(|v| v.as_bool())
                    .map(atelier_sampling_types::CompactionsRemaining::Dynamic)
            }),
        compaction_at_tokens: obj
            .get("compactionAtTokens")
            .or_else(|| obj.get("compaction_at_tokens"))
            .or_else(|| meta.and_then(|m| m.get("compactionAtTokens")))
            .and_then(parse_compaction_at_tokens),
        show_model_fingerprint: obj
            .get("showModelFingerprint")
            .or_else(|| obj.get("show_model_fingerprint"))
            .or_else(|| meta.and_then(|m| m.get("showModelFingerprint")))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        stream_tool_calls: obj
            .get("streamToolCalls")
            .or_else(|| obj.get("stream_tool_calls"))
            .and_then(|v| v.as_bool()),
        laziness_detector: get_object(obj, "lazinessDetector")
            .or_else(|| get_object(obj, "laziness_detector"))
            .or_else(|| meta.and_then(|m| get_object(m, "lazinessDetector")))
            .and_then(|v| match serde_json::from_value::<
                crate::agent::config::LazinessDetectorPerModelConfig,
            >(v.clone()) {
                Ok(cfg) => Some(cfg),
                Err(e) => {
                    tracing::warn!(
                        error = % e,
                        "Failed to deserialize laziness_detector block from remote model; falling back to default"
                    );
                    None
                }
            })
            .unwrap_or_default(),
    })
}
fn get_string(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}
/// Parse `env_key` / `envKey` as a single string or a string array.
fn get_env_keys(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<crate::agent::config::EnvKeys> {
    let v = obj.get(key)?;
    if let Some(s) = v.as_str() {
        return Some(crate::agent::config::EnvKeys::single(s));
    }
    if let Some(arr) = v.as_array() {
        let mut names = Vec::with_capacity(arr.len());
        for item in arr {
            let Some(s) = item.as_str() else {
                tracing::warn!(
                    key,
                    "env_key array has a non-string element; ignoring env_key"
                );
                return None;
            };
            if !s.is_empty() {
                names.push(s.to_owned());
            }
        }
        if names.is_empty() {
            return None;
        }
        return Some(crate::agent::config::EnvKeys::new(names));
    }
    None
}
fn parse_compaction_at_tokens(
    v: &serde_json::Value,
) -> Option<atelier_sampling_types::CompactionAtTokens> {
    use atelier_sampling_types::CompactionAtTokens;
    v.as_bool()
        .map(CompactionAtTokens::Enabled)
        .or_else(|| v.as_u64().map(CompactionAtTokens::Fixed))
}
fn parse_compactions_remaining(
    v: &serde_json::Value,
) -> Option<atelier_sampling_types::CompactionsRemaining> {
    use atelier_sampling_types::CompactionsRemaining;
    v.as_bool().map(CompactionsRemaining::Dynamic).or_else(|| {
        v.as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .map(CompactionsRemaining::Fixed)
    })
}
fn get_u64(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<u64> {
    obj.get(key).and_then(|v| v.as_u64())
}
fn get_f64(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<f64> {
    obj.get(key).and_then(|v| v.as_f64())
}
fn get_object<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a serde_json::Value> {
    obj.get(key).filter(|v| v.is_object())
}
fn get_string_map(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> IndexMap<String, String> {
    obj.get(key)
        .and_then(|v| v.as_object())
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    use std::sync::{Arc, Mutex};
    #[test]
    fn get_env_keys_parses_strings_and_rejects_non_strings() {
        use crate::agent::config::EnvKeys;
        let parse = |v: serde_json::Value| {
            let obj = serde_json::json!({ "env_key" : v });
            get_env_keys(obj.as_object().unwrap(), "env_key")
        };
        assert_eq!(parse(serde_json::json!("A")), Some(EnvKeys::single("A")));
        assert_eq!(
            parse(serde_json::json!(["A", "B"])),
            Some(EnvKeys::new(["A", "B"]))
        );
        assert_eq!(parse(serde_json::json!(["A", 123])), None);
        assert_eq!(parse(serde_json::json!([])), None);
    }
    fn test_auth() -> AtelierAuth {
        AtelierAuth {
            key: "token".to_string(),
            auth_mode: crate::auth::AuthMode::Oidc,
            create_time: chrono::Utc::now(),
            user_id: "user-1".to_string(),
            email: Some("test@example.com".to_string()),
            first_name: None,
            last_name: None,
            profile_image_asset_id: None,
            principal_type: None,
            principal_id: None,
            team_id: None,
            team_name: None,
            team_role: None,
            organization_id: None,
            organization_name: None,
            organization_role: None,
            user_blocked_reason: None,
            team_blocked_reasons: vec![],
            coding_data_retention_opt_out: false,
            has_atelier_code_access: None,
            refresh_token: None,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            oidc_issuer: None,
            oidc_client_id: None,
        }
    }
    fn test_auth_manager() -> Arc<crate::auth::AuthManager> {
        let dir = tempfile::tempdir().unwrap();
        let mgr =
            crate::auth::AuthManager::new(dir.path(), crate::auth::AtelierComConfig::default());
        mgr.hot_swap(test_auth());
        std::mem::forget(dir);
        Arc::new(mgr)
    }
    #[test]
    fn parse_openai_format_uses_id_field() {
        let value = serde_json::json!(
            { "id" : "atelier-3", "object" : "model", "owned_by" : "xai", "context_window" :
            131072 }
        );
        let result = parse_remote_model_value(&value, "https://api.atelier/v1").unwrap();
        assert_eq!(result.model, "atelier-3");
        assert_eq!(result.base_url, "https://api.atelier/v1");
        assert_eq!(result.name.as_deref(), Some("atelier-3"));
    }
    #[test]
    fn parse_model_field_takes_priority_over_id() {
        let value = serde_json::json!(
            { "id" : "display-key", "model" : "actual-model-id", "name" : "Display Name",
            "context_window" : 131072 }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(result.model, "actual-model-id");
        assert_eq!(result.name.as_deref(), Some("Display Name"));
    }
    #[test]
    fn parse_reads_reasoning_effort_fields() {
        use atelier_sampling_types::ReasoningEffort;
        let value = serde_json::json!(
            { "model" : "atelier-4.5", "context_window" : 1_000_000,
            "supports_reasoning_effort" : true, "reasoning_effort" : "high" }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert!(result.supports_reasoning_effort);
        assert_eq!(result.reasoning_effort, Some(ReasoningEffort::High));
        let value = serde_json::json!(
            { "model" : "atelier-4.5", "contextWindow" : 1_000_000,
            "supportsReasoningEffort" : true, "reasoningEffort" : "xhigh" }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert!(result.supports_reasoning_effort);
        assert_eq!(result.reasoning_effort, Some(ReasoningEffort::Xhigh));
        let value = serde_json::json!({ "model" : "x", "context_window" : 256_000 });
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert!(!result.supports_reasoning_effort);
        assert!(result.reasoning_effort.is_none());
    }
    #[test]
    fn parse_reads_reasoning_efforts_list() {
        use atelier_sampling_types::ReasoningEffort;
        let value = serde_json::json!(
            { "model" : "atelier-4.5", "context_window" : 1_000_000, "reasoning_efforts" :
            [{ "id" : "deep", "value" : "xhigh", "label" : "Deep" }, { "value" :
            "quantum" }, "low",] }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(result.reasoning_efforts.len(), 2);
        assert_eq!(result.reasoning_efforts[0].id, "deep");
        assert_eq!(result.reasoning_efforts[0].value, ReasoningEffort::Xhigh);
        assert_eq!(result.reasoning_efforts[1].value, ReasoningEffort::Low);
        for value in [
            serde_json::json!(
                { "model" : "m", "context_window" : 256_000, "reasoningEfforts" : [{
                "value" : "high" }] }
            ),
            serde_json::json!(
                { "model" : "m", "context_window" : 256_000, "_meta" : {
                "reasoningEfforts" : [{ "value" : "high" }] } }
            ),
        ] {
            let result = parse_remote_model_value(&value, "https://default.url").unwrap();
            assert_eq!(result.reasoning_efforts.len(), 1);
            assert_eq!(result.reasoning_efforts[0].value, ReasoningEffort::High);
        }
        let value = serde_json::json!({ "model" : "x", "context_window" : 256_000 });
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert!(result.reasoning_efforts.is_empty());
    }
    #[test]
    fn parse_reads_meta_fallback_fields() {
        let value = serde_json::json!(
            { "_meta" : { "model" : "meta-model-id", "contextWindow" : 131072,
            "agentType" : "concise" } }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(result.model, "meta-model-id");
        assert_eq!(
            result.context_window,
            std::num::NonZeroU64::new(131072).unwrap()
        );
        assert_eq!(result.agent_type, "concise");
    }
    #[test]
    fn parse_remote_model_value_no_laziness_detector_block_yields_default() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(
            result.laziness_detector,
            crate::agent::config::LazinessDetectorPerModelConfig::default()
        );
    }
    #[test]
    fn parse_remote_model_value_parses_camelcase_key() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "lazinessDetector" : {
            "enabled" : true, "max_nudges_per_session" : 2, "idle_threshold_ms" : 12_000,
            "min_confidence" : 0.75, }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        let expected = crate::agent::config::LazinessDetectorPerModelConfig {
            enabled: true,
            max_nudges_per_session: 2,
            idle_threshold_ms: Some(12_000),
            min_confidence: Some(0.75),
            include_reasoning: None,
        };
        assert_eq!(result.laziness_detector, expected);
    }
    #[test]
    fn parse_remote_model_value_parses_snake_case_laziness_detector() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "laziness_detector" : {
            "enabled" : true, "max_nudges_per_session" : 3, "idle_threshold_ms" : 8_000,
            "min_confidence" : 0.6, }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        let expected = crate::agent::config::LazinessDetectorPerModelConfig {
            enabled: true,
            max_nudges_per_session: 3,
            idle_threshold_ms: Some(8_000),
            min_confidence: Some(0.6),
            include_reasoning: None,
        };
        assert_eq!(result.laziness_detector, expected);
    }
    #[test]
    fn parse_remote_model_value_parses_meta_laziness_detector() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "_meta" : {
            "lazinessDetector" : { "enabled" : true, "max_nudges_per_session" : 1,
            "idle_threshold_ms" : 15_000, "min_confidence" : 0.9, }, }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        let expected = crate::agent::config::LazinessDetectorPerModelConfig {
            enabled: true,
            max_nudges_per_session: 1,
            idle_threshold_ms: Some(15_000),
            min_confidence: Some(0.9),
            include_reasoning: None,
        };
        assert_eq!(result.laziness_detector, expected);
    }
    #[test]
    fn parse_remote_model_value_partial_block_uses_field_defaults() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "lazinessDetector" : {
            "enabled" : true, }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        let expected = crate::agent::config::LazinessDetectorPerModelConfig {
            enabled: true,
            max_nudges_per_session: 0,
            idle_threshold_ms: None,
            min_confidence: None,
            include_reasoning: None,
        };
        assert_eq!(result.laziness_detector, expected);
    }
    #[test]
    fn parse_remote_model_value_malformed_block_falls_back_to_default() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "lazinessDetector" : {
            "enabled" : true, "max_nudges_per_session" : "abc", }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(
            result.laziness_detector,
            crate::agent::config::LazinessDetectorPerModelConfig::default()
        );
    }
    #[test]
    fn parse_remote_model_value_non_object_value_falls_back_to_default() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "lazinessDetector" :
            "not-an-object", }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(
            result.laziness_detector,
            crate::agent::config::LazinessDetectorPerModelConfig::default()
        );
    }
    #[test]
    fn parse_remote_model_value_top_level_camelcase_wins_over_snake_case() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "lazinessDetector" : {
            "enabled" : true, "max_nudges_per_session" : 7, }, "laziness_detector" : {
            "enabled" : false, "max_nudges_per_session" : 99, }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        let expected = crate::agent::config::LazinessDetectorPerModelConfig {
            enabled: true,
            max_nudges_per_session: 7,
            idle_threshold_ms: None,
            min_confidence: None,
            include_reasoning: None,
        };
        assert_eq!(result.laziness_detector, expected);
    }
    /// `include_reasoning: false` parses cleanly under the per-model
    /// `lazinessDetector` block (camelCase wrapper, snake_case inner —
    /// matching the existing field-naming convention used for the
    /// sibling `min_confidence`, `idle_threshold_ms`, etc.).
    #[test]
    fn parse_remote_model_value_parses_include_reasoning_under_camelcase_wrapper() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "lazinessDetector" : {
            "enabled" : true, "include_reasoning" : false, }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(result.laziness_detector.include_reasoning, Some(false));
    }
    #[test]
    fn parse_remote_model_value_parses_include_reasoning_under_snake_case_wrapper() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "laziness_detector" : {
            "enabled" : true, "include_reasoning" : true, }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(result.laziness_detector.include_reasoning, Some(true));
    }
    #[test]
    fn parse_remote_model_value_omitted_include_reasoning_defaults_to_none() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "lazinessDetector" : {
            "enabled" : true, "max_nudges_per_session" : 2, }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(
            result.laziness_detector.include_reasoning, None,
            "absent include_reasoning defers to harness default via None",
        );
    }
    #[test]
    fn parse_remote_model_value_top_level_wins_over_meta() {
        let value = serde_json::json!(
            { "model" : "atelier-4", "context_window" : 256_000, "lazinessDetector" : {
            "enabled" : true, "max_nudges_per_session" : 5, }, "_meta" : {
            "lazinessDetector" : { "enabled" : false, "max_nudges_per_session" : 99, },
            }, }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        let expected = crate::agent::config::LazinessDetectorPerModelConfig {
            enabled: true,
            max_nudges_per_session: 5,
            idle_threshold_ms: None,
            min_confidence: None,
            include_reasoning: None,
        };
        assert_eq!(result.laziness_detector, expected);
    }
    #[test]
    fn parse_reads_show_model_fingerprint_field() {
        let value = serde_json::json!(
            { "model" : "atelier-build", "context_window" : 256_000,
            "show_model_fingerprint" : true }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert!(result.show_model_fingerprint);
        let value = serde_json::json!(
            { "model" : "atelier-build", "contextWindow" : 256_000, "showModelFingerprint" :
            true }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert!(result.show_model_fingerprint);
        let value = serde_json::json!(
            { "model" : "atelier-build", "context_window" : 256_000, "_meta" : {
            "showModelFingerprint" : true } }
        );
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert!(result.show_model_fingerprint);
        let value = serde_json::json!({ "model" : "x", "context_window" : 256_000 });
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert!(!result.show_model_fingerprint);
    }
    #[test]
    fn get_object_returns_none_for_non_object_values() {
        let value = serde_json::json!(
            { "string" : "hello", "number" : 42, "bool" : true, "array" : [1, 2, 3],
            "null" : null, }
        );
        let obj = value.as_object().unwrap();
        assert!(get_object(obj, "string").is_none());
        assert!(get_object(obj, "number").is_none());
        assert!(get_object(obj, "bool").is_none());
        assert!(get_object(obj, "array").is_none());
        assert!(get_object(obj, "null").is_none());
        assert!(get_object(obj, "missing").is_none());
    }
    #[test]
    fn get_object_returns_some_for_actual_object() {
        let value = serde_json::json!({ "nested" : { "a" : 1, "b" : "two" }, });
        let obj = value.as_object().unwrap();
        let nested = get_object(obj, "nested").expect("nested key should resolve to object");
        assert!(nested.is_object());
        assert_eq!(nested["a"], serde_json::json!(1));
        assert_eq!(nested["b"], serde_json::json!("two"));
    }
    fn endpoints(
        proxy: &str,
        models_base_url: Option<&str>,
        models_list_url: Option<&str>,
    ) -> crate::agent::config::EndpointsConfig {
        crate::agent::config::EndpointsConfig {
            cli_chat_proxy_base_url: Some(proxy.to_owned()),
            models_base_url: models_base_url.map(|s| s.to_owned()),
            models_list_url: models_list_url.map(|s| s.to_owned()),
            ..Default::default()
        }
    }
    #[test]
    fn inference_url_defaults_to_proxy() {
        let ep = endpoints("https://proxy.atelier.com/v1", None, None);
        assert_eq!(
            ep.resolve_inference_base_url(),
            "https://proxy.atelier.com/v1"
        );
    }
    #[test]
    fn inference_url_uses_models_base_url() {
        let ep = endpoints(
            "https://proxy.atelier.com/v1",
            Some("https://enterprise.acme.com/v1"),
            None,
        );
        assert_eq!(
            ep.resolve_inference_base_url(),
            "https://enterprise.acme.com/v1"
        );
    }
    #[test]
    fn inference_url_base_url_wins_over_proxy() {
        let ep = endpoints(
            "https://proxy.atelier.com/v1",
            Some("https://inference.acme.com/v1"),
            Some("https://registry.acme.com/api/models"),
        );
        assert_eq!(
            ep.resolve_inference_base_url(),
            "https://inference.acme.com/v1"
        );
    }
    #[test]
    fn list_url_defaults_to_proxy_models() {
        let ep = endpoints("https://proxy.atelier.com/v1", None, None);
        assert_eq!(
            ep.resolve_models_list_url(),
            "https://proxy.atelier.com/v1/models"
        );
    }
    #[test]
    fn list_url_derived_from_base_url() {
        let ep = endpoints(
            "https://proxy.atelier.com/v1",
            Some("https://api.atelier/v1"),
            None,
        );
        assert_eq!(
            ep.resolve_models_list_url(),
            "https://api.atelier/v1/models"
        );
    }
    #[test]
    fn list_url_explicit_overrides_derivation() {
        let ep = endpoints(
            "https://proxy.atelier.com/v1",
            Some("https://inference.acme.com/v1"),
            Some("https://registry.acme.com/api/list-models"),
        );
        assert_eq!(
            ep.resolve_models_list_url(),
            "https://registry.acme.com/api/list-models"
        );
    }
    /// Generic model discovery only uses an explicit/custom endpoint or the
    /// configured API endpoint. Vendor Session/Deployment discovery has no URL.
    #[test]
    #[serial_test::serial]
    fn models_fetch_endpoint_matches_auth_mode() {
        use crate::agent::config::EndpointsConfig;
        use crate::agent::models::ModelFetchAuth;
        for k in [
            "ATELIER_CLI_CHAT_PROXY_BASE_URL",
            "ATELIER_XAI_API_BASE_URL",
            "ATELIER_MODELS_LIST_URL",
        ] {
            unsafe { std::env::remove_var(k) };
        }
        let cfg = EndpointsConfig::from_config_value(
            &toml::from_str(
                r#"[endpoints]
                xai_api_base_url = "https://inference.acme-corp.example/xai/v1""#,
            )
            .unwrap(),
        );
        let session = ListModelsEndpoint::from_endpoints(&cfg, ModelFetchAuth::Session);
        assert_eq!(session.url, "");
        assert_eq!(session.auth, EndpointAuth::Session);
        let deployment = ListModelsEndpoint::from_endpoints(&cfg, ModelFetchAuth::Deployment);
        assert_eq!(deployment.url, "");
        assert_eq!(deployment.auth, EndpointAuth::Session);
        let api = ListModelsEndpoint::from_endpoints(&cfg, ModelFetchAuth::ApiKey);
        assert_eq!(api.url, "https://inference.acme-corp.example/xai/v1/models");
        assert_eq!(api.auth, EndpointAuth::ApiKey);
        let default = EndpointsConfig::from_config_value(&toml::Value::Table(Default::default()));
        assert_eq!(
            ListModelsEndpoint::from_endpoints(&default, ModelFetchAuth::ApiKey).url,
            ""
        );
        let custom = EndpointsConfig::from_config_value(
            &toml::from_str(
                r#"[endpoints]
                models_base_url = "https://models.acme.com/v1""#,
            )
            .unwrap(),
        );
        let ep = ListModelsEndpoint::from_endpoints(&custom, ModelFetchAuth::Session);
        assert_eq!(ep.url, "https://models.acme.com/v1/models");
        assert_eq!(ep.auth, EndpointAuth::ApiKey);
    }
    #[test]
    #[serial_test::serial]
    fn vendor_session_model_discovery_is_disabled_before_network() {
        use crate::agent::config::EndpointsConfig;
        use crate::agent::models::ModelFetchAuth;
        for key in [
            "ATELIER_CLI_CHAT_PROXY_BASE_URL",
            "ATELIER_MODELS_BASE_URL",
            "ATELIER_MODELS_LIST_URL",
        ] {
            unsafe { std::env::remove_var(key) };
        }
        let endpoints = EndpointsConfig::from_config_value(&toml::Value::Table(Default::default()));
        assert!(matches!(
            fetch_models_blocking(&endpoints, None, ModelFetchAuth::Session),
            Err(BackendError::Disabled)
        ));
        assert!(matches!(
            fetch_models_blocking(&endpoints, None, ModelFetchAuth::ApiKey),
            Err(BackendError::Disabled)
        ));
    }
    /// REGRESSION: `atelier setup` must send the deployment key to
    /// the proxy, never the inference endpoint.
    #[test]
    #[serial_test::serial]
    #[cfg(any())] // Vendor managed-config proxy routing was removed from Atelier.
    fn deployment_config_url_uses_cli_chat_proxy_when_not_overridden() {
        use crate::agent::config::EndpointsConfig;
        for k in [
            "ATELIER_CLI_CHAT_PROXY_BASE_URL",
            "ATELIER_MANAGED_CONFIG_URL",
            "ATELIER_XAI_API_BASE_URL",
        ] {
            unsafe { std::env::remove_var(k) };
        }
        unsafe { std::env::set_var("ATELIER_DEPLOYMENT_KEY", "xai-token-ENTERPRISE") };
        let managed: toml::Value = toml::from_str(
            r#"[endpoints]
            deployment_key = "xai-token-ENTERPRISE"
            xai_api_base_url = "https://inference.acme-corp.example/xai/v1""#,
        )
        .unwrap();
        let url = EndpointsConfig::from_config_value(&managed).resolve_managed_config_url();
        assert_eq!(
            url,
            "https://cli-chat-proxy.atelier.com/v1/deployment/config"
        );
        assert!(
            !url.contains("acme-corp"),
            "deployment key would be sent to the inference host: {url}"
        );
        let pinned: toml::Value = toml::from_str(
            r#"[endpoints]
            xai_api_base_url = "https://inference.acme-corp.example/xai/v1"
            cli_chat_proxy_base_url = "https://proxy.acme-corp.example/v1""#,
        )
        .unwrap();
        assert_eq!(
            EndpointsConfig::from_config_value(&pinned).resolve_managed_config_url(),
            "https://proxy.acme-corp.example/v1/deployment/config"
        );
        unsafe { std::env::remove_var("ATELIER_DEPLOYMENT_KEY") };
    }
    /// `BackendClient::save_session_data` resolves auth from the attached
    /// `AuthManager` and sends the token as `Bearer <key>` on the wire.
    /// This is the writeback path used on every session flush.
    #[tokio::test(flavor = "current_thread")]
    async fn backend_client_resolves_auth_from_auth_manager() {
        let captured_auth = Arc::new(Mutex::new(None::<String>));
        let captured = captured_auth.clone();
        let app = Router::new().route(
            "/sessions/{id}/data",
            axum::routing::post(move |headers: HeaderMap| async move {
                *captured.lock().unwrap() = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                StatusCode::OK
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let am = test_auth_manager();
        let client = BackendClient::with_base_url(format!("http://{addr}")).with_auth_manager(am);
        client
            .save_session_data("test-session", &[], None)
            .await
            .unwrap();
        let sent = captured_auth
            .lock()
            .unwrap()
            .clone()
            .expect("server must receive Authorization header");
        assert_eq!(sent, "Bearer token", "must use token from AuthManager");
        server.abort();
    }
    /// Regression: reqwest .header() appends — duplicate
    /// or overlapping headers cause Cloudflare to reject the request.
    #[tokio::test(flavor = "current_thread")]
    async fn auth_headers_do_not_collide_with_json() {
        let client =
            BackendClient::with_base_url("http://localhost").with_auth_manager(test_auth_manager());
        let auth_headers = client.auth_header_map().await.unwrap();
        assert!(
            !auth_headers.contains_key("content-type"),
            "content-type in auth map would overwrite .json()"
        );
        let request = reqwest::Client::new()
            .put("http://localhost/sessions/test")
            .json(&serde_json::json!({ "test" : true }))
            .headers(auth_headers)
            .build()
            .unwrap();
        for name in request.headers().keys() {
            let count = request.headers().get_all(name).iter().count();
            assert_eq!(count, 1, "duplicate header {name}");
        }
    }
}
