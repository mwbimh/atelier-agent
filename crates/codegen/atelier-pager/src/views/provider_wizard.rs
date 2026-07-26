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
    Id,
    ExistingProvider,
    DisplayName,
    BaseUrl,
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
    auth: ProviderAuth,
    credential_kind: Option<CredentialKind>,
    credential_value: String,
    credential_args: Vec<String>,
    oauth_client_id: String,
    oauth_authorization_endpoint: String,
    oauth_token_endpoint: String,
    oauth_scopes: String,
    discovery: ProviderDiscovery,
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
            step: ProviderWizardStep::Id,
            provider_id: String::new(),
            display_name: String::new(),
            base_url: String::new(),
            input: String::new(),
            selected: 0,
            existing_provider_ids: BTreeSet::new(),
            replace_existing: false,
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

    fn is_choice_step(&self) -> bool {
        matches!(
            self.step,
            ProviderWizardStep::ExistingProvider
                | ProviderWizardStep::Auth
                | ProviderWizardStep::Credential
                | ProviderWizardStep::Discovery
        )
    }

    fn choice_count(&self) -> usize {
        match self.step {
            ProviderWizardStep::ExistingProvider => 2,
            ProviderWizardStep::Auth => 4,
            ProviderWizardStep::Credential => 4,
            ProviderWizardStep::Discovery => 3,
            _ => 0,
        }
    }

    fn advance_text(&mut self) -> Result<Option<ProviderConfig>, String> {
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
                if self.base_url.is_empty() {
                    self.base_url = match self.provider_id.as_str() {
                        "openai" => "https://api.openai.com/v1".into(),
                        "anthropic" => "https://api.anthropic.com/v1".into(),
                        "local" => "http://127.0.0.1:11434/v1".into(),
                        _ => String::new(),
                    };
                }
                self.input = self.base_url.clone();
                self.step = ProviderWizardStep::BaseUrl;
            }
            ProviderWizardStep::BaseUrl => {
                Url::parse(&value).map_err(|error| format!("Invalid base URL: {error}"))?;
                self.base_url = value;
                self.selected = match self.provider_id.as_str() {
                    "anthropic" => 1,
                    "local" => 3,
                    _ => 0,
                };
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
                    self.selected = 0;
                    self.input.clear();
                    self.step = ProviderWizardStep::Discovery;
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
                self.selected = 0;
                self.input.clear();
                self.step = ProviderWizardStep::Discovery;
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
            _ => return Ok(None),
        }
        Ok(None)
    }

    fn advance_choice(&mut self) {
        match self.step {
            ProviderWizardStep::ExistingProvider => {
                if self.selected == 0 {
                    self.input = self.provider_id.clone();
                    self.step = ProviderWizardStep::Id;
                } else {
                    self.replace_existing = true;
                    self.input = self.display_name.clone();
                    self.step = ProviderWizardStep::DisplayName;
                }
                self.selected = 0;
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
                self.credential_kind = Some(match self.selected {
                    0 => CredentialKind::Environment,
                    1 => CredentialKind::Command,
                    2 => CredentialKind::OAuthAuthorizationCode,
                    _ => CredentialKind::OAuthDeviceCode,
                });
                if matches!(
                    self.credential_kind,
                    Some(CredentialKind::Environment | CredentialKind::Command)
                ) {
                    self.input = if self.credential_value.is_empty()
                        && self.credential_kind == Some(CredentialKind::Environment)
                    {
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
                    } else {
                        self.credential_value.clone()
                    };
                    self.step = ProviderWizardStep::CredentialValue;
                } else {
                    self.input = self.oauth_client_id.clone();
                    self.step = ProviderWizardStep::OAuthClientId;
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
                .map_err(|error| format!("Invalid base URL: {error}"))?,
            credential: self.credential()?,
            auth: self.auth.clone(),
            discovery: self.discovery.clone(),
            extra_headers: BTreeMap::new(),
            enabled: true,
        };
        config.validate().map_err(|error| error.to_string())?;
        Ok(config)
    }

    fn back(&mut self) {
        self.error = None;
        self.status = None;
        match self.step {
            ProviderWizardStep::ExistingProvider => {
                self.input = self.provider_id.clone();
                self.step = ProviderWizardStep::Id;
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
            ProviderWizardStep::Auth => {
                self.input = self.base_url.clone();
                self.step = ProviderWizardStep::BaseUrl;
            }
            ProviderWizardStep::CustomHeader => self.step = ProviderWizardStep::Auth,
            ProviderWizardStep::Credential => self.step = ProviderWizardStep::Auth,
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
                self.step = ProviderWizardStep::Discovery
            }
            ProviderWizardStep::Id => {}
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
        ProviderWizardStep::Id => "Provider ID",
        ProviderWizardStep::ExistingProvider => "Provider already exists",
        ProviderWizardStep::DisplayName => "Display name",
        ProviderWizardStep::BaseUrl => "Base URL",
        ProviderWizardStep::Auth => "Credential injection",
        ProviderWizardStep::CustomHeader => "Credential header",
        ProviderWizardStep::Credential => "Credential source",
        ProviderWizardStep::CredentialValue => "Credential reference",
        ProviderWizardStep::CredentialArguments => "Credential command arguments",
        ProviderWizardStep::OAuthClientId => "OAuth client ID",
        ProviderWizardStep::OAuthAuthorizationEndpoint => "OAuth authorization endpoint",
        ProviderWizardStep::OAuthTokenEndpoint => "OAuth token endpoint",
        ProviderWizardStep::OAuthScopes => "OAuth scopes",
        ProviderWizardStep::Discovery => "Model discovery",
        ProviderWizardStep::DiscoveryPath => "Discovery path",
        ProviderWizardStep::Summary => "Review",
        ProviderWizardStep::Submitting => "Configuring Provider",
    }
}

fn choice_lines(state: &ProviderWizardState) -> Vec<Line<'static>> {
    let choices: &[(&str, &str)] = match state.step {
        ProviderWizardStep::ExistingProvider => &[
            ("Choose another ID", "Keep the existing Provider unchanged"),
            (
                "Replace existing Provider",
                "Overwrite its connection and credential reference",
            ),
        ],
        ProviderWizardStep::Auth => &[
            ("Bearer", "Authorization: Bearer <credential>"),
            ("x-api-key", "x-api-key: <credential>"),
            ("Custom header", "Send credential in a named header"),
            ("No authentication", "Do not send a credential"),
        ],
        ProviderWizardStep::Credential => &[
            ("Environment variable", "Read a named environment variable"),
            (
                "Credential command",
                "Run a program that prints the credential",
            ),
            (
                "OAuth authorization code",
                "Browser authorization with PKCE",
            ),
            ("OAuth device code", "Device authorization flow"),
        ],
        ProviderWizardStep::Discovery => &[
            ("GET models", "Fetch a Provider model catalog"),
            ("Static", "Use only local exact model definitions"),
            ("Disabled", "Do not discover models"),
        ],
        _ => &[],
    };
    choices
        .iter()
        .enumerate()
        .map(|(index, (label, description))| {
            let marker = if index == state.selected {
                "› "
            } else {
                "  "
            };
            Line::from(vec![
                Span::styled(
                    format!("{marker}{label}"),
                    if index == state.selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::raw(format!("  {description}")),
            ])
        })
        .collect()
}

fn summary_lines(state: &ProviderWizardState) -> Vec<Line<'static>> {
    let auth = match &state.auth {
        ProviderAuth::None => "none".to_owned(),
        ProviderAuth::Bearer => "Authorization: Bearer".to_owned(),
        ProviderAuth::Header { name } => format!("header: {name}"),
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
        Some(CredentialKind::OAuthAuthorizationCode) => "OAuth authorization code".to_owned(),
        Some(CredentialKind::OAuthDeviceCode) => "OAuth device code".to_owned(),
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
        Line::from(format!("Provider ID:  {}", state.provider_id)),
        Line::from(format!("Name:         {}", state.display_name)),
        Line::from(format!("Base URL:     {}", state.base_url)),
        Line::from(format!("Auth:         {auth}")),
        Line::from(format!("Credential:   {credential}")),
        Line::from(format!("Discovery:    {discovery}")),
        Line::from(""),
        Line::from(Span::styled(
            "Press Enter to save, test, discover models, and open /model.",
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ]);
    lines
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
            width_pct: 0.66,
            max_width: 92,
            min_width: 52,
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

    fn type_text(state: &mut ProviderWizardState, value: &str) {
        for character in value.chars() {
            let _ = handle_provider_wizard_key(
                state,
                &KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
    }

    #[test]
    fn wizard_builds_provider_without_a_wire_api() {
        let mut state = ProviderWizardState::default();
        type_text(&mut state, "allm");
        enter(&mut state);
        enter(&mut state);
        type_text(&mut state, "https://api.example.test/v1");
        enter(&mut state);
        enter(&mut state);
        enter(&mut state);
        enter(&mut state);
        enter(&mut state);
        enter(&mut state);
        let ProviderWizardOutcome::Submit(config) = enter(&mut state) else {
            panic!("wizard must submit a Provider config");
        };
        assert_eq!(config.id, "allm");
        assert_eq!(config.auth, ProviderAuth::Bearer);
        assert!(matches!(
            config.credential,
            CredentialRef::Environment { .. }
        ));
    }

    #[test]
    fn existing_provider_requires_explicit_replacement_confirmation() {
        let mut state = ProviderWizardState::with_existing_provider_ids(["allm".to_owned()]);
        type_text(&mut state, "allm");
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::ExistingProvider);
        assert!(!state.replace_existing);

        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Id);
        assert!(!state.replace_existing);

        enter(&mut state);
        let _ = handle_provider_wizard_key(
            &mut state,
            &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::DisplayName);
        assert!(state.replace_existing);
    }

    #[test]
    fn command_credential_preserves_explicit_arguments() {
        let mut state = ProviderWizardState::default();
        type_text(&mut state, "company");
        enter(&mut state);
        enter(&mut state);
        type_text(&mut state, "https://api.example.test/v1");
        enter(&mut state);
        enter(&mut state);
        let _ = handle_provider_wizard_key(
            &mut state,
            &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        enter(&mut state);
        type_text(&mut state, "credential-helper");
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::CredentialArguments);
        state.input = r#"["--profile","work"]"#.to_owned();
        enter(&mut state);
        enter(&mut state);
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
    fn wizard_reports_validation_errors_without_losing_input() {
        let mut state = ProviderWizardState::default();
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::Id);
        assert_eq!(state.error.as_deref(), Some("Provider ID is required"));

        type_text(&mut state, "allm");
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
                .is_some_and(|error| error.starts_with("Invalid base URL:"))
        );
    }

    #[test]
    fn wizard_supports_back_and_cancel() {
        let mut state = ProviderWizardState::default();
        type_text(&mut state, "allm");
        enter(&mut state);
        assert_eq!(state.step, ProviderWizardStep::DisplayName);

        let outcome = handle_provider_wizard_key(
            &mut state,
            &KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        );
        assert!(matches!(outcome, ProviderWizardOutcome::Changed));
        assert_eq!(state.step, ProviderWizardStep::Id);
        assert_eq!(state.input, "allm");

        let outcome = handle_provider_wizard_key(
            &mut state,
            &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(matches!(outcome, ProviderWizardOutcome::Cancel));
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
