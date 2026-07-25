//! `/fast-mode` — toggle the active main Role's fast-mode payload.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

pub struct FastModeCommand;

impl SlashCommand for FastModeCommand {
    fn name(&self) -> &str {
        "fast-mode"
    }

    fn description(&self) -> &str {
        "Enable or disable fast mode for the current session"
    }

    fn usage(&self) -> &str {
        "/fast-mode [on|off]"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<on|off>")
    }

    fn suggest_args(&self, ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        if !ctx.models.current_model_supports_fast_mode() {
            return None;
        }
        Some(
            [("on", true), ("off", false)]
                .into_iter()
                .map(|(label, enabled)| ArgItem {
                    display: label.to_owned(),
                    match_text: label.to_owned(),
                    insert_text: label.to_owned(),
                    description: if enabled {
                        "Enable the Provider/model fast-mode payload"
                    } else {
                        "Disable the Provider/model fast-mode payload"
                    }
                    .to_owned(),
                })
                .collect(),
        )
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if !ctx.models.current_model_supports_fast_mode() {
            return CommandResult::Error(
                "current model does not expose a fast-mode switch".to_owned(),
            );
        }
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return CommandResult::Action(Action::OpenSlashArgPicker {
                command: self.name().to_owned(),
            });
        }
        let enabled = match trimmed.to_ascii_lowercase().as_str() {
            "on" | "true" => true,
            "off" | "false" => false,
            _ => return CommandResult::Error("Usage: /fast-mode [on|off]".to_owned()),
        };
        let Some(session_id) = ctx.session_id else {
            return CommandResult::Error("No active session".to_owned());
        };
        CommandResult::Action(Action::RuntimeExtension {
            method: "_atelier/role/set_fast_mode".to_owned(),
            params: serde_json::json!({
                "roleId": "main",
                "sessionId": session_id.0.as_ref(),
                "enabled": enabled,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use agent_client_protocol as acp;
    use std::sync::Arc;

    fn state(supports: bool) -> ModelState {
        let mut state = ModelState::default();
        let id = acp::ModelId::new(Arc::from("allm/deepseek-v4-flash"));
        let info = acp::ModelInfo::new(id.clone(), "DeepSeek V4 Flash".to_owned()).meta(
            serde_json::json!({ "supportsFastMode": supports })
                .as_object()
                .cloned(),
        );
        state.available.insert(id.clone(), info);
        state.current = Some(id);
        state
    }

    fn exec_ctx<'a>(models: &'a ModelState, session_id: &'a acp::SessionId) -> CommandExecCtx<'a> {
        let bundle = Box::leak(Box::new(crate::app::bundle::BundleState::default()));
        CommandExecCtx {
            models,
            session_id: Some(session_id),
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn bare_command_opens_on_off_picker() {
        let models = state(true);
        let session_id = acp::SessionId::new("session-1");
        let mut ctx = exec_ctx(&models, &session_id);
        assert!(matches!(
            FastModeCommand.run(&mut ctx, ""),
            CommandResult::Action(Action::OpenSlashArgPicker { command }) if command == "fast-mode"
        ));
    }

    #[test]
    fn direct_on_targets_main_role_and_live_session() {
        let models = state(true);
        let session_id = acp::SessionId::new("session-1");
        let mut ctx = exec_ctx(&models, &session_id);
        assert!(matches!(
            FastModeCommand.run(&mut ctx, "on"),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/role/set_fast_mode"
                    && params["roleId"] == "main"
                    && params["sessionId"] == "session-1"
                    && params["enabled"] == true
        ));
    }

    #[test]
    fn unsupported_model_does_not_offer_fake_switch() {
        let models = state(false);
        let session_id = acp::SessionId::new("session-1");
        let mut ctx = exec_ctx(&models, &session_id);
        assert!(matches!(
            FastModeCommand.run(&mut ctx, "on"),
            CommandResult::Error(message) if message.contains("does not expose")
        ));
    }
}
