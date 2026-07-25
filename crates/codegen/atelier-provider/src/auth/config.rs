use super::{AuthorizationCodeConfig, DeviceCodeConfig, OAuthError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "flow")]
pub enum ProviderOAuthMethod {
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
            Self::AuthorizationCode { .. } => "authorization-code",
            Self::DeviceCode { .. } => "device-code",
        }
    }

    pub fn validate(&self, provider_id: &str) -> Result<(), OAuthError> {
        match self {
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
