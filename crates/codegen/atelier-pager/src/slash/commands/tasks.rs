//! `/tasks` -- list background tasks, subagents, and scheduled tasks.
//!
//! `/tasks` queries the unified Runtime task registry. The response includes
//! main turns, derived Agents, parallel Agents, and replay metadata.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
use serde_json::json;

/// List background tasks, subagents, and scheduled tasks.
pub struct TasksCommand;

const TASK_LIST: &str = "_atelier/task/list";

impl SlashCommand for TasksCommand {
    fn name(&self) -> &str {
        "tasks"
    }

    fn description(&self) -> &str {
        "List background tasks, subagents, and scheduled tasks"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn offered_when_session_less(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/tasks"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if !args.trim().is_empty() {
            return CommandResult::Error("Usage: /tasks".to_owned());
        }
        CommandResult::Action(Action::RuntimeExtension {
            method: TASK_LIST.to_owned(),
            params: json!({}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    static DEFAULT_BUNDLE_STATE: BundleState = BundleState {
        has_cache: false,
        version: String::new(),
        personas: Vec::new(),
        roles: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        persona_details: Vec::new(),
        role_details: Vec::new(),
    };

    fn run_with_session(sid: Option<&agent_client_protocol::SessionId>) -> CommandResult {
        let models = ModelState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: sid,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Minimal,
            pager_state: PagerLocalSnapshot::default(),
        };
        TasksCommand.run(&mut ctx, "")
    }

    #[test]
    fn with_session_dispatches_show_tasks() {
        let sid = agent_client_protocol::SessionId::from("s1".to_string());
        assert!(matches!(
            run_with_session(Some(&sid)),
            CommandResult::Action(Action::RuntimeExtension { method, .. })
                if method == "_atelier/task/list"
        ));
    }

    #[test]
    fn available_in_minimal_by_default() {
        assert!(TasksCommand.available_in_minimal());
    }

    #[test]
    fn available_without_a_session() {
        assert!(TasksCommand.offered_when_session_less());
        assert!(matches!(
            run_with_session(None),
            CommandResult::Action(Action::RuntimeExtension { method, .. })
                if method == "_atelier/task/list"
        ));
    }

    #[test]
    fn rejects_unexpected_arguments() {
        let models = ModelState::default();
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Minimal,
            pager_state: PagerLocalSnapshot::default(),
        };
        assert!(matches!(
            TasksCommand.run(&mut ctx, "extra"),
            CommandResult::Error(error) if error == "Usage: /tasks"
        ));
    }
}
