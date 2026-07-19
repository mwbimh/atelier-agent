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
        true
    }

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let query = args_query.trim_end();
        let mut parts = query.split_whitespace();
        let command = parts.next();
        let model_key = parts.next().unwrap_or_default();
        if command.is_some() && model_key.is_empty() {
            return Some(
                ["list", "get ", "wire ", "override ", "delete ", "test "]
                    .into_iter()
                    .map(|value| ArgItem {
                        display: value.trim().to_owned(),
                        match_text: value.trim().to_owned(),
                        insert_text: value.to_owned(),
                        description: "model Wire API management".to_owned(),
                    })
                    .collect(),
            );
        }
        if let Some(command) = command
            && ["get", "wire", "override", "delete", "test"].contains(&command)
            && !model_key.is_empty()
            && query.ends_with(char::is_whitespace)
        {
            let prefix = format!("{command} ");
            if command == "wire" || command == "override" {
                return Some(
                    ["chat_completions", "responses", "messages", "default"]
                        .into_iter()
                        .map(|wire_api| ArgItem {
                            display: wire_api.to_owned(),
                            match_text: wire_api.to_owned(),
                            insert_text: format!("{prefix}{model_key} {wire_api}"),
                            description: "Wire API".to_owned(),
                        })
                        .collect(),
                );
            }
            return None;
        }
        if command.is_none() || (command.is_some() && model_key.is_empty()) {
            return Some(model_items(ctx));
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
            "list" => extension(MODEL_LIST, json!({})),
            "get" => {
                let Some(model_key) = tokens.get(1) else {
                    return CommandResult::Error(
                        "Usage: /model-config get <provider/model>".to_owned(),
                    );
                };
                extension(MODEL_GET, json!({ "modelKey": model_key }))
            }
            "wire" => {
                let Some(model_key) = tokens.get(1) else {
                    return CommandResult::Error(
                        "Usage: /model-config wire <provider/model> <wire-api|default>".to_owned(),
                    );
                };
                let Some(wire_api) = tokens.get(2) else {
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
                let Some(model_key) = tokens.get(1) else {
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

fn model_items(ctx: &AppCtx) -> Vec<ArgItem> {
    ctx.models
        .available
        .iter()
        .map(|(id, info)| ArgItem {
            display: id.0.to_string(),
            match_text: format!("{} {}", id.0, info.name),
            insert_text: format!("get {}", id.0),
            description: info.name.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::ModelConfigCommand;
    use crate::acp::model_state::ModelState;
    use crate::app::actions::Action;
    use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
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

    #[test]
    fn bare_model_config_opens_interactive_entry() {
        let mut command_ctx = ctx();
        assert!(matches!(
            ModelConfigCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::OpenSlashArgPicker { command }) if command == "model-config"
        ));
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
}
