//! `/settings` -- open the settings modal.
//!
//! No `/settings <id>` direct-jump — args are silently discarded and
//! the modal always opens. Use the in-modal `/` filter to search.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

/// Open the settings modal.
pub struct SettingsCommand;

impl SlashCommand for SettingsCommand {
    fn name(&self) -> &str {
        "settings"
    }

    fn aliases(&self) -> &[&str] {
        &["config", "preferences", "prefs"]
    }

    fn description(&self) -> &str {
        "Open the settings modal"
    }

    fn usage(&self) -> &str {
        "/settings [open|reset-defaults]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn suggest_args(&self, _ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        Some(vec![
            ArgItem {
                display: "open".to_owned(),
                match_text: "open".to_owned(),
                insert_text: "open".to_owned(),
                description: "Open the interactive settings modal".to_owned(),
            },
            ArgItem {
                display: "reset-defaults".to_owned(),
                match_text: "reset defaults restore".to_owned(),
                insert_text: "reset-defaults".to_owned(),
                description: "Restore built-in model and context default presets".to_owned(),
            },
        ])
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        match args.trim() {
            "" | "open" => CommandResult::Action(Action::OpenSettings),
            "reset" | "reset-defaults" => CommandResult::Action(Action::RuntimeExtension {
                method: "_atelier/config/reset_defaults".to_owned(),
                params: serde_json::json!({}),
            }),
            _ => CommandResult::Error(format!("Usage: {}", self.usage())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;

    static DEFAULT_BUNDLE_STATE: crate::app::bundle::BundleState =
        crate::app::bundle::BundleState {
            has_cache: false,
            version: String::new(),
            personas: Vec::new(),
            roles: Vec::new(),
            agents: Vec::new(),
            skills: Vec::new(),
            persona_details: Vec::new(),
            role_details: Vec::new(),
        };

    fn make_ctx<'a>(models: &'a ModelState) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: crate::settings::PagerLocalSnapshot {
                multiline_mode: false,
                yolo_mode: false,
                ..crate::settings::PagerLocalSnapshot::default()
            },
        }
    }

    #[test]
    fn empty_args_dispatches_open_settings() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        let cmd = SettingsCommand;
        let result = cmd.run(&mut ctx, "");
        assert!(
            matches!(result, CommandResult::Action(Action::OpenSettings)),
            "expected OpenSettings, got {result:?}",
        );
    }

    #[test]
    fn open_arg_dispatches_open_settings() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        let cmd = SettingsCommand;
        for args in ["open", "  "] {
            let result = cmd.run(&mut ctx, args);
            assert!(
                matches!(result, CommandResult::Action(Action::OpenSettings)),
                "expected OpenSettings for args={args:?}, got {result:?}",
            );
        }
    }

    #[test]
    fn reset_defaults_dispatches_runtime_extension() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        assert!(matches!(
            SettingsCommand.run(&mut ctx, "reset-defaults"),
            CommandResult::Action(Action::RuntimeExtension { method, .. })
                if method == "_atelier/config/reset_defaults"
        ));
    }

    #[test]
    fn aliases_are_registered() {
        let cmd = SettingsCommand;
        assert_eq!(cmd.name(), "settings");
        assert_eq!(cmd.aliases(), &["config", "preferences", "prefs"]);
    }
}
