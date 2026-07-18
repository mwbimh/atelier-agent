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
        "/provider [list|add|edit|enable|disable|refresh|delete] [id] ..."
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn suggest_args(&self, _ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let query = args_query.trim_end();
        let mut parts = query.split_whitespace();
        let command = parts.next();
        let has_argument = parts.next().is_some();

        if command.is_some() && !has_argument {
            match command.unwrap_or_default() {
                "edit" | "enable" | "disable" | "delete" | "refresh" => {
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
                "list", "add ", "edit ", "enable ", "disable ", "refresh ", "delete ",
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

        let path = atelier_config::atelier_home().join("providers.toml");
        let mut registry = match ProviderRegistry::load_or_create(path) {
            Ok(registry) => registry,
            Err(error) => return CommandResult::Error(error.to_string()),
        };

        match command {
            "list" => CommandResult::Message(format_snapshot(&registry)),
            "add" | "edit" => {
                let spec = parts.collect::<Vec<_>>().join(" ");
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error(format!(
                        "Usage: /provider {command} <id> <protocol> <base-url> [credential]"
                    ));
                };
                if command == "edit" && registry.provider(provider_id).is_none() {
                    return CommandResult::Error(format!("Provider not found: {provider_id}"));
                }
                let config = match parse_provider_spec(provider_id, &spec) {
                    Ok(config) => config,
                    Err(error) => return CommandResult::Error(error),
                };
                if let Err(error) = registry.upsert_provider(config) {
                    return CommandResult::Error(error.to_string());
                }
                if let Err(error) = registry.save() {
                    return CommandResult::Error(error.to_string());
                }
                CommandResult::Message(format_snapshot(&registry))
            }
            "enable" | "disable" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error(format!("Usage: /provider {command} <id>"));
                };
                if let Err(error) = registry.set_provider_enabled(provider_id, command == "enable")
                {
                    return CommandResult::Error(error.to_string());
                }
                if let Err(error) = registry.save() {
                    return CommandResult::Error(error.to_string());
                }
                CommandResult::Message(format_snapshot(&registry))
            }
            "delete" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error("Usage: /provider delete <id>".into());
                };
                if let Err(error) = registry.remove_provider(provider_id) {
                    return CommandResult::Error(error.to_string());
                }
                if let Err(error) = registry.save() {
                    return CommandResult::Error(error.to_string());
                }
                CommandResult::Message(format_snapshot(&registry))
            }
            "refresh" => {
                let Some(provider_id) = provider_id else {
                    return CommandResult::Error("Usage: /provider refresh <id>".into());
                };
                if registry.provider(provider_id).is_none() {
                    return CommandResult::Error(format!("Provider not found: {provider_id}"));
                }
                CommandResult::Action(Action::RefreshProviderModels(provider_id.to_owned()))
            }
            _ => CommandResult::Error(format!("Usage: {}", self.usage())),
        }
    }
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

fn format_snapshot(registry: &ProviderRegistry) -> String {
    let snapshot = registry.snapshot();
    let mut output = String::from("Providers:\n");
    if snapshot.providers.is_empty() {
        output.push_str("  (none)\n");
    } else {
        for provider in snapshot.providers {
            output.push_str(&format!(
                "  {} [{}] {}\n",
                provider.id,
                if provider.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                provider.display_name
            ));
        }
    }
    output.push_str(&format!("Models: {}\n", snapshot.models.len()));
    if let Some(default_model) = snapshot.default_model {
        output.push_str(&format!("Default: {default_model}"));
    } else {
        output.push_str("Default: (none)");
    }
    output
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
            CommandResult::Message(message) if message.starts_with("Providers:")
        ));
    }

    #[test]
    fn provider_refresh_is_a_complete_command() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            ProviderCommand.run(&mut ctx, "refresh missing-provider"),
            CommandResult::Error(message) if message == "Provider not found: missing-provider"
        ));
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
