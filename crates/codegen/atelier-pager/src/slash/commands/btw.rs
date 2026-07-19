//! `/btw` -- ask a side question without interrupting the running agent.
//!
//! Returns `CommandResult::Action(Action::SendBtw(...))` so the dispatch layer
//! fires it as an ACP ext method (`atelier/btw`) that bypasses the prompt queue.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct BtwCommand;

impl SlashCommand for BtwCommand {
    fn name(&self) -> &str {
        "btw"
    }

    fn description(&self) -> &str {
        "Ask a side question without interrupting"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/btw <question>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<question>")
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if args.trim().is_empty() {
            return CommandResult::Action(Action::OpenSlashCommandInput {
                command: self.name().to_owned(),
            });
        }
        CommandResult::Action(Action::SendBtw(args.trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::BtwCommand;
    use crate::app::actions::Action;
    use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

    fn ctx() -> CommandExecCtx<'static> {
        let models = Box::leak(Box::new(crate::acp::model_state::ModelState::default()));
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
    fn bare_btw_opens_free_form_command_input() {
        let mut command_ctx = ctx();
        assert!(matches!(
            BtwCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::OpenSlashCommandInput { command })
                if command == "btw"
        ));
    }

    #[test]
    fn btw_with_question_keeps_complete_command_path() {
        let mut command_ctx = ctx();
        assert!(matches!(
            BtwCommand.run(&mut command_ctx, "why Responses?"),
            CommandResult::Action(Action::SendBtw(question)) if question == "why Responses?"
        ));
    }
}
