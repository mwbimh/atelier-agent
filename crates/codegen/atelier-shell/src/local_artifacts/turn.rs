use atelier_workspace::permission::PermissionEvent;
use tokio::sync::oneshot;

use super::manifest::LocalArtifactState;

pub(crate) struct SyntheticTurnTraceRequest {
    pub session_id: agent_client_protocol::SessionId,
    pub prompt_id: String,
    pub completion_rx: oneshot::Receiver<crate::session::commands::PromptTurnResult>,
    pub before_session_copy_rx:
        oneshot::Receiver<anyhow::Result<crate::session::persistence::SessionStateCopy>>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ArtifactWriteWait {
    Confirm,
    Deadline { deadline: tokio::time::Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalArtifactOutcome {
    Written,
    Failed,
}

impl LocalArtifactOutcome {
    pub(crate) fn is_written(self) -> bool {
        matches!(self, Self::Written)
    }
}

#[derive(Clone)]
pub(crate) struct PromptTraceContext {
    pub(crate) session_info: crate::session::info::Info,
    pub(crate) turn_number: u64,
    pub(crate) session_handle: crate::session::SessionHandle,
    pub(crate) session_registry_enabled: bool,
    pub(crate) local_artifact_state: LocalArtifactState,
}

impl PromptTraceContext {
    pub(crate) fn artifact_dir(&self) -> std::path::PathBuf {
        crate::session::persistence::session_dir(&self.session_info)
            .join("local_artifacts")
            .join(format!("turn_{}", self.turn_number))
    }
}

pub(crate) fn spawn_artifact_task<F>(task_name: &'static str, future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    use futures::FutureExt as _;
    use tracing::Instrument as _;
    let parent_span = tracing::Span::current();
    tokio::spawn(
        async move {
            if let Err(payload) = std::panic::AssertUnwindSafe(future).catch_unwind().await {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|value| (*value).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_owned());
                tracing::error!(task = task_name, panic = %message, "local artifact task panicked");
            }
        }
        .instrument(parent_span),
    );
}

pub(crate) async fn take_streaming_partial(
    cmd_tx: &tokio::sync::mpsc::UnboundedSender<crate::session::SessionCommand>,
    prompt_id: String,
    committed: bool,
    model_id: Option<String>,
) -> Option<crate::session::acp_session::StreamingTurnCapture> {
    use crate::session::SessionCommand;
    let (tx, rx) = oneshot::channel();
    if cmd_tx
        .send(SessionCommand::TakeStreamingCapture {
            prompt_id,
            respond_to: tx,
        })
        .is_err()
    {
        return None;
    }
    let taken = rx.await.ok().flatten();
    if committed {
        return taken
            .filter(|capture| capture.has_doom_loop_segments())
            .map(|mut capture| {
                capture
                    .model_id
                    .get_or_insert_with(|| model_id.unwrap_or_default());
                capture
                    .reason
                    .get_or_insert_with(|| "doom_loop_recovered".to_owned());
                capture
            });
    }
    taken.map(|mut capture| {
        if capture.model_id.is_none() {
            capture.model_id = model_id;
        }
        capture
    })
}

pub(crate) async fn complete_prompt_trace(
    ctx: PromptTraceContext,
    permission_events: Vec<PermissionEvent>,
    session_copy_rx: oneshot::Receiver<
        anyhow::Result<crate::session::persistence::SessionStateCopy>,
    >,
    turn_messages: Option<atelier_chat_state::TurnCapture>,
    streaming_partial: Option<crate::session::acp_session::StreamingTurnCapture>,
    _wait: ArtifactWriteWait,
) -> anyhow::Result<bool> {
    let session = super::artifacts::write_session_state(&ctx, "after", session_copy_rx).await;
    super::artifacts::write_permission_events(&ctx, &permission_events).await;
    if let Some(messages) = turn_messages {
        super::artifacts::write_turn_messages(&ctx, messages).await;
    }
    if let Some(capture) = streaming_partial.as_ref() {
        super::artifacts::write_streaming_partial(&ctx, capture).await;
    }
    super::artifacts::write_manifest(&ctx).await;
    Ok(session.is_written())
}

pub(crate) fn parse_agent_profile_from_meta(
    meta: Option<&agent_client_protocol::Meta>,
) -> Option<atelier_agent::AgentDefinition> {
    let value = meta?.get("agentProfile")?;
    if value.is_object() {
        return atelier_agent::AgentDefinition::from_json(value).ok();
    }
    value.as_str().and_then(atelier_agent::discovery::by_name)
}

pub(crate) fn parse_ask_user_question_from_meta(
    meta: Option<&agent_client_protocol::Meta>,
) -> Option<bool> {
    meta?.get("askUserQuestion")?.as_bool()
}

pub(crate) fn lookup_session_model(
    sessions: &std::collections::HashMap<
        agent_client_protocol::SessionId,
        crate::session::SessionHandle,
    >,
    session_id: Option<&agent_client_protocol::SessionId>,
    default_model_id: &agent_client_protocol::ModelId,
) -> agent_client_protocol::ModelId {
    session_id
        .and_then(|id| sessions.get(id).map(|handle| handle.model_id.clone()))
        .unwrap_or_else(|| default_model_id.clone())
}

pub(crate) fn apply_yolo_mode_to_matching_sessions(
    sessions: &mut std::collections::HashMap<
        agent_client_protocol::SessionId,
        crate::session::SessionHandle,
    >,
    sender_id: Option<&str>,
    yolo_mode: bool,
) -> usize {
    let mut updated = 0;
    for handle in sessions.values_mut() {
        if sender_id.is_none()
            || handle
                .origin_client
                .as_ref()
                .map(|client| client.product.as_str())
                == sender_id
        {
            handle.yolo_mode = yolo_mode;
            let _ = handle
                .cmd_tx
                .send(crate::session::SessionCommand::SetYoloMode { enabled: yolo_mode });
            updated += 1;
        }
    }
    updated
}
