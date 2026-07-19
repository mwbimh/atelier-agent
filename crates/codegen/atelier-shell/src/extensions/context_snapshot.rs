//! Context snapshot and derived-agent control plane.

use agent_client_protocol as acp;
use agent_client_protocol::Agent as _;
use serde::Deserialize;
use std::path::PathBuf;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::session::context_snapshot::ContextSnapshot;
use crate::session::info::Info;
use crate::session::persistence::list_summaries;

pub const SNAPSHOT_CREATE: &str = "_atelier/context_snapshot/create";
pub const SNAPSHOT_GET: &str = "_atelier/context_snapshot/get";
pub const SNAPSHOT_LIST: &str = "_atelier/context_snapshot/list";
pub const SNAPSHOT_DELETE: &str = "_atelier/context_snapshot/delete";
pub const AGENT_SPAWN_DERIVED: &str = "_atelier/agent/spawn_derived";
pub const AGENT_SPAWN_PARALLEL: &str = "_atelier/agent/spawn_parallel";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotParams {
    session_id: String,
    #[serde(default)]
    snapshot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateParams {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpawnParams {
    session_id: String,
    role: String,
    prompt: String,
    #[serde(default)]
    inherit_from: Option<String>,
    #[serde(default)]
    append_context: Option<String>,
    #[serde(default)]
    background: bool,
    #[serde(default)]
    isolation: Option<String>,
    #[serde(default)]
    fresh: bool,
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        SNAPSHOT_CREATE | "atelier/context_snapshot/create" => create(agent, args).await,
        SNAPSHOT_GET | "atelier/context_snapshot/get" => get(args).await,
        SNAPSHOT_LIST | "atelier/context_snapshot/list" => list(args).await,
        SNAPSHOT_DELETE | "atelier/context_snapshot/delete" => delete(args).await,
        AGENT_SPAWN_DERIVED | "atelier/agent/spawn_derived" => spawn_derived(agent, args).await,
        AGENT_SPAWN_PARALLEL | "atelier/agent/spawn_parallel" => spawn_parallel(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn create(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: CreateParams = parse_params(args)?;
    let handle = agent
        .session_handle_now(&params.session_id)
        .ok_or_else(|| acp::Error::invalid_params().data("session not found"))?;
    let items = handle.chat_state_handle.get_conversation().await;
    let snapshot = make_snapshot(agent, &params.session_id, &handle.info.cwd, items);
    snapshot
        .save(&handle.info)
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    to_raw_response(&snapshot)
}

async fn get(args: &acp::ExtRequest) -> ExtResult {
    let params: SnapshotParams = parse_params(args)?;
    let info = resolve_info(&params.session_id).await?;
    let snapshot_id = params
        .snapshot_id
        .ok_or_else(|| acp::Error::invalid_params().data("snapshotId is required"))?;
    let snapshot = ContextSnapshot::load(&info, &snapshot_id)
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
    to_raw_response(&snapshot)
}

async fn list(args: &acp::ExtRequest) -> ExtResult {
    let params: SnapshotParams = parse_params(args)?;
    let info = resolve_info(&params.session_id).await?;
    let snapshots = ContextSnapshot::list(&info)
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    to_raw_response(&serde_json::json!({ "snapshots": snapshots }))
}

async fn delete(args: &acp::ExtRequest) -> ExtResult {
    let params: SnapshotParams = parse_params(args)?;
    let info = resolve_info(&params.session_id).await?;
    let snapshot_id = params
        .snapshot_id
        .ok_or_else(|| acp::Error::invalid_params().data("snapshotId is required"))?;
    let deleted = ContextSnapshot::delete(&info, &snapshot_id)
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    to_raw_response(&serde_json::json!({ "snapshotId": snapshot_id, "deleted": deleted }))
}

async fn spawn_derived(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: SpawnParams = parse_params(args)?;
    spawn_one(agent, params).await
}

async fn spawn_parallel(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: Vec<SpawnParams> = parse_params(args)?;
    if params.is_empty() {
        return Err(acp::Error::invalid_params().data("at least one derived agent is required"));
    }
    let shared_snapshot = if params
        .iter()
        .any(|params| !params.fresh && params.inherit_from.is_none())
    {
        let source_session_id = params[0].session_id.clone();
        let source = agent
            .session_handle_now(&source_session_id)
            .ok_or_else(|| acp::Error::invalid_params().data("source session not found"))?;
        let snapshot = make_snapshot(
            agent,
            &source_session_id,
            &source.info.cwd,
            source.chat_state_handle.get_conversation().await,
        );
        snapshot
            .save(&source.info)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        Some(snapshot)
    } else {
        None
    };
    let mut results = Vec::with_capacity(params.len());
    for params in params {
        results.push(spawn_one_value(agent, params, shared_snapshot.as_ref()).await?);
    }
    to_raw_response(&serde_json::json!({ "agents": results }))
}

async fn spawn_one(agent: &MvpAgent, args: SpawnParams) -> ExtResult {
    to_raw_response(&spawn_one_value(agent, args, None).await?)
}

async fn spawn_one_value(
    agent: &MvpAgent,
    params: SpawnParams,
    shared_snapshot: Option<&ContextSnapshot>,
) -> Result<serde_json::Value, acp::Error> {
    let isolation = validate_isolation(params.isolation.as_deref())?;
    let source = agent
        .session_handle_now(&params.session_id)
        .ok_or_else(|| acp::Error::invalid_params().data("source session not found"))?;
    let role_id = params
        .role
        .parse::<atelier_provider::RoleId>()
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
    let registry = atelier_provider::ProviderRegistry::load_or_create(
        atelier_config::atelier_home().join("providers.toml"),
    )
    .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    let role = registry
        .role(role_id)
        .filter(|role| role.provider != "default" && role.model != "default")
        .cloned()
        .ok_or_else(|| {
            acp::Error::invalid_params().data(format!("role {role_id} is not configured"))
        })?;
    if params.fresh && params.inherit_from.is_some() {
        return Err(
            acp::Error::invalid_params().data("fresh and inheritFrom are mutually exclusive")
        );
    }
    let snapshot = if params.fresh {
        let snapshot = make_snapshot(agent, &params.session_id, &source.info.cwd, Vec::new());
        snapshot
            .save(&source.info)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        snapshot
    } else if let Some(snapshot_id) = params.inherit_from.as_deref() {
        let snapshot = ContextSnapshot::load(&source.info, snapshot_id)
            .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
        if !snapshot.belongs_to_session(params.session_id.as_str()) {
            return Err(acp::Error::invalid_params()
                .data("context snapshot belongs to a different source session"));
        }
        snapshot
    } else if let Some(snapshot) = shared_snapshot {
        snapshot.clone()
    } else {
        let items = source.chat_state_handle.get_conversation().await;
        let snapshot = make_snapshot(agent, &params.session_id, &source.info.cwd, items);
        snapshot
            .save(&source.info)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        snapshot
    };
    let cwd = PathBuf::from(source.info.cwd.clone());
    let mut meta = acp::Meta::new();
    meta.insert(
        "modelId".to_owned(),
        serde_json::Value::String(format!("{}/{}", role.provider, role.model)),
    );
    meta.insert(
        "atelier/derivedFrom".to_owned(),
        serde_json::json!(snapshot.id),
    );
    meta.insert("role".to_owned(), serde_json::json!(role_id.as_str()));
    meta.insert(
        "atelier/role".to_owned(),
        serde_json::json!(role_id.as_str()),
    );
    let new_session = agent
        .new_session(acp::NewSessionRequest::new(cwd).meta(meta))
        .await
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    let child = agent
        .session_handle_now(&new_session.session_id.to_string())
        .ok_or_else(|| acp::Error::internal_error().data("derived session was not registered"))?;
    child
        .chat_state_handle
        .replace_conversation(snapshot.append_context(params.append_context.as_deref()));
    let _ = child.chat_state_handle.get_conversation().await;

    let prompt_id = format!("derived-{}", uuid::Uuid::now_v7());
    if !params.prompt.trim().is_empty() {
        let request = acp::PromptRequest::new(
            new_session.session_id.clone(),
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                params.prompt,
            ))],
        )
        .meta(
            serde_json::json!({ "promptId": prompt_id.clone(), "role": role_id.as_str() })
                .as_object()
                .cloned(),
        );
        if params.background {
            let agent_ref = crate::agent::mvp_agent::LocalRef::new(agent);
            tokio::task::spawn_local(async move {
                let _ = agent_ref.get().prompt(request).await;
            });
        } else {
            agent
                .prompt(request)
                .await
                .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        }
    }
    Ok(serde_json::json!({
        "sessionId": new_session.session_id,
        "sourceSessionId": params.session_id,
        "role": role_id.as_str(),
        "snapshotId": snapshot.id,
        "fresh": params.fresh,
        "background": params.background,
        "isolation": isolation,
        "promptId": prompt_id,
    }))
}

fn validate_isolation(value: Option<&str>) -> Result<&'static str, acp::Error> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("none") => Ok("none"),
        Some("worktree") => Err(acp::Error::invalid_params().data(
            "isolation=worktree is not implemented for derived agents; use /fork --worktree",
        )),
        Some(value) => Err(acp::Error::invalid_params()
            .data(format!("unsupported isolation mode '{value}'; use 'none'",))),
    }
}

fn make_snapshot(
    agent: &MvpAgent,
    session_id: &str,
    cwd: &str,
    items: Vec<crate::sampling::ConversationItem>,
) -> ContextSnapshot {
    let status = agent.runtime_status(session_id);
    let source_turn_id = status.as_ref().and_then(|status| status.turn_id.clone());
    let source_revision = agent.runtime_event_bounds().1;
    ContextSnapshot::from_items_with_metadata(
        session_id.to_owned(),
        cwd.to_owned(),
        items,
        source_turn_id,
        source_revision,
    )
}

async fn resolve_info(session_id: &str) -> Result<Info, acp::Error> {
    let summaries = list_summaries(None)
        .await
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    summaries
        .into_iter()
        .find(|summary| summary.info.id.0.as_ref() == session_id)
        .map(|summary| summary.info)
        .ok_or_else(|| acp::Error::invalid_params().data("session not found"))
}

#[cfg(test)]
mod tests {
    use super::validate_isolation;

    #[test]
    fn omitted_and_none_isolation_are_explicitly_supported() {
        assert_eq!(validate_isolation(None).unwrap(), "none");
        assert_eq!(validate_isolation(Some("none")).unwrap(), "none");
    }

    #[test]
    fn unsupported_isolation_is_rejected_instead_of_echoed() {
        let error = validate_isolation(Some("worktree")).expect_err("worktree is not wired");
        assert!(error.to_string().contains("not implemented"));
        let error = validate_isolation(Some("sandbox")).expect_err("unknown mode");
        assert!(error.to_string().contains("unsupported isolation mode"));
    }
}
