//! Local runtime observability and recovery state.
//!
//! This module deliberately contains no network client and no UI concerns. It
//! is the small, in-process control plane shared by the ACP extensions and the
//! session actor integration. Secrets are never stored here; provider payloads
//! are expected to be sanitized by the caller before they are recorded.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

pub use atelier_acp_runtime::RuntimeState;

const DEFAULT_EVENT_LIMIT: usize = 512;
const DEFAULT_REQUEST_LIMIT: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub session_id: String,
    pub state: RuntimeState,
    pub started_at_ms: u64,
    pub last_progress_at_ms: u64,
    pub request_id: Option<String>,
    pub turn_id: Option<String>,
    pub role: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub timeout_ms: Option<u64>,
    pub retry_count: u32,
    pub cancel_supported: bool,
    pub diagnostic_message: Option<String>,
}

/// Stable task identity exposed by the third-batch runtime control plane.
///
/// `RuntimeStatus` is intentionally a live per-session view.  This separate
/// record keeps completed and detached work queryable after a later request
/// starts in the same session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTask {
    #[serde(rename = "taskId", alias = "id")]
    pub id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub agent_id: String,
    pub role: String,
    pub state: RuntimeState,
    pub started_at_ms: u64,
    pub last_event_id: u64,
    /// Whether a client may attach to this task's live turn.
    ///
    /// Auxiliary requests such as `/btw` are observable but do not own a
    /// session turn and must not be attached or cancelled as if they did.
    #[serde(default = "default_task_attachable")]
    pub attachable: bool,
    #[serde(default)]
    pub diagnostic_message: Option<String>,
}

fn default_task_attachable() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextBlock {
    pub name: String,
    pub source: String,
    pub tokens: u64,
    pub redacted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestSnapshot {
    pub request_id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub role: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub wire_api: Option<String>,
    #[serde(default)]
    pub wire_api_source: Option<String>,
    pub effort: Option<String>,
    pub fast_mode: Option<bool>,
    pub payload: Value,
    pub context_blocks: Vec<ContextBlock>,
    pub input_tokens: u64,
    pub output_token_budget: Option<u64>,
    pub first_token_latency_ms: Option<u64>,
    pub total_duration_ms: Option<u64>,
    pub retry_count: u32,
    pub http_status: Option<u16>,
    pub error_stage: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub state: RuntimeState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TraceRecord {
    pub event_id: u64,
    pub timestamp_ms: u64,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub kind: String,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorIssue {
    pub session_id: String,
    pub request_id: Option<String>,
    pub state: RuntimeState,
    pub stale_for_ms: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub checked_at_ms: u64,
    pub stale_after_ms: u64,
    pub issues: Vec<DoctorIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    Noop,
    Requested,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryResult {
    pub session_id: String,
    pub action: RecoveryAction,
    pub message: String,
}

#[derive(Debug)]
pub struct RuntimeControl {
    statuses: HashMap<String, RuntimeStatus>,
    tasks: VecDeque<RuntimeTask>,
    task_first_event_ids: HashMap<String, u64>,
    requests: VecDeque<RequestSnapshot>,
    events: VecDeque<TraceRecord>,
    pending_retry_counts: HashMap<String, u32>,
    next_event_id: u64,
    request_limit: usize,
    event_limit: usize,
    event_watch: tokio::sync::watch::Sender<u64>,
}

impl Default for RuntimeControl {
    fn default() -> Self {
        Self::new(DEFAULT_REQUEST_LIMIT, DEFAULT_EVENT_LIMIT)
    }
}

impl RuntimeControl {
    pub fn new(request_limit: usize, event_limit: usize) -> Self {
        let (event_watch, _) = tokio::sync::watch::channel(0);
        Self {
            statuses: HashMap::new(),
            tasks: VecDeque::new(),
            task_first_event_ids: HashMap::new(),
            requests: VecDeque::new(),
            events: VecDeque::new(),
            pending_retry_counts: HashMap::new(),
            next_event_id: 0,
            request_limit: request_limit.max(1),
            event_limit: event_limit.max(1),
            event_watch,
        }
    }

    pub fn begin_request(
        &mut self,
        session_id: impl Into<String>,
        request_id: impl Into<String>,
        turn_id: Option<String>,
        role: impl Into<String>,
        provider: Option<String>,
        model: Option<String>,
        now_ms: u64,
    ) {
        let session_id = session_id.into();
        let request_id = request_id.into();
        let role = role.into();
        let retry_count = self
            .pending_retry_counts
            .remove(&request_id)
            .unwrap_or_default();
        self.statuses.insert(
            session_id.clone(),
            RuntimeStatus {
                session_id: session_id.clone(),
                state: RuntimeState::PreparingContext,
                started_at_ms: now_ms,
                last_progress_at_ms: now_ms,
                request_id: Some(request_id.clone()),
                turn_id: turn_id.clone(),
                role: role.clone(),
                provider: provider.clone(),
                model: model.clone(),
                timeout_ms: None,
                retry_count,
                cancel_supported: true,
                diagnostic_message: None,
            },
        );
        self.tasks.retain(|task| task.id != request_id);
        self.tasks.push_back(RuntimeTask {
            id: request_id.clone(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            agent_id: session_id.clone(),
            role: role.clone(),
            state: RuntimeState::PreparingContext,
            started_at_ms: now_ms,
            last_event_id: 0,
            attachable: true,
            diagnostic_message: None,
        });
        self.requests.push_back(RequestSnapshot {
            request_id: request_id.clone(),
            session_id: session_id.clone(),
            turn_id,
            role,
            provider,
            model,
            wire_api: None,
            wire_api_source: None,
            effort: None,
            fast_mode: None,
            payload: Value::Object(Default::default()),
            context_blocks: Vec::new(),
            input_tokens: 0,
            output_token_budget: None,
            first_token_latency_ms: None,
            total_duration_ms: None,
            retry_count,
            http_status: None,
            error_stage: None,
            started_at_ms: now_ms,
            finished_at_ms: None,
            state: RuntimeState::PreparingContext,
        });
        self.trim();
        let event_id = self.record_event_at(
            now_ms,
            Some(session_id),
            Some(request_id.clone()),
            "request.started",
            Value::Null,
        );
        if let Some(task) = self.tasks.back_mut() {
            task.last_event_id = event_id;
        }
        self.task_first_event_ids
            .insert(request_id.clone(), event_id);
        self.trim();
    }

    pub fn update_status(
        &mut self,
        session_id: &str,
        state: RuntimeState,
        now_ms: u64,
        diagnostic_message: Option<String>,
    ) -> bool {
        let request_id = {
            let Some(status) = self.statuses.get_mut(session_id) else {
                return false;
            };
            status.state = state;
            status.last_progress_at_ms = now_ms;
            if diagnostic_message.is_some() {
                status.diagnostic_message = diagnostic_message
                    .as_deref()
                    .map(atelier_acp_runtime::redact_text);
            }
            status.request_id.clone()
        };
        if let Some(request_id) = request_id.clone()
            && let Some(request) = self
                .requests
                .iter_mut()
                .rev()
                .find(|request| request.request_id == request_id)
        {
            request.state = state;
        }
        let event_id = self.record_event_at(
            now_ms,
            Some(session_id.to_owned()),
            request_id.clone(),
            "runtime.state_changed",
            serde_json::json!({ "state": state }),
        );
        if let Some(request_id) = request_id
            && let Some(task) = self
                .tasks
                .iter_mut()
                .rev()
                .find(|task| task.id == request_id)
        {
            task.state = state;
            task.last_event_id = event_id;
            if diagnostic_message.is_some() {
                task.diagnostic_message = diagnostic_message
                    .as_deref()
                    .map(atelier_acp_runtime::redact_text);
            }
        }
        true
    }

    /// Record the first streamed token for the foreground request and move the
    /// visible Runtime state into streaming. Repeated calls only refresh
    /// progress; the first-token latency remains stable.
    pub fn mark_first_token(&mut self, session_id: &str, now_ms: u64) -> bool {
        let started_at_ms = self
            .statuses
            .get(session_id)
            .map(|status| status.started_at_ms);
        if !self.update_status(session_id, RuntimeState::StreamingResponse, now_ms, None) {
            return false;
        }
        let Some(started_at_ms) = started_at_ms else {
            return false;
        };
        let request_id = self
            .statuses
            .get(session_id)
            .and_then(|status| status.request_id.clone());
        if let Some(request_id) = request_id
            && let Some(request) = self
                .requests
                .iter_mut()
                .rev()
                .find(|request| request.request_id == request_id)
            && request.first_token_latency_ms.is_none()
        {
            request.first_token_latency_ms = Some(now_ms.saturating_sub(started_at_ms));
        }
        true
    }

    pub fn mark_retry(
        &mut self,
        session_id: &str,
        retry_count: u32,
        now_ms: u64,
        diagnostic_message: Option<String>,
    ) -> bool {
        if !self.update_status(
            session_id,
            RuntimeState::WaitingForProvider,
            now_ms,
            diagnostic_message,
        ) {
            return false;
        }
        if let Some(status) = self.statuses.get_mut(session_id) {
            status.retry_count = retry_count;
        }
        let request_id = self
            .statuses
            .get(session_id)
            .and_then(|status| status.request_id.clone());
        if let Some(request_id) = request_id
            && let Some(request) = self
                .requests
                .iter_mut()
                .rev()
                .find(|request| request.request_id == request_id)
        {
            request.retry_count = retry_count;
        }
        true
    }

    pub fn mark_http_status(&mut self, session_id: &str, status_code: u16) -> bool {
        let request_id = self
            .statuses
            .get(session_id)
            .and_then(|status| status.request_id.clone());
        let Some(request_id) = request_id else {
            return false;
        };
        let Some(request) = self
            .requests
            .iter_mut()
            .rev()
            .find(|request| request.request_id == request_id)
        else {
            return false;
        };
        request.http_status = Some(status_code);
        true
    }

    /// Register a model request which is visible in the task registry but is
    /// not the session's foreground turn.  This is used for side queries and
    /// other auxiliary runtime work so that recording it never overwrites the
    /// live `RuntimeStatus` for the parent session.
    pub fn begin_auxiliary_task(
        &mut self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        turn_id: Option<String>,
        agent_id: impl Into<String>,
        role: impl Into<String>,
        state: RuntimeState,
        attachable: bool,
        now_ms: u64,
    ) {
        let task_id = task_id.into();
        let session_id = session_id.into();
        self.tasks.retain(|task| task.id != task_id);
        self.tasks.push_back(RuntimeTask {
            id: task_id.clone(),
            session_id: session_id.clone(),
            turn_id,
            agent_id: agent_id.into(),
            role: role.into(),
            state,
            started_at_ms: now_ms,
            last_event_id: 0,
            attachable,
            diagnostic_message: None,
        });
        let event_id = self.record_event_at(
            now_ms,
            Some(session_id),
            Some(task_id.clone()),
            "runtime.task_started",
            serde_json::json!({ "state": state, "attachable": attachable }),
        );
        if let Some(task) = self.tasks.iter_mut().rev().find(|task| task.id == task_id) {
            task.last_event_id = event_id;
        }
        self.task_first_event_ids.insert(task_id.clone(), event_id);
        self.trim();
    }

    pub fn update_task(
        &mut self,
        task_id: &str,
        state: RuntimeState,
        now_ms: u64,
        diagnostic_message: Option<String>,
    ) -> bool {
        let redacted_diagnostic = diagnostic_message
            .as_deref()
            .map(atelier_acp_runtime::redact_text);
        let session_id = {
            let Some(task) = self.tasks.iter_mut().rev().find(|task| task.id == task_id) else {
                return false;
            };
            task.state = state;
            task.diagnostic_message = redacted_diagnostic.clone();
            task.session_id.clone()
        };
        let event_id = self.record_event_at(
            now_ms,
            Some(session_id),
            Some(task_id.to_owned()),
            "runtime.task_state_changed",
            serde_json::json!({
                "state": state,
                "diagnosticMessage": redacted_diagnostic,
            }),
        );
        if let Some(task) = self.tasks.iter_mut().rev().find(|task| task.id == task_id) {
            task.last_event_id = event_id;
        }
        true
    }

    pub fn finish_task(
        &mut self,
        task_id: &str,
        state: RuntimeState,
        now_ms: u64,
        diagnostic_message: Option<String>,
    ) -> bool {
        self.update_task(task_id, state, now_ms, diagnostic_message)
    }

    pub fn mark_task_detached(&mut self, task_id: &str, now_ms: u64) -> bool {
        let Some(session_id) = self
            .tasks
            .iter()
            .rev()
            .find(|task| task.id == task_id)
            .map(|task| task.session_id.clone())
        else {
            return false;
        };
        let event_id = self.record_event_at(
            now_ms,
            Some(session_id),
            Some(task_id.to_owned()),
            "runtime.task_detached",
            Value::Null,
        );
        if let Some(task) = self.tasks.iter_mut().rev().find(|task| task.id == task_id) {
            task.last_event_id = event_id;
        }
        true
    }

    pub fn finish_request(
        &mut self,
        session_id: &str,
        state: RuntimeState,
        now_ms: u64,
        error_stage: Option<String>,
        diagnostic_message: Option<String>,
    ) -> bool {
        let Some(request_id) = self
            .statuses
            .get(session_id)
            .and_then(|status| status.request_id.clone())
        else {
            return false;
        };
        self.finish_request_by_id(
            session_id,
            &request_id,
            state,
            now_ms,
            error_stage,
            diagnostic_message,
        )
    }

    pub fn finish_request_by_id(
        &mut self,
        session_id: &str,
        request_id: &str,
        state: RuntimeState,
        now_ms: u64,
        error_stage: Option<String>,
        diagnostic_message: Option<String>,
    ) -> bool {
        let redacted_diagnostic = diagnostic_message
            .as_deref()
            .map(atelier_acp_runtime::redact_text);
        let request_started_at = self
            .requests
            .iter()
            .rev()
            .find(|request| request.request_id == request_id && request.session_id == session_id)
            .map(|request| request.started_at_ms);
        let status_started_at = self
            .statuses
            .get(session_id)
            .filter(|status| status.request_id.as_deref() == Some(request_id))
            .map(|status| status.started_at_ms);
        let task_started_at = self
            .tasks
            .iter()
            .rev()
            .find(|task| task.id == request_id && task.session_id == session_id)
            .map(|task| task.started_at_ms);
        let Some(started_at) = request_started_at.or(status_started_at).or(task_started_at) else {
            return false;
        };

        if let Some(request) = self
            .requests
            .iter_mut()
            .rev()
            .find(|request| request.request_id == request_id)
        {
            request.state = state;
            request.error_stage = error_stage.map(|stage| atelier_acp_runtime::redact_text(&stage));
            request.finished_at_ms = Some(now_ms);
            request.total_duration_ms = Some(now_ms.saturating_sub(started_at));
        }
        if let Some(status) = self.statuses.get_mut(session_id)
            && status.request_id.as_deref() == Some(request_id)
        {
            status.state = state;
            status.last_progress_at_ms = now_ms;
            status.diagnostic_message = redacted_diagnostic.clone();
        }
        let event_id = self.record_event_at(
            now_ms,
            Some(session_id.to_owned()),
            Some(request_id.to_owned()),
            if state == RuntimeState::Completed {
                "request.completed"
            } else {
                "request.failed"
            },
            Value::Null,
        );
        if let Some(task) = self
            .tasks
            .iter_mut()
            .rev()
            .find(|task| task.id == request_id)
        {
            task.state = state;
            task.last_event_id = event_id;
            task.diagnostic_message = redacted_diagnostic;
        }
        true
    }

    pub fn set_request_context(
        &mut self,
        request_id: &str,
        context_blocks: Vec<ContextBlock>,
        input_tokens: u64,
        output_token_budget: Option<u64>,
        payload: Value,
    ) -> bool {
        let Some(request) = self
            .requests
            .iter_mut()
            .rev()
            .find(|request| request.request_id == request_id)
        else {
            return false;
        };
        request.context_blocks = context_blocks;
        request.input_tokens = input_tokens;
        request.output_token_budget = output_token_budget;
        request.payload = atelier_acp_runtime::redact_payload(&payload);
        true
    }

    pub fn set_request_parameters(
        &mut self,
        request_id: &str,
        effort: Option<String>,
        fast_mode: Option<bool>,
    ) -> bool {
        let Some(request) = self
            .requests
            .iter_mut()
            .rev()
            .find(|request| request.request_id == request_id)
        else {
            return false;
        };
        request.effort = effort;
        request.fast_mode = fast_mode;
        true
    }

    pub fn set_request_wire_api(
        &mut self,
        request_id: &str,
        wire_api: Option<String>,
        source: Option<String>,
    ) -> bool {
        let Some(request) = self
            .requests
            .iter_mut()
            .rev()
            .find(|request| request.request_id == request_id)
        else {
            return false;
        };
        request.wire_api = wire_api;
        request.wire_api_source = source;
        true
    }

    pub fn status(&self, session_id: &str) -> Option<RuntimeStatus> {
        self.statuses.get(session_id).cloned()
    }

    pub fn statuses(&self) -> Vec<RuntimeStatus> {
        let mut statuses: Vec<_> = self.statuses.values().cloned().collect();
        statuses.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        statuses
    }

    pub fn tasks(&self, session_id: Option<&str>) -> Vec<RuntimeTask> {
        self.tasks
            .iter()
            .filter(|task| session_id.is_none_or(|id| id == task.session_id))
            .cloned()
            .collect()
    }

    pub fn task(&self, task_id: &str) -> Option<RuntimeTask> {
        self.tasks
            .iter()
            .rev()
            .find(|task| task.id == task_id)
            .cloned()
    }

    pub fn request(&self, request_id: &str) -> Option<RequestSnapshot> {
        self.requests
            .iter()
            .rev()
            .find(|request| request.request_id == request_id)
            .cloned()
    }

    pub fn requests(&self, session_id: Option<&str>) -> Vec<RequestSnapshot> {
        self.requests
            .iter()
            .filter(|request| session_id.is_none_or(|id| id == request.session_id))
            .cloned()
            .collect()
    }

    pub fn record_event(
        &mut self,
        session_id: Option<String>,
        request_id: Option<String>,
        kind: impl Into<String>,
        details: Value,
    ) -> u64 {
        self.record_event_at(now_millis(), session_id, request_id, kind, details)
    }

    pub fn prepare_retry(
        &mut self,
        request_id: &str,
        retry_request_id: &str,
        now_ms: u64,
    ) -> Option<u32> {
        let (session_id, retry_count) = {
            let request = self
                .requests
                .iter()
                .rev()
                .find(|request| request.request_id == request_id)?;
            (
                request.session_id.clone(),
                request.retry_count.saturating_add(1),
            )
        };
        self.pending_retry_counts
            .insert(retry_request_id.to_owned(), retry_count);
        self.record_event_at(
            now_ms,
            Some(session_id),
            Some(request_id.to_owned()),
            "request.retry_requested",
            serde_json::json!({
                "retryOf": request_id,
                "requestId": retry_request_id,
                "retryCount": retry_count,
            }),
        );
        Some(retry_count)
    }

    pub fn discard_pending_retry(&mut self, retry_request_id: &str) {
        self.pending_retry_counts.remove(retry_request_id);
    }

    pub fn events_after(
        &self,
        session_id: Option<&str>,
        after_event_id: u64,
        limit: usize,
    ) -> Vec<TraceRecord> {
        self.events
            .iter()
            .filter(|event| event.event_id > after_event_id)
            .filter(|event| session_id.is_none_or(|id| event.session_id.as_deref() == Some(id)))
            .take(limit.max(1))
            .cloned()
            .collect()
    }

    pub fn events_after_task(
        &self,
        task_id: &str,
        after_event_id: u64,
        limit: usize,
    ) -> Vec<TraceRecord> {
        self.events
            .iter()
            .filter(|event| event.event_id > after_event_id)
            .filter(|event| event.request_id.as_deref() == Some(task_id))
            .take(limit.max(1))
            .cloned()
            .collect()
    }

    pub fn task_event_bounds(&self, task_id: &str) -> (Option<u64>, Option<u64>) {
        let mut event_ids = self
            .events
            .iter()
            .filter(|event| event.request_id.as_deref() == Some(task_id))
            .map(|event| event.event_id);
        let oldest = event_ids.next();
        let latest = event_ids.last().or(oldest);
        (oldest, latest)
    }

    pub fn task_replay_truncated(&self, task_id: &str, after_event_id: u64) -> bool {
        let Some(task) = self.tasks.iter().rev().find(|task| task.id == task_id) else {
            return false;
        };
        if task.last_event_id <= after_event_id {
            return false;
        }
        let Some(first_event_id) = self.task_first_event_ids.get(task_id).copied() else {
            return false;
        };
        match self.task_event_bounds(task_id).0 {
            Some(oldest) => oldest > first_event_id && after_event_id < oldest,
            None => true,
        }
    }

    pub fn oldest_event_id(&self) -> Option<u64> {
        self.events.front().map(|event| event.event_id)
    }

    pub fn latest_event_id(&self) -> Option<u64> {
        self.events.back().map(|event| event.event_id)
    }

    pub fn subscribe_events(&self) -> tokio::sync::watch::Receiver<u64> {
        self.event_watch.subscribe()
    }

    pub fn doctor(&self, now_ms: u64, stale_after_ms: u64) -> DoctorReport {
        let issues = self
            .statuses
            .values()
            .filter(|status| !status.state.is_terminal())
            .filter_map(|status| {
                let stale_for_ms = now_ms.saturating_sub(status.last_progress_at_ms);
                (stale_for_ms >= stale_after_ms).then(|| DoctorIssue {
                    session_id: status.session_id.clone(),
                    request_id: status.request_id.clone(),
                    state: status.state,
                    stale_for_ms,
                    message: status
                        .diagnostic_message
                        .clone()
                        .unwrap_or_else(|| "runtime has not reported progress".to_owned()),
                })
            })
            .collect();
        DoctorReport {
            checked_at_ms: now_ms,
            stale_after_ms,
            issues,
        }
    }

    pub fn recover(&mut self, session_id: &str, now_ms: u64) -> RecoveryResult {
        let request_id = {
            let Some(status) = self.statuses.get_mut(session_id) else {
                return RecoveryResult {
                    session_id: session_id.to_owned(),
                    action: RecoveryAction::Noop,
                    message: "session has no runtime state".to_owned(),
                };
            };
            if status.state.is_terminal() {
                return RecoveryResult {
                    session_id: session_id.to_owned(),
                    action: RecoveryAction::Noop,
                    message: "session is already terminal".to_owned(),
                };
            }
            status.state = RuntimeState::Recovering;
            status.last_progress_at_ms = now_ms;
            status.diagnostic_message = Some("recovery requested by client".to_owned());
            status.request_id.clone()
        };
        self.record_event_at(
            now_ms,
            Some(session_id.to_owned()),
            request_id,
            "runtime.recovery_requested",
            Value::Null,
        );
        RecoveryResult {
            session_id: session_id.to_owned(),
            action: RecoveryAction::Requested,
            message: "runtime recovery requested".to_owned(),
        }
    }

    fn record_event_at(
        &mut self,
        timestamp_ms: u64,
        session_id: Option<String>,
        request_id: Option<String>,
        kind: impl Into<String>,
        details: Value,
    ) -> u64 {
        self.next_event_id = self.next_event_id.saturating_add(1);
        let event_id = self.next_event_id;
        self.events.push_back(TraceRecord {
            event_id,
            timestamp_ms,
            session_id,
            request_id,
            kind: kind.into(),
            details: atelier_acp_runtime::redact_payload(&details),
        });
        self.trim();
        let _ = self.event_watch.send(event_id);
        event_id
    }

    fn trim(&mut self) {
        while self.requests.len() > self.request_limit {
            self.requests.pop_front();
        }
        while self.tasks.len() > self.request_limit {
            if let Some(task) = self.tasks.pop_front() {
                self.task_first_event_ids.remove(&task.id);
            }
        }
        while self.events.len() > self.event_limit {
            self.events.pop_front();
        }
    }
}

pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lifecycle_is_recorded_and_queryable() {
        let mut control = RuntimeControl::new(8, 8);
        control.begin_request(
            "session-1",
            "request-1",
            Some("turn-1".to_owned()),
            "main",
            Some("local".to_owned()),
            Some("model-a".to_owned()),
            100,
        );
        control.update_status("session-1", RuntimeState::WaitingForProvider, 120, None);
        assert!(control.set_request_context(
            "request-1",
            vec![ContextBlock {
                name: "system".to_owned(),
                source: "project".to_owned(),
                tokens: 12,
                redacted: false,
            }],
            12,
            Some(256),
            serde_json::json!({
                "effort": "high",
                "api_key": "must-not-leak",
            }),
        ));
        assert!(control.finish_request("session-1", RuntimeState::Completed, 180, None, None));

        let request = control.request("request-1").expect("request snapshot");
        assert_eq!(request.state, RuntimeState::Completed);
        assert_eq!(request.total_duration_ms, Some(80));
        assert_eq!(request.context_blocks[0].tokens, 12);
        assert_eq!(
            request.payload["api_key"],
            atelier_acp_runtime::REDACTED_VALUE
        );
        assert_eq!(
            control.status("session-1").unwrap().state,
            RuntimeState::Completed
        );
        assert_eq!(control.events_after(None, 0, 10).len(), 3);
    }

    #[test]
    fn doctor_finds_only_stale_non_terminal_requests() {
        let mut control = RuntimeControl::default();
        control.begin_request("session-1", "request-1", None, "main", None, None, 100);
        control.begin_request("session-2", "request-2", None, "explore", None, None, 190);
        let report = control.doctor(200, 50);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].session_id, "session-1");
        assert_eq!(report.issues[0].stale_for_ms, 100);
    }

    #[test]
    fn recovery_is_idempotent_for_terminal_and_unknown_sessions() {
        let mut control = RuntimeControl::default();
        let unknown = control.recover("missing", 100);
        assert_eq!(unknown.action, RecoveryAction::Noop);

        control.begin_request("session-1", "request-1", None, "main", None, None, 0);
        let requested = control.recover("session-1", 100);
        assert_eq!(requested.action, RecoveryAction::Requested);
        assert_eq!(
            control.status("session-1").unwrap().state,
            RuntimeState::Recovering
        );

        control.finish_request("session-1", RuntimeState::Completed, 200, None, None);
        assert_eq!(
            control.recover("session-1", 300).action,
            RecoveryAction::Noop
        );
    }

    #[test]
    fn bounded_buffers_keep_replay_deterministic() {
        let mut control = RuntimeControl::new(1, 2);
        control.begin_request("session-1", "request-1", None, "main", None, None, 1);
        control.begin_request("session-2", "request-2", None, "main", None, None, 2);
        assert!(control.request("request-1").is_none());
        assert_eq!(control.events_after(None, 0, 100).len(), 2);
        assert_eq!(control.events_after(None, 1, 100)[0].event_id, 2);
        assert_eq!(control.oldest_event_id(), Some(1));
        assert_eq!(control.latest_event_id(), Some(2));
    }

    #[test]
    fn runtime_task_wire_uses_pager_field_names() {
        let mut control = RuntimeControl::new(8, 8);
        control.begin_auxiliary_task(
            "task-1",
            "session-1",
            None,
            "agent-1",
            "main",
            RuntimeState::RunningTool,
            true,
            1,
        );
        control.finish_task(
            "task-1",
            RuntimeState::Failed,
            2,
            Some("provider timeout".to_owned()),
        );

        let value = serde_json::to_value(control.task("task-1")).expect("serialize runtime task");
        assert_eq!(value["taskId"], "task-1");
        assert_eq!(value["diagnosticMessage"], "provider timeout");
        assert!(value.get("id").is_none());
    }

    #[test]
    fn task_replay_filters_other_tasks_in_the_same_session() {
        let mut control = RuntimeControl::new(8, 16);
        control.record_event_at(
            1,
            Some("session-1".to_owned()),
            Some("task-1".to_owned()),
            "task.one",
            Value::Null,
        );
        control.record_event_at(
            2,
            Some("session-1".to_owned()),
            Some("task-2".to_owned()),
            "task.two",
            Value::Null,
        );

        let events = control.events_after_task("task-1", 0, 16);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].request_id.as_deref(), Some("task-1"));
    }

    #[test]
    fn task_replay_gap_ignores_interleaved_ids_but_detects_eviction() {
        let mut retained = RuntimeControl::new(8, 8);
        retained.begin_auxiliary_task(
            "task-1",
            "session-1",
            None,
            "agent-1",
            "main",
            RuntimeState::RunningTool,
            true,
            1,
        );
        retained.begin_auxiliary_task(
            "task-2",
            "session-1",
            None,
            "agent-1",
            "main",
            RuntimeState::RunningTool,
            true,
            2,
        );
        retained.update_task("task-1", RuntimeState::StreamingResponse, 3, None);
        assert!(!retained.task_replay_truncated("task-1", 0));

        let mut evicted = RuntimeControl::new(8, 2);
        evicted.begin_auxiliary_task(
            "task-1",
            "session-1",
            None,
            "agent-1",
            "main",
            RuntimeState::RunningTool,
            true,
            1,
        );
        evicted.begin_auxiliary_task(
            "task-2",
            "session-1",
            None,
            "agent-1",
            "main",
            RuntimeState::RunningTool,
            true,
            2,
        );
        evicted.update_task("task-1", RuntimeState::StreamingResponse, 3, None);
        assert!(evicted.task_replay_truncated("task-1", 0));
    }

    #[test]
    fn detach_marker_preserves_the_live_execution_state() {
        let mut control = RuntimeControl::new(8, 16);
        control.begin_request("session-1", "request-1", None, "main", None, None, 1);
        control.update_status(
            "session-1",
            RuntimeState::WaitingForPermission,
            2,
            Some("Waiting for permission: bash".to_owned()),
        );

        assert!(control.mark_task_detached("request-1", 3));

        assert_eq!(
            control.status("session-1").unwrap().state,
            RuntimeState::WaitingForPermission
        );
        assert_eq!(
            control.task("request-1").unwrap().state,
            RuntimeState::WaitingForPermission
        );
        let events = control.events_after_task("request-1", 0, 16);
        assert_eq!(events.last().unwrap().kind, "runtime.task_detached");
    }

    #[test]
    fn detached_request_completion_does_not_finish_a_newer_session_request() {
        let mut control = RuntimeControl::new(8, 32);
        control.begin_request("session-1", "request-old", None, "main", None, None, 1);
        control.begin_request("session-1", "request-new", None, "main", None, None, 2);

        assert!(control.finish_request_by_id(
            "session-1",
            "request-old",
            RuntimeState::Completed,
            3,
            None,
            None,
        ));

        assert_eq!(
            control.request("request-old").unwrap().state,
            RuntimeState::Completed
        );
        assert_eq!(
            control.request("request-new").unwrap().state,
            RuntimeState::PreparingContext
        );
        let status = control.status("session-1").unwrap();
        assert_eq!(status.request_id.as_deref(), Some("request-new"));
        assert_eq!(status.state, RuntimeState::PreparingContext);
    }

    #[test]
    fn request_can_finish_after_its_inspector_snapshot_is_evicted() {
        let mut control = RuntimeControl::new(8, 32);
        control.begin_request("session-1", "request-1", None, "main", None, None, 1);
        control.requests.clear();

        assert!(control.finish_request_by_id(
            "session-1",
            "request-1",
            RuntimeState::Completed,
            2,
            None,
            None,
        ));

        assert_eq!(
            control.status("session-1").unwrap().state,
            RuntimeState::Completed
        );
        assert_eq!(
            control.task("request-1").unwrap().state,
            RuntimeState::Completed
        );
    }

    #[test]
    fn auxiliary_side_query_lifecycle_never_overwrites_the_parent_request() {
        let mut control = RuntimeControl::new(8, 32);
        control.begin_request("session-1", "main-request", None, "main", None, None, 1);
        control.update_status("session-1", RuntimeState::StreamingResponse, 2, None);

        control.begin_auxiliary_task(
            "btw-1",
            "session-1",
            None,
            "session-1",
            "main",
            RuntimeState::PreparingContext,
            false,
            3,
        );
        control.finish_task(
            "btw-1",
            RuntimeState::Failed,
            4,
            Some("side failure".into()),
        );

        let parent = control.status("session-1").unwrap();
        assert_eq!(parent.request_id.as_deref(), Some("main-request"));
        assert_eq!(parent.state, RuntimeState::StreamingResponse);
        let side_query = control.task("btw-1").unwrap();
        assert_eq!(side_query.state, RuntimeState::Failed);
        assert!(!side_query.attachable);
    }

    #[test]
    fn replay_after_cursor_is_strict_and_never_repeats_the_cursor_event() {
        let mut control = RuntimeControl::new(8, 16);
        control.begin_auxiliary_task(
            "task-1",
            "session-1",
            None,
            "agent-1",
            "main",
            RuntimeState::WaitingForProvider,
            true,
            1,
        );
        control.update_task("task-1", RuntimeState::StreamingResponse, 2, None);
        control.update_task("task-1", RuntimeState::RunningTool, 3, None);
        let first_page = control.events_after_task("task-1", 0, 2);
        let cursor = first_page.last().unwrap().event_id;

        let second_page = control.events_after_task("task-1", cursor, 16);

        assert!(second_page.iter().all(|event| event.event_id > cursor));
        assert!(first_page.iter().all(|first| {
            second_page
                .iter()
                .all(|second| first.event_id != second.event_id)
        }));
    }

    #[test]
    fn trace_details_and_diagnostics_are_redacted() {
        let mut control = RuntimeControl::default();
        control.begin_request("session-1", "request-1", None, "main", None, None, 1);
        control.update_status(
            "session-1",
            RuntimeState::Failed,
            2,
            Some("Authorization: Bearer should-not-leak".to_owned()),
        );
        control.record_event(
            Some("session-1".to_owned()),
            Some("request-1".to_owned()),
            "provider.failed",
            serde_json::json!({
                "message": "token=should-not-leak",
                "nested": {"api_key": "also-secret"},
            }),
        );

        let status = control.status("session-1").unwrap();
        assert!(
            !status
                .diagnostic_message
                .unwrap()
                .contains("should-not-leak")
        );
        let events = control.events_after(None, 0, 10);
        let event = events.last().unwrap();
        let wire = serde_json::to_string(event).unwrap();
        assert!(!wire.contains("should-not-leak"));
        assert!(!wire.contains("also-secret"));
    }

    #[test]
    fn retry_count_is_carried_into_the_replayed_request() {
        let mut control = RuntimeControl::default();
        control.begin_request("session-1", "request-1", None, "main", None, None, 1);
        control.finish_request("session-1", RuntimeState::Failed, 2, None, None);
        assert_eq!(control.prepare_retry("request-1", "request-2", 3), Some(1));
        control.begin_request("session-1", "request-2", None, "main", None, None, 4);

        assert_eq!(control.status("session-1").unwrap().retry_count, 1);
        assert_eq!(control.request("request-2").unwrap().retry_count, 1);
    }

    #[test]
    fn task_registry_keeps_completed_tasks_when_a_session_starts_another_request() {
        let mut control = RuntimeControl::default();
        control.begin_request("session-1", "request-1", None, "main", None, None, 1);
        control.finish_request("session-1", RuntimeState::Completed, 2, None, None);
        control.begin_request("session-1", "request-2", None, "main", None, None, 3);

        let tasks = control.tasks(Some("session-1"));
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "request-1");
        assert_eq!(tasks[0].state, RuntimeState::Completed);
        assert_eq!(tasks[1].id, "request-2");
        assert_eq!(tasks[1].state, RuntimeState::PreparingContext);
        assert!(tasks[0].last_event_id < tasks[1].last_event_id);
    }

    #[test]
    fn task_registry_tracks_last_event_id_and_filters_by_session() {
        let mut control = RuntimeControl::default();
        control.begin_request("session-1", "request-1", None, "main", None, None, 1);
        control.begin_request("session-2", "request-2", None, "explore", None, None, 2);
        control.update_status("session-1", RuntimeState::WaitingForProvider, 3, None);

        let task = control.task("request-1").expect("task");
        assert_eq!(task.state, RuntimeState::WaitingForProvider);
        assert_eq!(task.last_event_id, 3);
        assert_eq!(control.tasks(Some("session-2")).len(), 1);
    }

    #[test]
    fn auxiliary_task_does_not_replace_the_parent_session_status() {
        let mut control = RuntimeControl::default();
        control.begin_request("session-1", "request-1", None, "main", None, None, 1);
        control.update_status("session-1", RuntimeState::RunningTool, 2, None);
        control.begin_auxiliary_task(
            "btw-1",
            "session-1",
            None,
            "session-1",
            "main",
            RuntimeState::WaitingForProvider,
            false,
            3,
        );

        assert_eq!(
            control.status("session-1").unwrap().state,
            RuntimeState::RunningTool
        );
        let task = control.task("btw-1").expect("auxiliary task");
        assert!(!task.attachable);
        control.finish_task("btw-1", RuntimeState::Completed, 4, None);
        assert_eq!(
            control.task("btw-1").unwrap().state,
            RuntimeState::Completed
        );
    }

    #[test]
    fn event_watch_is_woken_for_runtime_events() {
        let mut control = RuntimeControl::default();
        let receiver = control.subscribe_events();
        control.begin_request("session-1", "request-1", None, "main", None, None, 1);
        assert_eq!(*receiver.borrow(), 1);
        control.update_status("session-1", RuntimeState::Completed, 2, None);
        assert_eq!(*receiver.borrow(), 2);
    }
}
