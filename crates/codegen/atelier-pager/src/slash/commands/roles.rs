//! `/roles` fixed runtime-role management.

use atelier_provider::{RoleConfig, RoleId};
use serde_json::Value;
use std::str::FromStr;

use crate::app::actions::Action;
use crate::slash::command::{
    AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand, current_arg_fragment,
};

pub struct RolesCommand;

impl SlashCommand for RolesCommand {
    fn name(&self) -> &str {
        "roles"
    }

    fn aliases(&self) -> &[&str] {
        &["role"]
    }

    fn description(&self) -> &str {
        "Manage the fixed runtime Roles"
    }

    fn usage(&self) -> &str {
        "/roles [list|get|set|payload|test] [role] ..."
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        // Exact `/roles` is executable and opens the dedicated manager. Do not
        // let the generic argument picker intercept Enter before `run()` sees
        // the empty argument string. Typing a space still offers subcommands.
        if args_query.is_empty() {
            return None;
        }
        let trailing_space = args_query.chars().last().is_some_and(char::is_whitespace);
        let tokens = args_query.split_whitespace().collect::<Vec<_>>();
        let subcommands = || {
            ["list", "get ", "set ", "reset ", "payload ", "test "]
                .into_iter()
                .map(|command| ArgItem {
                    display: command.trim().into(),
                    match_text: command.trim().into(),
                    insert_text: command.into(),
                    description: "fixed runtime Role".into(),
                })
                .collect::<Vec<_>>()
        };
        let Some(command) = tokens.first().copied() else {
            return Some(subcommands());
        };
        if tokens.len() == 1 && !trailing_space {
            return Some(subcommands());
        }
        if !["get", "set", "reset", "payload", "test"].contains(&command) {
            return (command != "list").then(subcommands);
        }
        if tokens.len() == 1 {
            return Some(
                RoleId::ALL
                    .into_iter()
                    .map(|role| ArgItem {
                        display: role.to_string(),
                        match_text: role.to_string(),
                        insert_text: if matches!(command, "set" | "payload") {
                            format!("{command} {role} ")
                        } else {
                            format!("{command} {role}")
                        },
                        description: "fixed runtime Role".into(),
                    })
                    .collect(),
            );
        }
        let role = tokens[1];
        if command == "set" && tokens.len() == 2 && trailing_space {
            return Some(
                ctx.models
                    .available
                    .iter()
                    .map(|(id, info)| ArgItem {
                        display: info.name.clone(),
                        match_text: format!("{} {}", info.name, id.0),
                        insert_text: format!("set {role} {} ", id.0),
                        description: info.description.clone().unwrap_or_default(),
                    })
                    .collect(),
            );
        }
        if command == "set" && tokens.len() == 3 && trailing_space {
            let model = tokens[2];
            let Some(model_id) = ctx.models.resolve_by_name_or_id(model) else {
                return Some(Vec::new());
            };
            let mut options = ctx.models.reasoning_effort_options_for(&model_id);
            if options.is_empty() {
                return Some(vec![ArgItem {
                    display: "model default".to_owned(),
                    match_text: "default".to_owned(),
                    insert_text: format!("set {role} {model} - "),
                    description: "No explicit reasoning effort".into(),
                }]);
            }
            return Some(
                options
                    .drain(..)
                    .map(|option| ArgItem {
                        display: option.label,
                        match_text: option.id.clone(),
                        insert_text: format!("set {role} {model} {} ", option.id),
                        description: option
                            .description
                            .unwrap_or_else(|| "reasoning effort".into()),
                    })
                    .collect(),
            );
        }
        if command == "set" && tokens.len() == 4 && trailing_space {
            let model = tokens[2];
            let effort = tokens[3];
            return Some(
                [false, true]
                    .into_iter()
                    .map(|fast_mode| ArgItem {
                        display: fast_mode.to_string(),
                        match_text: fast_mode.to_string(),
                        insert_text: format!("set {role} {model} {effort} {fast_mode}"),
                        description: "fast mode".into(),
                    })
                    .collect(),
            );
        }
        None
    }

    fn arg_suggestion_filter_query<'a>(&self, args_query: &'a str) -> &'a str {
        current_arg_fragment(args_query)
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if args.trim().is_empty() {
            return CommandResult::Action(Action::OpenRolesModal);
        }
        let mut parts = args.split_whitespace();
        let command = parts.next().unwrap_or("list");
        match command {
            "list" => {
                if parts.next().is_some() {
                    return CommandResult::Error("Usage: /roles list".into());
                }
                extension("_atelier/role/list", serde_json::json!({}))
            }
            "get" => {
                let Some(role_name) = parts.next() else {
                    return CommandResult::Error("Usage: /roles get <role>".into());
                };
                let role_id = match parse_role_id(role_name) {
                    Ok(role_id) => role_id,
                    Err(error) => return CommandResult::Error(error),
                };
                if parts.next().is_some() {
                    return CommandResult::Error("Usage: /roles get <role>".into());
                }
                extension(
                    "_atelier/role/get",
                    serde_json::json!({ "roleId": role_id }),
                )
            }
            "set" => {
                let Some(role_name) = parts.next() else {
                    return CommandResult::Error(
                        "Usage: /roles set <role> (<provider/model> [effort] [fast_mode] | <field=value>...)".into(),
                    );
                };
                let fields: Vec<_> = parts.collect();
                let role_id = match parse_role_id(role_name) {
                    Ok(role_id) => role_id,
                    Err(error) => return CommandResult::Error(error),
                };
                let config = match parse_role_set(role_name, &fields) {
                    Ok(config) => config,
                    Err(error) => return CommandResult::Error(error),
                };
                extension(
                    "_atelier/role/update",
                    serde_json::json!({
                        "roleId": role_id,
                        "config": config,
                        "preservePayload": true,
                    }),
                )
            }
            "reset" => {
                let Some(role_name) = parts.next() else {
                    return CommandResult::Error("Usage: /roles reset <role>".into());
                };
                let role_id = match parse_role_id(role_name) {
                    Ok(RoleId::Main) => {
                        return CommandResult::Error(
                            "MAIN is managed by /model and cannot be reset in roles.toml".into(),
                        );
                    }
                    Ok(role_id) => role_id,
                    Err(error) => return CommandResult::Error(error),
                };
                if parts.next().is_some() {
                    return CommandResult::Error("Usage: /roles reset <role>".into());
                }
                extension(
                    "_atelier/role/delete",
                    serde_json::json!({ "roleId": role_id }),
                )
            }
            "payload" => {
                let Some(role_name) = parts.next() else {
                    return CommandResult::Error(
                        "Usage: /roles payload <role> <json-object>".into(),
                    );
                };
                let payload = parts.collect::<Vec<_>>().join(" ");
                let role_id = match parse_role_id(role_name) {
                    Ok(role_id) => role_id,
                    Err(error) => return CommandResult::Error(error),
                };
                let payload = match parse_role_payload(&payload) {
                    Ok(payload) => payload,
                    Err(error) => return CommandResult::Error(error),
                };
                extension(
                    "_atelier/role/update_payload",
                    serde_json::json!({ "roleId": role_id, "payload": payload }),
                )
            }
            "test" => {
                let Some(role_name) = parts.next() else {
                    return CommandResult::Error("Usage: /roles test <role>".into());
                };
                let role_id = match parse_role_id(role_name) {
                    Ok(role_id) => role_id,
                    Err(error) => return CommandResult::Error(error),
                };
                if parts.next().is_some() {
                    return CommandResult::Error("Usage: /roles test <role>".into());
                }
                extension(
                    "_atelier/role/test",
                    serde_json::json!({ "roleId": role_id }),
                )
            }
            _ => CommandResult::Error(format!("Usage: {}", self.usage())),
        }
    }
}

fn extension(method: &str, params: Value) -> CommandResult {
    CommandResult::Action(Action::RuntimeExtension {
        method: method.to_owned(),
        params,
    })
}

fn parse_role_id(value: &str) -> Result<RoleId, String> {
    RoleId::from_str(value).map_err(|error| error.to_string())
}

fn parse_role_set(_role_name: &str, fields: &[&str]) -> Result<RoleConfig, String> {
    if fields.iter().any(|field| field.contains('=')) {
        return parse_sparse_role_set(fields);
    }
    let (provider, model, options) = if let Some((provider, model)) = fields
        .first()
        .and_then(|model_key| model_key.split_once('/'))
    {
        (provider, model, &fields[1..])
    } else if fields.len() >= 2 {
        (fields[0], fields[1], &fields[2..])
    } else {
        return Err(
            "Usage: /roles set <role> (<provider/model> [effort] [fast_mode] | <field=value>...)"
                .to_owned(),
        );
    };
    if options.len() > 2 {
        return Err(
            "Usage: /roles set <role> (<provider/model> [effort] [fast_mode] | <field=value>...)"
                .to_owned(),
        );
    }
    let effort = options.first().and_then(|value| match *value {
        "-" => None,
        value => Some(value.to_owned()),
    });
    let fast_mode = match options.get(1) {
        Some(value) => value
            .parse::<bool>()
            .map_err(|_| "fast_mode must be true or false".to_owned())?,
        None => false,
    };
    let mut config = RoleConfig::new(provider, model).map_err(|error| error.to_string())?;
    config.effort = effort;
    config.fast_mode = fast_mode;
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

fn parse_sparse_role_set(fields: &[&str]) -> Result<RoleConfig, String> {
    let mut object = serde_json::Map::new();
    for field in fields {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| "Sparse Role settings must use key=value syntax".to_owned())?;
        if object.contains_key(key) {
            return Err(format!("duplicate Role setting: {key}"));
        }
        match key {
            "provider" | "effort" => {
                object.insert(key.to_owned(), Value::String(value.to_owned()));
            }
            "model" => {
                if let Some((provider, model)) = value.split_once('/') {
                    if object.contains_key("provider") {
                        return Err("provider is configured more than once".to_owned());
                    }
                    object.insert("provider".into(), Value::String(provider.to_owned()));
                    object.insert("model".into(), Value::String(model.to_owned()));
                } else {
                    object.insert("model".into(), Value::String(value.to_owned()));
                }
            }
            "fast_mode" => {
                let enabled = value
                    .parse::<bool>()
                    .map_err(|_| "fast_mode must be true or false".to_owned())?;
                object.insert("fast_mode".into(), Value::Bool(enabled));
            }
            _ => return Err(format!("unknown Role setting: {key}")),
        }
    }
    serde_json::from_value(Value::Object(object)).map_err(|error| error.to_string())
}

#[cfg(test)]
fn set_role_payload(config: &mut RoleConfig, raw: &str) -> Result<(), String> {
    config.payload = parse_role_payload(raw)?;
    Ok(())
}

fn parse_role_payload(raw: &str) -> Result<serde_json::Map<String, Value>, String> {
    if raw.is_empty() {
        return Err("Usage: /roles payload <role> <json-object>".to_owned());
    }
    let value: Value =
        serde_json::from_str(raw).map_err(|error| format!("invalid JSON payload: {error}"))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "Role payload must be a JSON object".to_owned())
}

#[cfg(test)]
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
    fn empty_roles_command_opens_dedicated_role_manager() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            super::RolesCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::OpenRolesModal)
        ));
    }

    #[test]
    fn exact_roles_command_is_not_intercepted_by_argument_suggestions() {
        let models = crate::acp::model_state::ModelState::default();
        let ctx = crate::slash::command::AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        assert!(super::RolesCommand.suggest_args(&ctx, "").is_none());
        assert!(
            super::RolesCommand
                .suggest_args(&ctx, " ")
                .is_some_and(|items| items.iter().any(|item| item.display == "list"))
        );
    }

    #[test]
    fn roles_list_remains_a_complete_command() {
        let mut ctx = empty_ctx();
        assert!(matches!(
            super::RolesCommand.run(&mut ctx, "list"),
            CommandResult::Action(Action::RuntimeExtension { method, .. })
                if method == "_atelier/role/list"
        ));
    }

    #[test]
    fn role_commands_reject_trailing_arguments() {
        let mut ctx = empty_ctx();
        for args in ["list extra", "get main extra", "test main extra"] {
            assert!(
                matches!(super::RolesCommand.run(&mut ctx, args), CommandResult::Error(error) if error.starts_with("Usage:")),
                "{args} must fail with Usage"
            );
        }
    }

    #[test]
    fn role_commands_report_usage_when_required_fields_are_missing() {
        let mut ctx = empty_ctx();
        for args in ["get", "set", "reset", "payload", "test"] {
            assert!(
                matches!(super::RolesCommand.run(&mut ctx, args), CommandResult::Error(error) if error.starts_with("Usage:")),
                "{args} must fail with Usage"
            );
        }
    }

    #[test]
    fn role_reset_restores_fixed_parent_inheritance() {
        let mut ctx = empty_ctx();
        let result = super::RolesCommand.run(&mut ctx, "reset review");
        let CommandResult::Action(Action::RuntimeExtension { method, params }) = result else {
            panic!("role reset must use the live runtime service");
        };
        assert_eq!(method, "_atelier/role/delete");
        assert_eq!(params["roleId"], "review");

        assert!(
            matches!(
                super::RolesCommand.run(&mut ctx, "reset main"),
                CommandResult::Error(error) if error.contains("/model")
            ),
            "MAIN must remain config.toml-managed"
        );
    }

    #[test]
    fn role_set_uses_the_live_runtime_service() {
        let mut ctx = empty_ctx();
        let result =
            super::RolesCommand.run(&mut ctx, "set main example deepseek-v4-flash high true");
        let CommandResult::Action(Action::RuntimeExtension { method, params }) = result else {
            panic!("role set must use the live runtime service");
        };
        assert_eq!(method, "_atelier/role/update");
        assert_eq!(params["roleId"], "main");
        assert_eq!(params["config"]["provider"], "example");
        assert_eq!(params["config"]["model"], "deepseek-v4-flash");
        assert_eq!(params["preservePayload"], true);
    }

    #[test]
    fn set_role_completion_walks_role_model_effort_and_fast_mode() {
        let mut models = crate::acp::model_state::ModelState::default();
        let model_id = agent_client_protocol::ModelId::new("example/deepseek-v4-flash");
        models.available.insert(
            model_id.clone(),
            agent_client_protocol::ModelInfo::new(model_id, "DeepSeek V4 Flash").meta(
                serde_json::json!({
                    "supportsReasoningEffort": true,
                    "reasoningEfforts": [
                        { "value": "low", "label": "Low" },
                        { "value": "high", "label": "High", "default": true }
                    ]
                })
                .as_object()
                .cloned(),
            ),
        );
        let ctx = crate::slash::command::AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        let models = super::RolesCommand
            .suggest_args(&ctx, "set main ")
            .expect("model options");
        assert_eq!(models[0].insert_text, "set main example/deepseek-v4-flash ");
        let efforts = super::RolesCommand
            .suggest_args(&ctx, "set main example/deepseek-v4-flash ")
            .expect("effort options");
        assert!(efforts.iter().any(|item| item.display == "High"));
        assert!(efforts.iter().all(|item| item.match_text != "xhigh"));
        let fast_modes = super::RolesCommand
            .suggest_args(&ctx, "set main example/deepseek-v4-flash high ")
            .expect("fast mode options");
        assert!(fast_modes.iter().any(|item| item.display == "true"));
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
    fn role_set_accepts_sparse_per_field_overrides() {
        let effort = parse_role_set("review", &["effort=high"]).unwrap();
        assert_eq!(effort.provider_override(), None);
        assert_eq!(effort.model_override(), None);
        assert_eq!(effort.effort.as_deref(), Some("high"));
        assert_eq!(effort.fast_mode_override(), None);

        let fast = parse_role_set("compact", &["fast_mode=false"]).unwrap();
        assert_eq!(fast.fast_mode_override(), Some(false));
        assert_eq!(
            serde_json::to_value(fast).unwrap(),
            json!({"fast_mode": false})
        );

        let model = parse_role_set("explore", &["model=provider/child-model"]).unwrap();
        assert_eq!(model.provider_override(), Some("provider"));
        assert_eq!(model.model_override(), Some("child-model"));
        assert_eq!(model.fast_mode_override(), None);
    }

    #[test]
    fn role_set_rejects_unknown_or_duplicate_sparse_fields() {
        assert!(parse_role_set("review", &["custom=value"]).is_err());
        assert!(parse_role_set("review", &["effort=low", "effort=high"]).is_err());
        assert!(
            parse_role_set("review", &["provider=a", "model=b/c"])
                .unwrap_err()
                .contains("provider")
        );
    }

    #[test]
    fn role_set_parser_keeps_the_small_common_parameter_set() {
        let config = parse_role_set("main", &["proxy", "coding-model", "high", "true"])
            .expect("valid role configuration");

        assert_eq!(config.provider, "proxy");
        assert_eq!(config.model, "coding-model");
        assert_eq!(config.effort.as_deref(), Some("high"));
        assert!(config.fast_mode);

        let config = parse_role_set("main", &["example/deepseek-v4-flash", "medium", "false"])
            .expect("provider/model composite is accepted for interactive completion");
        assert_eq!(config.provider, "example");
        assert_eq!(config.model, "deepseek-v4-flash");
        assert_eq!(config.effort.as_deref(), Some("medium"));
        assert!(!config.fast_mode);
    }

    #[test]
    fn roles_set_rejects_unknown_reasoning_effort_before_the_rpc_call() {
        let mut ctx = empty_ctx();
        let result = super::RolesCommand.run(
            &mut ctx,
            "set main example/deepseek-v4-flash nonsense false",
        );

        assert!(
            matches!(result, CommandResult::Error(error) if error.contains("invalid role reasoning effort"))
        );
    }

    #[test]
    fn role_set_parser_distinguishes_none_effort_from_unset() {
        let explicit_none = parse_role_set("main", &["example/deepseek-v4-flash", "none"])
            .expect("none is a valid reasoning effort");
        assert_eq!(explicit_none.effort.as_deref(), Some("none"));

        let unset = parse_role_set("main", &["example/deepseek-v4-flash", "-"])
            .expect("dash leaves reasoning effort unset");
        assert_eq!(unset.effort, None);
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
