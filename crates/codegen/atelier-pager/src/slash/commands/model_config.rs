//! `/wire-api` -- inspect and edit model Wire API settings.

use atelier_provider::{ProviderModelOverride, ProviderRegistry, WireApi};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

use crate::app::actions::Action;
use crate::slash::command::{
    AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand, current_arg_fragment,
};

const MODEL_LIST: &str = "_atelier/model/list";
const MODEL_GET: &str = "_atelier/model/get";
const OVERRIDE_SET: &str = "_atelier/model_provider_override/set";
const OVERRIDE_TEST: &str = "_atelier/model_provider_override/test";

pub struct ModelConfigCommand;

impl SlashCommand for ModelConfigCommand {
    fn name(&self) -> &str {
        "wire-api"
    }

    fn description(&self) -> &str {
        "Inspect and edit model Wire API configuration"
    }

    fn usage(&self) -> &str {
        "/wire-api [list|get|set|payload|reset|test] ..."
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<list|get|set|payload|reset|test> ...")
    }

    fn session_scoped(&self) -> bool {
        false
    }

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let trailing_space = args_query.chars().last().is_some_and(char::is_whitespace);
        let tokens = args_query.split_whitespace().collect::<Vec<_>>();
        let subcommands = || {
            ["list", "get", "set", "payload", "reset", "test"]
                .into_iter()
                .map(|command| ArgItem {
                    display: command.to_owned(),
                    match_text: command.to_owned(),
                    insert_text: if command == "list" {
                        command.to_owned()
                    } else {
                        format!("{command} ")
                    },
                    description: match command {
                        "list" => "List resolved model protocols",
                        "get" => "Inspect protocol, source, and exact override",
                        "set" => "Set the exact Provider/model protocol",
                        "payload" => "Set exact non-credential request payload fields",
                        "reset" => "Remove an exact override and restore inheritance",
                        "test" => "Preview or execute the resolved request",
                        _ => unreachable!(),
                    }
                    .to_owned(),
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
        if !["get", "set", "payload", "reset", "test"].contains(&command) {
            return Some(subcommands());
        }
        if tokens.len() == 1 || (tokens.len() == 2 && !trailing_space) {
            let append_space = matches!(command, "set" | "payload" | "test");
            return Some(model_items(ctx, command, append_space));
        }
        let model_key = tokens[1];
        if tokens.len() == 2 && trailing_space {
            if matches!(command, "payload" | "reset") {
                return None;
            }
            if command == "set" {
                return Some(
                    ["inherited", "chat_completions", "responses", "messages"]
                        .into_iter()
                        .map(|wire_api| ArgItem {
                            display: wire_api.to_owned(),
                            match_text: wire_api.to_owned(),
                            insert_text: format!("set {model_key} {wire_api}"),
                            description: if wire_api == "inherited" {
                                "Use the model definition or default Wire API".to_owned()
                            } else {
                                "Exact Provider/model Wire API".to_owned()
                            },
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

    fn arg_suggestion_filter_query<'a>(&self, args_query: &'a str) -> &'a str {
        current_arg_fragment(args_query)
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
                    return CommandResult::Error("Usage: /wire-api list".to_owned());
                }
                extension(MODEL_LIST, json!({}))
            }
            "get" => {
                let [_, model_key] = tokens.as_slice() else {
                    return CommandResult::Error(
                        "Usage: /wire-api get <provider/model>".to_owned(),
                    );
                };
                extension(MODEL_GET, json!({ "modelKey": model_key }))
            }
            "set" => {
                let [_, model_key, wire_api] = tokens.as_slice() else {
                    return CommandResult::Error(
                        "Usage: /wire-api set <provider/model> <inherited|chat_completions|responses|messages>"
                            .to_owned(),
                    );
                };
                extension(
                    OVERRIDE_SET,
                    json!({
                        "modelKey": model_key,
                        "wireApi": match parse_wire_api(wire_api) {
                            Ok(value) => value,
                            Err(error) => return CommandResult::Error(error),
                        },
                        "preservePayload": true,
                    }),
                )
            }
            "payload" => {
                let mut raw_parts = args.trim().splitn(3, char::is_whitespace);
                let _ = raw_parts.next();
                let Some(model_key) = raw_parts.next() else {
                    return CommandResult::Error(
                        "Usage: /wire-api payload <provider/model> <json-object>".to_owned(),
                    );
                };
                let Some(raw_payload) = raw_parts
                    .next()
                    .map(str::trim)
                    .filter(|raw| !raw.is_empty())
                else {
                    return CommandResult::Error(
                        "Usage: /wire-api payload <provider/model> <json-object>".to_owned(),
                    );
                };
                let payload = match parse_payload(raw_payload) {
                    Ok(payload) => payload,
                    Err(error) => return CommandResult::Error(error),
                };
                extension(
                    OVERRIDE_SET,
                    json!({
                        "modelKey": model_key,
                        "payload": payload,
                        "preserveWireApi": true,
                    }),
                )
            }
            "reset" => {
                let [_, model_key] = tokens.as_slice() else {
                    return CommandResult::Error(
                        "Usage: /wire-api reset <provider/model>".to_owned(),
                    );
                };
                let registry = match ProviderRegistry::load_or_create(
                    atelier_config::atelier_home().join("providers.toml"),
                ) {
                    Ok(registry) => registry,
                    Err(error) => {
                        return CommandResult::Error(format!(
                            "Could not read Wire API overrides: {error}"
                        ));
                    }
                };
                let key = match atelier_provider::ModelKey::parse(model_key) {
                    Ok(key) => key,
                    Err(error) => return CommandResult::Error(error.to_string()),
                };
                let exact = registry.model_provider_override(&key);
                let Some(model) = registry.model(&key) else {
                    return CommandResult::Error(format!("Unknown model {model_key}"));
                };
                let (inherited_wire_api, inherited_source) = match model.wire_api {
                    Some(wire_api) => (wire_api, "model definition"),
                    None => (WireApi::ChatCompletions, "default metadata"),
                };
                match reset_action(
                    model_key,
                    exact.as_ref(),
                    inherited_wire_api,
                    inherited_source,
                ) {
                    Ok(action) => CommandResult::Action(Action::OpenDestructiveConfirm { action }),
                    Err(error) => CommandResult::Error(error),
                }
            }
            "test" => {
                let Some(model_key) = tokens.get(1) else {
                    return CommandResult::Error(
                        "Usage: /wire-api test <provider/model> [execute]".to_owned(),
                    );
                };
                if tokens.len() > 3
                    || tokens
                        .get(2)
                        .is_some_and(|value| value.as_str() != "execute")
                {
                    return CommandResult::Error(
                        "Usage: /wire-api test <provider/model> [execute]".to_owned(),
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
        "inherited" | "default" | "none" => Ok(None),
        _ => Err(format!(
            "unknown Wire API '{value}'; use inherited, chat_completions, responses, or messages"
        )),
    }
}

fn parse_payload(raw: &str) -> Result<Map<String, Value>, String> {
    let value = serde_json::from_str::<Value>(raw)
        .map_err(|error| format!("invalid JSON payload: {error}"))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "payload must be a JSON object".to_owned())
}

fn model_items(ctx: &AppCtx, command: &str, append_space: bool) -> Vec<ArgItem> {
    if command == "reset" {
        let overrides =
            ProviderRegistry::load_or_create(atelier_config::atelier_home().join("providers.toml"))
                .ok()
                .map(|registry| registry.snapshot().model_provider_overrides)
                .unwrap_or_default();
        return reset_model_items(ctx, &overrides);
    }
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

fn reset_action(
    model_key: &str,
    exact: Option<&ProviderModelOverride>,
    inherited_wire_api: WireApi,
    inherited_source: &str,
) -> Result<crate::views::modal::DestructiveAction, String> {
    let Some(exact) = exact else {
        return Err(format!(
            "No exact Wire API override exists for {model_key}; nothing to reset"
        ));
    };
    let exact_protocol = exact
        .wire_api
        .map(wire_api_name)
        .unwrap_or("inherited protocol");
    let exact_payload = if exact.payload.is_empty() {
        "no payload".to_owned()
    } else {
        format!(
            "payload keys {}",
            exact.payload.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    };
    Ok(crate::views::modal::DestructiveAction::ResetWireApi {
        model_key: model_key.to_owned(),
        summary: format!(
            "Before: {exact_protocol} · {exact_payload}  →  After: {} ({inherited_source})",
            wire_api_name(inherited_wire_api)
        ),
    })
}

fn reset_model_items(
    ctx: &AppCtx,
    overrides: &BTreeMap<String, ProviderModelOverride>,
) -> Vec<ArgItem> {
    overrides
        .iter()
        .map(|(model_key, exact)| {
            let model_name = ctx
                .models
                .available
                .iter()
                .find(|(id, _)| id.0.as_ref() == model_key)
                .map(|(_, info)| info.name.as_str())
                .unwrap_or(model_key);
            let protocol = exact
                .wire_api
                .map(wire_api_name)
                .unwrap_or("inherited protocol");
            let payload = if exact.payload.is_empty() {
                "no payload keys".to_owned()
            } else {
                format!("{} payload key(s)", exact.payload.len())
            };
            ArgItem {
                display: model_key.clone(),
                match_text: format!("{model_key} {model_name}"),
                insert_text: format!("reset {model_key}"),
                description: format!("Exact override: {protocol} · {payload}"),
            }
        })
        .collect()
}

fn wire_api_name(wire_api: WireApi) -> &'static str {
    match wire_api {
        WireApi::ChatCompletions => "chat_completions",
        WireApi::Responses => "responses",
        WireApi::Messages => "messages",
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelConfigCommand, reset_action, reset_model_items};
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
    fn bare_wire_api_opens_interactive_entry() {
        let mut command_ctx = ctx();
        assert!(matches!(
            ModelConfigCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::OpenSlashArgPicker { command }) if command == "wire-api"
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
            vec!["list", "get", "set", "payload", "reset", "test"]
        );
        assert_eq!(insert_text(&items, "set"), "set ");
    }

    #[test]
    fn set_suggestions_chain_model_and_wire_api() {
        let models = suggested("set ");
        assert_eq!(insert_text(&models, "proxy/gpt-5"), "set proxy/gpt-5 ");

        let wire_apis = suggested("set proxy/gpt-5 ");
        assert_eq!(
            insert_text(&wire_apis, "responses"),
            "set proxy/gpt-5 responses"
        );
        assert_eq!(
            insert_text(&wire_apis, "inherited"),
            "set proxy/gpt-5 inherited"
        );
    }

    #[test]
    fn payload_suggestions_leave_json_as_a_free_form_tail() {
        let models = suggested("payload ");
        assert_eq!(insert_text(&models, "proxy/gpt-5"), "payload proxy/gpt-5 ");
        let command_ctx = ctx();
        let app_ctx = AppCtx {
            models: command_ctx.models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        assert!(
            ModelConfigCommand
                .suggest_args(&app_ctx, "payload proxy/gpt-5 ")
                .is_none()
        );
    }

    #[test]
    fn reset_suggestions_include_only_models_with_exact_overrides() {
        let command_ctx = ctx();
        let app_ctx = AppCtx {
            models: command_ctx.models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        let overrides = std::collections::BTreeMap::from([
            (
                "proxy/gpt-5".to_owned(),
                atelier_provider::ProviderModelOverride {
                    wire_api: Some(atelier_provider::WireApi::Responses),
                    payload: serde_json::from_value(serde_json::json!({ "temperature": 0.2 }))
                        .unwrap(),
                },
            ),
            (
                "other/not-in-catalog".to_owned(),
                atelier_provider::ProviderModelOverride::empty(),
            ),
        ]);
        let models = reset_model_items(&app_ctx, &overrides);
        assert_eq!(insert_text(&models, "proxy/gpt-5"), "reset proxy/gpt-5");
        assert_eq!(
            models
                .iter()
                .find(|item| item.display == "proxy/gpt-5")
                .unwrap()
                .description,
            "Exact override: responses · 1 payload key(s)"
        );
        assert_eq!(models.len(), 2, "every exact override is resettable");
        assert!(
            ModelConfigCommand
                .suggest_args(&app_ctx, "reset proxy/gpt-5 ")
                .is_none()
        );
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
    fn set_command_updates_only_the_wire_api_field() {
        let mut command_ctx = ctx();
        assert!(matches!(
            ModelConfigCommand.run(&mut command_ctx, "set proxy/gpt-5 responses"),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/model_provider_override/set"
                    && params["modelKey"] == "proxy/gpt-5"
                    && params["wireApi"] == "responses"
                    && params["preservePayload"] == true
        ));
        assert!(matches!(
            ModelConfigCommand.run(&mut command_ctx, "set proxy/gpt-5 inherited"),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/model_provider_override/set"
                    && params["wireApi"].is_null()
                    && params["preservePayload"] == true
        ));
    }

    #[test]
    fn payload_command_updates_only_the_payload_field() {
        let mut command_ctx = ctx();
        assert!(matches!(
            ModelConfigCommand.run(
                &mut command_ctx,
                r#"payload proxy/gpt-5 {"temperature":0.2}"#
            ),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/model_provider_override/set"
                    && params["payload"]["temperature"] == 0.2
                    && params["preserveWireApi"] == true
        ));
    }

    #[test]
    fn model_config_commands_reject_trailing_or_unknown_arguments() {
        let mut command_ctx = ctx();
        for args in [
            "list extra",
            "get proxy/gpt-5 extra",
            "set proxy/gpt-5 responses extra",
            "wire proxy/gpt-5 responses",
            "override proxy/gpt-5 responses {}",
            "test proxy/gpt-5 preview extra",
            "test proxy/gpt-5 send",
            "test proxy/gpt-5 execute extra",
        ] {
            assert!(
                matches!(ModelConfigCommand.run(&mut command_ctx, args), CommandResult::Error(error) if error.starts_with("Usage:")),
                "{args} must fail with Usage"
            );
        }
        assert!(matches!(
            ModelConfigCommand.run(&mut command_ctx, "payload proxy/gpt-5 []"),
            CommandResult::Error(error) if error == "payload must be a JSON object"
        ));
    }

    #[test]
    fn reset_command_requires_an_exact_override_before_confirmation() {
        let overrides = std::collections::BTreeMap::from([(
            "proxy/gpt-5".to_owned(),
            atelier_provider::ProviderModelOverride {
                wire_api: Some(atelier_provider::WireApi::Responses),
                payload: serde_json::Map::new(),
            },
        )]);
        let exact = overrides.get("proxy/gpt-5").unwrap();
        assert_eq!(
            reset_action(
                "proxy/gpt-5",
                Some(exact),
                atelier_provider::WireApi::ChatCompletions,
                "default metadata",
            )
            .unwrap(),
            crate::views::modal::DestructiveAction::ResetWireApi {
                model_key: "proxy/gpt-5".to_owned(),
                summary:
                    "Before: responses · no payload  →  After: chat_completions (default metadata)"
                        .to_owned(),
            }
        );
        assert!(
            reset_action(
                "proxy/missing",
                None,
                atelier_provider::WireApi::ChatCompletions,
                "default metadata",
            )
            .is_err()
        );

        let mut command_ctx = ctx();
        assert!(matches!(
            ModelConfigCommand.run(&mut command_ctx, "delete proxy/gpt-5 confirm"),
            CommandResult::Error(error) if error.starts_with("Usage:")
        ));
    }
}
