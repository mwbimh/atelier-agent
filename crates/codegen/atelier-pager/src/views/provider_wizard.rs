use atelier_provider::auth::{ProviderOAuthMethod, ProviderOAuthMethod as OAuthMethod};
use atelier_provider::{CredentialRef, ProviderAuth, ProviderConfig, ProviderDiscovery};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

use crate::theme::Theme;
use crate::views::modal_window::{
    self as mw, ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderWizardStep {
    Provider,
    Id,
    ExistingProvider,
    DisplayName,
    BaseUrl,
    KnownAuth,
    Auth,
    CustomHeader,
    Credential,
    CredentialValue,
    CredentialArguments,
    OAuthClientId,
    OAuthAuthorizationEndpoint,
    OAuthTokenEndpoint,
    OAuthScopes,
    Discovery,
    DiscoveryPath,
    Summary,
    Submitting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialKind {
    Environment,
    Command,
    OAuthAuthorizationCode,
    OAuthDeviceCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KnownProvider {
    OpenAi,
    Anthropic,
    Google,
    DeepSeek,
    Xai,
}

struct ProviderChoice {
    label: &'static str,
    description: &'static str,
    known: Option<KnownProvider>,
}

struct ProviderPreset {
    id: &'static str,
    display_name: &'static str,
    base_url: &'static str,
    auth: ProviderAuth,
    environment_variable: &'static str,
    discovery: ProviderDiscovery,
    extra_headers: BTreeMap<String, String>,
}

fn provider_choices() -> Vec<ProviderChoice> {
    vec![
        ProviderChoice {
            label: "OpenAI",
            description: "Connect with an OpenAI API key",
            known: Some(KnownProvider::OpenAi),
        },
        ProviderChoice {
            label: "Anthropic",
            description: "Connect with an Anthropic API key",
            known: Some(KnownProvider::Anthropic),
        },
        ProviderChoice {
            label: "Google AI Studio",
            description: "Connect with a Gemini API key",
            known: Some(KnownProvider::Google),
        },
        ProviderChoice {
            label: "DeepSeek",
            description: "Connect with a DeepSeek API key",
            known: Some(KnownProvider::DeepSeek),
        },
        ProviderChoice {
            label: "xAI",
            description: "Connect with an xAI API key",
            known: Some(KnownProvider::Xai),
        },
        ProviderChoice {
            label: "Custom endpoint",
            description: "Advanced setup for a proxy, gateway, or self-hosted API",
            known: None,
        },
    ]
}

fn provider_choice_count() -> usize {
    provider_choices().len()
}

fn provider_preset(provider: KnownProvider) -> ProviderPreset {
    match provider {
        KnownProvider::OpenAi => ProviderPreset {
            id: "openai",
            display_name: "OpenAI",
            base_url: "https://api.openai.com/v1",
            auth: ProviderAuth::Bearer,
            environment_variable: "OPENAI_API_KEY",
            discovery: ProviderDiscovery::OpenAiModels {
                path: "models".into(),
            },
            extra_headers: BTreeMap::new(),
        },
        KnownProvider::Anthropic => ProviderPreset {
            id: "anthropic",
            display_name: "Anthropic",
            base_url: "https://api.anthropic.com/v1",
            auth: ProviderAuth::Header {
                name: "x-api-key".into(),
            },
            environment_variable: "ANTHROPIC_API_KEY",
            discovery: ProviderDiscovery::OpenAiModels {
                path: "models".into(),
            },
            extra_headers: BTreeMap::from([("anthropic-version".into(), "2023-06-01".into())]),
        },
        KnownProvider::Google => ProviderPreset {
            id: "google",
            display_name: "Google AI Studio",
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            auth: ProviderAuth::Bearer,
            environment_variable: "GEMINI_API_KEY",
            discovery: ProviderDiscovery::OpenAiModels {
                path: "models".into(),
            },
            extra_headers: BTreeMap::new(),
        },
        KnownProvider::DeepSeek => ProviderPreset {
            id: "deepseek",
            display_name: "DeepSeek",
            base_url: "https://api.deepseek.com",
            auth: ProviderAuth::Bearer,
            environment_variable: "DEEPSEEK_API_KEY",
            discovery: ProviderDiscovery::OpenAiModels {
                path: "models".into(),
            },
            extra_headers: BTreeMap::new(),
        },
        KnownProvider::Xai => ProviderPreset {
            id: "xai",
            display_name: "xAI",
            base_url: "https://api.x.ai/v1",
            auth: ProviderAuth::Bearer,
            environment_variable: "XAI_API_KEY",
            discovery: ProviderDiscovery::OpenAiModels {
                path: "models".into(),
            },
            extra_headers: BTreeMap::new(),
        },
    }
}

pub struct ProviderWizardState {
    pub window: ModalWindowState,
    pub step: ProviderWizardStep,
    pub provider_id: String,
    pub display_name: String,
    pub base_url: String,
    pub input: String,
    pub selected: usize,
    existing_provider_ids: BTreeSet<String>,
    replace_existing: bool,
    known_provider: Option<KnownProvider>,
    auth: ProviderAuth,
    credential_kind: Option<CredentialKind>,
    credential_value: String,
    credential_args: Vec<String>,
    oauth_client_id: String,
    oauth_authorization_endpoint: String,
    oauth_token_endpoint: String,
    oauth_scopes: String,
    discovery: ProviderDiscovery,
    extra_headers: BTreeMap<String, String>,
    pub error: Option<String>,
    pub status: Option<String>,
}

pub enum ProviderWizardOutcome {
    Changed,
    Cancel,
    Submit(Box<ProviderConfig>),
}

impl Default for ProviderWizardState {
    fn default() -> Self {
        Self {
            window: ModalWindowState::new(),
            step: ProviderWizardStep::Provider,
            provider_id: String::new(),
            display_name: String::new(),
            base_url: String::new(),
            input: String::new(),
            selected: 0,
            existing_provider_ids: BTreeSet::new(),
            replace_existing: false,
            known_provider: None,
            auth: ProviderAuth::Bearer,
            credential_kind: None,
            credential_value: String::new(),
            credential_args: Vec::new(),
            oauth_client_id: String::new(),
            oauth_authorization_endpoint: String::new(),
            oauth_token_endpoint: String::new(),
            oauth_scopes: String::new(),
            discovery: ProviderDiscovery::OpenAiModels {
                path: "models".to_owned(),
            },
            extra_headers: BTreeMap::new(),
            error: None,
            status: None,
        }
    }
}

impl ProviderWizardState {
    pub fn with_existing_provider_ids(provider_ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            existing_provider_ids: provider_ids.into_iter().collect(),
            ..Self::default()
        }
    }

    pub fn replaces_existing_provider(&self) -> bool {
        self.replace_existing
    }

    pub fn mark_persisted(&mut self) {
        self.replace_existing = true;
    }

    pub fn oauth_flow_name(&self) -> Option<&'static str> {
        match self.credential_kind {
            Some(CredentialKind::OAuthAuthorizationCode) => Some("authorization-code"),
            Some(CredentialKind::OAuthDeviceCode) => Some("device-code"),
            _ => None,
        }
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.step = ProviderWizardStep::Summary;
        self.error = Some(message.into());
        self.status = None;
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.step = ProviderWizardStep::Submitting;
        self.status = Some(status.into());
        self.error = None;
    }

    fn is_known(&self) -> bool {
        self.known_provider.is_some()
    }

    fn is_choice_step(&self) -> bool {
        matches!(
            self.step,
            ProviderWizardStep::Provider
                | ProviderWizardStep::ExistingProvider
                | ProviderWizardStep::KnownAuth
                | ProviderWizardStep::Auth
                | ProviderWizardStep::Credential
                | ProviderWizardStep::Discovery
        )
    }

    fn choice_count(&self) -> usize {
        match self.step {
            ProviderWizardStep::Provider => provider_choice_count(),
            ProviderWizardStep::ExistingProvider => 2,
            ProviderWizardStep::KnownAuth => 1,
            ProviderWizardStep::Auth => 4,
            ProviderWizardStep::Credential if self.is_known() => 2,
            ProviderWizardStep::Credential => 4,
            ProviderWizardStep::Discovery => 3,
            _ => 0,
        }
    }

    fn apply_known_provider(&mut self, provider: KnownProvider) {
        let preset = provider_preset(provider);
        self.known_provider = Some(provider);
        self.provider_id = preset.id.into();
        self.display_name = preset.display_name.into();
        self.base_url = preset.base_url.into();
        self.auth = preset.auth;
        self.credential_kind = None;
        self.credential_value = preset.environment_variable.into();
        self.credential_args.clear();
        self.discovery = preset.discovery;
        self.extra_headers = preset.extra_headers;
        self.replace_existing = false;
        self.input.clear();
    }

    fn reset_custom_provider(&mut self) {
        self.known_provider = None;
        self.provider_id.clear();
        self.display_name.clear();
        self.base_url.clear();
        self.auth = ProviderAuth::Bearer;
        self.credential_kind = None;
        self.credential_value.clear();
        self.credential_args.clear();
        self.oauth_client_id.clear();
        self.oauth_authorization_endpoint.clear();
        self.oauth_token_endpoint.clear();
        self.oauth_scopes.clear();
        self.discovery = ProviderDiscovery::OpenAiModels {
            path: "models".into(),
        };
        self.extra_headers.clear();
        self.replace_existing = false;
        self.input.clear();
    }

    fn default_environment_variable(&self) -> String {
        if let Some(provider) = self.known_provider {
            return provider_preset(provider).environment_variable.into();
        }
        format!(
            "{}_API_KEY",
            self.provider_id
                .chars()
                .map(|character| if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                })
                .collect::<String>()
        )
    }

    fn after_credential_reference(&mut self) {
        self.selected = 0;
        self.input.clear();
        self.step = if self.is_known() {
            ProviderWizardStep::Summary
        } else {
            ProviderWizardStep::Discovery
        };
    }

    fn advance_text(&mut self) -> Result<(), String> {
        let value = self.input.trim().to_owned();
        match self.step {
            ProviderWizardStep::Id => {
                if value.is_empty() {
                    return Err("Provider ID is required".into());
                }
                self.provider_id = value;
                self.replace_existing = false;
                if self.display_name.is_empty() {
                    self.display_name = self.provider_id.clone();
                }
                self.input.clear();
                if self.existing_provider_ids.contains(&self.provider_id) {
                    self.selected = 0;
                    self.step = ProviderWizardStep::ExistingProvider;
                } else {
                    self.input = self.display_name.clone();
                    self.step = ProviderWizardStep::DisplayName;
                }
            }
            ProviderWizardStep::DisplayName => {
                if value.is_empty() {
                    return Err("Display name is required".into());
                }
                self.display_name = value;
                self.input = self.base_url.clone();
                self.step = ProviderWizardStep::BaseUrl;
            }
            ProviderWizardStep::BaseUrl => {
                Url::parse(&value).map_err(|error| format!("Invalid API base URL: {error}"))?;
                self.base_url = value;
                self.selected = 0;
                self.input.clear();
                self.step = ProviderWizardStep::Auth;
            }
            ProviderWizardStep::CustomHeader => {
                if value.is_empty() {
                    return Err("Credential header name is required".into());
                }
                self.auth = ProviderAuth::Header { name: value };
                self.selected = 0;
                self.input.clear();
                self.step = ProviderWizardStep::Credential;
            }
            ProviderWizardStep::CredentialValue => {
                if value.is_empty() {
                    return Err("Credential reference value is required".into());
                }
                self.credential_value = value;
                if self.credential_kind == Some(CredentialKind::Command) {
                    self.input = serde_json::to_string(&self.credential_args)
                        .unwrap_or_else(|_| "[]".to_owned());
                    self.step = ProviderWizardStep::CredentialArguments;
                } else {
                    self.after_credential_reference();
                }
            }
            ProviderWizardStep::CredentialArguments => {
                let args = serde_json::from_str::<Vec<String>>(&value).map_err(|error| {
                    format!("Command arguments must be a JSON string array: {error}")
                })?;
                if args.iter().any(|argument| argument.contains('\0')) {
                    return Err("Command arguments must not contain NUL".into());
                }
                self.credential_args = args;
                self.after_credential_reference();
            }
            ProviderWizardStep::OAuthClientId => {
                if value.is_empty() {
                    return Err("OAuth client ID is required".into());
                }
                self.oauth_client_id = value;
                self.input = self.oauth_authorization_endpoint.clone();
                self.step = ProviderWizardStep::OAuthAuthorizationEndpoint;
            }
            ProviderWizardStep::OAuthAuthorizationEndpoint => {
                Url::parse(&value)
                    .map_err(|error| format!("Invalid OAuth authorization endpoint: {error}"))?;
                self.oauth_authorization_endpoint = value;
                self.input = self.oauth_token_endpoint.clone();
                self.step = ProviderWizardStep::OAuthTokenEndpoint;
            }
            ProviderWizardStep::OAuthTokenEndpoint => {
                Url::parse(&value)
                    .map_err(|error| format!("Invalid OAuth token endpoint: {error}"))?;
                self.oauth_token_endpoint = value;
                self.input = self.oauth_scopes.clone();
                self.step = ProviderWizardStep::OAuthScopes;
            }
            ProviderWizardStep::OAuthScopes => {
                self.oauth_scopes = value;
                self.selected = 0;
                self.input.clear();
                self.step = ProviderWizardStep::Discovery;
            }
            ProviderWizardStep::DiscoveryPath => {
                if value.is_empty() {
                    return Err("Discovery path is required".into());
                }
                self.discovery = ProviderDiscovery::OpenAiModels { path: value };
                self.input.clear();
                self.step = ProviderWizardStep::Summary;
            }
            _ => {}
        }
        Ok(())
    }

    fn advance_choice(&mut self) {
        match self.step {
            ProviderWizardStep::Provider => {
                let choices = provider_choices();
                let Some(choice) = choices.get(self.selected) else {
                    return;
                };
                if let Some(provider) = choice.known {
                    self.apply_known_provider(provider);
                    self.step = if self.existing_provider_ids.contains(&self.provider_id) {
                        ProviderWizardStep::ExistingProvider
                    } else {
                        ProviderWizardStep::KnownAuth
                    };
                } else {
                    self.reset_custom_provider();
                    self.step = ProviderWizardStep::Id;
                }
                self.selected = 0;
            }
            ProviderWizardStep::ExistingProvider => {
                if self.selected == 0 {
                    if self.is_known() {
                        self.selected = self
                            .known_provider
                            .and_then(|provider| {
                                provider_choices()
                                    .iter()
                                    .position(|choice| choice.known == Some(provider))
                            })
                            .unwrap_or(0);
                        self.step = ProviderWizardStep::Provider;
                    } else {
                        self.input = self.provider_id.clone();
                        self.step = ProviderWizardStep::Id;
                    }
                    self.replace_existing = false;
                } else {
                    self.replace_existing = true;
                    self.selected = 0;
                    if self.is_known() {
                        self.step = ProviderWizardStep::KnownAuth;
                    } else {
                        self.input = self.display_name.clone();
                        self.step = ProviderWizardStep::DisplayName;
                    }
                }
            }
            ProviderWizardStep::KnownAuth => {
                self.credential_kind = Some(CredentialKind::Environment);
                self.credential_value = self.default_environment_variable();
                self.selected = 0;
                self.step = ProviderWizardStep::Credential;
            }
            ProviderWizardStep::Auth => match self.selected {
                0 => {
                    self.auth = ProviderAuth::Bearer;
                    self.selected = 0;
                    self.step = ProviderWizardStep::Credential;
                }
                1 => {
                    self.auth = ProviderAuth::Header {
                        name: "x-api-key".into(),
                    };
                    self.selected = 0;
                    self.step = ProviderWizardStep::Credential;
                }
                2 => {
                    self.input.clear();
                    self.step = ProviderWizardStep::CustomHeader;
                }
                _ => {
                    self.auth = ProviderAuth::None;
                    self.credential_kind = None;
                    self.selected = 0;
                    self.step = ProviderWizardStep::Discovery;
                }
            },
            ProviderWizardStep::Credential => {
                let previous_kind = self.credential_kind;
                let selected_kind = if self.is_known() {
                    match self.selected {
                        0 => CredentialKind::Environment,
                        _ => CredentialKind::Command,
                    }
                } else {
                    match self.selected {
                        0 => CredentialKind::Environment,
                        1 => CredentialKind::Command,
                        2 => CredentialKind::OAuthAuthorizationCode,
                        _ => CredentialKind::OAuthDeviceCode,
                    }
                };
                self.credential_kind = Some(selected_kind);
                match selected_kind {
                    CredentialKind::Environment => {
                        if previous_kind != Some(CredentialKind::Environment) {
                            self.credential_value = self.default_environment_variable();
                        }
                        self.input = self.credential_value.clone();
                        self.step = ProviderWizardStep::CredentialValue;
                    }
                    CredentialKind::Command => {
                        if previous_kind != Some(CredentialKind::Command) {
                            self.credential_value.clear();
                            self.credential_args.clear();
                        }
                        self.input = self.credential_value.clone();
                        self.step = ProviderWizardStep::CredentialValue;
                    }
                    CredentialKind::OAuthAuthorizationCode | CredentialKind::OAuthDeviceCode => {
                        self.input = self.oauth_client_id.clone();
                        self.step = ProviderWizardStep::OAuthClientId;
                    }
                }
            }
            ProviderWizardStep::Discovery => match self.selected {
                0 => {
                    let path = match &self.discovery {
                        ProviderDiscovery::OpenAiModels { path } => path.clone(),
                        _ => "models".into(),
                    };
                    self.input = path;
                    self.step = ProviderWizardStep::DiscoveryPath;
                }
                1 => {
                    self.discovery = ProviderDiscovery::Static;
                    self.step = ProviderWizardStep::Summary;
                }
                _ => {
                    self.discovery = ProviderDiscovery::Disabled;
                    self.step = ProviderWizardStep::Summary;
                }
            },
            _ => {}
        }
    }

    fn credential(&self) -> Result<CredentialRef, String> {
        match self.credential_kind {
            None => Ok(CredentialRef::None),
            Some(CredentialKind::Environment) => Ok(CredentialRef::Environment {
                variable: self.credential_value.clone(),
            }),
            Some(CredentialKind::Command) => Ok(CredentialRef::Command {
                program: self.credential_value.clone(),
                args: self.credential_args.clone(),
            }),
            Some(CredentialKind::OAuthAuthorizationCode)
            | Some(CredentialKind::OAuthDeviceCode) => {
                let authorization_endpoint = Url::parse(&self.oauth_authorization_endpoint)
                    .map_err(|error| format!("Invalid OAuth authorization endpoint: {error}"))?;
                let token_endpoint = Url::parse(&self.oauth_token_endpoint)
                    .map_err(|error| format!("Invalid OAuth token endpoint: {error}"))?;
                let mut method =
                    if self.credential_kind == Some(CredentialKind::OAuthAuthorizationCode) {
                        ProviderOAuthMethod::authorization_code(
                            &self.oauth_client_id,
                            authorization_endpoint,
                            token_endpoint,
                        )
                    } else {
                        OAuthMethod::device_code(
                            &self.oauth_client_id,
                            authorization_endpoint,
                            token_endpoint,
                        )
                    };
                let scopes = self
                    .oauth_scopes
                    .split(',')
                    .filter(|scope| !scope.trim().is_empty())
                    .map(|scope| scope.trim().to_owned())
                    .collect::<Vec<_>>();
                match &mut method {
                    ProviderOAuthMethod::AuthorizationCode {
                        scopes: method_scopes,
                        ..
                    }
                    | ProviderOAuthMethod::DeviceCode {
                        scopes: method_scopes,
                        ..
                    } => *method_scopes = scopes,
                }
                Ok(CredentialRef::OAuth {
                    provider_id: self.provider_id.clone(),
                    methods: vec![method],
                })
            }
        }
    }

    fn config(&self) -> Result<ProviderConfig, String> {
        let config = ProviderConfig {
            id: self.provider_id.clone(),
            display_name: self.display_name.clone(),
            base_url: Url::parse(&self.base_url)
                .map_err(|error| format!("Invalid API base URL: {error}"))?,
            credential: self.credential()?,
            auth: self.auth.clone(),
            discovery: self.discovery.clone(),
            extra_headers: self.extra_headers.clone(),
            enabled: true,
        };
        config.validate().map_err(|error| error.to_string())?;
        Ok(config)
    }

    fn back_from_summary(&mut self) {
        if self.is_known() {
            match self.credential_kind {
                Some(CredentialKind::Command) => {
                    self.input = serde_json::to_string(&self.credential_args)
                        .unwrap_or_else(|_| "[]".to_owned());
                    self.step = ProviderWizardStep::CredentialArguments;
                }
                Some(CredentialKind::Environment) => {
                    self.input = self.credential_value.clone();
                    self.step = ProviderWizardStep::CredentialValue;
                }
                _ => self.step = ProviderWizardStep::KnownAuth,
            }
        } else {
            self.step = ProviderWizardStep::Discovery;
        }
    }

    fn back(&mut self) {
        self.error = None;
        self.status = None;
        match self.step {
            ProviderWizardStep::Provider => {}
            ProviderWizardStep::Id => self.step = ProviderWizardStep::Provider,
            ProviderWizardStep::ExistingProvider => {
                if self.is_known() {
                    self.step = ProviderWizardStep::Provider;
                } else {
                    self.input = self.provider_id.clone();
                    self.step = ProviderWizardStep::Id;
                }
            }
            ProviderWizardStep::DisplayName => {
                if self.replace_existing {
                    self.selected = 1;
                    self.input.clear();
                    self.step = ProviderWizardStep::ExistingProvider;
                } else {
                    self.input = self.provider_id.clone();
                    self.step = ProviderWizardStep::Id;
                }
            }
            ProviderWizardStep::BaseUrl => {
                self.input = self.display_name.clone();
                self.step = ProviderWizardStep::DisplayName;
            }
            ProviderWizardStep::KnownAuth => {
                if self.replace_existing {
                    self.selected = 1;
                    self.step = ProviderWizardStep::ExistingProvider;
                } else {
                    self.selected = self
                        .known_provider
                        .and_then(|provider| {
                            provider_choices()
                                .iter()
                                .position(|choice| choice.known == Some(provider))
                        })
                        .unwrap_or(0);
                    self.step = ProviderWizardStep::Provider;
                }
            }
            ProviderWizardStep::Auth => {
                self.input = self.base_url.clone();
                self.step = ProviderWizardStep::BaseUrl;
            }
            ProviderWizardStep::CustomHeader => self.step = ProviderWizardStep::Auth,
            ProviderWizardStep::Credential => {
                self.step = if self.is_known() {
                    ProviderWizardStep::KnownAuth
                } else {
                    ProviderWizardStep::Auth
                }
            }
            ProviderWizardStep::CredentialValue | ProviderWizardStep::OAuthClientId => {
                self.step = ProviderWizardStep::Credential
            }
            ProviderWizardStep::CredentialArguments => {
                self.input = self.credential_value.clone();
                self.step = ProviderWizardStep::CredentialValue;
            }
            ProviderWizardStep::OAuthAuthorizationEndpoint => {
                self.input = self.oauth_client_id.clone();
                self.step = ProviderWizardStep::OAuthClientId;
            }
            ProviderWizardStep::OAuthTokenEndpoint => {
                self.input = self.oauth_authorization_endpoint.clone();
                self.step = ProviderWizardStep::OAuthAuthorizationEndpoint;
            }
            ProviderWizardStep::OAuthScopes => {
                self.input = self.oauth_token_endpoint.clone();
                self.step = ProviderWizardStep::OAuthTokenEndpoint;
            }
            ProviderWizardStep::Discovery => {
                if self.auth == ProviderAuth::None {
                    self.step = ProviderWizardStep::Auth;
                } else if matches!(
                    self.credential_kind,
                    Some(CredentialKind::Environment | CredentialKind::Command)
                ) {
                    if self.credential_kind == Some(CredentialKind::Command) {
                        self.input = serde_json::to_string(&self.credential_args)
                            .unwrap_or_else(|_| "[]".to_owned());
                        self.step = ProviderWizardStep::CredentialArguments;
                    } else {
                        self.input = self.credential_value.clone();
                        self.step = ProviderWizardStep::CredentialValue;
                    }
                } else {
                    self.input = self.oauth_scopes.clone();
                    self.step = ProviderWizardStep::OAuthScopes;
                }
            }
            ProviderWizardStep::DiscoveryPath => self.step = ProviderWizardStep::Discovery,
            ProviderWizardStep::Summary | ProviderWizardStep::Submitting => {
                self.back_from_summary()
            }
        }
    }
}

pub fn handle_provider_wizard_key(
    state: &mut ProviderWizardState,
    key: &KeyEvent,
) -> ProviderWizardOutcome {
    if key.code == KeyCode::Esc {
        return ProviderWizardOutcome::Cancel;
    }
    if key.code == KeyCode::BackTab {
        state.back();
        return ProviderWizardOutcome::Changed;
    }
    if state.step == ProviderWizardStep::Submitting {
        return ProviderWizardOutcome::Changed;
    }
    state.error = None;
    if state.is_choice_step() {
        match key.code {
            KeyCode::Up => {
                state.selected = state.selected.saturating_sub(1);
            }
            KeyCode::Down => {
                state.selected = (state.selected + 1).min(state.choice_count().saturating_sub(1));
            }
            KeyCode::Enter => state.advance_choice(),
            _ => {}
        }
        return ProviderWizardOutcome::Changed;
    }
    if state.step == ProviderWizardStep::Summary {
        if key.code == KeyCode::Enter {
            return match state.config() {
                Ok(config) => {
                    state.set_status("Saving Provider connection…");
                    ProviderWizardOutcome::Submit(Box::new(config))
                }
                Err(error) => {
                    state.error = Some(error);
                    ProviderWizardOutcome::Changed
                }
            };
        }
        return ProviderWizardOutcome::Changed;
    }
    match key.code {
        KeyCode::Enter => {
            if let Err(error) = state.advance_text() {
                state.error = Some(error);
            }
        }
        KeyCode::Backspace => {
            state.input.pop();
        }
        KeyCode::Char(character)
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
        {
            state.input.push(character);
        }
        _ => {}
    }
    ProviderWizardOutcome::Changed
}

fn step_title(step: ProviderWizardStep) -> &'static str {
    match step {
        ProviderWizardStep::Provider => "Select Provider",
        ProviderWizardStep::Id => "Provider ID",
        ProviderWizardStep::ExistingProvider => "Provider already exists",
        ProviderWizardStep::DisplayName => "Display name",
        ProviderWizardStep::BaseUrl => "API base URL (custom endpoint)",
        ProviderWizardStep::KnownAuth => "Authentication method",
        ProviderWizardStep::Auth => "Credential injection (advanced)",
        ProviderWizardStep::CustomHeader => "Credential header",
        ProviderWizardStep::Credential => "Credential source",
        ProviderWizardStep::CredentialValue => "Credential reference",
        ProviderWizardStep::CredentialArguments => "Credential command arguments",
        ProviderWizardStep::OAuthClientId => "Advanced custom OAuth client ID",
        ProviderWizardStep::OAuthAuthorizationEndpoint => {
            "Advanced custom OAuth authorization endpoint"
        }
        ProviderWizardStep::OAuthTokenEndpoint => "Advanced custom OAuth token endpoint",
        ProviderWizardStep::OAuthScopes => "Advanced custom OAuth scopes",
        ProviderWizardStep::Discovery => "Model discovery",
        ProviderWizardStep::DiscoveryPath => "Discovery path",
        ProviderWizardStep::Summary => "Review",
        ProviderWizardStep::Submitting => "Configuring Provider",
    }
}

fn provider_choice_lines(state: &ProviderWizardState) -> Vec<Line<'static>> {
    provider_choices()
        .into_iter()
        .enumerate()
        .map(|(index, choice)| choice_line(index, state.selected, choice.label, choice.description))
        .collect()
}

fn choice_line(
    index: usize,
    selected: usize,
    label: impl Into<String>,
    description: impl Into<String>,
) -> Line<'static> {
    let marker = if index == selected { "› " } else { "  " };
    Line::from(vec![
        Span::styled(
            format!("{marker}{}", label.into()),
            if index == selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ),
        Span::raw(format!("  {}", description.into())),
    ])
}

fn choice_lines(state: &ProviderWizardState) -> Vec<Line<'static>> {
    if state.step == ProviderWizardStep::Provider {
        return provider_choice_lines(state);
    }
    let choices: Vec<(&str, &str)> = match state.step {
        ProviderWizardStep::ExistingProvider => vec![
            (
                "Choose another Provider",
                "Keep the existing Provider unchanged",
            ),
            (
                "Replace existing Provider",
                "Overwrite its connection and credential reference",
            ),
        ],
        ProviderWizardStep::KnownAuth => vec![(
            "API key",
            "Use the selected Provider's built-in endpoint and authentication policy",
        )],
        ProviderWizardStep::Auth => vec![
            ("Bearer token", "Authorization: Bearer <credential>"),
            (
                "API key header",
                "Use x-api-key, commonly required by Anthropic-compatible APIs",
            ),
            (
                "Custom header",
                "Send the credential in a named HTTP header",
            ),
            ("No authentication", "Do not send a credential"),
        ],
        ProviderWizardStep::Credential if state.is_known() => vec![
            ("Environment variable", "Read a named environment variable"),
            (
                "Credential command",
                "Run a program that prints the credential",
            ),
        ],
        ProviderWizardStep::Credential => vec![
            ("Environment variable", "Read a named environment variable"),
            (
                "Credential command",
                "Run a program that prints the credential",
            ),
            (
                "Advanced custom OAuth: authorization code",
                "Configure Provider-owned OAuth metadata with PKCE",
            ),
            (
                "Advanced custom OAuth: device code",
                "Configure a Provider-owned device authorization flow",
            ),
        ],
        ProviderWizardStep::Discovery => vec![
            ("GET models", "Fetch a Provider model catalog"),
            ("Static", "Use only local exact model definitions"),
            ("Disabled", "Do not discover models"),
        ],
        _ => vec![],
    };
    choices
        .into_iter()
        .enumerate()
        .map(|(index, (label, description))| choice_line(index, state.selected, label, description))
        .collect()
}

fn summary_lines(state: &ProviderWizardState) -> Vec<Line<'static>> {
    let auth = if state.is_known() {
        "Provider-managed API key policy".to_owned()
    } else {
        match &state.auth {
            ProviderAuth::None => "none".to_owned(),
            ProviderAuth::Bearer => "Authorization: Bearer".to_owned(),
            ProviderAuth::Header { name } if name.eq_ignore_ascii_case("x-api-key") => {
                "API key header (x-api-key)".to_owned()
            }
            ProviderAuth::Header { name } => format!("custom header: {name}"),
        }
    };
    let credential = match state.credential_kind {
        None => "none".to_owned(),
        Some(CredentialKind::Environment) => format!("environment: {}", state.credential_value),
        Some(CredentialKind::Command) => format!(
            "command: {} ({} argument{})",
            state.credential_value,
            state.credential_args.len(),
            if state.credential_args.len() == 1 {
                ""
            } else {
                "s"
            }
        ),
        Some(CredentialKind::OAuthAuthorizationCode) => {
            "advanced custom OAuth authorization code".to_owned()
        }
        Some(CredentialKind::OAuthDeviceCode) => "advanced custom OAuth device code".to_owned(),
    };
    let discovery = match &state.discovery {
        ProviderDiscovery::OpenAiModels { path } => format!("GET {path}"),
        ProviderDiscovery::Static => "static".to_owned(),
        ProviderDiscovery::Disabled => "disabled".to_owned(),
    };
    let mut lines = Vec::new();
    if state.replace_existing {
        lines.push(Line::from(Span::styled(
            "This will replace the existing Provider configuration.",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
    }
    lines.extend([
        Line::from(format!("Provider ID:    {}", state.provider_id)),
        Line::from(format!("Name:           {}", state.display_name)),
        Line::from(format!("API endpoint:   {}", state.base_url)),
        Line::from(format!("Authentication: {auth}")),
        Line::from(format!("Credential:     {credential}")),
        Line::from(format!("Discovery:      {discovery}")),
        Line::from(""),
        Line::from(Span::styled(
            "Press Enter to save, test, discover models, and open /model.",
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ]);
    lines
}

fn helper_line(state: &ProviderWizardState) -> Option<Line<'static>> {
    match state.step {
        ProviderWizardStep::Provider => Some(Line::from(
            "Known Providers use reviewed endpoints and authentication policies. Choose Custom endpoint for a proxy or self-hosted API.",
        )),
        ProviderWizardStep::BaseUrl => Some(Line::from(
            "This is the model API endpoint. OAuth authorization and token endpoints are configured separately.",
        )),
        ProviderWizardStep::KnownAuth => Some(Line::from(
            "Atelier supplies the API endpoint and HTTP authentication policy for this Provider.",
        )),
        ProviderWizardStep::Auth => Some(Line::from(
            "Advanced custom endpoint setup: choose how the resolved credential is sent.",
        )),
        ProviderWizardStep::OAuthClientId
        | ProviderWizardStep::OAuthAuthorizationEndpoint
        | ProviderWizardStep::OAuthTokenEndpoint
        | ProviderWizardStep::OAuthScopes => Some(Line::from(
            "Advanced custom OAuth is for Provider metadata you administer or trust. Known Provider login never asks for these endpoints.",
        )),
        _ => None,
    }
}

pub fn render_provider_wizard(
    buf: &mut Buffer,
    area: Rect,
    state: &mut ProviderWizardState,
    compact: bool,
    theme: &Theme,
) {
    let shortcuts = [
        Shortcut {
            label: "Enter next",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Shift+Tab back",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Esc cancel",
            clickable: false,
            id: 0,
        },
    ];
    let config = ModalWindowConfig {
        title: "Add Provider",
        tabs: None,
        shortcuts: &shortcuts,
        sizing: ModalSizing {
            width_pct: 0.72,
            max_width: 104,
            min_width: 56,
            v_margin: 3,
            h_pad: 2,
            v_pad: 1,
            footer_lines: 2,
        }
        .with_compact(compact),
        fold_info: None,
    };
    let Some(content) = mw::render_modal_window(buf, area, &mut state.window, &config, theme)
    else {
        return;
    };
    let mut lines = vec![
        Line::from(Span::styled(
            step_title(state.step),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    if let Some(helper) = helper_line(state) {
        lines.push(helper);
        lines.push(Line::from(""));
    }
    if state.is_choice_step() {
        lines.extend(choice_lines(state));
    } else if state.step == ProviderWizardStep::Summary {
        lines.extend(summary_lines(state));
    } else if state.step == ProviderWizardStep::Submitting {
        lines.push(Line::from(
            state
                .status
                .as_deref()
                .unwrap_or("Configuring Provider…")
                .to_owned(),
        ));
    } else {
        lines.push(Line::from(format!("> {}_", state.input)));
    }
    if let Some(error) = &state.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
    }
    Paragraph::new(lines).render(content.content, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter(state: &mut ProviderWizardState) -> ProviderWizardOutcome {
        handle_provider_wizard_key(state, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    }

    fn down(state: &mut ProviderWizardState, count: usize) {
        for _ in 0..count {
            let _ = handle_provider_wizard_key(
                state,
                &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            );
        }
    }

    fn type_text(state: &mut ProviderWizardState, value: &str) {
        for character in value.chars() {
            let _ = handle_provider_wizard_key(
                state,
                &KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
    }

    fn select_custom_provider(state: &mut ProviderWizardState) {
        assert_eq!(state.step, ProviderWizardStep::Provider);
        down(state, provider_choice_count() - 1);
        enter(state);
        assert_eq!(state.step, ProviderWizardStep::Id);
    }

    #[test]
    fn known_openai_provider_does_not_ask_for_a_base_url_or_header_name() {
        let mut state = ProviderWizardState::default();
        assert_eq!(state.step, ProviderWizardStep::Provider);

        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::KnownAuth);
        assert_eq!(state.provider_id, "openai");
        assert_eq!(state.base_url, "https://api.openai.com/v1");

        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Credential);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::CredentialValue);
        assert_eq!(state.input, "OPENAI_API_KEY");
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Summary);

        let ProviderWizardOutcome::Submit(config) = enter(&mut state) else {
            panic!("known Provider must submit a Provider config");
        };
        assert_eq!(config.id, "openai");
        assert_eq!(config.auth, ProviderAuth::Bearer);
        assert_eq!(
            config.credential,
            CredentialRef::Environment {
                variable: "OPENAI_API_KEY".into(),
            }
        );
    }

    #[test]
    fn known_provider_command_source_does_not_reuse_the_environment_variable_name() {
        let mut state = ProviderWizardState::default();
        enter(&mut state);
        enter(&mut state);
        down(&mut state, 1);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::CredentialValue);
        assert!(state.input.is_empty());
    }

    #[test]
    fn known_anthropic_provider_owns_its_api_key_header_policy() {
        let mut state = ProviderWizardState::default();
        down(&mut state, 1);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::KnownAuth);
        assert_eq!(state.provider_id, "anthropic");

        enter(&mut state);
        enter(&mut state);
        enter(&mut state);
        let ProviderWizardOutcome::Submit(config) = enter(&mut state) else {
            panic!("Anthropic preset must submit");
        };
        assert_eq!(
            config.auth,
            ProviderAuth::Header {
                name: "x-api-key".into(),
            }
        );
        assert_eq!(
            config.credential,
            CredentialRef::Environment {
                variable: "ANTHROPIC_API_KEY".into(),
            }
        );
        assert_eq!(
            config
                .extra_headers
                .get("anthropic-version")
                .map(String::as_str),
            Some("2023-06-01")
        );
    }

    #[test]
    fn custom_provider_keeps_endpoint_and_low_level_auth_in_advanced_flow() {
        let mut state = ProviderWizardState::default();
        select_custom_provider(&mut state);
        type_text(&mut state, "example-provider");
        enter(&mut state);
        enter(&mut state);
        type_text(&mut state, "https://api.example.test/v1");
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Auth);

        down(&mut state, 1);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Credential);
        enter(&mut state);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Discovery);
        down(&mut state, 1);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Summary);

        let ProviderWizardOutcome::Submit(config) = enter(&mut state) else {
            panic!("custom Provider must submit");
        };
        assert_eq!(config.id, "example-provider");
        assert_eq!(
            config.auth,
            ProviderAuth::Header {
                name: "x-api-key".into(),
            }
        );
    }

    #[test]
    fn advanced_custom_oauth_collects_oauth_endpoints_after_the_api_endpoint() {
        let mut state = ProviderWizardState::default();
        select_custom_provider(&mut state);
        type_text(&mut state, "company-sso");
        enter(&mut state);
        enter(&mut state);
        type_text(&mut state, "https://api.example.test/v1");
        enter(&mut state);
        enter(&mut state);
        down(&mut state, 2);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::OAuthClientId);
    }

    #[test]
    fn existing_known_provider_requires_explicit_replacement_confirmation() {
        let mut state = ProviderWizardState::with_existing_provider_ids(["openai".to_owned()]);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::ExistingProvider);
        assert!(!state.replace_existing);

        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Provider);
        assert!(!state.replace_existing);

        enter(&mut state);
        down(&mut state, 1);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::KnownAuth);
        assert!(state.replace_existing);
    }

    #[test]
    fn command_credential_preserves_explicit_arguments_for_custom_provider() {
        let mut state = ProviderWizardState::default();
        select_custom_provider(&mut state);
        type_text(&mut state, "company");
        enter(&mut state);
        enter(&mut state);
        type_text(&mut state, "https://api.example.test/v1");
        enter(&mut state);
        enter(&mut state);
        down(&mut state, 1);
        enter(&mut state);
        type_text(&mut state, "credential-helper");
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::CredentialArguments);
        state.input = r#"["--profile","work"]"#.to_owned();
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Discovery);
        down(&mut state, 1);
        enter(&mut state);
        let ProviderWizardOutcome::Submit(config) = enter(&mut state) else {
            panic!("wizard must submit the command credential");
        };
        assert_eq!(
            config.credential,
            CredentialRef::Command {
                program: "credential-helper".into(),
                args: vec!["--profile".into(), "work".into()],
            }
        );
    }

    #[test]
    fn custom_wizard_reports_validation_errors_without_losing_input() {
        let mut state = ProviderWizardState::default();
        select_custom_provider(&mut state);
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Id);
        assert_eq!(state.error.as_deref(), Some("Provider ID is required"));

        type_text(&mut state, "example-provider");
        enter(&mut state);
        enter(&mut state);
        type_text(&mut state, "not a URL");
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::BaseUrl);
        assert_eq!(state.input, "not a URL");
        assert!(
            state
                .error
                .as_deref()
                .is_some_and(|error| error.starts_with("Invalid API base URL:"))
        );
    }

    #[test]
    fn wizard_supports_back_and_cancel_from_provider_selection() {
        let mut state = ProviderWizardState::default();
        select_custom_provider(&mut state);

        let outcome = handle_provider_wizard_key(
            &mut state,
            &KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        );
        assert!(matches!(outcome, ProviderWizardOutcome::Changed));
        assert_eq!(state.step, ProviderWizardStep::Provider);

        let outcome = handle_provider_wizard_key(
            &mut state,
            &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, ProviderWizardOutcome::Cancel));
    }

    #[test]
    fn public_provider_choices_are_known_providers_plus_custom_endpoint() {
        let labels = provider_choices()
            .into_iter()
            .map(|choice| choice.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "OpenAI",
                "Anthropic",
                "Google AI Studio",
                "DeepSeek",
                "xAI",
                "Custom endpoint",
            ]
        );
    }

    #[test]
    fn wizard_never_collects_a_raw_secret() {
        let state = ProviderWizardState::default();
        let rendered = format!(
            "{}{}{}{}",
            state.provider_id, state.credential_value, state.oauth_client_id, state.input
        );
        assert!(rendered.is_empty());
    }
}
