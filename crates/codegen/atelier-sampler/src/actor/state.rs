//! Actor-internal state.
//!
//! All fields are touched only from the actor task, so no mutex /
//! atomic synchronization is needed -- the actor's command-loop
//! serialization gives us a "single-threaded with shared state"
//! discipline matching the hunk-tracker pattern.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::config::{RetryPolicy, SamplerConfig};
use crate::retry::ConsecutiveRetryBudget;
use crate::types::RequestId;

/// In-flight request bookkeeping.
///
/// `cancel_token` is owned by the actor (cloned into the spawned
/// per-request task). The completion oneshot is moved into the
/// per-request task at spawn time and is therefore not stored here.
pub(crate) struct ActiveRequest {
    pub(crate) cancel_token: CancellationToken,
}

/// Actor-owned state.
pub(crate) struct ActorState {
    pub(crate) active_requests: HashMap<RequestId, ActiveRequest>,
    pub(crate) config: SamplerConfig,
    pub(crate) retry_policy: RetryPolicy,
    /// Shared across request IDs created by one logical sampler operation.
    /// Successful complete responses reset it in the request task.
    pub(crate) consecutive_retry_budget: Arc<ConsecutiveRetryBudget>,
}

impl ActorState {
    pub(crate) fn new(config: SamplerConfig, retry_policy: RetryPolicy) -> Self {
        Self {
            active_requests: HashMap::new(),
            config,
            retry_policy,
            consecutive_retry_budget: Arc::new(ConsecutiveRetryBudget::default()),
        }
    }

    /// Register a newly-spawned request. Returns the previous entry if
    /// the same `request_id` was already in flight (callers should
    /// cancel the previous token before overwriting).
    pub(crate) fn register(
        &mut self,
        request_id: RequestId,
        active: ActiveRequest,
    ) -> Option<ActiveRequest> {
        self.active_requests.insert(request_id, active)
    }

    /// Remove a request from the active set without cancelling its
    /// token. Used by the cleanup signal sent from per-request tasks
    /// when they exit normally.
    pub(crate) fn remove(&mut self, request_id: &RequestId) -> Option<ActiveRequest> {
        self.active_requests.remove(request_id)
    }

    /// Cancel and remove an in-flight request.
    pub(crate) fn cancel(&mut self, request_id: &RequestId) -> bool {
        if let Some(active) = self.active_requests.remove(request_id) {
            active.cancel_token.cancel();
            true
        } else {
            false
        }
    }

    /// Replace the default config. The next request submitted without
    /// an override will use this.
    pub(crate) fn update_config(&mut self, config: SamplerConfig) {
        self.config = config;
    }

    /// Reset transport recovery at an explicit logical-operation boundary.
    pub(crate) fn reset_retry_budget(&self) {
        self.consecutive_retry_budget.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ApiBackend;
    use indexmap::IndexMap;

    /// Minimal config builder for tests in this module.
    fn cfg() -> SamplerConfig {
        SamplerConfig {
            api_key: None,
            base_url: "https://example.test".into(),
            provider_id: None,
            model: "test-model".into(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            request_payload: Default::default(),
            remote_compaction_endpoint: None,
            image_generation_endpoint: None,
            api_backend: ApiBackend::ChatCompletions,
            auth_scheme: Default::default(),
            extra_headers: IndexMap::new(),
            context_window: 8192,
            force_http1: false,
            max_retries: None,
            stream_tool_calls: false,
            idle_timeout_secs: None,
            reasoning_effort: None,
            origin_client: None,
            client_identifier: None,
            deployment_id: None,
            user_id: None,
            client_version: None,
            attribution_callback: None,
            bearer_resolver: None,
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            doom_loop_recovery: None,
            header_injector: None,
        }
    }

    #[test]
    fn cancel_unknown_request_returns_false() {
        let mut state = ActorState::new(cfg(), RetryPolicy::default());
        assert!(!state.cancel(&RequestId::from("unknown")));
    }

    #[test]
    fn register_then_cancel_removes() {
        let mut state = ActorState::new(cfg(), RetryPolicy::default());
        let id = RequestId::from("req-1");
        state.register(
            id.clone(),
            ActiveRequest {
                cancel_token: CancellationToken::new(),
            },
        );
        assert_eq!(state.active_requests.len(), 1);
        assert!(state.cancel(&id));
        assert_eq!(state.active_requests.len(), 0);
    }

    #[test]
    fn register_returns_previous_when_same_id() {
        let mut state = ActorState::new(cfg(), RetryPolicy::default());
        let id = RequestId::from("req-1");
        let first = ActiveRequest {
            cancel_token: CancellationToken::new(),
        };
        let second = ActiveRequest {
            cancel_token: CancellationToken::new(),
        };
        assert!(state.register(id.clone(), first).is_none());
        assert!(state.register(id.clone(), second).is_some());
    }

    #[test]
    fn consecutive_retry_budget_is_shared_and_success_resets_it() {
        let state = ActorState::new(cfg(), RetryPolicy::default());
        let budget = state.consecutive_retry_budget.clone();

        assert_eq!(budget.current(), 0);
        assert_eq!(budget.consume(5), Some(1));
        assert_eq!(budget.consume(5), Some(2));
        assert_eq!(budget.current(), 2);

        budget.reset();
        assert_eq!(budget.current(), 0);
        assert_eq!(budget.consume(5), Some(1));
    }

    #[test]
    fn consecutive_retry_budget_refuses_more_than_configured_retries() {
        let state = ActorState::new(cfg(), RetryPolicy::default());
        let budget = state.consecutive_retry_budget;

        for expected in 1..=5 {
            assert_eq!(budget.consume(5), Some(expected));
        }
        assert_eq!(budget.consume(5), None);
        assert_eq!(budget.current(), 5);
    }

    #[test]
    fn independent_sampler_actors_never_share_retry_budget_state() {
        let first = ActorState::new(cfg(), RetryPolicy::default());
        let second = ActorState::new(cfg(), RetryPolicy::default());

        assert!(!Arc::ptr_eq(
            &first.consecutive_retry_budget,
            &second.consecutive_retry_budget
        ));
        assert_eq!(first.consecutive_retry_budget.consume(5), Some(1));
        assert_eq!(first.consecutive_retry_budget.consume(5), Some(2));
        assert_eq!(second.consecutive_retry_budget.current(), 0);
        assert_eq!(second.consecutive_retry_budget.consume(5), Some(1));
    }

    #[test]
    fn explicit_operation_boundary_clears_prior_terminal_failure_debit() {
        let state = ActorState::new(cfg(), RetryPolicy::default());
        assert_eq!(state.consecutive_retry_budget.consume(5), Some(1));
        assert_eq!(state.consecutive_retry_budget.consume(5), Some(2));

        state.reset_retry_budget();

        assert_eq!(state.consecutive_retry_budget.current(), 0);
        assert_eq!(state.consecutive_retry_budget.consume(5), Some(1));
    }
}
