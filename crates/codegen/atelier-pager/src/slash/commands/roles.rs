//! `/roles` fixed runtime-role management.

use atelier_provider::{ProviderRegistry, RoleConfig, RoleId};
use serde_json::Value;
use std::str::FromStr;

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

pub struct RolesCommand;

impl SlashCommand for RolesCommand {
    fn name(&self) -> &str {
        "roles"
    }

    fn aliases(&self) -> &[&str] {
        &["role"]
    }

    fn description(&self) -> &str {
        "Manage the eight fixed runtime Roles"
    }

    fn usage(&self) -> &str {
        "/roles [list|get|set|payload|test] [role] ..."
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn suggest_args(&self, _ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let query = args_query.trim_end();
        let mut parts = query.split_whitespace();
        let command = parts.next();
        let has_role = parts.next().is_some();
        if command.is_some() && !has_role {
            match command.unwrap_or_default() {
                "get" | "set" | "payload" | "test" => {
                    let prefix = command.unwrap_or_default();
                    return Some(
                        RoleId::ALL
                            .into_iter()
                            .map(|role| ArgItem {
                                display: role.to_string(),
                                match_text: role.to_string(),
                                insert_text: if matches!(prefix, "set" | "payload") {
                                    format!("{prefix} {role} ")
                                } else {
                                    format!("{prefix} {role}")
                                },
                                description: "fixed runtime Role".into(),
                            })
                            .collect(),
                    );
                }
                _ => {}
            }
        }
        if has_role && matches!(command, Some("set" | "payload")) {
            // These commands have a free-form provider/model or JSON tail.
            // Stop the picker after the role id and return the partial command
            // to the composer for the remaining fields.
            return None;
        }

        Some(
            ["list", "get ", "set ", "payload ", "test "]
                .into_iter()
                .map(|command| ArgItem {
                    display: command.trim().into(),
                    match_text: command.trim().into(),
                    insert_text: command.into(),
                    description: "fixed runtime Role".into(),
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
        let path = atelier_config::atelier_home().join("providers.toml");
        let mut registry = match ProviderRegistry::load_or_create(path) {
            Ok(registry) => registry,
            Err(error) => return CommandResult::Error(error.to_string()),
        };

        match command {
            "list" => CommandResult::Message(format_roles(&registry)),
            "get" => {
                let Some(role_name) = parts.next() else {
                    return CommandResult::Error("Usage: /roles get <role>".into());
                };
                let role_id = match parse_role_id(role_name) {
                    Ok(role_id) => role_id,
                    Err(error) => return CommandResult::Error(error),
                };
                match registry.role(role_id) {
                    Some(config) => CommandResult::Message(format_role(role_id, config)),
                    None => CommandResult::Error(format!("Role is not configured: {role_id}")),
                }
            }
            "set" => {
                let role_name = parts.next().unwrap_or_default();
                let fields: Vec<_> = parts.collect();
                let role_id = match parse_role_id(role_name) {
                    Ok(role_id) => role_id,
                    Err(error) => return CommandResult::Error(error),
                };
                let mut config = match parse_role_set(role_name, &fields) {
                    Ok(config) => config,
                    Err(error) => return CommandResult::Error(error),
                };
                if let Some(existing) = registry.role(role_id) {
                    config.payload = existing.payload.clone();
                }
                if let Err(error) = registry.update_role(role_id, config) {
                    return CommandResult::Error(error.to_string());
                }
                if let Err(error) = registry.save() {
                    return CommandResult::Error(error.to_string());
                }
                CommandResult::Message(format_roles(&registry))
            }
            "payload" => {
                let role_name = parts.next().unwrap_or_default();
                let payload = parts.collect::<Vec<_>>().join(" ");
                let role_id = match parse_role_id(role_name) {
                    Ok(role_id) => role_id,
                    Err(error) => return CommandResult::Error(error),
                };
                let Some(mut config) = registry.role(role_id).cloned() else {
                    return CommandResult::Error(format!("Role is not configured: {role_id}"));
                };
                if let Err(error) = set_role_payload(&mut config, &payload) {
                    return CommandResult::Error(error);
                }
                if let Err(error) = registry.update_role(role_id, config) {
                    return CommandResult::Error(error.to_string());
                }
                if let Err(error) = registry.save() {
                    return CommandResult::Error(error.to_string());
                }
                CommandResult::Message(
                    registry
                        .role(role_id)
                        .map(|config| format_role(role_id, config))
                        .unwrap_or_else(|| format!("{role_id}: not configured\n")),
                )
            }
            "test" => {
                let role_name = parts.next().unwrap_or_default();
                let role_id = match parse_role_id(role_name) {
                    Ok(role_id) => role_id,
                    Err(error) => return CommandResult::Error(error),
                };
                CommandResult::Message(test_role(&registry, role_id))
            }
            _ => CommandResult::Error(format!("Usage: {}", self.usage())),
        }
    }
}

fn parse_role_id(value: &str) -> Result<RoleId, String> {
    RoleId::from_str(value).map_err(|error| error.to_string())
}

fn parse_role_set(_role_name: &str, fields: &[&str]) -> Result<RoleConfig, String> {
    if fields.len() < 2 || fields.len() > 4 {
        return Err("Usage: /roles set <role> <provider> <model> [effort] [fast_mode]".to_owned());
    }
    let effort = fields.get(2).and_then(|value| match *value {
        "-" | "none" => None,
        value => Some(value.to_owned()),
    });
    let fast_mode = match fields.get(3) {
        Some(value) => value
            .parse::<bool>()
            .map_err(|_| "fast_mode must be true or false".to_owned())?,
        None => false,
    };
    let mut config = RoleConfig::new(fields[0], fields[1]).map_err(|error| error.to_string())?;
    config.effort = effort;
    config.fast_mode = fast_mode;
    Ok(config)
}

fn set_role_payload(config: &mut RoleConfig, raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        return Err("Usage: /roles payload <role> <json-object>".to_owned());
    }
    let value: Value =
        serde_json::from_str(raw).map_err(|error| format!("invalid JSON payload: {error}"))?;
    config.payload = value
        .as_object()
        .cloned()
        .ok_or_else(|| "Role payload must be a JSON object".to_owned())?;
    Ok(())
}

fn format_roles(registry: &ProviderRegistry) -> String {
    let mut output = String::from("Roles:\n");
    for role_id in RoleId::ALL {
        if let Some(config) = registry.role(role_id) {
            output.push_str("  ");
            output.push_str(&format_role(role_id, config));
        } else {
            output.push_str(&format!("  {role_id}: (unset)\n"));
        }
    }
    output
}

fn format_role(role_id: RoleId, config: &RoleConfig) -> String {
    let payload_keys = config
        .payload
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{role_id}: provider={} model={} effort={} fast_mode={} payload_keys=[{}]\n",
        config.provider,
        config.model,
        config.effort.as_deref().unwrap_or("-"),
        config.fast_mode,
        payload_keys,
    )
}

fn test_role(registry: &ProviderRegistry, role_id: RoleId) -> String {
    let Some(config) = registry.role(role_id) else {
        return format!("{role_id}: not configured");
    };
    if config.provider == "default" || config.model == "default" {
        return format!("{role_id}: not configured");
    }
    let provider_exists = registry.provider(&config.provider).is_some();
    let model_exists = atelier_provider::ModelKey::new(&config.provider, &config.model)
        .ok()
        .and_then(|key| registry.model(&key))
        .is_some();
    let credential_available = registry
        .provider(&config.provider)
        .is_some_and(|provider| provider.credential.is_available());
    if !provider_exists {
        return format!(
            "{role_id}: provider '{}' is not configured",
            config.provider
        );
    }
    if !model_exists {
        return format!(
            "{role_id}: model '{}/{}' is not in the local catalog",
            config.provider, config.model
        );
    }
    if !credential_available {
        return format!("{role_id}: provider credential is not available");
    }
    format!("{role_id}: ready")
}

#[cfg(test)]
mod tests {
    use super::{format_role, parse_role_id, parse_role_set, set_role_payload};
    use crate::app::actions::Action;
    use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
    use atelier_provider::{RoleConfig, RoleId};
    use serde_json::json;

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
    fn empty_roles_command_opens_interactive_picker() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            super::RolesCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::OpenSlashArgPicker { command }) if command == "roles"
        ));
    }

    #[test]
    fn roles_list_remains_a_complete_command() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            super::RolesCommand.run(&mut ctx, "list"),
            CommandResult::Message(message) if message.starts_with("Roles:")
        ));
    }

    #[test]
    fn set_role_completion_leaves_free_form_args_in_the_composer() {
        let models = crate::acp::model_state::ModelState::default();
        let ctx = crate::slash::command::AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        assert!(
            super::RolesCommand
                .suggest_args(&ctx, "set main ")
                .is_none()
        );
        assert!(
            super::RolesCommand
                .suggest_args(&ctx, "payload main ")
                .is_none()
        );
    }

    #[test]
    fn role_parser_accepts_only_the_fixed_role_names() {
        assert_eq!(parse_role_id("main").unwrap(), RoleId::Main);
        assert_eq!(parse_role_id("title").unwrap(), RoleId::Title);
        assert!(parse_role_id("custom").is_err());
    }

    #[test]
    fn role_set_parser_keeps_the_small_common_parameter_set() {
        let config = parse_role_set("main", &["proxy", "coding-model", "high", "true"])
            .expect("valid role configuration");

        assert_eq!(config.provider, "proxy");
        assert_eq!(config.model, "coding-model");
        assert_eq!(config.effort.as_deref(), Some("high"));
        assert!(config.fast_mode);
    }

    #[test]
    fn payload_editor_replaces_json_payload_and_display_redacts_values() {
        let mut config = RoleConfig::new("proxy", "coding-model").unwrap();
        set_role_payload(
            &mut config,
            r#"{"temperature":0.2,"api_key":"role-secret"}"#,
        )
        .expect("valid JSON payload");

        let display = format_role(RoleId::Main, &config);
        assert!(display.contains("temperature"));
        assert!(!display.contains("role-secret"));
        assert_eq!(config.payload["temperature"], json!(0.2));
    }
}
