use std::path::{Component, Path, PathBuf};

use atelier_workspace::permission::PermissionEvent;
use base64::Engine as _;
use serde::Serialize;
use tokio::sync::oneshot;

use super::manifest::{ArtifactResult, LocalArtifactManifest, record_artifact, skip_artifact};
use super::turn::{ArtifactWriteWait, LocalArtifactOutcome, PromptTraceContext};

pub(crate) const LOCAL_ARTIFACT_SCHEMA_VERSION: &str = "1";

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LocalSandboxTelemetry {
    pub profile: String,
    pub applied: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct PromptMetadata {
    pub schema_version: String,
    pub session_id: String,
    pub turn_number: u64,
    pub request_id: String,
    pub turn_started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,
    pub host_os: String,
    pub host_arch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_has_image: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_was_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_verbatim: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<LocalSandboxTelemetry>,
}

pub(crate) fn local_sandbox_telemetry() -> Option<LocalSandboxTelemetry> {
    let profile = atelier_sandbox::configured_profile_name()?;
    Some(LocalSandboxTelemetry {
        profile: profile.to_owned(),
        applied: atelier_sandbox::is_active(),
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SubagentSpawnedRef {
    pub(crate) subagent_id: String,
    pub(crate) child_session_id: String,
    pub(crate) subagent_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) persona: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resumed_from: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct TurnResultMetadata {
    pub(crate) schema_version: &'static str,
    pub(crate) request_id: String,
    pub(crate) completed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cached_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    pub(crate) finished_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signals: Option<crate::session::signals::SessionSignals>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) turn_delta: Option<crate::session::signals::SessionSignalsDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) subagents_spawned: Vec<SubagentSpawnedRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_prompt_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) end_prompt_mode: Option<String>,
}

pub(crate) async fn write_manifest(ctx: &PromptTraceContext) {
    let manifest = super::manifest::build_manifest(&ctx.local_artifact_state);
    write_manifest_value(ctx, &manifest).await;
}

pub(crate) async fn write_error_manifest(ctx: &PromptTraceContext) {
    let mut manifest = super::manifest::build_manifest(&ctx.local_artifact_state);
    manifest.complete = false;
    write_manifest_value(ctx, &manifest).await;
}

async fn write_manifest_value(ctx: &PromptTraceContext, manifest: &LocalArtifactManifest) {
    let path = ctx.artifact_dir().join("manifest.json");
    if let Err(error) = write_json_path(path, manifest).await {
        tracing::warn!(error = %error, "failed to write local artifact manifest");
    }
}

pub(crate) async fn write_metadata(ctx: &PromptTraceContext, mut metadata: PromptMetadata) {
    enrich_git_metadata(ctx, &mut metadata).await;
    write_json(ctx, "metadata.json", &metadata).await;
}

pub(crate) async fn write_images(
    ctx: &PromptTraceContext,
    images: &[agent_client_protocol::ImageContent],
) {
    for (index, image) in images.iter().enumerate() {
        let filename = format!(
            "images/image_{index}.{}",
            mime_type_to_extension(&image.mime_type)
        );
        match base64::engine::general_purpose::STANDARD.decode(&image.data) {
            Ok(bytes) => write_bytes(ctx, &filename, &bytes).await,
            Err(error) => record_artifact(
                &ctx.local_artifact_state,
                &filename,
                ArtifactResult::Failed {
                    reason: "decode_failed",
                    error: Some(&error.to_string()),
                },
            ),
        }
    }
}

pub(crate) fn mime_type_to_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/avif" => "avif",
        _ => "bin",
    }
}

pub(crate) async fn write_full_prompt_txt(ctx: &PromptTraceContext, full_prompt: &str) {
    write_bytes(ctx, "full_prompt.txt", full_prompt.as_bytes()).await;
}

pub(crate) async fn write_config(
    ctx: &PromptTraceContext,
    _agent_config: &crate::agent::config::Config,
) {
    skip_artifact(
        &ctx.local_artifact_state,
        "config.json",
        "sensitive_config_not_copied",
    );
}

pub(crate) async fn write_config_files(ctx: &PromptTraceContext) {
    skip_artifact(
        &ctx.local_artifact_state,
        "config_files",
        "sensitive_config_files_not_copied",
    );
}

pub(crate) async fn write_plugin_state(
    ctx: &PromptTraceContext,
    _registry: Option<&atelier_agent::plugins::PluginRegistry>,
) {
    skip_artifact(
        &ctx.local_artifact_state,
        "plugins.json",
        "plugin_snapshot_not_available",
    );
}

pub(crate) async fn write_turn_result(
    ctx: &PromptTraceContext,
    result: &TurnResultMetadata,
    _wait: ArtifactWriteWait,
) {
    write_json(ctx, "turn_result.json", result).await;
}

pub(crate) async fn write_streaming_partial(
    ctx: &PromptTraceContext,
    capture: &crate::session::acp_session::StreamingTurnCapture,
) {
    write_json(ctx, "streaming_partial.json", capture).await;
}

pub(crate) async fn write_session_state(
    ctx: &PromptTraceContext,
    phase: &str,
    session_copy_rx: oneshot::Receiver<
        anyhow::Result<crate::session::persistence::SessionStateCopy>,
    >,
) -> LocalArtifactOutcome {
    let copy = match session_copy_rx.await {
        Ok(Ok(copy)) => copy,
        Ok(Err(error)) => {
            record_artifact(
                &ctx.local_artifact_state,
                "session_state",
                ArtifactResult::Failed {
                    reason: "session_copy_failed",
                    error: Some(&error.to_string()),
                },
            );
            return LocalArtifactOutcome::Failed;
        }
        Err(error) => {
            record_artifact(
                &ctx.local_artifact_state,
                "session_state",
                ArtifactResult::Failed {
                    reason: "session_copy_cancelled",
                    error: Some(&error.to_string()),
                },
            );
            return LocalArtifactOutcome::Failed;
        }
    };
    let base = ctx
        .artifact_dir()
        .join("session_state")
        .join(safe_leaf(phase));
    for file in copy.files {
        let path = base.join(safe_relative_path(&file.name));
        if let Err(error) = write_path(path, &file.data).await {
            record_artifact(
                &ctx.local_artifact_state,
                "session_state",
                ArtifactResult::Failed {
                    reason: "write_failed",
                    error: Some(&error.to_string()),
                },
            );
            return LocalArtifactOutcome::Failed;
        }
    }
    record_artifact(
        &ctx.local_artifact_state,
        "session_state",
        ArtifactResult::Written,
    );
    LocalArtifactOutcome::Written
}

pub(crate) async fn write_permission_events(ctx: &PromptTraceContext, events: &[PermissionEvent]) {
    write_json(ctx, "permission_decisions.json", events).await;
}

pub(crate) async fn write_turn_messages(
    ctx: &PromptTraceContext,
    capture: atelier_chat_state::TurnCapture,
) -> bool {
    write_json(ctx, "turn_messages.json", &capture).await;
    true
}

pub(crate) async fn write_memory_state(ctx: &PromptTraceContext) {
    skip_artifact(
        &ctx.local_artifact_state,
        "memory.tar.gz",
        "local_memory_already_persisted",
    );
}

pub(crate) async fn write_unified_log(ctx: &PromptTraceContext, _wait: ArtifactWriteWait) {
    skip_artifact(
        &ctx.local_artifact_state,
        "unified_log.jsonl",
        "local_log_kept_in_standard_log_path",
    );
}

pub(crate) async fn write_harness_session_archive(
    ctx: &PromptTraceContext,
    archive: Result<Vec<u8>, SessionStateBuildError>,
) -> bool {
    match archive {
        Ok(bytes) => {
            write_bytes(ctx, "harness_session.tar.gz", &bytes).await;
            true
        }
        Err(error) => {
            record_artifact(
                &ctx.local_artifact_state,
                "harness_session.tar.gz",
                ArtifactResult::Failed {
                    reason: "archive_failed",
                    error: Some(&error.to_string()),
                },
            );
            false
        }
    }
}

pub(crate) async fn write_trace_artifact(
    ctx: &PromptTraceContext,
    content: &[u8],
    relative_path: &str,
    _content_type: &str,
    artifact_name: &str,
) {
    let path = safe_relative_path(relative_path);
    let filename = if path.as_os_str().is_empty() {
        PathBuf::from(safe_leaf(artifact_name))
    } else {
        path
    };
    write_bytes(ctx, &filename.to_string_lossy(), content).await;
}

pub(crate) async fn flush_then_write_error_manifest(
    ctx: &PromptTraceContext,
    _deadline: tokio::time::Instant,
) {
    write_error_manifest(ctx).await;
}

#[derive(Debug)]
pub(crate) struct SessionStateBuildError {
    message: String,
}

impl std::fmt::Display for SessionStateBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SessionStateBuildError {}

pub(crate) fn build_chat_history_session_state(
    _messages: &[atelier_sampling_types::conversation::ConversationItem],
) -> Result<Vec<u8>, SessionStateBuildError> {
    use std::io::Write as _;
    let mut output = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut output, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let bytes: &[u8] = &[];
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o600);
        header.set_cksum();
        archive
            .append_data(&mut header, "chat_history.jsonl", bytes)
            .map_err(|error| SessionStateBuildError {
                message: error.to_string(),
            })?;
        archive
            .into_inner()
            .and_then(|encoder| encoder.finish())
            .map_err(|error| SessionStateBuildError {
                message: error.to_string(),
            })?;
    }
    output.flush().map_err(|error| SessionStateBuildError {
        message: error.to_string(),
    })?;
    Ok(output)
}

async fn write_json<T: Serialize + ?Sized>(ctx: &PromptTraceContext, name: &str, value: &T) {
    let path = ctx.artifact_dir().join(safe_relative_path(name));
    match write_json_path(path, value).await {
        Ok(()) => record_artifact(&ctx.local_artifact_state, name, ArtifactResult::Written),
        Err(error) => record_artifact(
            &ctx.local_artifact_state,
            name,
            ArtifactResult::Failed {
                reason: "write_failed",
                error: Some(&error.to_string()),
            },
        ),
    }
}

async fn write_bytes(ctx: &PromptTraceContext, name: &str, bytes: &[u8]) {
    let path = ctx.artifact_dir().join(safe_relative_path(name));
    match write_path(path, bytes).await {
        Ok(()) => record_artifact(&ctx.local_artifact_state, name, ArtifactResult::Written),
        Err(error) => record_artifact(
            &ctx.local_artifact_state,
            name,
            ArtifactResult::Failed {
                reason: "write_failed",
                error: Some(&error.to_string()),
            },
        ),
    }
}

async fn write_json_path<T: Serialize + ?Sized>(path: PathBuf, value: &T) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_path(path, &bytes).await
}

async fn write_path(path: PathBuf, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, bytes).await?;
    Ok(())
}

fn safe_relative_path(value: &str) -> PathBuf {
    Path::new(value)
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect()
}

fn safe_leaf(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

async fn enrich_git_metadata(ctx: &PromptTraceContext, metadata: &mut PromptMetadata) {
    let cwd = ctx.session_info.cwd.clone();
    let result = tokio::task::spawn_blocking(move || {
        let repo = git2::Repository::discover(&cwd).ok()?;
        let root = repo
            .workdir()
            .map(|path| path.to_string_lossy().into_owned());
        let remote = repo
            .find_remote("origin")
            .ok()
            .and_then(|remote| remote.url().map(str::to_owned));
        Some((root, remote))
    })
    .await
    .ok()
    .flatten();
    if let Some((root, remote)) = result {
        metadata.repo_root = metadata.repo_root.take().or(root);
        metadata.remote_url = metadata.remote_url.take().or(remote);
        metadata.workspace_type = Some("git".to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_path_drops_parent_and_root_components() {
        assert_eq!(
            safe_relative_path("../../root/turn.json"),
            PathBuf::from("root").join("turn.json")
        );
    }

    #[test]
    fn source_has_no_remote_storage_primitives() {
        let source = include_str!("artifacts.rs");
        for forbidden in [
            concat!("atelier_runtime_events", "::gcs"),
            concat!("atelier_runtime_events", "::s3"),
            concat!("Upload", "Queue"),
            concat!("req", "west"),
        ] {
            assert!(
                !source.contains(forbidden),
                "found forbidden primitive: {forbidden}"
            );
        }
    }
}
