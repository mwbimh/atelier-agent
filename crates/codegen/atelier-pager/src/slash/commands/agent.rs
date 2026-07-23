//! `/agent` -- explicitly derive a new Agent from the current session.

use atelier_provider::RoleId;
use serde_json::json;
use std::str::FromStr;

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

const SPAWN_DERIVED: &str = "_atelier/agent/spawn_derived";

pub struct AgentCommand;

impl SlashCommand for AgentCommand {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "Derive an Agent with explicit context inheritance"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/agent [--fresh] [--background] <role> [--append <context>] <prompt>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<role> <prompt>")
    }

    fn suggest_args(&self, _ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let query = args_query.trim();
        if query.is_empty() {
            return Some(derived_role_items(""));
        }
        let first = query.split_whitespace().next().unwrap_or_default();
        if parse_derived_role(first).is_ok() && query.ends_with(char::is_whitespace) {
            return None;
        }
        Some(derived_role_items(query))
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let Some(session_id) = ctx.session_id else {
            return CommandResult::Error("No active session".to_owned());
        };
        if args.trim().is_empty() {
            return CommandResult::Action(Action::OpenSlashArgPicker {
                command: self.name().to_owned(),
            });
        }
        let parsed = match parse_agent_args(args) {
            Ok(parsed) => parsed,
            Err(error) => return CommandResult::Error(error),
        };
        CommandResult::Action(Action::RuntimeExtension {
            method: SPAWN_DERIVED.to_owned(),
            params: json!({
                "sessionId": session_id.to_string(),
                "role": parsed.role.as_str(),
                "prompt": parsed.prompt,
                "appendContext": parsed.append_context,
                "background": parsed.background,
                "fresh": parsed.fresh,
            }),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AgentArgs {
    pub role: RoleId,
    pub prompt: String,
    pub append_context: Option<String>,
    pub background: bool,
    pub fresh: bool,
}

pub(crate) fn parse_agent_args(args: &str) -> Result<AgentArgs, String> {
    let tokens = tokenize(args)?;
    let mut fresh = false;
    let mut background = false;
    let mut index = 0;
    while let Some(token) = tokens.get(index).map(String::as_str) {
        match token {
            "--fresh" => {
                fresh = true;
                index += 1;
            }
            "--background" | "--bg" => {
                background = true;
                index += 1;
            }
            _ => break,
        }
    }
    let role_name = tokens.get(index).ok_or_else(|| {
        "Usage: /agent [--fresh] [--background] <role> [--append <context>] <prompt>".to_owned()
    })?;
    let role = parse_derived_role(role_name)?;
    index += 1;

    let mut append_context = None;
    let mut prompt = Vec::new();
    while let Some(token) = tokens.get(index) {
        if token == "--append" {
            index += 1;
            let context = tokens
                .get(index)
                .ok_or_else(|| "--append requires a context string".to_owned())?;
            append_context = Some(context.clone());
        } else {
            prompt.push(token.clone());
        }
        index += 1;
    }
    if prompt.is_empty() {
        return Err(
            "Usage: /agent [--fresh] [--background] <role> [--append <context>] <prompt>"
                .to_owned(),
        );
    }
    Ok(AgentArgs {
        role,
        prompt: prompt.join(" "),
        append_context,
        background,
        fresh,
    })
}

pub(crate) fn tokenize(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        match quote {
            Some(delimiter) if character == delimiter => quote = None,
            Some(delimiter) if character == '\\' && characters.peek() == Some(&delimiter) => {
                current.push(delimiter);
                characters.next();
            }
            Some(_) => current.push(character),
            None if character == '\'' || character == '"' => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if quote.is_some() {
        return Err("unterminated quote in command arguments".to_owned());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn derived_role_items(query: &str) -> Vec<ArgItem> {
    let normalized = query.to_lowercase();
    [
        RoleId::Explore,
        RoleId::Implement,
        RoleId::Review,
        RoleId::Test,
        RoleId::Main,
    ]
    .into_iter()
    .filter(|role| role.as_str().starts_with(&normalized))
    .map(|role| ArgItem {
        display: role.to_string(),
        match_text: role.to_string(),
        insert_text: format!("{role} "),
        description: "derived Agent Role".to_owned(),
    })
    .collect()
}

fn parse_derived_role(value: &str) -> Result<RoleId, String> {
    let role = RoleId::from_str(value).map_err(|error| error.to_string())?;
    if matches!(
        role,
        RoleId::Explore | RoleId::Implement | RoleId::Review | RoleId::Test | RoleId::Main
    ) {
        Ok(role)
    } else {
        Err(format!(
            "Role '{value}' cannot be spawned with /agent; use main, explore, implement, review, or test"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentCommand, derived_role_items, parse_agent_args, tokenize};
    use crate::app::actions::Action;
    use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};
    use atelier_provider::RoleId;

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
    fn parses_role_prompt_and_append_context() {
        assert_eq!(
            parse_agent_args(
                r#"explore --append "focus on Windows worker recovery" inspect Provider loading"#
            )
            .unwrap(),
            super::AgentArgs {
                role: RoleId::Explore,
                prompt: "inspect Provider loading".to_owned(),
                append_context: Some("focus on Windows worker recovery".to_owned()),
                background: false,
                fresh: false,
            }
        );
    }

    #[test]
    fn parses_fresh_background_flags_before_role() {
        let parsed = parse_agent_args("--fresh --background review check the diff").unwrap();
        assert!(parsed.fresh);
        assert!(parsed.background);
        assert_eq!(parsed.role, RoleId::Review);
    }

    #[test]
    fn bare_agent_opens_role_picker() {
        let mut command_ctx = ctx();
        assert!(matches!(
            AgentCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::OpenSlashArgPicker { command }) if command == "agent"
        ));
    }

    #[test]
    fn full_agent_command_emits_spawn_request() {
        let mut command_ctx = ctx();
        let result = AgentCommand.run(&mut command_ctx, "explore inspect Provider");
        assert!(matches!(
            result,
            CommandResult::Action(Action::RuntimeExtension { method, params })
                if method == "_atelier/agent/spawn_derived"
                    && params["role"] == "explore"
                    && params["prompt"] == "inspect Provider"
        ));
    }

    #[test]
    fn full_agent_command_allows_exactly_the_interactive_roles() {
        let interactive = derived_role_items("")
            .into_iter()
            .map(|item| item.display)
            .collect::<Vec<_>>();
        for role in &interactive {
            assert!(
                parse_agent_args(&format!("{role} inspect provider")).is_ok(),
                "interactive role {role} must work in the full command"
            );
        }
        for role in ["compact", "summary", "title"] {
            assert!(
                parse_agent_args(&format!("{role} inspect provider")).is_err(),
                "internal role {role} must not bypass the interactive role list"
            );
        }
    }

    #[test]
    fn tokenizer_rejects_unterminated_quotes() {
        assert!(tokenize("explore --append \"missing").is_err());
    }

    #[test]
    fn tokenizer_preserves_backslashes_in_quoted_windows_paths() {
        assert_eq!(
            tokenize(r#"explore "inspect C:\Users\alice\atelier repo""#).unwrap(),
            vec![
                "explore".to_owned(),
                r#"inspect C:\Users\alice\atelier repo"#.to_owned(),
            ]
        );
    }

    #[test]
    fn internal_runtime_roles_cannot_be_spawned_by_users() {
        for role in ["compact", "summary", "title"] {
            let error = parse_agent_args(&format!("{role} run internal work"))
                .expect_err("internal runtime roles must not be user-spawnable");
            assert!(
                error.contains("cannot be spawned"),
                "unexpected error: {error}"
            );
        }
    }
}
