//! Runtime control-plane slash commands.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

const RUNTIME_STATUS: &str = "_atelier/runtime/status";
const RUNTIME_DOCTOR: &str = "_atelier/runtime/doctor";
const RUNTIME_RECOVER: &str = "_atelier/runtime/recover";
const REQUEST_LIST: &str = "_atelier/request/list";
const REQUEST_GET: &str = "_atelier/request/get";
const TRACE_GET: &str = "_atelier/trace/get";

fn extension(method: &str, params: serde_json::Value) -> CommandResult {
    CommandResult::Action(Action::RuntimeExtension {
        method: method.to_owned(),
        params,
    })
}

fn require_session(ctx: &CommandExecCtx<'_>) -> Option<CommandResult> {
    ctx.session_id
        .is_none()
        .then(|| CommandResult::Error("No active session".to_owned()))
}

pub struct StatusCommand;

impl SlashCommand for StatusCommand {
    fn name(&self) -> &str {
        "status"
    }

    fn description(&self) -> &str {
        "Show Runtime status"
    }

    fn usage(&self) -> &str {
        "/status"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        require_session(ctx).unwrap_or_else(|| extension(RUNTIME_STATUS, serde_json::json!({})))
    }
}

pub struct RequestCommand;

impl SlashCommand for RequestCommand {
    fn name(&self) -> &str {
        "request"
    }

    fn description(&self) -> &str {
        "List Runtime requests or inspect one request"
    }

    fn usage(&self) -> &str {
        "/request [request-id]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if let Some(error) = require_session(ctx) {
            return error;
        }
        let request_id = args.trim();
        if request_id.is_empty() {
            extension(REQUEST_LIST, serde_json::json!({}))
        } else {
            extension(REQUEST_GET, serde_json::json!({ "requestId": request_id }))
        }
    }
}

pub struct TraceCommand;

impl SlashCommand for TraceCommand {
    fn name(&self) -> &str {
        "trace"
    }

    fn description(&self) -> &str {
        "Show Runtime trace events"
    }

    fn usage(&self) -> &str {
        "/trace [after-event-id]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if let Some(error) = require_session(ctx) {
            return error;
        }
        let after_event_id = args.trim();
        if after_event_id.is_empty() {
            return extension(TRACE_GET, serde_json::json!({}));
        }
        match after_event_id.parse::<u64>() {
            Ok(after_event_id) => extension(
                TRACE_GET,
                serde_json::json!({ "afterEventId": after_event_id }),
            ),
            Err(_) => CommandResult::Error("Usage: /trace [after-event-id]".to_owned()),
        }
    }
}

pub struct DoctorCommand;

impl SlashCommand for DoctorCommand {
    fn name(&self) -> &str {
        "doctor"
    }

    fn description(&self) -> &str {
        "Show Runtime diagnostics"
    }

    fn usage(&self) -> &str {
        "/doctor"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        require_session(ctx).unwrap_or_else(|| extension(RUNTIME_DOCTOR, serde_json::json!({})))
    }
}

pub struct RecoverCommand;

impl SlashCommand for RecoverCommand {
    fn name(&self) -> &str {
        "recover"
    }

    fn description(&self) -> &str {
        "Request Runtime recovery"
    }

    fn usage(&self) -> &str {
        "/recover"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn run(&self, ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        require_session(ctx).unwrap_or_else(|| extension(RUNTIME_RECOVER, serde_json::json!({})))
    }
}
