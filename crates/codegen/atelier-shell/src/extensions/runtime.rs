//! Runtime control-plane ACP methods.
//!
//! The handlers are intentionally read-mostly. They expose local runtime
//! state, request/context snapshots, replayable trace records, and explicit
//! recovery requests without adding a second session execution engine.

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::runtime_control::{RuntimeState, RuntimeStatus, now_millis};
use crate::session::{SessionCommand, SessionLiveState};
use agent_client_protocol as acp;
use serde::Deserialize;
use serde_json::Value;

pub const PROTOCOL_INFO: &str = "_atelier/protocol/info";
pub const RUNTIME_STATUS: &str = "_atelier/runtime/status";
pub const RUNTIME_DOCTOR: &str = "_atelier/runtime/doctor";
pub const RUNTIME_RECOVER: &str = "_atelier/runtime/recover";
pub const RUNTIME_CANCEL: &str = "_atelier/runtime/cancel";
pub const RUNTIME_RETRY: &str = "_atelier/runtime/retry";
pub const RUNTIME_TASKS: &str = "_atelier/runtime/tasks";
pub const CONTEXT_CURRENT: &str = "_atelier/context/current";
pub const CONTEXT_LIST: &str = "_atelier/context/list";
pub const CONTEXT_GET: &str = "_atelier/context/get";
pub const REQUEST_LIST: &str = "_atelier/request/list";
pub const REQUEST_GET: &str = "_atelier/request/get";
pub const TRACE_GET: &str = "_atelier/trace/get";

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionParams {
    session_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestParams {
    session_id: Option<String>,
    request_id: Option<String>,
    event_id: Option<u64>,
    after_event_id: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoctorParams {
    stale_after_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoverParams {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelParams {
    session_id: String,
    #[serde(default = "default_true")]
    cancel_subagents: bool,
}

fn default_true() -> bool {
    true
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        PROTOCOL_INFO | "atelier/protocol/info" => protocol_info(args),
        RUNTIME_STATUS | "atelier/runtime/status" => runtime_status(agent, args),
        RUNTIME_DOCTOR | "atelier/runtime/doctor" => runtime_doctor(agent, args),
        RUNTIME_RECOVER | "atelier/runtime/recover" => runtime_recover(agent, args),
        RUNTIME_CANCEL | "atelier/runtime/cancel" => runtime_cancel(agent, args),
        RUNTIME_RETRY | "atelier/runtime/retry" => runtime_retry(agent, args).await,
        RUNTIME_TASKS | "atelier/runtime/tasks" => runtime_tasks(agent, args).await,
        CONTEXT_CURRENT | "atelier/context/current" => context_current(agent, args),
        CONTEXT_LIST | "atelier/context/list" => context_list(agent, args),
        CONTEXT_GET | "atelier/context/get" => context_get(agent, args),
        REQUEST_LIST | "atelier/request/list" => request_list(agent, args),
        REQUEST_GET | "atelier/request/get" => request_get(agent, args),
        TRACE_GET | "atelier/trace/get" => trace_get(agent, args),
        _ => Err(acp::Error::method_not_found()),
    }
}

fn protocol_info(args: &acp::ExtRequest) -> ExtResult {
    let requested = requested_protocol_versions(args);
    let base = protocol_info_document();
    let negotiated = (!requested.is_empty())
        .then(|| xai_acp_lib::ProtocolInfo::negotiate(&requested, &base.supported_versions))
        .flatten();
    let protocol = base.with_negotiated_version(negotiated);
    to_raw_response(&protocol)
}

fn requested_protocol_versions(args: &acp::ExtRequest) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(args.params.get()) else {
        return Vec::new();
    };
    [
        "supportedVersions",
        "supported_versions",
        "protocolVersions",
        "versions",
    ]
    .into_iter()
    .find_map(|key| value.get(key).and_then(Value::as_array))
    .map(|versions| {
        versions
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

fn protocol_info_document() -> xai_acp_lib::ProtocolInfo {
    xai_acp_lib::ProtocolInfo::new(
        xai_acp_lib::ATELIER_PROTOCOL_VERSION,
        xai_acp_lib::ATELIER_SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .copied(),
        xai_acp_lib::ATELIER_PROTOCOL_CAPABILITIES.iter().copied(),
        [
            PROTOCOL_INFO,
            RUNTIME_STATUS,
            RUNTIME_DOCTOR,
            RUNTIME_RECOVER,
            RUNTIME_CANCEL,
            RUNTIME_RETRY,
            RUNTIME_TASKS,
            CONTEXT_CURRENT,
            CONTEXT_LIST,
            CONTEXT_GET,
            REQUEST_LIST,
            REQUEST_GET,
            TRACE_GET,
            crate::extensions::roles::ROLE_LIST,
            crate::extensions::roles::ROLE_GET,
            crate::extensions::roles::ROLE_UPDATE,
            crate::extensions::roles::ROLE_TEST,
            crate::extensions::policy::POLICY_INFO,
            crate::extensions::policy::POLICY_EVALUATE,
            crate::extensions::policy::POLICY_REDACT,
            crate::extensions::policy::POLICY_CONFIGURE,
            crate::extensions::sandbox::SANDBOX_STATUS,
            crate::extensions::sandbox::SANDBOX_DOCTOR,
            crate::extensions::provider::PROVIDER_LIST,
            crate::extensions::provider::PROVIDER_CREATE,
            crate::extensions::provider::PROVIDER_UPDATE,
            crate::extensions::provider::PROVIDER_DELETE,
            crate::extensions::provider::PROVIDER_TEST,
            crate::extensions::provider::PROVIDER_REFRESH_MODELS,
            crate::extensions::provider::PROVIDER_ENABLE,
            crate::extensions::provider::MODEL_LIST,
            crate::extensions::provider::MODEL_UPDATE,
            crate::extensions::provider::MODEL_SET_DEFAULT,
            crate::extensions::provider::MODEL_SET_CAPABILITIES,
            crate::extensions::provider::CREDENTIAL_STATUS,
            crate::extensions::provider::CREDENTIAL_SET,
            crate::extensions::provider::CREDENTIAL_DELETE,
        ],
    )
}

fn runtime_status(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: SessionParams = parse_params(args)?;
    let statuses = if let Some(session_id) = params.session_id.as_deref() {
        vec![
            agent
                .runtime_status(session_id)
                .unwrap_or_else(|| fallback_status(agent, session_id)),
        ]
    } else {
        let mut statuses = agent.runtime_statuses();
        for session_id in session_ids(agent) {
            if !statuses
                .iter()
                .any(|status| status.session_id == session_id)
            {
                statuses.push(fallback_status(agent, &session_id));
            }
        }
        statuses.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        statuses
    };
    to_raw_response(&serde_json::json!({
        "protocolVersion": xai_acp_lib::ATELIER_PROTOCOL_VERSION,
        "statuses": statuses,
    }))
}

fn fallback_status(agent: &MvpAgent, session_id: &str) -> RuntimeStatus {
    let now = now_millis();
    let handle = agent.session_handle_now(session_id);
    let running = handle
        .as_ref()
        .and_then(|handle| handle.current_prompt_id.lock().ok())
        .and_then(|prompt| prompt.clone())
        .is_some();
    let waiting_for_permission = handle
        .as_ref()
        .and_then(|handle| handle.pending_interactions.lock().ok())
        .is_some_and(|pending| !pending.is_empty());
    let state = if waiting_for_permission {
        RuntimeState::WaitingForPermission
    } else if running
        || matches!(
            agent.session_liveness(session_id),
            Some(SessionLiveState::Working)
        )
    {
        RuntimeState::StreamingResponse
    } else if matches!(
        agent.session_liveness(session_id),
        Some(SessionLiveState::DeadFailed)
    ) {
        RuntimeState::Failed
    } else {
        RuntimeState::Paused
    };
    let model = handle.as_ref().map(|handle| handle.model_id.0.to_string());
    let (provider, model_name) = model
        .as_deref()
        .and_then(|model| model.split_once('/'))
        .map(|(provider, model)| (Some(provider.to_owned()), Some(model.to_owned())))
        .unwrap_or((None, model));
    RuntimeStatus {
        session_id: session_id.to_owned(),
        state,
        started_at_ms: now,
        last_progress_at_ms: now,
        request_id: handle
            .as_ref()
            .and_then(|handle| handle.current_prompt_id.lock().ok())
            .and_then(|prompt| prompt.clone()),
        turn_id: None,
        role: "main".to_owned(),
        provider,
        model: model_name,
        timeout_ms: None,
        retry_count: 0,
        cancel_supported: handle.is_some(),
        diagnostic_message: None,
    }
}

fn runtime_doctor(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: DoctorParams = parse_params(args)?;
    let stale_after_ms = params.stale_after_ms.unwrap_or(30_000);
    let report = agent.runtime_doctor(now_millis(), stale_after_ms);
    to_raw_response(&report)
}

fn runtime_recover(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: RecoverParams = parse_params(args)?;
    to_raw_response(&agent.runtime_recover(&params.session_id))
}

fn runtime_cancel(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: CancelParams = parse_params(args)?;
    let Some(handle) = agent.session_handle_now(&params.session_id) else {
        return to_raw_response(&serde_json::json!({
            "accepted": false,
            "sessionId": params.session_id,
            "message": "session not found",
        }));
    };
    let accepted = handle
        .cmd_tx
        .send(SessionCommand::Cancel {
            cancel_subagents: params.cancel_subagents,
            kill_background_tasks: false,
            rewind_if_pristine: false,
            trigger: Some("runtime_cancel".to_owned()),
        })
        .is_ok();
    if accepted {
        agent.runtime_update_status(
            &acp::SessionId::new(params.session_id.as_str()),
            RuntimeState::Paused,
            Some("cancel requested by client".to_owned()),
        );
    }
    to_raw_response(&serde_json::json!({
        "accepted": accepted,
        "sessionId": params.session_id,
    }))
}

async fn runtime_retry(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: RequestParams = parse_params(args)?;
    let request_id = params.request_id.unwrap_or_default();
    if request_id.is_empty() {
        return Err(acp::Error::invalid_params().data("requestId is required"));
    }
    let Some(snapshot) = agent.runtime_request(&request_id) else {
        return to_raw_response(&serde_json::json!({
            "accepted": false,
            "requestId": request_id,
            "known": false,
            "message": "request not found",
        }));
    };
    match agent.retry_request(&request_id).await {
        Ok(retry_request_id) => to_raw_response(&serde_json::json!({
            "accepted": true,
            "requestId": request_id,
            "retryRequestId": retry_request_id,
            "sessionId": snapshot.session_id,
        })),
        Err(message) => to_raw_response(&serde_json::json!({
            "accepted": false,
            "requestId": request_id,
            "known": true,
            "message": message,
        })),
    }
}

async fn runtime_tasks(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: SessionParams = parse_params(args)?;
    let handles: Vec<_> = agent
        .runtime_session_handles(params.session_id.as_deref())
        .into_iter()
        .collect();
    let mut sessions = Vec::with_capacity(handles.len());
    for (session_id, handle) in handles {
        let tasks = handle.list_tasks().await.unwrap_or_default();
        sessions.push(serde_json::json!({
            "sessionId": session_id,
            "tasks": tasks,
        }));
    }
    to_raw_response(&serde_json::json!({ "sessions": sessions }))
}

fn context_current(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: SessionParams = parse_params(args)?;
    let request = params
        .session_id
        .as_deref()
        .and_then(|session_id| agent.runtime_requests(Some(session_id)).pop())
        .or_else(|| agent.runtime_requests(None).pop());
    let available = request
        .as_ref()
        .is_some_and(|request| !request.context_blocks.is_empty());
    to_raw_response(&serde_json::json!({
        "request": request,
        "available": available,
    }))
}

fn context_list(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: RequestParams = parse_params(args)?;
    let requests = agent.runtime_requests(params.session_id.as_deref());
    let contexts: Vec<_> = requests
        .into_iter()
        .map(|request| {
            serde_json::json!({
                "requestId": request.request_id,
                "sessionId": request.session_id,
                "blocks": request.context_blocks,
                "inputTokens": request.input_tokens,
                "outputTokenBudget": request.output_token_budget,
            })
        })
        .collect();
    to_raw_response(&serde_json::json!({ "contexts": contexts }))
}

fn context_get(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: RequestParams = parse_params(args)?;
    let request_id = params
        .request_id
        .ok_or_else(|| acp::Error::invalid_params().data("requestId is required"))?;
    let request = agent.runtime_request(&request_id);
    to_raw_response(&serde_json::json!({
        "requestId": request_id,
        "request": request,
    }))
}

fn request_list(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: RequestParams = parse_params(args)?;
    let mut requests = agent.runtime_requests(params.session_id.as_deref());
    if let Some(limit) = params.limit {
        let keep = limit.max(1);
        let start = requests.len().saturating_sub(keep);
        requests = requests.split_off(start);
    }
    to_raw_response(&serde_json::json!({ "requests": requests }))
}

fn request_get(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: RequestParams = parse_params(args)?;
    let request_id = params
        .request_id
        .ok_or_else(|| acp::Error::invalid_params().data("requestId is required"))?;
    to_raw_response(&serde_json::json!({
        "requestId": request_id,
        "request": agent.runtime_request(&request_id),
    }))
}

fn trace_get(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: RequestParams = parse_params(args)?;
    let after_event_id = params
        .after_event_id
        .or(params.event_id)
        .unwrap_or_default();
    let events = agent.runtime_events(
        params.session_id.as_deref(),
        after_event_id,
        params.limit.unwrap_or(256),
    );
    let (oldest_event_id, latest_event_id) = agent.runtime_event_bounds();
    let truncated = oldest_event_id.is_some_and(|oldest| after_event_id.saturating_add(1) < oldest);
    to_raw_response(&serde_json::json!({
        "events": events,
        "afterEventId": after_event_id,
        "oldestEventId": oldest_event_id,
        "latestEventId": latest_event_id,
        "truncated": truncated,
        "gap": truncated,
    }))
}

fn session_ids(agent: &MvpAgent) -> Vec<String> {
    agent
        .runtime_session_handles(None)
        .into_iter()
        .map(|(session_id, _)| session_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_info_advertises_version_capabilities_and_methods() {
        let info = protocol_info_document();

        assert_eq!(info.protocol_version, xai_acp_lib::ATELIER_PROTOCOL_VERSION);
        assert_eq!(
            info.supported_versions,
            xai_acp_lib::ATELIER_SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .map(|version| (*version).to_owned())
                .collect::<Vec<_>>()
        );
        assert!(
            info.capabilities
                .contains(&"provider_management".to_owned())
        );
        assert!(info.capabilities.contains(&"typed_hooks".to_owned()));

        for method in [
            PROTOCOL_INFO,
            CONTEXT_CURRENT,
            TRACE_GET,
            crate::extensions::roles::ROLE_UPDATE,
            crate::extensions::policy::POLICY_EVALUATE,
            crate::extensions::policy::POLICY_CONFIGURE,
            crate::extensions::sandbox::SANDBOX_STATUS,
            crate::extensions::provider::PROVIDER_REFRESH_MODELS,
        ] {
            assert!(
                info.methods.iter().any(|advertised| advertised == method),
                "missing advertised method: {method}"
            );
        }
    }

    #[test]
    fn protocol_info_negotiates_versions_from_client_params() {
        let raw = serde_json::value::to_raw_value(&serde_json::json!({
            "supportedVersions": ["1.0", xai_acp_lib::ATELIER_PROTOCOL_VERSION]
        }))
        .unwrap();
        let request = acp::ExtRequest::new(PROTOCOL_INFO, std::sync::Arc::from(raw));
        let response = protocol_info(&request).expect("protocol info response");
        let value: Value = serde_json::from_str(response.0.get()).unwrap();
        assert_eq!(
            value["negotiatedVersion"],
            xai_acp_lib::ATELIER_PROTOCOL_VERSION
        );

        let raw = serde_json::value::to_raw_value(&serde_json::json!({
            "supportedVersions": ["1.0"]
        }))
        .unwrap();
        let request = acp::ExtRequest::new(PROTOCOL_INFO, std::sync::Arc::from(raw));
        let response = protocol_info(&request).expect("protocol info response");
        let value: Value = serde_json::from_str(response.0.get()).unwrap();
        assert!(value.get("negotiatedVersion").is_none());
    }
}
