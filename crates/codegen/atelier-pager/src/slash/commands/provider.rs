//! `/provider` local Provider registry management.

use atelier_provider::ProviderRegistry;

use crate::app::actions::Action;
use crate::slash::command::{
    AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand, current_arg_fragment,
};

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
        "/provider <list|add|delete|refresh> [id]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<list|add|delete|refresh> [id]")
    }

    fn suggest_args(&self, _ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let trailing_space = args_query.chars().last().is_some_and(char::is_whitespace);
        let tokens = args_query.split_whitespace().collect::<Vec<_>>();
        let Some(command) = tokens.first().copied() else {
            return Some(root_items());
        };
        if tokens.len() == 1 && !trailing_space {
            return Some(root_items());
        }
        if !matches!(command, "delete" | "refresh") {
            return None;
        }
        if tokens.len() > 2 || (tokens.len() == 2 && trailing_space) {
            return Some(Vec::new());
        }
        let registry =
            ProviderRegistry::load_or_create(atelier_config::atelier_home().join("providers.toml"))
                .ok()?;
        Some(provider_items(command, &registry))
    }

    fn arg_suggestion_filter_query<'a>(&self, args_query: &'a str) -> &'a str {
        current_arg_fragment(args_query)
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if args.trim().is_empty() {
            return CommandResult::Action(Action::OpenSlashArgPicker {
                command: self.name().to_owned(),
            });
        }
        let tokens = args.split_whitespace().collect::<Vec<_>>();
        match tokens.as_slice() {
            ["list"] => runtime_extension("_atelier/provider/list"),
            ["add"] => CommandResult::Action(Action::OpenProviderWizard),
            ["delete", provider_id] => CommandResult::Action(Action::OpenDestructiveConfirm {
                action: crate::views::modal::DestructiveAction::DeleteProvider {
                    provider_id: (*provider_id).to_owned(),
                },
            }),
            ["refresh", provider_id] => {
                CommandResult::Action(Action::RefreshProviderModels((*provider_id).to_owned()))
            }
            _ => CommandResult::Error(format!("Usage: {}", self.usage())),
        }
    }
}

fn runtime_extension(method: &str) -> CommandResult {
    CommandResult::Action(Action::RuntimeExtension {
        method: method.to_owned(),
        params: serde_json::json!({}),
    })
}

fn root_items() -> Vec<ArgItem> {
    [
        ("list", "List configured Providers"),
        ("add", "Open the Provider setup wizard"),
        ("delete", "Delete a configured Provider"),
        ("refresh", "Refresh a Provider model catalog"),
    ]
    .into_iter()
    .map(|(command, description)| ArgItem {
        display: command.to_owned(),
        match_text: command.to_owned(),
        insert_text: if matches!(command, "delete" | "refresh") {
            format!("{command} ")
        } else {
            command.to_owned()
        },
        description: description.to_owned(),
    })
    .collect()
}

fn provider_items(command: &str, registry: &ProviderRegistry) -> Vec<ArgItem> {
    registry
        .providers()
        .map(|provider| ArgItem {
            display: provider.id.clone(),
            match_text: format!("{} {}", provider.id, provider.display_name),
            insert_text: format!("{command} {}", provider.id),
            description: match command {
                "delete" => "Delete Provider".to_owned(),
                "refresh" => "Refresh model catalog".to_owned(),
                _ => unreachable!(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ProviderCommand, provider_items, root_items};
    use crate::app::actions::Action;
    use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
    use atelier_provider::{CredentialRef, ProviderAuth, ProviderConfig, ProviderRegistry};
    use std::collections::BTreeMap;
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

    fn registry() -> ProviderRegistry {
        let mut registry = ProviderRegistry::in_memory();
        registry
            .upsert_provider(ProviderConfig {
                id: "example".to_owned(),
                display_name: "Example Provider".to_owned(),
                auth: ProviderAuth::None,
                base_url: Url::parse("https://example.test/v1").unwrap(),
                credential: CredentialRef::None,
                discovery: atelier_provider::ProviderDiscovery::Disabled,
                extra_headers: BTreeMap::new(),
                enabled: true,
            })
            .unwrap();
        registry
    }

    #[test]
    fn command_metadata_is_provider_specific() {
        let command = ProviderCommand;
        assert_eq!(command.name(), "provider");
        assert_eq!(command.usage(), "/provider <list|add|delete|refresh> [id]");
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
    fn provider_picker_offers_only_list_add_delete_and_refresh() {
        let items = root_items();
        assert_eq!(
            items
                .iter()
                .map(|item| item.display.as_str())
                .collect::<Vec<_>>(),
            vec!["list", "add", "delete", "refresh"]
        );
        assert_eq!(items[0].insert_text, "list");
        assert_eq!(items[1].insert_text, "add");
        assert_eq!(items[2].insert_text, "delete ");
        assert_eq!(items[3].insert_text, "refresh ");
    }

    #[test]
    fn list_and_add_are_complete_commands() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            ProviderCommand.run(&mut ctx, "list"),
            CommandResult::Action(Action::RuntimeExtension { method, .. })
                if method == "_atelier/provider/list"
        ));
        assert!(matches!(
            ProviderCommand.run(&mut ctx, "add"),
            CommandResult::Action(Action::OpenProviderWizard)
        ));
    }

    #[test]
    fn delete_and_refresh_target_one_provider() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            ProviderCommand.run(&mut ctx, "delete example"),
            CommandResult::Action(Action::OpenDestructiveConfirm { action })
                if action == crate::views::modal::DestructiveAction::DeleteProvider {
                    provider_id: "example".to_owned(),
                }
        ));
        assert!(matches!(
            ProviderCommand.run(&mut ctx, "refresh example"),
            CommandResult::Action(Action::RefreshProviderModels(provider_id))
                if provider_id == "example"
        ));
    }

    #[test]
    fn delete_and_refresh_have_provider_pickers() {
        let registry = registry();
        for command in ["delete", "refresh"] {
            let items = provider_items(command, &registry);
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].display, "example");
            assert_eq!(items[0].insert_text, format!("{command} example"));
        }
    }

    #[test]
    fn removed_and_malformed_commands_are_rejected() {
        let mut ctx = empty_ctx();
        for args in [
            "edit example",
            "enable example",
            "disable example",
            "test example",
            "login example",
            "logout example",
            "add example",
            "list extra",
            "delete",
            "delete example confirm",
            "refresh",
            "refresh example extra",
        ] {
            assert!(
                matches!(ProviderCommand.run(&mut ctx, args), CommandResult::Error(error) if error.starts_with("Usage:")),
                "{args} must fail with Usage"
            );
        }
    }
}
