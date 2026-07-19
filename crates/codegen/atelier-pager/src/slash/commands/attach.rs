//! `/attach` and `/fg` -- reconnect to a runtime task and replay events.

use serde_json::json;

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

const TASK_ATTACH: &str = "_atelier/task/attach";

pub struct AttachCommand;

impl SlashCommand for AttachCommand {
    fn name(&self) -> &str {
        "attach"
    }

    fn aliases(&self) -> &[&str] {
        &["fg"]
    }

    fn description(&self) -> &str {
        "Attach to a runtime task and replay its events"
    }

    fn usage(&self) -> &str {
        "/attach <task-id>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error("No active session".to_owned());
        }
        let task_id = args.trim();
        if task_id.is_empty() {
            return CommandResult::Action(Action::OpenSlashCommandInput {
                command: self.name().to_owned(),
            });
        }
        if task_id.split_whitespace().count() != 1 {
            return CommandResult::Error("Usage: /attach <task-id>".to_owned());
        }
        CommandResult::Action(Action::RuntimeExtension {
            method: TASK_ATTACH.to_owned(),
            params: json!({ "taskId": task_id }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::AttachCommand;
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
    fn bare_attach_opens_interactive_task_id_entry() {
        let mut command_ctx = ctx();
        assert!(matches!(
            AttachCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::OpenSlashCommandInput { command })
                if command == "attach"
        ));
    }

    #[test]
    fn attach_with_id_replays_task_events() {
        let mut command_ctx = ctx();
        assert!(matches!(
            AttachCommand.run(&mut command_ctx, "task-21"),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/task/attach" && params["taskId"] == "task-21"
        ));
    }
}
