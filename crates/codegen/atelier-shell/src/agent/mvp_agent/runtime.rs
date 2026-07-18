use super::MvpAgent;
use crate::runtime_control::{
    ContextBlock, DoctorReport, RecoveryResult, RequestSnapshot, RuntimeState, RuntimeStatus,
    TraceRecord,
};
use crate::session::{SessionCommand, SessionHandle, SessionLiveState};
use agent_client_protocol as acp;
use serde_json::Value;

const MAX_RETRYABLE_PROMPTS: usize = 64;

fn build_retry_request(
    mut request: acp::PromptRequest,
    request_id: &str,
) -> (String, acp::PromptRequest) {
    let retry_request_id = format!("runtime-retry-{}", uuid::Uuid::now_v7());
    let meta = request.meta.get_or_insert_with(Default::default);
    meta.insert(
        "promptId".to_owned(),
        Value::String(retry_request_id.clone()),
    );
    meta.insert("retryOf".to_owned(), Value::String(request_id.to_owned()));
    meta.insert("sendNow".to_owned(), Value::Bool(false));
    meta.remove("turnId");
    (retry_request_id, request)
}

impl MvpAgent {
    pub(crate) fn runtime_status(&self, session_id: &str) -> Option<RuntimeStatus> {
        self.runtime_control.borrow().status(session_id)
    }

    pub(crate) fn runtime_statuses(&self) -> Vec<RuntimeStatus> {
        self.runtime_control.borrow().statuses()
    }

    pub(crate) fn runtime_requests(&self, session_id: Option<&str>) -> Vec<RequestSnapshot> {
        self.runtime_control.borrow().requests(session_id)
    }

    pub(crate) fn runtime_request(&self, request_id: &str) -> Option<RequestSnapshot> {
        self.runtime_control.borrow().request(request_id)
    }

    pub(crate) fn remember_retryable_prompt(
        &self,
        request_id: &str,
        request: acp::PromptRequest,
    ) {
        let mut prompts = self.retryable_prompts.borrow_mut();
        if !prompts.contains_key(request_id) && prompts.len() >= MAX_RETRYABLE_PROMPTS {
            if let Some(oldest_id) = prompts.keys().next().cloned() {
                prompts.remove(&oldest_id);
            }
        }
        prompts.insert(request_id.to_owned(), request);
    }

    pub(crate) fn forget_retryable_prompt(&self, request_id: &str) {
        self.retryable_prompts.borrow_mut().remove(request_id);
    }

    pub(crate) async fn retry_request(&self, request_id: &str) -> Result<String, String> {
        let snapshot = self
            .runtime_request(request_id)
            .ok_or_else(|| "request not found".to_owned())?;
        if !matches!(snapshot.state, RuntimeState::Failed | RuntimeState::Paused) {
            return Err(format!(
                "request is not retryable in state {:?}",
                snapshot.state
            ));
        }
        let request = self
            .retryable_prompts
            .borrow()
            .get(request_id)
            .cloned()
            .ok_or_else(|| "request payload is no longer available for replay".to_owned())?;
        let (retry_request_id, retry_request) = build_retry_request(request, request_id);
        if self
            .runtime_control
            .borrow_mut()
            .prepare_retry(
                request_id,
                &retry_request_id,
                crate::runtime_control::now_millis(),
            )
            .is_none()
        {
            return Err("request is no longer available for replay".to_owned());
        }
        match acp::Agent::prompt(self, retry_request).await {
            Ok(_) => {
                self.forget_retryable_prompt(request_id);
                Ok(retry_request_id)
            }
            Err(error) => {
                self.runtime_control
                    .borrow_mut()
                    .discard_pending_retry(&retry_request_id);
                Err(xai_acp_lib::redact_text(&error.to_string()))
            }
        }
    }

    pub(crate) fn runtime_events(
        &self,
        session_id: Option<&str>,
        after_event_id: u64,
        limit: usize,
    ) -> Vec<TraceRecord> {
        self.runtime_control
            .borrow()
            .events_after(session_id, after_event_id, limit)
    }

    pub(crate) fn runtime_event_bounds(&self) -> (Option<u64>, Option<u64>) {
        let control = self.runtime_control.borrow();
        (control.oldest_event_id(), control.latest_event_id())
    }

    pub(crate) fn runtime_doctor(&self, now_ms: u64, stale_after_ms: u64) -> DoctorReport {
        self.runtime_control.borrow().doctor(now_ms, stale_after_ms)
    }

    pub(crate) fn runtime_recover(&self, session_id: &str) -> RecoveryResult {
        let result = self
            .runtime_control
            .borrow_mut()
            .recover(session_id, crate::runtime_control::now_millis());
        if matches!(result.action, crate::runtime_control::RecoveryAction::Requested)
            && let Some(handle) = self.session_handle_now(session_id)
        {
            let _ = handle.cmd_tx.send(SessionCommand::Cancel {
                cancel_subagents: true,
                kill_background_tasks: false,
                rewind_if_pristine: false,
                trigger: Some("runtime_recover".to_owned()),
            });
        }
        result
    }

    pub(crate) fn runtime_begin_request(
        &self,
        session_id: &acp::SessionId,
        request_id: &str,
        turn_id: Option<String>,
        role: &str,
        provider: Option<String>,
        model: Option<String>,
    ) {
        self.runtime_control.borrow_mut().begin_request(
            session_id.0.to_string(),
            request_id,
            turn_id,
            role,
            provider,
            model,
            crate::runtime_control::now_millis(),
        );
    }

    pub(crate) fn runtime_update_status(
        &self,
        session_id: &acp::SessionId,
        state: RuntimeState,
        diagnostic_message: Option<String>,
    ) {
        let _ = self.runtime_control.borrow_mut().update_status(
            session_id.0.as_ref(),
            state,
            crate::runtime_control::now_millis(),
            diagnostic_message,
        );
    }

    pub(crate) fn runtime_set_request_context(
        &self,
        request_id: &str,
        context_blocks: Vec<ContextBlock>,
        input_tokens: u64,
        output_token_budget: Option<u64>,
        payload: Value,
    ) {
        let _ = self.runtime_control.borrow_mut().set_request_context(
            request_id,
            context_blocks,
            input_tokens,
            output_token_budget,
            payload,
        );
    }

    pub(crate) fn runtime_set_request_parameters(
        &self,
        request_id: &str,
        effort: Option<String>,
        fast_mode: Option<bool>,
    ) {
        let _ = self
            .runtime_control
            .borrow_mut()
            .set_request_parameters(request_id, effort, fast_mode);
    }

    pub(crate) fn runtime_finish_request(
        &self,
        session_id: &acp::SessionId,
        state: RuntimeState,
        error_stage: Option<String>,
        diagnostic_message: Option<String>,
    ) {
        let _ = self.runtime_control.borrow_mut().finish_request(
            session_id.0.as_ref(),
            state,
            crate::runtime_control::now_millis(),
            error_stage,
            diagnostic_message,
        );
    }

    pub(crate) fn session_liveness(&self, session_id: &str) -> Option<SessionLiveState> {
        self.session_live_state
            .borrow()
            .get(&acp::SessionId::new(session_id))
            .copied()
    }

    pub(crate) fn session_handle_now(&self, session_id: &str) -> Option<SessionHandle> {
        self.sessions
            .borrow()
            .get(&acp::SessionId::new(session_id))
            .cloned()
    }

    pub(crate) fn runtime_session_handles(
        &self,
        session_id: Option<&str>,
    ) -> Vec<(String, SessionHandle)> {
        self.sessions
            .borrow()
            .iter()
            .filter(|(id, _)| session_id.is_none_or(|wanted| id.0.as_ref() == wanted))
            .map(|(id, handle)| (id.0.to_string(), handle.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_request_gets_a_new_prompt_id_without_reusing_turn_metadata() {
        let request = acp::PromptRequest::new(
            "session-1",
            vec![acp::ContentBlock::Text(acp::TextContent::new("retry me"))],
        );

        let (retry_id, retry_request) = build_retry_request(request, "request-1");
        let meta = retry_request.meta.expect("retry metadata");

        assert_eq!(meta["promptId"], Value::String(retry_id.clone()));
        assert_eq!(meta["retryOf"], Value::String("request-1".to_owned()));
        assert_eq!(meta["sendNow"], Value::Bool(false));
        assert!(!meta.contains_key("turnId"));
        assert_ne!(retry_id, "request-1");
    }
}
