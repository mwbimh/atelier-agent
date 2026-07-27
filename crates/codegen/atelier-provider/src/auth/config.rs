use super::{
    AuthorizationCodeConfig, DeviceCodeConfig, OAuthError, OAuthTokenRequestFormat,
    RefreshTokenConfig,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::Url;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderOAuthPreset {
    #[serde(rename = "openai-codex-browser")]
    OpenAiCodexBrowser,
    #[serde(rename = "anthropic-browser")]
    AnthropicBrowser,
    #[serde(rename = "xai-device")]
    XaiDevice,
}

impl ProviderOAuthPreset {
    pub const fn id(self) -> &'static str {
        match self {
            Self::OpenAiCodexBrowser => "openai-codex-browser",
            Self::AnthropicBrowser => "anthropic-browser",
            Self::XaiDevice => "xai-device",
        }
    }

    pub const fn provider_id(self) -> &'static str {
        match self {
            Self::OpenAiCodexBrowser => "openai",
            Self::AnthropicBrowser => "anthropic",
            Self::XaiDevice => "xai",
        }
    }

    pub fn authorization_code_config(self) -> Result<Option<AuthorizationCodeConfig>, OAuthError> {
        let mut config = match self {
            Self::OpenAiCodexBrowser => AuthorizationCodeConfig::new(
                self.provider_id(),
                "app_EMoamEEZ73f0CkXaXp7hrann",
                preset_url("https://auth.openai.com/oauth/authorize")?,
                preset_url("https://auth.openai.com/oauth/token")?,
            ),
            Self::AnthropicBrowser => AuthorizationCodeConfig::new(
                self.provider_id(),
                "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
                preset_url("https://claude.ai/oauth/authorize")?,
                preset_url("https://platform.claude.com/v1/oauth/token")?,
            ),
            Self::XaiDevice => return Ok(None),
        };
        match self {
            Self::OpenAiCodexBrowser => {
                config.scopes = split_scopes("openid profile email offline_access");
                config.callback_port = 1455;
                config.callback_host = "localhost".into();
                config.callback_path = "/auth/callback".into();
                config
                    .authorization_params
                    .insert("id_token_add_organizations".into(), "true".into());
                config
                    .authorization_params
                    .insert("codex_cli_simplified_flow".into(), "true".into());
                config
                    .authorization_params
                    .insert("originator".into(), "atelier".into());
            }
            Self::AnthropicBrowser => {
                config.scopes = split_scopes(
                    "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload",
                );
                config.callback_port = 53_692;
                config.callback_host = "localhost".into();
                config.callback_path = "/callback".into();
                config
                    .authorization_params
                    .insert("code".into(), "true".into());
                config.token_request_format = OAuthTokenRequestFormat::Json;
                config.state_from_pkce_verifier = true;
                config.include_state_in_token_request = true;
            }
            Self::XaiDevice => unreachable!(),
        }
        Ok(Some(config))
    }

    pub fn device_code_config(self) -> Result<Option<DeviceCodeConfig>, OAuthError> {
        let mut config = match self {
            Self::XaiDevice => DeviceCodeConfig::new(
                self.provider_id(),
                "b1a00492-073a-47ea-816f-4c329264a828",
                preset_url("https://auth.x.ai/oauth2/device/code")?,
                preset_url("https://auth.x.ai/oauth2/token")?,
            ),
            Self::OpenAiCodexBrowser | Self::AnthropicBrowser => {
                return Ok(None);
            }
        };
        config.scopes =
            split_scopes("openid profile email offline_access grok-cli:access api:access");
        config
            .authorization_params
            .insert("referrer".into(), "atelier".into());
        Ok(Some(config))
    }

    pub fn refresh_token_config(self) -> Result<RefreshTokenConfig, OAuthError> {
        let (client_id, token_endpoint, format) = match self {
            Self::OpenAiCodexBrowser => (
                "app_EMoamEEZ73f0CkXaXp7hrann",
                "https://auth.openai.com/oauth/token",
                OAuthTokenRequestFormat::Form,
            ),
            Self::AnthropicBrowser => (
                "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
                "https://platform.claude.com/v1/oauth/token",
                OAuthTokenRequestFormat::Json,
            ),
            Self::XaiDevice => (
                "b1a00492-073a-47ea-816f-4c329264a828",
                "https://auth.x.ai/oauth2/token",
                OAuthTokenRequestFormat::Form,
            ),
        };
        let mut config =
            RefreshTokenConfig::new(self.provider_id(), client_id, preset_url(token_endpoint)?);
        config.token_request_format = format;
        Ok(config)
    }
}

fn preset_url(value: &str) -> Result<Url, OAuthError> {
    Url::parse(value).map_err(|error| OAuthError::InvalidConfig(error.to_string()))
}

fn split_scopes(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_owned).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "flow")]
pub enum ProviderOAuthMethod {
    #[serde(rename = "preset")]
    Preset { id: ProviderOAuthPreset },
    #[serde(rename = "authorization-code")]
    AuthorizationCode {
        client_id: String,
        authorization_endpoint: Url,
        token_endpoint: Url,
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        callback_port: u16,
        #[serde(default = "default_callback_path")]
        callback_path: String,
        #[serde(default)]
        authorization_params: BTreeMap<String, String>,
        #[serde(default)]
        token_params: BTreeMap<String, String>,
    },
    #[serde(rename = "device-code")]
    DeviceCode {
        client_id: String,
        device_authorization_endpoint: Url,
        token_endpoint: Url,
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        authorization_params: BTreeMap<String, String>,
        #[serde(default)]
        token_params: BTreeMap<String, String>,
    },
}

impl ProviderOAuthMethod {
    pub const fn preset(id: ProviderOAuthPreset) -> Self {
        Self::Preset { id }
    }

    pub fn authorization_code(
        client_id: impl Into<String>,
        authorization_endpoint: Url,
        token_endpoint: Url,
    ) -> Self {
        Self::AuthorizationCode {
            client_id: client_id.into(),
            authorization_endpoint,
            token_endpoint,
            scopes: Vec::new(),
            callback_port: 0,
            callback_path: default_callback_path(),
            authorization_params: BTreeMap::new(),
            token_params: BTreeMap::new(),
        }
    }

    pub fn device_code(
        client_id: impl Into<String>,
        device_authorization_endpoint: Url,
        token_endpoint: Url,
    ) -> Self {
        Self::DeviceCode {
            client_id: client_id.into(),
            device_authorization_endpoint,
            token_endpoint,
            scopes: Vec::new(),
            authorization_params: BTreeMap::new(),
            token_params: BTreeMap::new(),
        }
    }

    pub const fn flow_name(&self) -> &'static str {
        match self {
            Self::Preset { id } => id.id(),
            Self::AuthorizationCode { .. } => "authorization-code",
            Self::DeviceCode { .. } => "device-code",
        }
    }

    pub fn validate(&self, provider_id: &str) -> Result<(), OAuthError> {
        match self {
            Self::Preset { id } if id.provider_id() == provider_id => Ok(()),
            Self::Preset { id } => Err(OAuthError::InvalidConfig(format!(
                "OAuth preset {} belongs to Provider {}, not {provider_id}",
                id.id(),
                id.provider_id()
            ))),
            Self::AuthorizationCode { .. } => {
                self.authorization_code_config(provider_id)?.validate()
            }
            Self::DeviceCode { .. } => self.device_code_config(provider_id)?.validate(),
        }
    }

    pub fn authorization_code_config(
        &self,
        provider_id: &str,
    ) -> Result<AuthorizationCodeConfig, OAuthError> {
        let Self::AuthorizationCode {
            client_id,
            authorization_endpoint,
            token_endpoint,
            scopes,
            callback_port,
            callback_path,
            authorization_params,
            token_params,
        } = self
        else {
            return Err(OAuthError::InvalidConfig(format!(
                "OAuth method {} is not an authorization-code flow",
                self.flow_name()
            )));
        };
        let mut config = AuthorizationCodeConfig::new(
            provider_id,
            client_id,
            authorization_endpoint.clone(),
            token_endpoint.clone(),
        );
        config.scopes = scopes.clone();
        config.callback_port = *callback_port;
        config.callback_path = callback_path.clone();
        config.authorization_params = authorization_params.clone();
        config.token_params = token_params.clone();
        Ok(config)
    }

    pub fn device_code_config(&self, provider_id: &str) -> Result<DeviceCodeConfig, OAuthError> {
        let Self::DeviceCode {
            client_id,
            device_authorization_endpoint,
            token_endpoint,
            scopes,
            authorization_params,
            token_params,
        } = self
        else {
            return Err(OAuthError::InvalidConfig(format!(
                "OAuth method {} is not a device-code flow",
                self.flow_name()
            )));
        };
        let mut config = DeviceCodeConfig::new(
            provider_id,
            client_id,
            device_authorization_endpoint.clone(),
            token_endpoint.clone(),
        );
        config.scopes = scopes.clone();
        config.authorization_params = authorization_params.clone();
        config.token_params = token_params.clone();
        Ok(config)
    }
}

fn default_callback_path() -> String {
    "/oauth/callback".into()
}
