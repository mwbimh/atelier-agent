//! `/sandbox-approvals on|off` — session-scoped automatic AllowOnce for
//! per-command sandbox override requests.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

pub struct SandboxApprovalsCommand;

impl SlashCommand for SandboxApprovalsCommand {
    fn name(&self) -> &str {
        "sandbox-approvals"
    }

    fn description(&self) -> &str {
        "Auto-approve or re-prompt for one-command sandbox overrides"
    }

    fn usage(&self) -> &str {
        "/sandbox-approvals <on|off>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn suggest_args(&self, _ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        Some(vec![
            ArgItem {
                display: "on".to_owned(),
                match_text: "on enable allow auto approve".to_owned(),
                insert_text: "on".to_owned(),
                description: "Allow each host-execution request once for this session".to_owned(),
            },
            ArgItem {
                display: "off".to_owned(),
                match_text: "off disable ask prompt".to_owned(),
                insert_text: "off".to_owned(),
                description: "Require interactive approval for every request".to_owned(),
            },
        ])
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let enabled = match args.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "enable" | "enabled" => true,
            "off" | "false" | "disable" | "disabled" => false,
            _ => return CommandResult::Error(format!("Usage: {}", self.usage())),
        };
        CommandResult::Action(Action::RuntimeExtension {
            method: "_atelier/sandbox/set_override_auto_approve".to_owned(),
            params: serde_json::json!({ "enabled": enabled }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    fn ctx<'a>(models: &'a ModelState, bundle: &'a BundleState) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn on_and_off_dispatch_session_scoped_runtime_control() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = ctx(&models, &bundle);
        for (arg, expected) in [("on", true), ("off", false)] {
            let CommandResult::Action(Action::RuntimeExtension { method, params }) =
                SandboxApprovalsCommand.run(&mut ctx, arg)
            else {
                panic!("expected runtime extension action");
            };
            assert_eq!(method, "_atelier/sandbox/set_override_auto_approve");
            assert_eq!(params["enabled"], expected);
        }
    }

    #[test]
    fn invalid_value_fails_closed() {
        let models = ModelState::default();
        let bundle = BundleState::default();
        let mut ctx = ctx(&models, &bundle);
        assert!(matches!(
            SandboxApprovalsCommand.run(&mut ctx, "maybe"),
            CommandResult::Error(message) if message.contains("on|off")
        ));
    }
}
