use crate::SecretString;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use url::Url;

#[derive(Clone, PartialEq, Eq)]
pub struct OAuthHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl fmt::Debug for OAuthHttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthHttpResponse")
            .field("status", &self.status)
            .field(
                "body",
                &format_args!("REDACTED ({} bytes)", self.body.len()),
            )
            .finish()
    }
}

/// HTTP seam for OAuth endpoints.
///
/// Implementations must use Atelier's purpose-scoped Provider network client.
/// Keeping the seam here prevents OAuth from creating an untracked outbound
/// client and makes every exchange independently testable.
pub trait OAuthHttpClient: Send + Sync {
    fn post_form(
        &self,
        url: &Url,
        form: &[(String, String)],
    ) -> Result<OAuthHttpResponse, OAuthError>;
}

#[derive(Clone, PartialEq, Eq)]
pub struct OAuthCredential {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub token_type: String,
    pub expires_at_unix_seconds: Option<u64>,
    pub scopes: Vec<String>,
}

impl OAuthCredential {
    pub fn new(
        access_token: impl Into<String>,
        refresh_token: Option<impl Into<String>>,
        expires_at_unix_seconds: u64,
    ) -> Self {
        Self {
            access_token: SecretString::new(access_token),
            refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
            token_type: "Bearer".into(),
            expires_at_unix_seconds: Some(expires_at_unix_seconds),
            scopes: Vec::new(),
        }
    }

    pub fn is_expired_at(&self, unix_seconds: u64) -> bool {
        self.expires_at_unix_seconds
            .is_some_and(|expires_at| expires_at <= unix_seconds)
    }

    pub(crate) fn from_token_response(
        response: TokenResponse,
        previous_refresh_token: Option<&SecretString>,
    ) -> Result<Self, OAuthError> {
        if response.access_token.trim().is_empty() {
            return Err(OAuthError::InvalidResponse(
                "token response contains an empty access_token".into(),
            ));
        }
        let refresh_token = response
            .refresh_token
            .filter(|token| !token.trim().is_empty())
            .map(SecretString::new)
            .or_else(|| previous_refresh_token.cloned());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| OAuthError::ClockBeforeUnixEpoch)?
            .as_secs();
        let expires_at_unix_seconds = response
            .expires_in
            .map(|expires_in| now.saturating_add(expires_in));
        let scopes = response
            .scope
            .map(|scope| scope.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        Ok(Self {
            access_token: SecretString::new(response.access_token),
            refresh_token,
            token_type: response.token_type.unwrap_or_else(|| "Bearer".into()),
            expires_at_unix_seconds,
            scopes,
        })
    }

    pub(crate) fn encode_secret(&self) -> Result<SecretString, OAuthError> {
        let wire = OAuthCredentialWire {
            access_token: self.access_token.expose_secret(),
            refresh_token: self.refresh_token.as_ref().map(SecretString::expose_secret),
            token_type: &self.token_type,
            expires_at_unix_seconds: self.expires_at_unix_seconds,
            scopes: &self.scopes,
        };
        serde_json::to_string(&wire)
            .map(SecretString::new)
            .map_err(OAuthError::CredentialEncoding)
    }

    pub(crate) fn decode_secret(secret: &SecretString) -> Result<Self, OAuthError> {
        let wire: OAuthCredentialOwned =
            serde_json::from_str(secret.expose_secret()).map_err(OAuthError::CredentialDecoding)?;
        if wire.access_token.trim().is_empty() {
            return Err(OAuthError::InvalidResponse(
                "stored OAuth credential contains an empty access token".into(),
            ));
        }
        Ok(Self {
            access_token: SecretString::new(wire.access_token),
            refresh_token: wire.refresh_token.map(SecretString::new),
            token_type: wire.token_type,
            expires_at_unix_seconds: wire.expires_at_unix_seconds,
            scopes: wire.scopes,
        })
    }
}

impl fmt::Debug for OAuthCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthCredential")
            .field("access_token", &"REDACTED")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "REDACTED"),
            )
            .field("token_type", &self.token_type)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .field("scopes", &self.scopes)
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("invalid OAuth configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid Provider credential namespace: {0}")]
    InvalidProviderId(String),
    #[error("OAuth callback could not bind to localhost: {0}")]
    CallbackBind(#[source] std::io::Error),
    #[error("OAuth callback timed out")]
    CallbackTimeout,
    #[error("OAuth callback request is invalid: {0}")]
    CallbackInvalid(String),
    #[error("OAuth callback state does not match")]
    StateMismatch,
    #[error("OAuth authorization was denied: {0}")]
    AuthorizationDenied(String),
    #[error("OAuth HTTP transport failed: {0}")]
    Transport(String),
    #[error("OAuth endpoint returned HTTP {status}: {code}")]
    Server {
        status: u16,
        code: String,
        description: Option<String>,
    },
    #[error("OAuth response is invalid: {0}")]
    InvalidResponse(String),
    #[error("OAuth response JSON is invalid: {0}")]
    ResponseJson(#[source] serde_json::Error),
    #[error("OAuth credential is not configured for Provider {0}")]
    CredentialMissing(String),
    #[error("OAuth credential has no refresh token")]
    RefreshTokenMissing,
    #[error("OAuth credential store operation failed: {0}")]
    CredentialStore(#[from] crate::CredentialError),
    #[error("OAuth credential could not be encoded: {0}")]
    CredentialEncoding(#[source] serde_json::Error),
    #[error("OAuth credential could not be decoded: {0}")]
    CredentialDecoding(#[source] serde_json::Error),
    #[error("OAuth credential metadata I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("OAuth credential metadata is invalid: {0}")]
    MetadataDecoding(#[source] serde_json::Error),
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
}

impl OAuthError {
    pub fn transport(message: impl Into<String>) -> Self {
        Self::Transport(message.into())
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct TokenResponse {
    pub(crate) access_token: String,
    #[serde(default)]
    pub(crate) refresh_token: Option<String>,
    #[serde(default)]
    pub(crate) expires_in: Option<u64>,
    #[serde(default)]
    pub(crate) token_type: Option<String>,
    #[serde(default)]
    pub(crate) scope: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ServerErrorResponse {
    pub(crate) error: String,
    #[serde(default)]
    pub(crate) error_description: Option<String>,
}

pub(crate) fn parse_token_response(
    response: OAuthHttpResponse,
    previous_refresh_token: Option<&SecretString>,
) -> Result<OAuthCredential, OAuthError> {
    if (200..300).contains(&response.status) {
        let token: TokenResponse =
            serde_json::from_slice(&response.body).map_err(OAuthError::ResponseJson)?;
        return OAuthCredential::from_token_response(token, previous_refresh_token);
    }
    Err(parse_server_error(response))
}

pub(crate) fn parse_server_error(response: OAuthHttpResponse) -> OAuthError {
    match serde_json::from_slice::<ServerErrorResponse>(&response.body) {
        Ok(error) => OAuthError::Server {
            status: response.status,
            code: error.error,
            description: error.error_description,
        },
        Err(_) => OAuthError::Server {
            status: response.status,
            code: "oauth_request_failed".into(),
            description: None,
        },
    }
}

#[derive(Serialize)]
struct OAuthCredentialWire<'a> {
    access_token: &'a str,
    refresh_token: Option<&'a str>,
    token_type: &'a str,
    expires_at_unix_seconds: Option<u64>,
    scopes: &'a [String],
}

#[derive(Deserialize)]
struct OAuthCredentialOwned {
    access_token: String,
    refresh_token: Option<String>,
    token_type: String,
    expires_at_unix_seconds: Option<u64>,
    #[serde(default)]
    scopes: Vec<String>,
}
