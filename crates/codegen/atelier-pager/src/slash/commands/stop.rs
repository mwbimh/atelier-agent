//! `/stop` -- cancel a runtime task or the current session turn.

use serde_json::json;

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

const TASK_CANCEL: &str = "_atelier/task/cancel";

pub struct StopCommand;

impl SlashCommand for StopCommand {
    fn name(&self) -> &str {
        "stop"
    }

    fn description(&self) -> &str {
        "Cancel a runtime task or the current turn"
    }

    fn usage(&self) -> &str {
        "/stop [task-id]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let Some(session_id) = ctx.session_id else {
            return CommandResult::Error("No active session".to_owned());
        };
        let task_id = args.trim();
        if task_id.split_whitespace().count() > 1 {
            return CommandResult::Error("Usage: /stop [task-id]".to_owned());
        }
        let mut params = json!({ "sessionId": session_id.to_string() });
        if !task_id.is_empty() {
            params["taskId"] = json!(task_id);
        }
        CommandResult::Action(Action::RuntimeExtension {
            method: TASK_CANCEL.to_owned(),
            params,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::StopCommand;
    use crate::app::actions::Action;
    use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

    fn ctx() -> CommandExecCtx<'static> {
        let models = Box::leak(Box::new(crate::acp::model_state::ModelState::default()));
        let bundle = Box::leak(Box::new(crate::app::bundle::BundleState::default()));
        let session_id = Box::leak(Box::new(agent_client_protocol::SessionId::new("s1")));
        CommandExecCtx {
            models,
            session_id: Some(session_id),
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn stop_without_id_cancels_current_session() {
        let mut command_ctx = ctx();
        assert!(matches!(
            StopCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/task/cancel" && params["sessionId"] == "s1"
        ));
    }

    #[test]
    fn stop_with_id_cancels_named_task() {
        let mut command_ctx = ctx();
        assert!(matches!(
            StopCommand.run(&mut command_ctx, "task-21"),
            CommandResult::Action(Action::RuntimeExtension { params, .. })
                if params["taskId"] == "task-21"
        ));
    }
}
