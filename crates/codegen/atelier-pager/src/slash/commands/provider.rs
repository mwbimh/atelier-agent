// `/provider` local Provider registry management.

use atelier_provider::auth::ProviderOAuthMethod;
use atelier_provider::{
    CredentialRef, ProviderConfig, ProviderDiscovery, ProviderProtocol, ProviderRegistry,
};
use std::collections::BTreeMap;
use url::Url;

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

pub struct ProviderCommand;

impl SlashCommand for ProviderCommand {
    fn name(&self) -> &str {
        "provider"
    }

    fn aliases(&self) -> &[&str] {
        &["providers"]
    }

    fn description(&self) -> &str {
        "Manage local model Providers"
    }

    fn usage(&self) -> &str {
        "/provider [list|add|edit|enable|disable|test|refresh|login|logout|delete] [id] ..."
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn suggest_args(&self, _ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let trailing_space = args_query.chars().last().is_some_and(char::is_whitespace);
        let query = args_query.trim_end();
        let tokens = query.split_whitespace().collect::<Vec<_>>();
        let command = tokens.first().copied();
        let has_argument = tokens.len() > 1;

        if matches!(command, Some("add" | "edit")) {
            return provider_form_items(command.unwrap_or_default(), &tokens[1..], trailing_space);
        }

        if command == Some("login") && has_argument {
            let mut parts = query.split_whitespace();
            let _ = parts.next();
            let provider_id = parts.next()?;
            if parts.next().is_none() && trailing_space {
                let path = atelier_config::atelier_home().join("providers.toml");
                let registry = ProviderRegistry::load_or_create(path).ok()?;
                let provider = registry.provider(provider_id)?;
                return provider_oauth_flow_items(provider_id, &provider.credential);
            }
        }

        if command.is_some() && !has_argument {
            match command.unwrap_or_default() {
                "edit" | "enable" | "disable" | "test" | "delete" | "refresh" | "login"
                | "logout" => {
                    let path = atelier_config::atelier_home().join("providers.toml");
                    let registry = ProviderRegistry::load_or_create(path).ok()?;
                    let prefix = command.unwrap_or_default();
                    let items = registry
                        .providers()
                        .map(|provider| ArgItem {
                            display: provider.id.clone(),
                            match_text: provider.id.clone(),
                            insert_text: provider_id_insert_text(&prefix, &provider.id),
                            description: format!("{prefix} Provider"),
                        })
                        .collect::<Vec<_>>();
                    return (!items.is_empty()).then_some(items);
                }
                _ => {}
            }
        }

        Some(
            [
                "list", "add ", "edit ", "enable ", "disable ", "test ", "refresh ", "login ",
                "logout ", "delete ",
            ]
            .into_iter()
            .map(|command| ArgItem {
                display: command.trim().into(),
                match_text: command.trim().into(),
                insert_text: command.into(),
                description: "local Provider registry".into(),
            })
            .collect(),
        )
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if args.trim().is_empty() {
            return CommandResult::Action(Action::OpenSlashArgPicker {
                command: self.name().to_owned(),
            });
        }
        let mut parts = args.split_whitespace();
        let command = parts.next().unwrap_or("list");
        let provider_id = parts.next();

        match command {
            "list" => {
                if provider_id.is_some() {
                    return CommandResult::Error("Usage: /provider list".into());
                }
                runtime_extension("_atelier/provider/list", serde_json::json!({}))
            }
            "add" | "edit" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error(format!(
                        "Usage: /provider {command} <id> <protocol> <base-url> [credential]"
                    ));
                };
                let fields = parts.collect::<Vec<_>>();
                if fields.len() < 2 {
                    return CommandResult::Error(format!(
                        "Usage: /provider {command} <id> <protocol> <base-url> [credential]"
                    ));
                }
                let credential_provided = fields.len() >= 3;
                let spec = fields.join(" ");
                let config = match parse_provider_spec(provider_id, &spec) {
                    Ok(config) => config,
                    Err(error) => return CommandResult::Error(error),
                };
                let mut params = serde_json::json!({ "provider": config });
                if command == "edit" {
                    params["preserveExisting"] = serde_json::Value::Bool(true);
                    params["preserveExistingCredential"] =
                        serde_json::Value::Bool(!credential_provided);
                }
                runtime_extension(
                    if command == "add" {
                        "_atelier/provider/create"
                    } else {
                        "_atelier/provider/update"
                    },
                    params,
                )
            }
            "enable" | "disable" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error(format!("Usage: /provider {command} <id>"));
                };
                if parts.next().is_some() {
                    return CommandResult::Error(format!("Usage: /provider {command} <id>"));
                }
                runtime_extension(
                    "_atelier/provider/enable",
                    serde_json::json!({
                        "providerId": provider_id,
                        "enabled": command == "enable",
                    }),
                )
            }
            "delete" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error("Usage: /provider delete <id>".into());
                };
                if parts.next().is_some() {
                    return CommandResult::Error("Usage: /provider delete <id>".into());
                }
                runtime_extension(
                    "_atelier/provider/delete",
                    serde_json::json!({ "providerId": provider_id }),
                )
            }
            "test" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error("Usage: /provider test <id>".into());
                };
                if parts.next().is_some() {
                    return CommandResult::Error("Usage: /provider test <id>".into());
                }
                runtime_extension(
                    "_atelier/provider/test",
                    serde_json::json!({ "providerId": provider_id }),
                )
            }
            "refresh" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error("Usage: /provider refresh <id>".into());
                };
                if parts.next().is_some() {
                    return CommandResult::Error("Usage: /provider refresh <id>".into());
                }
                CommandResult::Action(Action::RefreshProviderModels(provider_id.to_owned()))
            }
            "login" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error(
                        "Usage: /provider login <id> [authorization-code|device-code]".into(),
                    );
                };
                let flow = parts.next();
                if parts.next().is_some()
                    || flow
                        .is_some_and(|flow| !matches!(flow, "authorization-code" | "device-code"))
                {
                    return CommandResult::Error(
                        "Usage: /provider login <id> [authorization-code|device-code]".into(),
                    );
                }
                runtime_extension(
                    "_atelier/provider/oauth_begin",
                    serde_json::json!({
                        "providerId": provider_id,
                        "flow": flow,
                    }),
                )
            }
            "logout" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error("Usage: /provider logout <id>".into());
                };
                if parts.next().is_some() {
                    return CommandResult::Error("Usage: /provider logout <id>".into());
                }
                runtime_extension(
                    "_atelier/provider/oauth_logout",
                    serde_json::json!({ "providerId": provider_id }),
                )
            }
            _ => CommandResult::Error(format!("Usage: {}", self.usage())),
        }
    }
}

fn provider_oauth_flow_items(
    provider_id: &str,
    credential: &CredentialRef,
) -> Option<Vec<ArgItem>> {
    let CredentialRef::OAuth { methods, .. } = credential else {
        return None;
    };
    let items = methods
        .iter()
        .map(|method| ArgItem {
            display: method.flow_name().into(),
            match_text: method.flow_name().into(),
            insert_text: format!("login {provider_id} {}", method.flow_name()),
            description: "Provider OAuth flow".into(),
        })
        .collect::<Vec<_>>();
    (!items.is_empty()).then_some(items)
}

fn provider_form_items(
    command: &str,
    fields: &[&str],
    trailing_space: bool,
) -> Option<Vec<ArgItem>> {
    match fields {
        [] => Some(provider_id_items(command)),
        [_provider_id] if !trailing_space => Some(provider_id_items(command)),
        [provider_id] => Some(provider_protocol_items(command, provider_id)),
        [provider_id, _protocol] if !trailing_space => {
            Some(provider_protocol_items(command, provider_id))
        }
        [provider_id, protocol] => Some(provider_base_url_items(command, provider_id, protocol)),
        [provider_id, protocol, _base_url] if !trailing_space => {
            Some(provider_base_url_items(command, provider_id, protocol))
        }
        [provider_id, protocol, base_url] => Some(provider_credential_items(
            command,
            provider_id,
            protocol,
            base_url,
        )),
        [provider_id, protocol, base_url, "oauth"] => Some(provider_oauth_kind_items(
            command,
            provider_id,
            protocol,
            base_url,
        )),
        [provider_id, protocol, base_url, "oauth", flow]
            if matches!(*flow, "authorization-code" | "device-code") =>
        {
            Some(provider_oauth_template_items(
                command,
                provider_id,
                protocol,
                base_url,
                flow,
            ))
        }
        _ => None,
    }
}

fn provider_id_items(command: &str) -> Vec<ArgItem> {
    if command == "edit" {
        let path = atelier_config::atelier_home().join("providers.toml");
        let configured = ProviderRegistry::load_or_create(path)
            .ok()
            .map(|registry| {
                registry
                    .providers()
                    .map(|provider| ArgItem {
                        display: provider.id.clone(),
                        match_text: provider.id.clone(),
                        insert_text: format!("edit {} ", provider.id),
                        description: "Edit configured Provider".to_owned(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !configured.is_empty() {
            return configured;
        }
        return vec![ArgItem {
            display: "add a Provider first".to_owned(),
            match_text: "add provider".to_owned(),
            insert_text: "add ".to_owned(),
            description: "No configured Providers are available to edit".to_owned(),
        }];
    }
    [
        ("allm", "General OpenAI-compatible Provider"),
        ("openai", "OpenAI Provider"),
        ("anthropic", "Anthropic Provider"),
        ("local", "Local model Provider"),
    ]
    .into_iter()
    .map(|(provider_id, description)| ArgItem {
        display: provider_id.to_owned(),
        match_text: provider_id.to_owned(),
        insert_text: format!("{command} {provider_id} "),
        description: description.to_owned(),
    })
    .collect()
}

fn provider_protocol_items(command: &str, provider_id: &str) -> Vec<ArgItem> {
    [
        ("responses", "OpenAI Responses API"),
        ("chat", "OpenAI Chat Completions API"),
        ("anthropic", "Anthropic Messages API"),
    ]
    .into_iter()
    .map(|(protocol, description)| ArgItem {
        display: protocol.to_owned(),
        match_text: protocol.to_owned(),
        insert_text: format!("{command} {provider_id} {protocol} "),
        description: description.to_owned(),
    })
    .collect()
}

fn provider_base_url_items(command: &str, provider_id: &str, protocol: &str) -> Vec<ArgItem> {
    let urls: &[(&str, &str)] = match protocol {
        "responses" | "openai-responses" => &[
            ("https://api.openai.com/v1", "OpenAI public API"),
            (
                "https://api.example.com/v1",
                "Editable HTTPS Provider template",
            ),
        ],
        "anthropic" | "messages" => &[
            ("https://api.anthropic.com", "Anthropic public API"),
            (
                "https://api.example.com",
                "Editable HTTPS Provider template",
            ),
        ],
        _ => &[
            (
                "https://api.example.com/v1",
                "Editable HTTPS Provider template",
            ),
            ("http://127.0.0.1:11434/v1", "Local Ollama-compatible API"),
            ("http://127.0.0.1:1234/v1", "Local LM Studio-compatible API"),
        ],
    };
    urls.iter()
        .map(|(base_url, description)| ArgItem {
            display: (*base_url).to_owned(),
            match_text: (*base_url).to_owned(),
            insert_text: format!("{command} {provider_id} {protocol} {base_url} "),
            description: (*description).to_owned(),
        })
        .collect()
}

fn provider_credential_items(
    command: &str,
    provider_id: &str,
    protocol: &str,
    base_url: &str,
) -> Vec<ArgItem> {
    let prefix = format!("{command} {provider_id} {protocol} {base_url}");
    let environment_variable = format!(
        "{}_API_KEY",
        provider_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    );
    let mut items = Vec::new();
    if command == "edit" {
        items.push(ArgItem {
            display: "keep-existing".to_owned(),
            match_text: "keep-existing".to_owned(),
            insert_text: prefix.clone(),
            description: "Keep the Provider's existing credential".to_owned(),
        });
    }
    items.extend([
        ArgItem {
            display: format!("env:{environment_variable}"),
            match_text: format!("env:{environment_variable}"),
            insert_text: format!("{prefix} env:{environment_variable}"),
            description: "Read the API credential from this environment variable".to_owned(),
        },
        ArgItem {
            display: "cmd:PROGRAM".to_owned(),
            match_text: "cmd command program".to_owned(),
            insert_text: format!("{prefix} cmd:PROGRAM"),
            description: "Replace PROGRAM with a credential command".to_owned(),
        },
        ArgItem {
            display: "none".to_owned(),
            match_text: "none no credential".to_owned(),
            insert_text: format!("{prefix} none"),
            description: "Send no Provider credential".to_owned(),
        },
        ArgItem {
            display: "oauth authorization-code".to_owned(),
            match_text: "oauth authorization code browser pkce".to_owned(),
            insert_text: format!("{prefix} oauth authorization-code "),
            description: "Configure OAuth authorization-code + PKCE".to_owned(),
        },
        ArgItem {
            display: "oauth device-code".to_owned(),
            match_text: "oauth device code".to_owned(),
            insert_text: format!("{prefix} oauth device-code "),
            description: "Configure OAuth device authorization".to_owned(),
        },
    ]);
    items
}

fn provider_oauth_kind_items(
    command: &str,
    provider_id: &str,
    protocol: &str,
    base_url: &str,
) -> Vec<ArgItem> {
    [
        ("authorization-code", "OAuth authorization-code + PKCE"),
        ("device-code", "OAuth device authorization"),
    ]
    .into_iter()
    .map(|(flow, description)| ArgItem {
        display: flow.to_owned(),
        match_text: flow.to_owned(),
        insert_text: format!("{command} {provider_id} {protocol} {base_url} oauth {flow} "),
        description: description.to_owned(),
    })
    .collect()
}

fn provider_oauth_template_items(
    command: &str,
    provider_id: &str,
    protocol: &str,
    base_url: &str,
    flow: &str,
) -> Vec<ArgItem> {
    let endpoint = if flow == "authorization-code" {
        "AUTHORIZATION_ENDPOINT"
    } else {
        "DEVICE_AUTHORIZATION_ENDPOINT"
    };
    vec![ArgItem {
        display: "CLIENT_ID + endpoints".to_owned(),
        match_text: "client id endpoint token scopes".to_owned(),
        insert_text: format!(
            "{command} {provider_id} {protocol} {base_url} oauth {flow} CLIENT_ID {endpoint} TOKEN_ENDPOINT"
        ),
        description: "Replace placeholders; optionally append comma-separated scopes".to_owned(),
    }]
}

fn runtime_extension(method: &str, params: serde_json::Value) -> CommandResult {
    CommandResult::Action(Action::RuntimeExtension {
        method: method.to_owned(),
        params,
    })
}

/// Provider commands that need a free-form tail leave a trailing space after
/// the selected id. The generic ArgPicker then returns the half-completed
/// command to the composer instead of submitting an incomplete `/provider
/// edit` invocation.
fn provider_id_insert_text(command: &str, provider_id: &str) -> String {
    if matches!(command, "edit" | "login") {
        format!("{command} {provider_id} ")
    } else {
        format!("{command} {provider_id}")
    }
}

fn parse_provider_spec(provider_id: &str, spec: &str) -> Result<ProviderConfig, String> {
    let mut parts = spec.split_whitespace();
    let protocol = match parts.next().unwrap_or_default() {
        "responses" | "openai-responses" => ProviderProtocol::OpenAiResponses,
        "chat" | "chat-completions" | "openai-chat" => ProviderProtocol::OpenAiChatCompletions,
        "anthropic" | "messages" => ProviderProtocol::AnthropicMessages,
        value => {
            return Err(format!(
                "Unknown protocol '{value}'. Use responses, chat, or anthropic."
            ));
        }
    };
    let base_url = Url::parse(parts.next().unwrap_or_default())
        .map_err(|error| format!("Invalid provider base URL: {error}"))?;
    let credential_fields = parts.collect::<Vec<_>>();
    let credential = parse_credential_spec(provider_id, &credential_fields)?;
    let config = ProviderConfig {
        id: provider_id.to_owned(),
        display_name: provider_id.to_owned(),
        protocol,
        base_url,
        credential,
        discovery: ProviderDiscovery::OpenAiModels {
            path: "models".to_owned(),
        },
        extra_headers: BTreeMap::new(),
        enabled: true,
    };
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

fn parse_credential_spec(provider_id: &str, fields: &[&str]) -> Result<CredentialRef, String> {
    match fields {
        [] | ["none"] => Ok(CredentialRef::None),
        [value] if value.starts_with("env:") => Ok(CredentialRef::Environment {
            variable: value.strip_prefix("env:").unwrap_or_default().to_owned(),
        }),
        [value] if value.starts_with("cmd:") => Ok(CredentialRef::Command {
            program: value.strip_prefix("cmd:").unwrap_or_default().to_owned(),
            args: Vec::new(),
        }),
        [
            "oauth",
            flow,
            client_id,
            authorization_endpoint,
            token_endpoint,
        ] => oauth_credential(
            provider_id,
            flow,
            client_id,
            authorization_endpoint,
            token_endpoint,
            Vec::new(),
        ),
        [
            "oauth",
            flow,
            client_id,
            authorization_endpoint,
            token_endpoint,
            scopes,
        ] => oauth_credential(
            provider_id,
            flow,
            client_id,
            authorization_endpoint,
            token_endpoint,
            scopes
                .split(',')
                .filter(|scope| !scope.trim().is_empty())
                .map(|scope| scope.trim().to_owned())
                .collect(),
        ),
        ["oauth", ..] => Err(
            "OAuth credential usage: oauth <authorization-code|device-code> <client-id> <authorization-endpoint> <token-endpoint> [comma-separated-scopes]."
                .to_owned(),
        ),
        [value, ..] => Err(format!(
            "Unknown credential spec '{value}'. Use env:NAME, cmd:PROGRAM, none, or oauth <authorization-code|device-code> <client-id> <authorization-endpoint> <token-endpoint> [comma-separated-scopes]."
        )),
    }
}

fn oauth_credential(
    provider_id: &str,
    flow: &str,
    client_id: &str,
    authorization_endpoint: &str,
    token_endpoint: &str,
    scopes: Vec<String>,
) -> Result<CredentialRef, String> {
    let authorization_endpoint = Url::parse(authorization_endpoint)
        .map_err(|error| format!("Invalid OAuth authorization endpoint: {error}"))?;
    let token_endpoint = Url::parse(token_endpoint)
        .map_err(|error| format!("Invalid OAuth token endpoint: {error}"))?;
    let mut method = match flow {
        "authorization-code" => ProviderOAuthMethod::authorization_code(
            client_id,
            authorization_endpoint,
            token_endpoint,
        ),
        "device-code" => {
            ProviderOAuthMethod::device_code(client_id, authorization_endpoint, token_endpoint)
        }
        value => {
            return Err(format!(
                "Unknown OAuth flow '{value}'. Use authorization-code or device-code."
            ));
        }
    };
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
        provider_id: provider_id.to_owned(),
        methods: vec![method],
    })
}

#[cfg(test)]
mod tests {
    use super::{ProviderCommand, parse_provider_spec, provider_oauth_flow_items};
    use crate::app::actions::Action;
    use crate::slash::command::SlashCommand;
    use crate::slash::command::{CommandExecCtx, CommandResult};
    use atelier_provider::auth::ProviderOAuthMethod;
    use atelier_provider::{CredentialRef, ProviderProtocol};
    use url::Url;

    fn empty_ctx() -> CommandExecCtx<'static> {
        let models = Box::leak(Box::new(crate::acp::model_state::ModelState::default()));
        let bundle = Box::leak(Box::new(crate::app::bundle::BundleState::default()));
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn command_metadata_is_provider_specific() {
        let command = ProviderCommand;
        assert_eq!(command.name(), "provider");
        assert!(command.description().contains("Provider"));
    }

    #[test]
    fn empty_provider_command_opens_interactive_picker() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            ProviderCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::OpenSlashArgPicker { command }) if command == "provider"
        ));
    }

    #[test]
    fn provider_list_remains_a_complete_command() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            ProviderCommand.run(&mut ctx, "list"),
            CommandResult::Action(Action::RuntimeExtension { method, .. })
                if method == "_atelier/provider/list"
        ));
    }

    #[test]
    fn provider_refresh_is_a_complete_command() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            ProviderCommand.run(&mut ctx, "refresh missing-provider"),
            CommandResult::Action(Action::RefreshProviderModels(provider_id))
                if provider_id == "missing-provider"
        ));
    }

    #[test]
    fn provider_add_uses_the_live_runtime_service() {
        let mut ctx = empty_ctx();
        let result = ProviderCommand.run(
            &mut ctx,
            "add allm chat https://example.test/v1 env:ALLM_API_KEY",
        );
        let CommandResult::Action(Action::RuntimeExtension { method, params }) = result else {
            panic!("provider add must use the live runtime service");
        };
        assert_eq!(method, "_atelier/provider/create");
        assert_eq!(params["provider"]["id"], "allm");
        assert_eq!(params["provider"]["protocol"], "open_ai_chat_completions");
    }

    #[test]
    fn provider_edit_requests_merge_semantics() {
        let mut ctx = empty_ctx();
        let result = ProviderCommand.run(
            &mut ctx,
            "edit allm chat https://example.test/v1 env:ALLM_API_KEY",
        );
        let CommandResult::Action(Action::RuntimeExtension { method, params }) = result else {
            panic!("provider edit must use the live runtime service");
        };
        assert_eq!(method, "_atelier/provider/update");
        assert_eq!(params["preserveExisting"], true);
    }

    #[test]
    fn provider_edit_without_credential_preserves_existing_credential() {
        let mut ctx = empty_ctx();
        let result = ProviderCommand.run(&mut ctx, "edit allm chat https://example.test/v1");
        let CommandResult::Action(Action::RuntimeExtension { method, params }) = result else {
            panic!("provider edit must use the live runtime service");
        };
        assert_eq!(method, "_atelier/provider/update");
        assert_eq!(params["preserveExistingCredential"], true);
    }

    #[test]
    fn provider_edit_with_explicit_none_clears_existing_credential() {
        let mut ctx = empty_ctx();
        let result = ProviderCommand.run(&mut ctx, "edit allm chat https://example.test/v1 none");
        let CommandResult::Action(Action::RuntimeExtension { method, params }) = result else {
            panic!("provider edit must use the live runtime service");
        };
        assert_eq!(method, "_atelier/provider/update");
        assert_eq!(params["preserveExistingCredential"], false);
    }

    #[test]
    fn provider_add_and_edit_submit_oauth_credentials_to_the_runtime() {
        let mut ctx = empty_ctx();
        let cases = [
            (
                "add company responses https://api.example.test/v1 oauth authorization-code desktop-client https://login.example.test/authorize https://login.example.test/token openid,profile",
                "_atelier/provider/create",
                "authorization-code",
            ),
            (
                "edit company chat https://api.example.test/v1 oauth device-code desktop-client https://login.example.test/device https://login.example.test/token",
                "_atelier/provider/update",
                "device-code",
            ),
        ];
        for (args, expected_method, expected_flow) in cases {
            let CommandResult::Action(Action::RuntimeExtension { method, params }) =
                ProviderCommand.run(&mut ctx, args)
            else {
                panic!("OAuth Provider command must use the live runtime service");
            };
            assert_eq!(method, expected_method);
            assert_eq!(params["provider"]["credential"]["type"], "o_auth");
            assert_eq!(
                params["provider"]["credential"]["methods"][0]["flow"],
                expected_flow
            );
        }
    }

    #[test]
    fn provider_test_uses_runtime_probe() {
        let mut ctx = empty_ctx();
        let result = ProviderCommand.run(&mut ctx, "test allm");
        assert!(matches!(
            result,
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/provider/test" && params["providerId"] == "allm"
        ));
    }

    #[test]
    fn provider_login_supports_explicit_and_automatic_flow_selection() {
        let mut ctx = empty_ctx();
        for (args, expected_flow) in [
            ("login allm", serde_json::Value::Null),
            (
                "login allm authorization-code",
                serde_json::Value::String("authorization-code".into()),
            ),
            (
                "login allm device-code",
                serde_json::Value::String("device-code".into()),
            ),
        ] {
            let CommandResult::Action(Action::RuntimeExtension { method, params }) =
                ProviderCommand.run(&mut ctx, args)
            else {
                panic!("provider login must use the runtime OAuth extension");
            };
            assert_eq!(method, "_atelier/provider/oauth_begin");
            assert_eq!(params["providerId"], "allm");
            assert_eq!(params["flow"], expected_flow);
        }
    }

    #[test]
    fn provider_logout_uses_runtime_oauth_extension() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            ProviderCommand.run(&mut ctx, "logout allm"),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/provider/oauth_logout" && params["providerId"] == "allm"
        ));
    }

    #[test]
    fn provider_picker_exposes_login_and_logout() {
        let models = crate::acp::model_state::ModelState::default();
        let app_ctx = crate::slash::command::AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        let items = ProviderCommand
            .suggest_args(&app_ctx, "")
            .expect("provider root picker");
        assert!(items.iter().any(|item| item.insert_text == "login "));
        assert!(items.iter().any(|item| item.insert_text == "logout "));
    }

    #[test]
    fn provider_login_picker_exposes_each_configured_oauth_flow() {
        let credential = CredentialRef::OAuth {
            provider_id: "allm".into(),
            methods: vec![
                ProviderOAuthMethod::authorization_code(
                    "desktop-client",
                    Url::parse("https://login.example/authorize").unwrap(),
                    Url::parse("https://login.example/token").unwrap(),
                ),
                ProviderOAuthMethod::device_code(
                    "desktop-client",
                    Url::parse("https://login.example/device").unwrap(),
                    Url::parse("https://login.example/token").unwrap(),
                ),
            ],
        };
        let items = provider_oauth_flow_items("allm", &credential).unwrap();
        assert_eq!(items[0].insert_text, "login allm authorization-code");
        assert_eq!(items[1].insert_text, "login allm device-code");
    }

    #[test]
    fn provider_commands_reject_trailing_arguments() {
        let mut ctx = empty_ctx();
        for args in [
            "list extra",
            "enable allm extra",
            "disable allm extra",
            "delete allm extra",
            "test allm extra",
            "refresh allm extra",
        ] {
            assert!(
                matches!(ProviderCommand.run(&mut ctx, args), CommandResult::Error(error) if error.starts_with("Usage:")),
                "{args} must fail with Usage"
            );
        }
    }

    #[test]
    fn provider_add_and_edit_report_usage_when_required_fields_are_missing() {
        let mut ctx = empty_ctx();
        for args in ["add allm", "add allm chat", "edit allm", "edit allm chat"] {
            assert!(
                matches!(ProviderCommand.run(&mut ctx, args), CommandResult::Error(error) if error.starts_with("Usage:")),
                "{args} must fail with Usage"
            );
        }
    }

    #[test]
    fn edit_provider_id_completion_keeps_free_form_args() {
        assert_eq!(
            super::provider_id_insert_text("edit", "proxy"),
            "edit proxy "
        );
        assert_eq!(
            super::provider_id_insert_text("refresh", "proxy"),
            "refresh proxy"
        );
        assert_eq!(
            super::provider_id_insert_text("login", "proxy"),
            "login proxy "
        );
    }

    #[test]
    fn provider_add_and_edit_picker_handoff_to_free_form_fields() {
        let models = crate::acp::model_state::ModelState::default();
        let app_ctx = crate::slash::command::AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };

        let add_ids = ProviderCommand
            .suggest_args(&app_ctx, "add ")
            .expect("provider add must keep the staged picker open");
        assert!(add_ids.iter().any(|item| item.insert_text == "add allm "));

        let add_protocols = ProviderCommand
            .suggest_args(&app_ctx, "add allm ")
            .expect("provider add must suggest protocols after the id");
        assert!(
            add_protocols
                .iter()
                .any(|item| item.insert_text == "add allm chat ")
        );

        let add_urls = ProviderCommand
            .suggest_args(&app_ctx, "add allm chat ")
            .expect("provider add must suggest base URLs after the protocol");
        assert!(
            add_urls
                .iter()
                .any(|item| item.insert_text.ends_with("/v1 "))
        );

        let add_credentials = ProviderCommand
            .suggest_args(&app_ctx, "add allm chat https://api.example.test/v1 ")
            .expect("provider add must suggest credential types after the base URL");
        assert!(
            add_credentials
                .iter()
                .any(|item| item.insert_text.ends_with(" env:ALLM_API_KEY"))
        );
        assert!(
            add_credentials
                .iter()
                .any(|item| { item.insert_text.ends_with(" oauth authorization-code ") })
        );
        assert!(
            add_credentials
                .iter()
                .any(|item| { item.insert_text.ends_with(" oauth device-code ") })
        );

        let edit_protocols = ProviderCommand
            .suggest_args(&app_ctx, "edit proxy ")
            .expect("provider edit must continue with protocol suggestions");
        assert!(
            edit_protocols
                .iter()
                .any(|item| item.insert_text == "edit proxy responses ")
        );
    }

    #[test]
    fn provider_oauth_picker_inserts_editable_complete_templates() {
        let models = crate::acp::model_state::ModelState::default();
        let app_ctx = crate::slash::command::AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };

        let authorization_code = ProviderCommand
            .suggest_args(
                &app_ctx,
                "add allm chat https://api.example.test/v1 oauth authorization-code ",
            )
            .expect("authorization-code must expose an editable command template");
        assert_eq!(authorization_code.len(), 1);
        assert_eq!(
            authorization_code[0].insert_text,
            "add allm chat https://api.example.test/v1 oauth authorization-code CLIENT_ID AUTHORIZATION_ENDPOINT TOKEN_ENDPOINT"
        );

        let device_code = ProviderCommand
            .suggest_args(
                &app_ctx,
                "edit allm responses https://api.example.test/v1 oauth device-code ",
            )
            .expect("device-code must expose an editable command template");
        assert_eq!(device_code.len(), 1);
        assert_eq!(
            device_code[0].insert_text,
            "edit allm responses https://api.example.test/v1 oauth device-code CLIENT_ID DEVICE_AUTHORIZATION_ENDPOINT TOKEN_ENDPOINT"
        );
    }

    #[test]
    fn provider_spec_parses_protocol_and_environment_credential() {
        let config = parse_provider_spec(
            "local",
            "responses https://example.test/v1 env:LOCAL_API_KEY",
        )
        .expect("valid provider spec");
        assert_eq!(config.protocol, ProviderProtocol::OpenAiResponses);
        assert_eq!(
            config.credential,
            CredentialRef::Environment {
                variable: "LOCAL_API_KEY".into()
            }
        );
    }

    #[test]
    fn provider_spec_parses_authorization_code_oauth_with_optional_scopes() {
        let config = parse_provider_spec(
            "oauth-provider",
            "responses https://api.example.test/v1 oauth authorization-code desktop-client https://login.example.test/authorize https://login.example.test/token openid,profile,offline_access",
        )
        .expect("valid authorization-code Provider spec");
        let CredentialRef::OAuth {
            provider_id,
            methods,
        } = config.credential
        else {
            panic!("expected OAuth credential");
        };
        assert_eq!(provider_id, "oauth-provider");
        assert_eq!(methods.len(), 1);
        let ProviderOAuthMethod::AuthorizationCode {
            client_id,
            authorization_endpoint,
            token_endpoint,
            scopes,
            ..
        } = &methods[0]
        else {
            panic!("expected authorization-code method");
        };
        assert_eq!(client_id, "desktop-client");
        assert_eq!(
            authorization_endpoint.as_str(),
            "https://login.example.test/authorize"
        );
        assert_eq!(token_endpoint.as_str(), "https://login.example.test/token");
        assert_eq!(
            scopes.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["openid", "profile", "offline_access"]
        );
    }

    #[test]
    fn provider_spec_parses_device_code_oauth_without_scopes() {
        let config = parse_provider_spec(
            "oauth-provider",
            "chat https://api.example.test/v1 oauth device-code desktop-client https://login.example.test/device https://login.example.test/token",
        )
        .expect("valid device-code Provider spec");
        let CredentialRef::OAuth { methods, .. } = config.credential else {
            panic!("expected OAuth credential");
        };
        let ProviderOAuthMethod::DeviceCode {
            client_id,
            device_authorization_endpoint,
            token_endpoint,
            scopes,
            ..
        } = &methods[0]
        else {
            panic!("expected device-code method");
        };
        assert_eq!(client_id, "desktop-client");
        assert_eq!(
            device_authorization_endpoint.as_str(),
            "https://login.example.test/device"
        );
        assert_eq!(token_endpoint.as_str(), "https://login.example.test/token");
        assert!(scopes.is_empty());
    }

    #[test]
    fn provider_spec_rejects_unknown_protocol() {
        assert!(parse_provider_spec("local", "grpc https://example.test none").is_err());
    }
}
