//! Local feedback persistence and request heuristics.
//!
//! Atelier keeps user-authored feedback in the session log. This module has no
//! REST client, analytics sync loop, upload queue or vendor configuration.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::RwLock;

use crate::session::feedback::{
    FeedbackEvaluation, FeedbackHeuristics, FeedbackRequest, FeedbackTier, TriggerCondition,
};
use crate::session::feedback_types::{
    ClientType, FeedbackContent, FeedbackMode, FeedbackSubmission, FeedbackToolOutcome,
};
use crate::session::persistence::{LocalFeedbackEntry, PersistenceMsg, UserFeedbackEntry};
use crate::session::signals::{SessionSignalsActor, SessionSignalsHandle, TurnDeltaSnapshot};

pub(crate) enum SubmitOutcome {
    LocalOnly,
}

pub(crate) fn new_submission(
    session_id: String,
    client_type: ClientType,
    content: FeedbackContent,
) -> FeedbackSubmission {
    let mut submission = FeedbackSubmission::with_content(session_id, client_type, content);
    submission.shell_version = Some(atelier_version::VERSION.to_string());
    submission
}

pub(crate) async fn submit_feedback_workflow(
    submission: &mut FeedbackSubmission,
    persistence_tx: Option<&tokio::sync::mpsc::UnboundedSender<PersistenceMsg>>,
    solicited: bool,
    _telemetry_enabled: bool,
) -> SubmitOutcome {
    if let Some(tx) = persistence_tx {
        let entry = LocalFeedbackEntry::UserFeedback(UserFeedbackEntry {
            submitted_at: chrono::Utc::now(),
            session_id: submission.session_id.clone(),
            turn_number: submission.turn_number,
            solicited,
            request_id: submission.request_id.clone(),
            dismissed: false,
            submission: Some(submission.clone()),
        });
        if tx.send(PersistenceMsg::Feedback(entry)).is_err() {
            tracing::warn!(
                session_id = %submission.session_id,
                "feedback persistence channel closed; entry dropped",
            );
        }
    }
    SubmitOutcome::LocalOnly
}

pub(crate) struct SessionFeedbackData {
    pub model_id: Option<String>,
    pub resolved_model_id: Option<String>,
    pub client_version: Option<String>,
    pub session_cwd: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FeedbackFlags {
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct FeedbackManagerConfig {
    pub sync_interval: Duration,
    pub feedback_enabled: bool,
    pub telemetry_enabled: bool,
    pub client_type: ClientType,
    pub loc_tracking_enabled: bool,
    pub drain_timeout: Duration,
}

impl Default for FeedbackManagerConfig {
    fn default() -> Self {
        Self {
            sync_interval: Duration::from_secs(60),
            feedback_enabled: false,
            telemetry_enabled: false,
            client_type: ClientType::Agent,
            loc_tracking_enabled: false,
            drain_timeout: Duration::from_secs(30),
        }
    }
}

pub struct FeedbackManager {
    session_id: String,
    signals_handle: SessionSignalsHandle,
    heuristics: Arc<RwLock<FeedbackHeuristics>>,
    config: FeedbackManagerConfig,
    config_loaded: AtomicBool,
}

impl FeedbackManager {
    pub fn new(
        session_id: impl Into<String>,
        _removed_remote_client: Option<()>,
        config: FeedbackManagerConfig,
    ) -> Self {
        let (signals_handle, actor) = SessionSignalsActor::with_sync_interval(config.sync_interval);
        tokio::spawn(actor.run());
        Self {
            session_id: session_id.into(),
            signals_handle,
            heuristics: Arc::new(RwLock::new(FeedbackHeuristics::new())),
            config,
            config_loaded: AtomicBool::new(true),
        }
    }

    pub fn local_only(session_id: impl Into<String>) -> Self {
        Self::new(session_id, None, FeedbackManagerConfig::default())
    }

    pub fn signals_handle(&self) -> SessionSignalsHandle {
        self.signals_handle.clone()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn is_enabled(&self) -> bool {
        self.config.feedback_enabled
    }

    pub fn client_type(&self) -> ClientType {
        self.config.client_type
    }

    pub(crate) async fn submit_text_feedback(
        &self,
        text: String,
        session_data: SessionFeedbackData,
        persistence_tx: Option<&tokio::sync::mpsc::UnboundedSender<PersistenceMsg>>,
        telemetry_enabled: bool,
    ) -> SubmitOutcome {
        let signals_handle = self.signals_handle();
        let (signals, tool_outcomes) = tokio::join!(
            signals_handle.snapshot(),
            signals_handle.last_turn_tool_outcomes()
        );
        let signals = signals.unwrap_or_default();
        let tool_outcomes = tool_outcomes
            .into_iter()
            .map(|outcome| FeedbackToolOutcome {
                tool_name: outcome.tool_name,
                calls: outcome.successes + outcome.failures,
                failures: outcome.failures,
            })
            .collect();
        let mut submission = new_submission(
            self.session_id.clone(),
            self.config.client_type,
            FeedbackContent::Text(text),
        );
        submission.turn_number = Some(signals.turn_count.saturating_sub(1) as i64);
        submission.model_id = session_data.model_id;
        submission.resolved_model_id = session_data.resolved_model_id;
        submission.tool_outcomes = tool_outcomes;
        submission.session_cwd = Some(session_data.session_cwd);
        submission.compaction_count = Some(signals.compaction_count as i64);
        submission.context_window_usage = Some(signals.context_window_usage);
        submission.context_tokens_used = Some(signals.context_tokens_used);
        submission.context_window_tokens = Some(signals.context_window_tokens);
        submission.client_version = session_data.client_version;
        if let Some(user_meta) =
            crate::agent::mvp_agent::parse_json_object_env("ATELIER_USER_METADATA")
        {
            submission.merge_metadata(user_meta);
        }
        submit_feedback_workflow(&mut submission, persistence_tx, false, telemetry_enabled).await
    }

    pub fn is_config_loaded(&self) -> bool {
        self.config_loaded.load(Ordering::Relaxed)
    }

    pub async fn load_config(&self) {
        self.config_loaded.store(true, Ordering::Relaxed);
    }

    pub async fn maybe_request_feedback(
        &self,
        _prompt_id: Option<String>,
    ) -> Option<FeedbackRequest> {
        if !self.config.feedback_enabled {
            return None;
        }
        let signals = self.signals_handle.snapshot().await?;
        let mut heuristics = self.heuristics.write().await;
        if !heuristics.is_enabled() {
            return None;
        }
        let evaluation = heuristics.evaluate(&signals);
        let condition = evaluation.trigger_condition?;
        if !evaluation.should_request {
            return None;
        }
        let mode = heuristics.feedback_mode(condition.tier);
        let dismissible = heuristics.dismissible(condition.tier);
        let prompt = heuristics.prompt(condition.tier);
        Some(FeedbackRequest::with_mode(
            self.session_id.clone(),
            condition,
            mode,
            dismissible,
            Some(prompt),
        ))
    }

    pub async fn evaluate_heuristics(&self) -> Option<FeedbackEvaluation> {
        let signals = self.signals_handle.snapshot().await?;
        Some(self.heuristics.write().await.evaluate(&signals))
    }

    pub async fn force_feedback_request(
        &self,
        tier: FeedbackTier,
        mode: FeedbackMode,
    ) -> FeedbackRequest {
        use crate::session::feedback::TriggerSignalSnapshot;
        let condition = TriggerCondition {
            tier,
            condition: "debug/trigger_feedback (local)".to_string(),
            signal_snapshot: TriggerSignalSnapshot {
                turn_count: 0,
                tool_calls_count: 0,
                compactions_count: 0,
                errors_count: 0,
                cancellations_count: 0,
                has_reverted: false,
            },
        };
        FeedbackRequest::with_mode(self.session_id.clone(), condition, mode, true, None)
    }

    pub async fn send_turn_delta_with_snapshot(
        &self,
        _snapshot: Option<TurnDeltaSnapshot>,
        _request_id: Option<String>,
        _turn_duration_ms: Option<i64>,
        _turn_outcome: Option<String>,
        _model_fingerprint: Option<String>,
    ) {
    }

    pub async fn sync_signals(&self) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn force_sync_signals(&self) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn run_sync_loop(self: Arc<Self>, cancel: tokio_util::sync::CancellationToken) {
        self.load_config().await;
        cancel.cancelled().await;
    }

    pub async fn shutdown(&self) {
        self.signals_handle.shutdown();
    }
}
