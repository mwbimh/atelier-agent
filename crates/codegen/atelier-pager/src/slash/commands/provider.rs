//! `/provider` local Provider registry management.

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
        "/provider [list|add|edit|enable|disable|test|refresh|delete] [id] ..."
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn suggest_args(&self, _ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let trailing_space = args_query.chars().last().is_some_and(char::is_whitespace);
        let query = args_query.trim_end();
        let mut parts = query.split_whitespace();
        let command = parts.next();
        let has_argument = parts.next().is_some();

        if command == Some("edit") && has_argument && trailing_space {
            return None;
        }

        if command.is_some() && !has_argument {
            match command.unwrap_or_default() {
                "edit" | "enable" | "disable" | "test" | "delete" | "refresh" => {
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
                "add" => return None,
                _ => {}
            }
        }

        Some(
            [
                "list", "add ", "edit ", "enable ", "disable ", "test ", "refresh ", "delete ",
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
            _ => CommandResult::Error(format!("Usage: {}", self.usage())),
        }
    }
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
    if command == "edit" {
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
    let credential = match parts.next() {
        None | Some("none") => CredentialRef::None,
        Some(value) if value.strip_prefix("env:").is_some() => CredentialRef::Environment {
            variable: value.strip_prefix("env:").unwrap_or_default().to_owned(),
        },
        Some(value) if value.strip_prefix("cmd:").is_some() => CredentialRef::Command {
            program: value.strip_prefix("cmd:").unwrap_or_default().to_owned(),
            args: Vec::new(),
        },
        Some(value) => {
            return Err(format!(
                "Unknown credential spec '{value}'. Use env:NAME, cmd:PROGRAM, or none."
            ));
        }
    };
    if parts.next().is_some() {
        return Err("Provider spec has too many arguments".to_owned());
    }
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

#[cfg(test)]
mod tests {
    use super::{ProviderCommand, parse_provider_spec};
    use crate::app::actions::Action;
    use crate::slash::command::SlashCommand;
    use crate::slash::command::{CommandExecCtx, CommandResult};
    use atelier_provider::{CredentialRef, ProviderProtocol};

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

        assert!(ProviderCommand.suggest_args(&app_ctx, "add ").is_none());
        assert!(
            ProviderCommand
                .suggest_args(&app_ctx, "edit proxy ")
                .is_none(),
            "after choosing a Provider, edit must return the partial command to the composer"
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
    fn provider_spec_rejects_unknown_protocol() {
        assert!(parse_provider_spec("local", "grpc https://example.test none").is_err());
    }
}
