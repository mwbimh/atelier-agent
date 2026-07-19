//! `/background` and `/bg` -- detach the current turn without cancelling it.

use serde_json::json;

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

const TASK_DETACH: &str = "_atelier/task/detach";

pub struct BackgroundCommand;

impl SlashCommand for BackgroundCommand {
    fn name(&self) -> &str {
        "background"
    }

    fn aliases(&self) -> &[&str] {
        &["bg"]
    }

    fn description(&self) -> &str {
        "Detach the current turn and keep it running"
    }

    fn usage(&self) -> &str {
        "/background"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error("No active session".to_owned());
        }
        if !args.trim().is_empty() {
            return CommandResult::Error("Usage: /background".to_owned());
        }
        CommandResult::Action(Action::RuntimeExtension {
            method: TASK_DETACH.to_owned(),
            params: json!({}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::BackgroundCommand;
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
    fn background_detaches_without_cancelling() {
        let mut command_ctx = ctx();
        assert!(matches!(
            BackgroundCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::RuntimeExtension { method, .. })
                if method == "_atelier/task/detach"
        ));
    }
}
