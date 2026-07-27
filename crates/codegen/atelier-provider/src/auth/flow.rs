use super::callback::LocalhostCallback;
use super::pkce::{Pkce, generate_state};
use super::types::{
    OAuthCredential, OAuthError, OAuthHttpClient, ServerErrorResponse, parse_server_error,
    parse_token_response,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use url::Url;

const DEFAULT_CALLBACK_PATH: &str = "/oauth/callback";
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OAuthTokenRequestFormat {
    #[default]
    Form,
    Json,
}

#[derive(Debug, Clone)]
pub struct AuthorizationCodeConfig {
    pub provider_id: String,
    pub client_id: String,
    pub authorization_endpoint: Url,
    pub token_endpoint: Url,
    pub scopes: Vec<String>,
    pub callback_port: u16,
    pub callback_host: String,
    pub callback_path: String,
    pub authorization_params: BTreeMap<String, String>,
    pub token_params: BTreeMap<String, String>,
    pub token_request_format: OAuthTokenRequestFormat,
    pub state_from_pkce_verifier: bool,
    pub include_state_in_token_request: bool,
}

impl AuthorizationCodeConfig {
    pub fn new(
        provider_id: impl Into<String>,
        client_id: impl Into<String>,
        authorization_endpoint: Url,
        token_endpoint: Url,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            client_id: client_id.into(),
            authorization_endpoint,
            token_endpoint,
            scopes: Vec::new(),
            callback_port: 0,
            callback_host: "127.0.0.1".into(),
            callback_path: DEFAULT_CALLBACK_PATH.into(),
            authorization_params: BTreeMap::new(),
            token_params: BTreeMap::new(),
            token_request_format: OAuthTokenRequestFormat::Form,
            state_from_pkce_verifier: false,
            include_state_in_token_request: false,
        }
    }

    pub fn validate(&self) -> Result<(), OAuthError> {
        validate_common(
            &self.provider_id,
            &self.client_id,
            &[&self.authorization_endpoint, &self.token_endpoint],
        )?;
        reject_reserved(
            &self.authorization_params,
            &[
                "response_type",
                "client_id",
                "redirect_uri",
                "scope",
                "state",
                "code_challenge",
                "code_challenge_method",
            ],
        )?;
        reject_reserved(
            &self.token_params,
            &[
                "grant_type",
                "code",
                "client_id",
                "redirect_uri",
                "code_verifier",
            ],
        )
    }
}

pub struct AuthorizationCodeSession {
    config: AuthorizationCodeConfig,
    callback: LocalhostCallback,
    pkce: Pkce,
    state: String,
    authorization_url: Url,
}

impl AuthorizationCodeSession {
    pub fn begin(config: AuthorizationCodeConfig) -> Result<Self, OAuthError> {
        config.validate()?;
        let callback = LocalhostCallback::bind(
            config.callback_port,
            &config.callback_path,
            &config.callback_host,
        )?;
        let pkce = Pkce::generate();
        let state = if config.state_from_pkce_verifier {
            pkce.verifier.clone()
        } else {
            generate_state()
        };
        let mut authorization_url = config.authorization_endpoint.clone();
        {
            let mut query = authorization_url.query_pairs_mut();
            for (name, value) in &config.authorization_params {
                query.append_pair(name, value);
            }
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &config.client_id)
                .append_pair("redirect_uri", callback.redirect_uri().as_str())
                .append_pair("state", &state)
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256");
            if !config.scopes.is_empty() {
                query.append_pair("scope", &config.scopes.join(" "));
            }
        }
        Ok(Self {
            config,
            callback,
            pkce,
            state,
            authorization_url,
        })
    }

    pub fn authorization_url(&self) -> &Url {
        &self.authorization_url
    }

    pub fn redirect_uri(&self) -> &Url {
        self.callback.redirect_uri()
    }

    pub fn callback_port(&self) -> u16 {
        self.callback.port()
    }

    pub fn callback_path(&self) -> &str {
        self.callback.path()
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    pub fn complete(
        self,
        client: &dyn OAuthHttpClient,
        timeout: Duration,
    ) -> Result<OAuthCredential, OAuthError> {
        let Self {
            config,
            callback,
            pkce,
            state,
            ..
        } = self;
        let redirect_uri = callback.redirect_uri().clone();
        let result = callback.wait(timeout, &state)?;
        let mut form = config.token_params.into_iter().collect::<Vec<_>>();
        form.extend([
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), result.code),
            ("client_id".into(), config.client_id),
            ("redirect_uri".into(), redirect_uri.into()),
            ("code_verifier".into(), pkce.verifier),
        ]);
        if config.include_state_in_token_request {
            form.push(("state".into(), state));
        }
        let response = match config.token_request_format {
            OAuthTokenRequestFormat::Form => client.post_form(&config.token_endpoint, &form)?,
            OAuthTokenRequestFormat::Json => {
                client.post_json(&config.token_endpoint, &form_json(form))?
            }
        };
        parse_token_response(response, None, None)
    }
}

#[derive(Debug, Clone)]
pub struct DeviceCodeConfig {
    pub provider_id: String,
    pub client_id: String,
    pub device_authorization_endpoint: Url,
    pub token_endpoint: Url,
    pub scopes: Vec<String>,
    pub authorization_params: BTreeMap<String, String>,
    pub token_params: BTreeMap<String, String>,
}

impl DeviceCodeConfig {
    pub fn new(
        provider_id: impl Into<String>,
        client_id: impl Into<String>,
        device_authorization_endpoint: Url,
        token_endpoint: Url,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            client_id: client_id.into(),
            device_authorization_endpoint,
            token_endpoint,
            scopes: Vec::new(),
            authorization_params: BTreeMap::new(),
            token_params: BTreeMap::new(),
        }
    }

    pub fn validate(&self) -> Result<(), OAuthError> {
        validate_common(
            &self.provider_id,
            &self.client_id,
            &[&self.device_authorization_endpoint, &self.token_endpoint],
        )?;
        reject_reserved(&self.authorization_params, &["client_id", "scope"])?;
        reject_reserved(
            &self.token_params,
            &["grant_type", "device_code", "client_id"],
        )
    }
}

#[derive(Debug)]
pub enum DeviceCodePoll {
    Pending,
    SlowDown,
    Complete(OAuthCredential),
}

pub struct DeviceCodeSession {
    config: DeviceCodeConfig,
    device_code: String,
    user_code: String,
    verification_uri: Url,
    verification_uri_complete: Option<Url>,
    poll_interval: Duration,
    expires_at: Instant,
}

impl DeviceCodeSession {
    pub fn begin(
        client: &dyn OAuthHttpClient,
        config: DeviceCodeConfig,
    ) -> Result<Self, OAuthError> {
        config.validate()?;
        let mut form = config
            .authorization_params
            .clone()
            .into_iter()
            .collect::<Vec<_>>();
        form.push(("client_id".into(), config.client_id.clone()));
        if !config.scopes.is_empty() {
            form.push(("scope".into(), config.scopes.join(" ")));
        }
        let response = client.post_form(&config.device_authorization_endpoint, &form)?;
        if !(200..300).contains(&response.status) {
            return Err(parse_server_error(response));
        }
        let response: DeviceAuthorizationResponse =
            serde_json::from_slice(&response.body).map_err(OAuthError::ResponseJson)?;
        if response.device_code.trim().is_empty() || response.user_code.trim().is_empty() {
            return Err(OAuthError::InvalidResponse(
                "device authorization response is missing device_code or user_code".into(),
            ));
        }
        let verification_uri = Url::parse(&response.verification_uri)
            .map_err(|error| OAuthError::InvalidResponse(error.to_string()))?;
        let verification_uri_complete = response
            .verification_uri_complete
            .map(|url| {
                Url::parse(&url).map_err(|error| OAuthError::InvalidResponse(error.to_string()))
            })
            .transpose()?;
        let expires_in = Duration::from_secs(response.expires_in.max(1));
        Ok(Self {
            config,
            device_code: response.device_code,
            user_code: response.user_code,
            verification_uri,
            verification_uri_complete,
            poll_interval: Duration::from_secs(response.interval.unwrap_or(5).max(1)),
            expires_at: Instant::now() + expires_in,
        })
    }

    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    pub fn verification_uri(&self) -> &Url {
        &self.verification_uri
    }

    pub fn verification_uri_complete(&self) -> Option<&Url> {
        self.verification_uri_complete.as_ref()
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub fn poll_once(
        &mut self,
        client: &dyn OAuthHttpClient,
    ) -> Result<DeviceCodePoll, OAuthError> {
        if Instant::now() >= self.expires_at {
            return Err(OAuthError::Server {
                status: 400,
                code: "expired_token".into(),
                description: None,
            });
        }
        let mut form = self
            .config
            .token_params
            .clone()
            .into_iter()
            .collect::<Vec<_>>();
        form.extend([
            ("grant_type".into(), DEVICE_GRANT_TYPE.into()),
            ("device_code".into(), self.device_code.clone()),
            ("client_id".into(), self.config.client_id.clone()),
        ]);
        let response = client.post_form(&self.config.token_endpoint, &form)?;
        if (200..300).contains(&response.status) {
            return parse_token_response(response, None, None).map(DeviceCodePoll::Complete);
        }
        let status = response.status;
        let error: ServerErrorResponse =
            serde_json::from_slice(&response.body).map_err(OAuthError::ResponseJson)?;
        match error.error.as_str() {
            "authorization_pending" => Ok(DeviceCodePoll::Pending),
            "slow_down" => {
                self.poll_interval = self.poll_interval.saturating_add(Duration::from_secs(5));
                Ok(DeviceCodePoll::SlowDown)
            }
            _ => Err(OAuthError::Server {
                status,
                code: error.error,
                description: error.error_description,
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RefreshTokenConfig {
    pub provider_id: String,
    pub client_id: String,
    pub token_endpoint: Url,
    pub token_params: BTreeMap<String, String>,
    pub token_request_format: OAuthTokenRequestFormat,
}

impl RefreshTokenConfig {
    pub fn new(
        provider_id: impl Into<String>,
        client_id: impl Into<String>,
        token_endpoint: Url,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            client_id: client_id.into(),
            token_endpoint,
            token_params: BTreeMap::new(),
            token_request_format: OAuthTokenRequestFormat::Form,
        }
    }

    fn validate(&self) -> Result<(), OAuthError> {
        validate_common(&self.provider_id, &self.client_id, &[&self.token_endpoint])?;
        reject_reserved(
            &self.token_params,
            &["grant_type", "refresh_token", "client_id"],
        )
    }
}

pub fn refresh_credential(
    client: &dyn OAuthHttpClient,
    config: &RefreshTokenConfig,
    credential: &OAuthCredential,
) -> Result<OAuthCredential, OAuthError> {
    config.validate()?;
    let refresh_token = credential
        .refresh_token
        .as_ref()
        .ok_or(OAuthError::RefreshTokenMissing)?;
    let mut form = config.token_params.clone().into_iter().collect::<Vec<_>>();
    form.extend([
        ("grant_type".into(), "refresh_token".into()),
        ("refresh_token".into(), refresh_token.expose_secret().into()),
        ("client_id".into(), config.client_id.clone()),
    ]);
    let response = match config.token_request_format {
        OAuthTokenRequestFormat::Form => client.post_form(&config.token_endpoint, &form)?,
        OAuthTokenRequestFormat::Json => {
            client.post_json(&config.token_endpoint, &form_json(form))?
        }
    };
    parse_token_response(
        response,
        Some(refresh_token),
        credential.identity_token.as_ref(),
    )
}

fn form_json(form: Vec<(String, String)>) -> serde_json::Value {
    serde_json::Value::Object(
        form.into_iter()
            .map(|(name, value)| (name, serde_json::Value::String(value)))
            .collect(),
    )
}

fn validate_common(
    provider_id: &str,
    client_id: &str,
    endpoints: &[&Url],
) -> Result<(), OAuthError> {
    validate_provider_id(provider_id)?;
    if client_id.trim().is_empty() || client_id.contains(['\0', '\r', '\n']) {
        return Err(OAuthError::InvalidConfig(
            "client_id must not be empty or contain control characters".into(),
        ));
    }
    for endpoint in endpoints {
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(OAuthError::InvalidConfig(format!(
                "unsupported OAuth endpoint scheme: {}",
                endpoint.scheme()
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_provider_id(provider_id: &str) -> Result<(), OAuthError> {
    let valid = !provider_id.is_empty()
        && provider_id != "."
        && provider_id != ".."
        && provider_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));
    if !valid {
        return Err(OAuthError::InvalidProviderId(provider_id.into()));
    }
    Ok(())
}

fn reject_reserved(params: &BTreeMap<String, String>, reserved: &[&str]) -> Result<(), OAuthError> {
    if let Some(name) = params
        .keys()
        .find(|name| reserved.iter().any(|reserved| name == reserved))
    {
        return Err(OAuthError::InvalidConfig(format!(
            "OAuth parameter '{name}' is managed by Atelier"
        )));
    }
    Ok(())
}

#[derive(Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}
