//! `/parallel` -- spawn several derived Agents from one immutable snapshot.

use atelier_provider::RoleId;
use serde_json::json;
use std::str::FromStr;

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

const SPAWN_PARALLEL: &str = "_atelier/agent/spawn_parallel";

pub struct ParallelCommand;

impl SlashCommand for ParallelCommand {
    fn name(&self) -> &str {
        "parallel"
    }

    fn description(&self) -> &str {
        "Spawn parallel derived Agents from one context snapshot"
    }

    fn usage(&self) -> &str {
        "/parallel <role> <prompt>; <role> <prompt> ..."
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

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<role> <prompt>; ...")
    }

    fn suggest_args(&self, _ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        if args_query.trim().is_empty() {
            return Some(role_items());
        }
        let first = args_query.split_whitespace().next().unwrap_or_default();
        if parse_parallel_role(first).is_ok() && args_query.ends_with(char::is_whitespace) {
            return None;
        }
        Some(role_items())
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
        let mut tasks = Vec::new();
        let task_args = match split_parallel_tasks(args) {
            Ok(tasks) => tasks,
            Err(error) => return CommandResult::Error(error),
        };
        for task in task_args {
            let task = task.trim();
            if task.is_empty() {
                continue;
            }
            let Some((role_name, prompt)) = task.split_once(char::is_whitespace) else {
                return CommandResult::Error(
                    "Usage: /parallel <role> <prompt>; <role> <prompt> ...".to_owned(),
                );
            };
            let role = match parse_parallel_role(role_name) {
                Ok(role) => role,
                Err(error) => return CommandResult::Error(error),
            };
            if prompt.is_empty() {
                return CommandResult::Error("parallel task prompt must not be empty".to_owned());
            }
            tasks.push(json!({
                "sessionId": session_id.to_string(),
                "role": role.as_str(),
                "prompt": prompt,
                "background": true,
            }));
        }
        if tasks.is_empty() {
            return CommandResult::Error("at least one parallel task is required".to_owned());
        }
        CommandResult::Action(Action::RuntimeExtension {
            method: SPAWN_PARALLEL.to_owned(),
            params: serde_json::Value::Array(tasks),
        })
    }
}

fn split_parallel_tasks(input: &str) -> Result<Vec<String>, String> {
    let mut tasks = Vec::new();
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
            None if character == ';' || character == '\n' => {
                tasks.push(std::mem::take(&mut current));
            }
            None => current.push(character),
        }
    }

    if quote.is_some() {
        return Err("unterminated quote in command arguments".to_owned());
    }
    tasks.push(current);
    Ok(tasks)
}

fn role_items() -> Vec<ArgItem> {
    [
        RoleId::Explore,
        RoleId::Implement,
        RoleId::Review,
        RoleId::Test,
    ]
    .into_iter()
    .map(|role| ArgItem {
        display: role.to_string(),
        match_text: role.to_string(),
        insert_text: format!("{role} "),
        description: "parallel derived Agent".to_owned(),
    })
    .collect()
}

fn parse_parallel_role(value: &str) -> Result<RoleId, String> {
    let role = RoleId::from_str(value).map_err(|error| error.to_string())?;
    if matches!(
        role,
        RoleId::Explore | RoleId::Implement | RoleId::Review | RoleId::Test
    ) {
        Ok(role)
    } else {
        Err(format!(
            "Role '{value}' cannot be spawned with /parallel; use explore, implement, review, or test"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{ParallelCommand, role_items};
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
    fn parses_multiple_tasks_into_one_parallel_request() {
        let mut command_ctx = ctx();
        let result = ParallelCommand.run(
            &mut command_ctx,
            "explore inspect Provider; review check Wire API; test design regression tests",
        );
        match result {
            CommandResult::Action(Action::RuntimeExtension { method, params }) => {
                assert_eq!(method, "_atelier/agent/spawn_parallel");
                assert_eq!(params.as_array().unwrap().len(), 3);
                assert_eq!(params[0]["role"], "explore");
                assert_eq!(params[1]["prompt"], "check Wire API");
            }
            other => panic!("expected parallel request, got {other:?}"),
        }
    }

    #[test]
    fn bare_parallel_opens_interactive_role_picker() {
        let mut command_ctx = ctx();
        assert!(matches!(
            ParallelCommand.run(&mut command_ctx, ""),
            CommandResult::Action(Action::OpenSlashArgPicker { command }) if command == "parallel"
        ));
    }

    #[test]
    fn quoted_semicolon_stays_inside_parallel_task_prompt() {
        let mut command_ctx = ctx();
        let result = ParallelCommand.run(
            &mut command_ctx,
            r#"explore "compare alpha; beta"; review check the conclusion"#,
        );
        match result {
            CommandResult::Action(Action::RuntimeExtension { params, .. }) => {
                assert_eq!(params.as_array().unwrap().len(), 2);
                assert_eq!(params[0]["prompt"], "compare alpha; beta");
                assert_eq!(params[1]["role"], "review");
            }
            other => panic!("expected parallel request, got {other:?}"),
        }
    }

    #[test]
    fn full_parallel_command_allows_exactly_the_interactive_roles() {
        let interactive = role_items()
            .into_iter()
            .map(|item| item.display)
            .collect::<Vec<_>>();
        for role in &interactive {
            let mut command_ctx = ctx();
            assert!(matches!(
                ParallelCommand.run(&mut command_ctx, &format!("{role} inspect provider")),
                CommandResult::Action(Action::RuntimeExtension { .. })
            ));
        }
        for role in ["main", "compact", "summary", "title"] {
            let mut command_ctx = ctx();
            assert!(
                matches!(
                    ParallelCommand.run(&mut command_ctx, &format!("{role} inspect provider")),
                    CommandResult::Error(_)
                ),
                "role {role} must not bypass the interactive role list"
            );
        }
    }

    #[test]
    fn parallel_rejects_internal_runtime_roles() {
        let mut command_ctx = ctx();
        for role in ["compact", "summary", "title"] {
            let result = ParallelCommand.run(&mut command_ctx, &format!("{role} internal work"));
            assert!(
                matches!(&result, CommandResult::Error(message) if message.contains("cannot be spawned")),
                "{role} unexpectedly accepted: {result:?}"
            );
        }
    }
}
