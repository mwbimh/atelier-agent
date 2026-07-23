//! `/model-config` and `/models` -- inspect and edit model Wire API settings.

use serde_json::{Map, Value, json};

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

const MODEL_LIST: &str = "_atelier/model/list";
const MODEL_GET: &str = "_atelier/model/get";
const MODEL_UPDATE_WIRE_API: &str = "_atelier/model/update_wire_api";
const OVERRIDE_SET: &str = "_atelier/model_provider_override/set";
const OVERRIDE_DELETE: &str = "_atelier/model_provider_override/delete";
const OVERRIDE_TEST: &str = "_atelier/model_provider_override/test";

pub struct ModelConfigCommand;

impl SlashCommand for ModelConfigCommand {
    fn name(&self) -> &str {
        "model-config"
    }

    fn aliases(&self) -> &[&str] {
        &["models"]
    }

    fn description(&self) -> &str {
        "Inspect and edit model Wire API configuration"
    }

    fn usage(&self) -> &str {
        "/model-config [list|get|wire|override|delete|test] ..."
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn session_scoped(&self) -> bool {
        false
    }

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let trailing_space = args_query.chars().last().is_some_and(char::is_whitespace);
        let tokens = args_query.split_whitespace().collect::<Vec<_>>();
        let subcommands = || {
            ["list", "get", "wire", "override", "delete", "test"]
                .into_iter()
                .map(|command| ArgItem {
                    display: command.to_owned(),
                    match_text: command.to_owned(),
                    insert_text: if command == "list" {
                        command.to_owned()
                    } else {
                        format!("{command} ")
                    },
                    description: "model Wire API management".to_owned(),
                })
                .collect::<Vec<_>>()
        };
        let Some(command) = tokens.first().copied() else {
            return Some(subcommands());
        };
        if tokens.len() == 1 && !trailing_space {
            return Some(subcommands());
        }
        if command == "list" {
            return None;
        }
        if !["get", "wire", "override", "delete", "test"].contains(&command) {
            return Some(subcommands());
        }
        if tokens.len() == 1 {
            let append_space = matches!(command, "wire" | "override" | "test");
            return Some(model_items(ctx, command, append_space));
        }
        if tokens.len() == 2 && !trailing_space {
            let append_space = matches!(command, "wire" | "override" | "test");
            return Some(model_items(ctx, command, append_space));
        }
        let model_key = tokens[1];
        if tokens.len() == 2 && trailing_space {
            if matches!(command, "wire" | "override") {
                return Some(
                    ["chat_completions", "responses", "messages", "default"]
                        .into_iter()
                        .map(|wire_api| ArgItem {
                            display: wire_api.to_owned(),
                            match_text: wire_api.to_owned(),
                            insert_text: format!(
                                "{command} {model_key} {wire_api}{}",
                                if command == "override" { " " } else { "" }
                            ),
                            description: "Wire API".to_owned(),
                        })
                        .collect(),
                );
            }
            if command == "test" {
                return Some(vec![
                    ArgItem {
                        display: "preview".to_owned(),
                        match_text: "preview".to_owned(),
                        insert_text: format!("test {model_key}"),
                        description: "Preview the resolved request without sending it".to_owned(),
                    },
                    ArgItem {
                        display: "execute".to_owned(),
                        match_text: "execute".to_owned(),
                        insert_text: format!("test {model_key} execute"),
                        description: "Send a real request through the runtime sampler".to_owned(),
                    },
                ]);
            }
        }
        None
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if args.trim().is_empty() {
            return CommandResult::Action(Action::OpenSlashArgPicker {
                command: self.name().to_owned(),
            });
        }
        let tokens = match crate::slash::commands::agent::tokenize(args) {
            Ok(tokens) => tokens,
            Err(error) => return CommandResult::Error(error),
        };
        let command = tokens.first().map(String::as_str).unwrap_or("list");
        match command {
            "list" => {
                if tokens.len() != 1 {
                    return CommandResult::Error("Usage: /model-config list".to_owned());
                }
                extension(MODEL_LIST, json!({}))
            }
            "get" => {
                let [_, model_key] = tokens.as_slice() else {
                    return CommandResult::Error(
                        "Usage: /model-config get <provider/model>".to_owned(),
                    );
                };
                extension(MODEL_GET, json!({ "modelKey": model_key }))
            }
            "wire" => {
                let [_, model_key, wire_api] = tokens.as_slice() else {
                    return CommandResult::Error(
                        "Usage: /model-config wire <provider/model> <wire-api|default>".to_owned(),
                    );
                };
                extension(
                    MODEL_UPDATE_WIRE_API,
                    json!({
                        "modelKey": model_key,
                        "wireApi": match parse_wire_api(wire_api) {
                            Ok(value) => value,
                            Err(error) => return CommandResult::Error(error),
                        },
                    }),
                )
            }
            "override" => {
                let mut raw_parts = args.trim().splitn(4, char::is_whitespace);
                let _ = raw_parts.next();
                let Some(model_key) = raw_parts.next() else {
                    return CommandResult::Error("Usage: /model-config override <provider/model> <wire-api|default> [json-payload]".to_owned());
                };
                let Some(wire_api) = raw_parts.next() else {
                    return CommandResult::Error("Usage: /model-config override <provider/model> <wire-api|default> [json-payload]".to_owned());
                };
                let payload = match raw_parts
                    .next()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    Some(raw) => {
                        let value = match serde_json::from_str::<Value>(raw) {
                            Ok(value) => value,
                            Err(error) => {
                                return CommandResult::Error(format!(
                                    "invalid JSON payload: {error}"
                                ));
                            }
                        };
                        match value.as_object() {
                            Some(object) => object.clone(),
                            None => {
                                return CommandResult::Error(
                                    "override payload must be a JSON object".to_owned(),
                                );
                            }
                        }
                    }
                    _ => Map::new(),
                };
                extension(
                    OVERRIDE_SET,
                    json!({
                        "modelKey": model_key,
                        "wireApi": match parse_wire_api(wire_api) {
                            Ok(value) => value,
                            Err(error) => return CommandResult::Error(error),
                        },
                        "payload": payload,
                    }),
                )
            }
            "delete" => {
                let [_, model_key] = tokens.as_slice() else {
                    return CommandResult::Error(
                        "Usage: /model-config delete <provider/model>".to_owned(),
                    );
                };
                extension(OVERRIDE_DELETE, json!({ "modelKey": model_key }))
            }
            "test" => {
                let Some(model_key) = tokens.get(1) else {
                    return CommandResult::Error(
                        "Usage: /model-config test <provider/model> [execute]".to_owned(),
                    );
                };
                if tokens.len() > 3
                    || tokens
                        .get(2)
                        .is_some_and(|value| value.as_str() != "execute")
                {
                    return CommandResult::Error(
                        "Usage: /model-config test <provider/model> [execute]".to_owned(),
                    );
                }
                extension(
                    OVERRIDE_TEST,
                    json!({ "modelKey": model_key, "execute": tokens.get(2).is_some_and(|value| value == "execute") }),
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

fn parse_wire_api(value: &str) -> Result<Option<&'static str>, String> {
    match value {
        "chat" | "chat_completions" => Ok(Some("chat_completions")),
        "responses" => Ok(Some("responses")),
        "messages" => Ok(Some("messages")),
        "default" | "none" => Ok(None),
        _ => Err(format!(
            "unknown Wire API '{value}'; use chat_completions, responses, messages, or default"
        )),
    }
}

fn model_items(ctx: &AppCtx, command: &str, append_space: bool) -> Vec<ArgItem> {
    ctx.models
        .available
        .iter()
        .map(|(id, info)| ArgItem {
            display: id.0.to_string(),
            match_text: format!("{} {}", id.0, info.name),
            insert_text: format!("{command} {}{}", id.0, if append_space { " " } else { "" }),
            description: info.name.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::ModelConfigCommand;
    use crate::acp::model_state::ModelState;
    use crate::app::actions::Action;
    use crate::slash::command::{AppCtx, CommandExecCtx, CommandResult, SlashCommand};
    use agent_client_protocol as acp;
    use std::sync::Arc;

    fn ctx() -> CommandExecCtx<'static> {
        let mut models = ModelState::default();
        let id = acp::ModelId::new(Arc::from("proxy/gpt-5"));
        models
            .available
            .insert(id.clone(), acp::ModelInfo::new(id, "GPT-5".to_owned()));
        let models = Box::leak(Box::new(models));
        let bundle = Box::leak(Box::new(crate::app::bundle::BundleState::default()));
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    fn suggested(query: &str) -> Vec<crate::slash::command::ArgItem> {
        let command_ctx = ctx();
        let app_ctx = AppCtx {
            models: command_ctx.models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        ModelConfigCommand
            .suggest_args(&app_ctx, query)
            .expect("suggestions")
    }

    fn insert_text(items: &[crate::slash::command::ArgItem], display: &str) -> String {
        items
            .iter()
            .find(|item| item.display == display)
            .unwrap_or_else(|| panic!("missing suggestion {display}"))
            .insert_text
            .clone()
    }

    #[test]
    fn bare_model_config_opens_interactive_entry() {
        let mut command_ctx = ctx();
        assert!(matches!(
            ModelConfigCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::OpenSlashArgPicker { command }) if command == "model-config"
        ));
    }

    #[test]
    fn bare_model_config_suggests_subcommands_before_models() {
        let items = suggested("");
        assert_eq!(
            items
                .iter()
                .map(|item| item.display.as_str())
                .collect::<Vec<_>>(),
            vec!["list", "get", "wire", "override", "delete", "test"]
        );
        assert_eq!(insert_text(&items, "wire"), "wire ");
    }

    #[test]
    fn wire_suggestions_chain_subcommand_model_and_wire_api() {
        let models = suggested("wire ");
        assert_eq!(insert_text(&models, "proxy/gpt-5"), "wire proxy/gpt-5 ");

        let wire_apis = suggested("wire proxy/gpt-5 ");
        assert_eq!(
            insert_text(&wire_apis, "responses"),
            "wire proxy/gpt-5 responses"
        );
    }

    #[test]
    fn override_suggestions_leave_json_payload_as_free_form_tail() {
        let models = suggested("override ");
        assert_eq!(insert_text(&models, "proxy/gpt-5"), "override proxy/gpt-5 ");

        let wire_apis = suggested("override proxy/gpt-5 ");
        assert_eq!(
            insert_text(&wire_apis, "chat_completions"),
            "override proxy/gpt-5 chat_completions "
        );

        let command_ctx = ctx();
        let app_ctx = AppCtx {
            models: command_ctx.models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        assert!(
            ModelConfigCommand
                .suggest_args(&app_ctx, "override proxy/gpt-5 chat_completions ")
                .is_none()
        );
    }

    #[test]
    fn delete_suggestions_finish_after_model_selection() {
        let models = suggested("delete ");
        assert_eq!(insert_text(&models, "proxy/gpt-5"), "delete proxy/gpt-5");
    }

    #[test]
    fn test_suggestions_offer_preview_or_real_execute_after_model() {
        let models = suggested("test ");
        assert_eq!(insert_text(&models, "proxy/gpt-5"), "test proxy/gpt-5 ");

        let modes = suggested("test proxy/gpt-5 ");
        assert_eq!(insert_text(&modes, "preview"), "test proxy/gpt-5");
        assert_eq!(insert_text(&modes, "execute"), "test proxy/gpt-5 execute");
    }

    #[test]
    fn wire_command_updates_model_default_wire_api() {
        let mut command_ctx = ctx();
        assert!(matches!(
            ModelConfigCommand.run(&mut command_ctx, "wire proxy/gpt-5 responses"),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/model/update_wire_api"
                    && params["modelKey"] == "proxy/gpt-5"
                    && params["wireApi"] == "responses"
        ));
    }

    #[test]
    fn override_command_keeps_payload_json_structured() {
        let mut command_ctx = ctx();
        assert!(matches!(
            ModelConfigCommand.run(
                &mut command_ctx,
                r#"override proxy/gpt-5 chat_completions {"temperature":0.2}"#
            ),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/model_provider_override/set"
                    && params["payload"]["temperature"] == 0.2
        ));
    }

    #[test]
    fn model_config_commands_reject_trailing_or_unknown_arguments() {
        let mut command_ctx = ctx();
        for args in [
            "list extra",
            "get proxy/gpt-5 extra",
            "wire proxy/gpt-5 responses extra",
            "delete proxy/gpt-5 extra",
            "test proxy/gpt-5 preview extra",
            "test proxy/gpt-5 send",
            "test proxy/gpt-5 execute extra",
        ] {
            assert!(
                matches!(ModelConfigCommand.run(&mut command_ctx, args), CommandResult::Error(error) if error.starts_with("Usage:")),
                "{args} must fail with Usage"
            );
        }
    }
}
