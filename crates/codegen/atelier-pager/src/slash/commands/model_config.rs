//! `/wire-api` -- set the Wire API for the current Provider/model pair.

use serde_json::json;

use crate::app::actions::Action;
use crate::slash::command::{
    AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand, current_arg_fragment,
};

const MODEL_UPDATE_WIRE_API: &str = "_atelier/model/update_wire_api";

pub struct ModelConfigCommand;

impl SlashCommand for ModelConfigCommand {
    fn name(&self) -> &str {
        "wire-api"
    }

    fn description(&self) -> &str {
        "Set the current Provider/model Wire API"
    }

    fn usage(&self) -> &str {
        "/wire-api <responses|message|chat>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<responses|message|chat>")
    }

    fn session_scoped(&self) -> bool {
        false
    }

    fn suggest_args(&self, _ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let trimmed = args_query.trim();
        if trimmed.split_whitespace().count() > 1
            || (!trimmed.is_empty() && args_query.chars().last().is_some_and(char::is_whitespace))
        {
            return None;
        }
        Some(
            [
                ("responses", "OpenAI Responses API"),
                ("message", "Messages API"),
                ("chat", "Chat Completions API"),
            ]
            .into_iter()
            .map(|(wire_api, description)| ArgItem {
                display: wire_api.to_owned(),
                match_text: wire_api.to_owned(),
                insert_text: wire_api.to_owned(),
                description: description.to_owned(),
            })
            .collect(),
        )
    }

    fn arg_suggestion_filter_query<'a>(&self, args_query: &'a str) -> &'a str {
        current_arg_fragment(args_query)
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if args.trim().is_empty() {
            return CommandResult::Action(Action::OpenSlashArgPicker {
                command: self.name().to_owned(),
            });
        }
        let tokens = args.split_whitespace().collect::<Vec<_>>();
        let [wire_api] = tokens.as_slice() else {
            return CommandResult::Error(format!("Usage: {}", self.usage()));
        };
        let wire_api = match *wire_api {
            "responses" => "responses",
            "message" => "messages",
            "chat" => "chat_completions",
            value => {
                return CommandResult::Error(format!(
                    "unknown Wire API '{value}'; use responses, message, or chat"
                ));
            }
        };
        let Some(model_key) = ctx.models.current_model_id_str() else {
            return CommandResult::Error(
                "No current Provider/model pair; select a model with /model first".to_owned(),
            );
        };
        if !model_key.contains('/') {
            return CommandResult::Error(format!(
                "Current model '{model_key}' is not a Provider/model pair"
            ));
        }
        CommandResult::Action(Action::RuntimeExtension {
            method: MODEL_UPDATE_WIRE_API.to_owned(),
            params: json!({
                "modelKey": model_key,
                "wireApi": wire_api,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ModelConfigCommand;
    use crate::acp::model_state::ModelState;
    use crate::app::actions::Action;
    use crate::slash::command::{AppCtx, CommandExecCtx, CommandResult, SlashCommand};
    use agent_client_protocol as acp;
    use std::sync::Arc;

    fn ctx_with_current_model(current: Option<&str>) -> CommandExecCtx<'static> {
        let mut models = ModelState::default();
        if let Some(model_key) = current {
            let id = acp::ModelId::new(Arc::from(model_key));
            models.available.insert(
                id.clone(),
                acp::ModelInfo::new(id.clone(), "Current model".to_owned()),
            );
            models.current = Some(id);
        }
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
        let command_ctx = ctx_with_current_model(Some("proxy/gpt-5"));
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

    #[test]
    fn bare_wire_api_opens_interactive_picker() {
        let mut command_ctx = ctx_with_current_model(Some("proxy/gpt-5"));
        assert!(matches!(
            ModelConfigCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::OpenSlashArgPicker { command }) if command == "wire-api"
        ));
    }

    #[test]
    fn wire_api_picker_offers_only_simple_protocol_names() {
        let items = suggested("");
        assert_eq!(
            items
                .iter()
                .map(|item| item.display.as_str())
                .collect::<Vec<_>>(),
            vec!["responses", "message", "chat"]
        );
        assert_eq!(
            items
                .iter()
                .map(|item| item.insert_text.as_str())
                .collect::<Vec<_>>(),
            vec!["responses", "message", "chat"]
        );
    }

    #[test]
    fn simple_command_updates_the_current_provider_model_pair() {
        let mut command_ctx = ctx_with_current_model(Some("proxy/gpt-5"));
        for (input, wire_api) in [
            ("responses", "responses"),
            ("message", "messages"),
            ("chat", "chat_completions"),
        ] {
            assert!(matches!(
                ModelConfigCommand.run(&mut command_ctx, input),
                CommandResult::Action(Action::RuntimeExtension { method, params })
                    if method == "_atelier/model/update_wire_api"
                        && params["modelKey"] == "proxy/gpt-5"
                        && params["wireApi"] == wire_api
            ));
        }
    }

    #[test]
    fn wire_api_requires_a_current_provider_model_pair() {
        let mut no_model = ctx_with_current_model(None);
        assert!(matches!(
            ModelConfigCommand.run(&mut no_model, "responses"),
            CommandResult::Error(error) if error.contains("select a model with /model first")
        ));

        let mut non_pair = ctx_with_current_model(Some("builtin-model"));
        assert!(matches!(
            ModelConfigCommand.run(&mut non_pair, "responses"),
            CommandResult::Error(error) if error.contains("not a Provider/model pair")
        ));
    }

    #[test]
    fn legacy_and_extra_arguments_are_rejected() {
        let mut command_ctx = ctx_with_current_model(Some("proxy/gpt-5"));
        for args in [
            "list",
            "get proxy/gpt-5",
            "set proxy/gpt-5 responses",
            "messages",
            "chat_completions",
            "responses extra",
        ] {
            assert!(
                matches!(
                    ModelConfigCommand.run(&mut command_ctx, args),
                    CommandResult::Error(_)
                ),
                "{args} must be rejected"
            );
        }
    }
}
