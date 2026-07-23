//! `/attach` and `/fg` -- reconnect to a runtime task and replay events.

use serde_json::json;

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

const TASK_ATTACH: &str = "_atelier/task/attach";

type RuntimeTaskCursorStore = std::sync::Mutex<std::collections::HashMap<String, u64>>;

fn runtime_task_cursors() -> &'static RuntimeTaskCursorStore {
    static CURSORS: std::sync::OnceLock<RuntimeTaskCursorStore> = std::sync::OnceLock::new();
    CURSORS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn remember_runtime_task_cursor(task_id: &str, cursor: u64) {
    let mut cursors = runtime_task_cursors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let saved = cursors.entry(task_id.to_owned()).or_default();
    *saved = (*saved).max(cursor);
}

pub(crate) fn runtime_task_cursor(task_id: &str) -> Option<u64> {
    runtime_task_cursors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(task_id)
        .copied()
}

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

    fn offered_when_session_less(&self) -> bool {
        true
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let task_id = args.trim();
        if task_id.is_empty() {
            return CommandResult::Action(Action::OpenSlashCommandInput {
                command: self.name().to_owned(),
            });
        }
        if task_id.split_whitespace().count() != 1 {
            return CommandResult::Error("Usage: /attach <task-id>".to_owned());
        }
        let mut params = json!({ "taskId": task_id });
        if let Some(cursor) = runtime_task_cursor(task_id) {
            params["afterEventId"] = json!(cursor);
        }
        CommandResult::Action(Action::RuntimeExtension {
            method: TASK_ATTACH.to_owned(),
            params,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::AttachCommand;
    use crate::app::actions::Action;
    use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

    fn ctx_with_session(has_session: bool) -> CommandExecCtx<'static> {
        let models = Box::leak(Box::new(crate::acp::model_state::ModelState::default()));
        let bundle = Box::leak(Box::new(crate::app::bundle::BundleState::default()));
        let session_id = Box::leak(Box::new(agent_client_protocol::SessionId::new("s1")));
        CommandExecCtx {
            models,
            session_id: has_session.then_some(session_id),
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    fn ctx() -> CommandExecCtx<'static> {
        ctx_with_session(true)
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

    #[test]
    fn attach_with_id_is_available_without_a_session() {
        assert!(AttachCommand.offered_when_session_less());
        let mut command_ctx = ctx_with_session(false);
        assert!(matches!(
            AttachCommand.run(&mut command_ctx, "task-21"),
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/task/attach" && params["taskId"] == "task-21"
        ));
    }

    #[test]
    fn attach_rejects_trailing_arguments() {
        let mut command_ctx = ctx();
        assert!(matches!(
            AttachCommand.run(&mut command_ctx, "task-21 extra"),
            CommandResult::Error(error) if error == "Usage: /attach <task-id>"
        ));
    }
}
