//! `/model` (alias `/m`) — persist and switch the active Provider/model.
//! Chained autocomplete walks Provider → model → model-specific effort.

use agent_client_protocol as acp;
use atelier_shell::sampling::types::supports_reasoning_effort_meta;

use crate::acp::model_state::ModelState;
use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};
use crate::slash::commands::effort_levels::build_effort_arg_items;

/// Switch the active model (and optionally its reasoning effort).
pub struct ModelCommand;

impl SlashCommand for ModelCommand {
    fn name(&self) -> &str {
        "model"
    }

    fn aliases(&self) -> &[&str] {
        &["m"]
    }

    fn description(&self) -> &str {
        "Switch the active model"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn offered_when_session_less(&self) -> bool {
        // The dashboard offers `/model` to pick the model for the next
        // spawned agent (intercepted in `dispatch_dashboard_dispatch_slash`).
        true
    }

    fn usage(&self) -> &str {
        "/model <provider/model> [effort]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<provider/model> [effort]")
    }

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        if ctx.models.is_empty() {
            return None;
        }

        if let Some(model_id) = detect_effort_phase(ctx.models, args_query) {
            return Some(build_effort_items(ctx.models, &model_id));
        }
        if let Some(provider_id) = provider_phase(ctx.models, args_query) {
            return Some(build_model_items(ctx.models, &provider_id));
        }
        Some(build_provider_items(ctx.models))
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return CommandResult::Action(Action::OpenSlashArgPicker {
                command: self.name().to_owned(),
            });
        }

        let trimmed = trimmed.strip_prefix("default ").unwrap_or(trimmed).trim();

        // Prefer the exact composite key. Display-name matching remains for
        // command-line compatibility, but every successful selection is
        // canonicalized to `provider/model` before it is persisted.
        if let Some(id) = ctx.models.resolve_by_name_or_id(trimmed) {
            return persist_model_selection(id, None);
        }

        // Trailing effort token + reasoning model → typed switch. Successful
        // completion switches only the current Session.
        // Resolve via the shared gate so a rejected
        // level (e.g. `none` on atelier-4.5) surfaces the effort error with the
        // model's offered ids — not "Unknown model: … none".
        if let Some((prefix, token)) = split_trailing_token(trimmed)
            && let Some(id) = resolve_model(ctx.models, prefix)
            && ctx
                .models
                .available
                .get(&id)
                .map(supports_reasoning_effort)
                .unwrap_or(false)
        {
            return match ctx.models.resolve_effort_for_model(&id, token) {
                Ok(effort) => persist_model_selection(id, Some(effort)),
                Err(err) => CommandResult::Error(err.message()),
            };
        }

        CommandResult::Error(format!("Unknown model: {trimmed}"))
    }
}

/// Look up a model by case-insensitive display name OR model id match.
fn resolve_model(models: &ModelState, name: &str) -> Option<acp::ModelId> {
    models.resolve_by_name_or_id(name)
}

fn supports_reasoning_effort(info: &acp::ModelInfo) -> bool {
    supports_reasoning_effort_meta(info.meta.as_ref())
}

/// Split `args` into `(prefix, last_token)` on the final whitespace run.
/// Returns `None` when there is no interior whitespace to split on. The token is
/// resolved to an effort against the picked model's options by the caller.
fn split_trailing_token(args: &str) -> Option<(&str, &str)> {
    let (prefix, last) = args.rsplit_once(char::is_whitespace)?;
    let prefix = prefix.trim_end();
    if prefix.is_empty() || last.is_empty() {
        return None;
    }
    Some((prefix, last))
}

fn persist_model_selection(
    model_id: acp::ModelId,
    effort: Option<atelier_shell::sampling::types::ReasoningEffort>,
) -> CommandResult {
    CommandResult::Action(Action::RuntimeExtension {
        method: "_atelier/config/update".to_owned(),
        params: serde_json::json!({
            "model": model_id.0.as_ref(),
            "switch": true,
            "effort": effort.map(|value| value.as_str()),
        }),
    })
}

/// Returns the matched model id when `args_query` is
/// `"<provider/model> ..."`. Display names remain accepted for typed legacy
/// input, but interactive completion always uses the composite key.
fn detect_effort_phase(models: &ModelState, args_query: &str) -> Option<acp::ModelId> {
    let mut candidates: Vec<(&acp::ModelId, String)> = models
        .available
        .iter()
        .filter(|(_, info)| supports_reasoning_effort(info))
        .flat_map(|(id, info)| {
            [id.0.to_string(), info.name.clone()]
                .into_iter()
                .map(move |label| (id, label))
        })
        .collect();
    candidates.sort_by_key(|(_, label)| std::cmp::Reverse(label.len()));

    for (id, label) in candidates {
        if args_query.len() > label.len()
            && args_query.is_char_boundary(label.len())
            && args_query[..label.len()].eq_ignore_ascii_case(&label)
            && args_query[label.len()..].starts_with(char::is_whitespace)
        {
            return Some(id.clone());
        }
    }
    None
}

fn provider_phase(models: &ModelState, args_query: &str) -> Option<String> {
    let first = args_query.split_whitespace().next().unwrap_or(args_query);
    let (provider, _) = first.split_once('/')?;
    models
        .available
        .keys()
        .any(|id| {
            id.0.as_ref()
                .split_once('/')
                .is_some_and(|(candidate, _)| candidate == provider)
        })
        .then(|| provider.to_owned())
}

fn build_provider_items(models: &ModelState) -> Vec<ArgItem> {
    let mut providers = indexmap::IndexMap::<String, usize>::new();
    for id in models.available.keys() {
        if let Some((provider, _)) = id.0.as_ref().split_once('/') {
            *providers.entry(provider.to_owned()).or_default() += 1;
        }
    }
    providers
        .into_iter()
        .map(|(provider, count)| ArgItem {
            display: provider.clone(),
            match_text: provider.clone(),
            insert_text: format!("{provider}/"),
            description: format!(
                "{count} available model{}",
                if count == 1 { "" } else { "s" }
            ),
        })
        .collect()
}

/// One row per logical model for the selected Provider. Every row shows and
/// inserts its full composite ID, eliminating ambiguity across Providers.
fn build_model_items(models: &ModelState, provider_id: &str) -> Vec<ArgItem> {
    let current_id = models.current.as_ref();
    let mut items = Vec::new();
    for (id, info) in &models.available {
        let Some((provider, _)) = id.0.as_ref().split_once('/') else {
            continue;
        };
        if provider != provider_id {
            continue;
        }
        let is_current = current_id == Some(id);
        let supports = supports_reasoning_effort(info);
        let key = id.0.to_string();
        let display = if is_current {
            format!("{key} — {} (current)", info.name)
        } else {
            format!("{key} — {}", info.name)
        };
        let insert_text = if supports {
            format!("{key} ")
        } else {
            key.clone()
        };
        items.push(ArgItem {
            display,
            match_text: format!("{key} {}", info.name),
            insert_text,
            description: info.description.clone().unwrap_or_default(),
        });
    }
    items
}

/// One row per effort level for the `/model` chained effort phase.
/// `insert_text` is `"ModelName high"` so selecting a row completes both tokens.
fn build_effort_items(models: &ModelState, model_id: &acp::ModelId) -> Vec<ArgItem> {
    if !models.available.contains_key(model_id) {
        return Vec::new();
    }
    let model_name = model_id.0.to_string();
    let is_current_model = models.current.as_ref() == Some(model_id);
    let options = models.reasoning_effort_options_for(model_id);
    build_effort_arg_items(
        &options,
        models.reasoning_effort,
        is_current_model,
        |option| format!("{model_name} {}", option.id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn model_with_reasoning(id: &str, name: &str) -> (acp::ModelId, acp::ModelInfo) {
        let id = acp::ModelId::new(Arc::from(id));
        let info = acp::ModelInfo::new(id.clone(), name.to_string()).meta(
            serde_json::json!({
                "supportsReasoningEffort": true,
                "reasoningEfforts": [
                    { "value": "low", "label": "Low" },
                    { "value": "medium", "label": "Medium", "default": true },
                    { "value": "high", "label": "High" },
                ],
            })
            .as_object()
            .cloned(),
        );
        (id, info)
    }

    fn plain_model(id: &str, name: &str) -> (acp::ModelId, acp::ModelInfo) {
        let id = acp::ModelId::new(Arc::from(id));
        let info = acp::ModelInfo::new(id.clone(), name.to_string());
        (id, info)
    }

    static EMPTY_BUNDLE: crate::app::bundle::BundleState = crate::app::bundle::BundleState {
        has_cache: false,
        version: String::new(),
        personas: Vec::new(),
        roles: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        persona_details: Vec::new(),
        role_details: Vec::new(),
    };

    fn dummy_exec_ctx(models: &ModelState) -> CommandExecCtx<'_> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &EMPTY_BUNDLE,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: crate::settings::PagerLocalSnapshot {
                multiline_mode: false,
                yolo_mode: false,
                ..crate::settings::PagerLocalSnapshot::default()
            },
        }
    }

    #[test]
    fn split_trailing_token_splits_on_final_whitespace() {
        assert_eq!(
            split_trailing_token("Reasoning X high"),
            Some(("Reasoning X", "high"))
        );
        assert_eq!(
            split_trailing_token("reasoning-x  xhigh"),
            Some(("reasoning-x", "xhigh"))
        );
        // No interior whitespace → nothing to split off.
        assert!(split_trailing_token("reasoning-x-pro").is_none());
    }

    #[test]
    fn autocomplete_walks_provider_then_unambiguous_model_ids() {
        let mut state = ModelState::default();
        let (rid, rinfo) = model_with_reasoning("example/reasoning-x", "Reasoning X");
        let (pid, pinfo) = plain_model("other/atelier-4.5", "Atelier 4.5");
        state.available.insert(rid, rinfo);
        state.available.insert(pid, pinfo);

        let cmd = ModelCommand;
        let ctx = AppCtx {
            models: &state,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Fullscreen,
        };
        let providers = cmd.suggest_args(&ctx, "").unwrap();
        assert_eq!(providers.len(), 2);
        assert!(providers.iter().any(|item| item.insert_text == "example/"));
        assert!(providers.iter().any(|item| item.insert_text == "other/"));

        let models = cmd.suggest_args(&ctx, "example/").unwrap();
        assert_eq!(models.len(), 1);
        assert!(models[0].display.contains("example/reasoning-x"));
        assert_eq!(models[0].insert_text, "example/reasoning-x ");
    }

    #[test]
    fn trailing_space_after_reasoning_model_enters_effort_phase() {
        let mut state = ModelState::default();
        let (id, info) = model_with_reasoning("example/reasoning-x", "Reasoning X");
        state.available.insert(id, info);

        let cmd = ModelCommand;
        let ctx = AppCtx {
            models: &state,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Fullscreen,
        };
        let items = cmd.suggest_args(&ctx, "example/reasoning-x ").unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].insert_text, "example/reasoning-x low");
        assert_eq!(items[1].insert_text, "example/reasoning-x medium");
        assert_eq!(items[2].insert_text, "example/reasoning-x high");
        assert_eq!(items[0].display, "Low");
        assert!(items[0].match_text.starts_with("a "));
        assert!(items[2].match_text.starts_with("c "));
    }

    #[test]
    fn partial_effort_query_still_in_effort_phase() {
        let mut state = ModelState::default();
        let (id, info) = model_with_reasoning("example/reasoning-x", "Reasoning X");
        state.available.insert(id, info);

        let cmd = ModelCommand;
        let ctx = AppCtx {
            models: &state,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Fullscreen,
        };
        let items = cmd.suggest_args(&ctx, "example/reasoning-x h").unwrap();
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn partial_model_query_stays_in_selected_provider_phase() {
        let mut state = ModelState::default();
        let (id, info) = model_with_reasoning("example/reasoning-x", "Reasoning X");
        state.available.insert(id, info);

        let cmd = ModelCommand;
        let ctx = AppCtx {
            models: &state,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            screen_mode: crate::app::ScreenMode::Fullscreen,
        };
        let items = cmd.suggest_args(&ctx, "example/Reason").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text, "example/reasoning-x ");
    }

    #[test]
    fn run_persists_model_plus_effort_before_switching() {
        let mut state = ModelState::default();
        let (id, info) = model_with_reasoning("example/reasoning-x", "Reasoning X");
        state.available.insert(id, info);
        let mut ctx = dummy_exec_ctx(&state);
        let result = ModelCommand.run(&mut ctx, "example/reasoning-x high");
        match result {
            CommandResult::Action(Action::RuntimeExtension { method, params }) => {
                assert_eq!(method, "_atelier/config/update");
                assert_eq!(params["model"], "example/reasoning-x");
                assert_eq!(params["effort"], "high");
                assert_eq!(params["switch"], true);
            }
            other => panic!("expected persisted model selection, got {other:?}"),
        }
    }

    #[test]
    fn run_rejects_unoffered_effort_with_effort_error_not_unknown_model() {
        // Regression: previously `resolve_effort_token_for` returned None and
        // the handler fell through to `Unknown model: Reasoning X none`.
        let mut state = ModelState::default();
        let (id, info) = model_with_reasoning("reasoning-x", "Reasoning X");
        state.available.insert(id, info);
        let mut ctx = dummy_exec_ctx(&state);
        let result = ModelCommand.run(&mut ctx, "Reasoning X none");
        match result {
            CommandResult::Error(msg) => {
                assert!(
                    msg.contains("unknown effort level 'none'"),
                    "expected effort error, got {msg}"
                );
                assert!(
                    msg.contains("use one of:"),
                    "expected offered levels in message, got {msg}"
                );
                assert!(
                    !msg.to_lowercase().contains("unknown model"),
                    "must not misreport as unknown model: {msg}"
                );
                let offered = msg.split_once("; ").map(|(_, r)| r).unwrap_or("");
                assert!(
                    !offered.contains("none"),
                    "must not list none as offered: {msg}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_prefers_full_multi_word_model_name_over_prefix_plus_effort() {
        let mut state = ModelState::default();
        let (short_id, short_info) = model_with_reasoning("example/atelier", "Atelier");
        let (long_id, long_info) = model_with_reasoning("example/atelier-4.5", "Atelier 4.5");
        state.available.insert(short_id, short_info);
        state.available.insert(long_id.clone(), long_info);
        let mut ctx = dummy_exec_ctx(&state);
        let result = ModelCommand.run(&mut ctx, "Atelier 4.5");
        match result {
            CommandResult::Action(Action::RuntimeExtension { method, params }) => {
                assert_eq!(method, "_atelier/config/update");
                assert_eq!(params["model"], long_id.0.as_ref());
                assert!(params["effort"].is_null());
            }
            other => panic!("expected persisted Atelier 4.5 selection, got {other:?}"),
        }
    }

    #[test]
    fn run_rejects_effort_for_non_reasoning_model() {
        let mut state = ModelState::default();
        let (id, info) = plain_model("example/atelier-4.5", "Atelier 4.5");
        state.available.insert(id, info);
        let mut ctx = dummy_exec_ctx(&state);
        let result = ModelCommand.run(&mut ctx, "example/atelier-4.5 high");
        assert!(matches!(result, CommandResult::Error(_)));
    }

    #[test]
    fn run_bare_model_name_persists_the_composite_id() {
        let mut state = ModelState::default();
        let (id, info) = plain_model("example/atelier-4.5", "Atelier 4.5");
        state.available.insert(id.clone(), info);
        let mut ctx = dummy_exec_ctx(&state);
        let result = ModelCommand.run(&mut ctx, "Atelier 4.5");
        match result {
            CommandResult::Action(Action::RuntimeExtension { method, params }) => {
                assert_eq!(method, "_atelier/config/update");
                assert_eq!(params["model"], id.0.as_ref());
                assert_eq!(params["switch"], true);
            }
            other => panic!("expected persisted model selection, got {other:?}"),
        }
    }

    #[test]
    fn run_model_name_resolves_case_insensitively() {
        let mut state = ModelState::default();
        let (id, info) = plain_model("example/atelier-4.5", "Atelier 4.5");
        state.available.insert(id.clone(), info);
        let mut ctx = dummy_exec_ctx(&state);
        let result = ModelCommand.run(&mut ctx, "atelier 4.5");
        match result {
            CommandResult::Action(Action::RuntimeExtension { method, params }) => {
                assert_eq!(method, "_atelier/config/update");
                assert_eq!(params["model"], id.0.as_ref());
            }
            other => panic!("expected persisted model selection, got {other:?}"),
        }
    }

    #[test]
    fn legacy_default_subcommand_uses_the_same_persist_and_switch_path() {
        let mut state = ModelState::default();
        let (id, info) = plain_model("provider/model", "Model");
        state.available.insert(id, info);
        let mut ctx = dummy_exec_ctx(&state);
        let result = ModelCommand.run(&mut ctx, "default provider/model");
        match result {
            CommandResult::Action(Action::RuntimeExtension { method, params }) => {
                assert_eq!(method, "_atelier/config/update");
                assert_eq!(params["model"], "provider/model");
                assert_eq!(params["switch"], true);
            }
            other => panic!("expected config update extension, got {other:?}"),
        }
    }
}
