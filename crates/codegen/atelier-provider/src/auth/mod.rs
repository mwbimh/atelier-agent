//! Provider-scoped OAuth flows and credential persistence.
//!
//! OAuth endpoints are supplied by Provider configuration. This module does
//! not create an HTTP client on its own: callers must provide an
//! [`OAuthHttpClient`] backed by Atelier's purpose-scoped network layer.

mod callback;
mod config;
mod flow;
mod pkce;
mod store;
mod types;

pub use config::{ProviderOAuthMethod, ProviderOAuthPreset};
pub use flow::{
    AuthorizationCodeConfig, AuthorizationCodeSession, DeviceCodeConfig, DeviceCodePoll,
    DeviceCodeSession, OAuthTokenRequestFormat, RefreshTokenConfig, refresh_credential,
};
pub use store::{
    OAuthSecretStore, ProviderCredentialNamespace, ProviderOAuthCredentialStore,
    SystemOAuthSecretStore, resolve_system_access_token,
};
pub use types::{OAuthCredential, OAuthError, OAuthHttpClient, OAuthHttpResponse};
