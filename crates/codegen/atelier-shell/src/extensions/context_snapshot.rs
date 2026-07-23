//! Context snapshot and derived-agent control plane.

use agent_client_protocol as acp;
use agent_client_protocol::Agent as _;
use serde::Deserialize;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::session::context_snapshot::{
    ContextSnapshot, compose_derived_conversation, snapshot_items_at_completed_boundary,
};
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

struct PreparedSpawn {
    params: SpawnParams,
    source: crate::session::SessionHandle,
    role_id: atelier_provider::RoleId,
    role: atelier_provider::RoleConfig,
    isolation: &'static str,
}

struct SpawnedDerived {
    session_id: acp::SessionId,
    source_session_id: String,
    role_id: atelier_provider::RoleId,
    snapshot_id: String,
    fresh: bool,
    background: bool,
    isolation: &'static str,
    worktree_path: Option<PathBuf>,
    prompt_id: Option<String>,
    prompt_route: DerivedPromptRoute,
}

#[derive(Debug, PartialEq, Eq)]
enum DerivedPromptRoute {
    None,
    Background(String),
    PagerAfterLoad(String),
}

fn route_derived_prompt(background: bool, prompt: String) -> DerivedPromptRoute {
    if prompt.trim().is_empty() {
        DerivedPromptRoute::None
    } else if background {
        DerivedPromptRoute::Background(prompt)
    } else {
        DerivedPromptRoute::PagerAfterLoad(prompt)
    }
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
    let deleted = ContextSnapshot::delete(&info, &snapshot_id).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidInput {
            acp::Error::invalid_params().data(error.to_string())
        } else {
            acp::Error::internal_error().data(error.to_string())
        }
    })?;
    to_raw_response(&serde_json::json!({ "snapshotId": snapshot_id, "deleted": deleted }))
}

async fn spawn_derived(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: SpawnParams = parse_params(args)?;
    let registry = load_provider_registry()?;
    let prepared = prepare_spawn(agent, params, &registry)?;
    let mut implicit_snapshots = HashMap::new();
    let snapshot = prepare_snapshot(agent, &prepared, &mut implicit_snapshots).await?;
    let spawned = create_spawned_session(agent, prepared, snapshot).await?;
    to_raw_response(&activate_spawned_session(agent, spawned))
}

async fn spawn_parallel(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: Vec<SpawnParams> = parse_params(args)?;
    if params.is_empty() {
        return Err(acp::Error::invalid_params().data("at least one derived agent is required"));
    }
    let registry = load_provider_registry()?;
    let prepared = prevalidate_all(params, |params| prepare_spawn(agent, params, &registry))?;

    // Snapshot loading/creation is also completed before any child session is
    // registered. A bad later item therefore cannot hide already-spawned
    // children from the caller by failing the batch before their ids are
    // returned.
    let mut implicit_snapshots = HashMap::new();
    let mut ready = Vec::with_capacity(prepared.len());
    for prepared in prepared {
        let snapshot = prepare_snapshot(agent, &prepared, &mut implicit_snapshots).await?;
        ready.push((prepared, snapshot));
    }

    // Session creation is transactional across the batch. First prompts are
    // deliberately not started until every child exists, so a later
    // `new_session` failure cannot leave either a live hidden child or a task
    // running for a child whose id was never returned to the caller.
    let spawned = create_all_or_rollback(
        ready,
        |(prepared, snapshot)| create_spawned_session(agent, prepared, snapshot),
        |spawned| rollback_spawned_session(agent, spawned),
    )
    .await?;
    let results = spawned
        .into_iter()
        .map(|spawned| activate_spawned_session(agent, spawned))
        .collect::<Vec<_>>();
    to_raw_response(&serde_json::json!({ "agents": results }))
}

fn load_provider_registry() -> Result<atelier_provider::ProviderRegistry, acp::Error> {
    atelier_provider::ProviderRegistry::load_or_create(
        atelier_config::atelier_home().join("providers.toml"),
    )
    .map_err(|error| acp::Error::internal_error().data(error.to_string()))
}

fn prevalidate_all<T, U, E>(
    values: Vec<T>,
    validate: impl FnMut(T) -> Result<U, E>,
) -> Result<Vec<U>, E> {
    values.into_iter().map(validate).collect()
}

async fn create_all_or_rollback<I, O, E, Create, CreateFuture, Rollback>(
    inputs: Vec<I>,
    mut create: Create,
    mut rollback: Rollback,
) -> Result<Vec<O>, E>
where
    Create: FnMut(I) -> CreateFuture,
    CreateFuture: Future<Output = Result<O, E>>,
    Rollback: FnMut(&O),
{
    let mut created = Vec::with_capacity(inputs.len());
    for input in inputs {
        match create(input).await {
            Ok(value) => created.push(value),
            Err(error) => {
                for value in created.iter().rev() {
                    rollback(value);
                }
                return Err(error);
            }
        }
    }
    Ok(created)
}

fn prepare_spawn(
    agent: &MvpAgent,
    params: SpawnParams,
    registry: &atelier_provider::ProviderRegistry,
) -> Result<PreparedSpawn, acp::Error> {
    let isolation = validate_isolation(params.isolation.as_deref())?;
    if params.fresh && params.inherit_from.is_some() {
        return Err(
            acp::Error::invalid_params().data("fresh and inheritFrom are mutually exclusive")
        );
    }
    let source = agent
        .session_handle_now(&params.session_id)
        .ok_or_else(|| acp::Error::invalid_params().data("source session not found"))?;
    let role_id = params
        .role
        .parse::<atelier_provider::RoleId>()
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
    ensure_user_derived_role(role_id)?;
    let role = registry
        .role(role_id)
        .filter(|role| role.is_configured())
        .cloned()
        .ok_or_else(|| {
            acp::Error::invalid_params().data(format!("role {role_id} is not configured"))
        })?;

    Ok(PreparedSpawn {
        params,
        source,
        role_id,
        role,
        isolation,
    })
}

fn ensure_user_derived_role(role_id: atelier_provider::RoleId) -> Result<(), acp::Error> {
    match role_id {
        atelier_provider::RoleId::Main
        | atelier_provider::RoleId::Explore
        | atelier_provider::RoleId::Implement
        | atelier_provider::RoleId::Review
        | atelier_provider::RoleId::Test => Ok(()),
        atelier_provider::RoleId::Compact
        | atelier_provider::RoleId::Summary
        | atelier_provider::RoleId::Title => Err(acp::Error::invalid_params().data(format!(
            "role {role_id} is an internal runtime role and cannot be spawned as a derived agent"
        ))),
    }
}

fn resolve_role_model_id(
    role_id: atelier_provider::RoleId,
    configured_role: Option<&atelier_provider::RoleConfig>,
    _source_model_id: &acp::ModelId,
) -> Result<acp::ModelId, acp::Error> {
    ensure_user_derived_role(role_id)?;
    if let Some(role) = configured_role {
        return Ok(acp::ModelId::new(format!(
            "{}/{}",
            role.provider, role.model
        )));
    }
    Err(acp::Error::invalid_params().data(format!("role {role_id} is not configured")))
}

async fn prepare_snapshot(
    agent: &MvpAgent,
    prepared: &PreparedSpawn,
    implicit_snapshots: &mut HashMap<String, ContextSnapshot>,
) -> Result<ContextSnapshot, acp::Error> {
    let params = &prepared.params;
    let source = &prepared.source;
    let snapshot = if params.fresh {
        let snapshot = make_snapshot(agent, &params.session_id, &source.info.cwd, Vec::new());
        snapshot
            .save(&source.info)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        snapshot
    } else if let Some(snapshot_id) = params.inherit_from.as_deref() {
        load_inherited_snapshot(&source.info, snapshot_id)?
    } else {
        if !implicit_snapshots.contains_key(&params.session_id) {
            let items = source.chat_state_handle.get_conversation().await;
            let snapshot = make_snapshot(agent, &params.session_id, &source.info.cwd, items);
            snapshot
                .save(&source.info)
                .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
            implicit_snapshots.insert(params.session_id.clone(), snapshot);
        }
        snapshot_for_source_session(implicit_snapshots, &params.session_id)?.clone()
    };

    validate_snapshot_session(&snapshot, &params.session_id)?;
    Ok(snapshot)
}

fn load_inherited_snapshot(info: &Info, snapshot_id: &str) -> Result<ContextSnapshot, acp::Error> {
    ContextSnapshot::load(info, snapshot_id)
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))
}

fn snapshot_for_source_session<'a>(
    snapshots: &'a HashMap<String, ContextSnapshot>,
    session_id: &str,
) -> Result<&'a ContextSnapshot, acp::Error> {
    let snapshot = snapshots.get(session_id).ok_or_else(|| {
        acp::Error::internal_error().data("context snapshot was not prepared for source session")
    })?;
    validate_snapshot_session(snapshot, session_id)?;
    Ok(snapshot)
}

fn validate_snapshot_session(
    snapshot: &ContextSnapshot,
    session_id: &str,
) -> Result<(), acp::Error> {
    if snapshot.belongs_to_session(session_id) {
        Ok(())
    } else {
        Err(acp::Error::invalid_params()
            .data("context snapshot belongs to a different source session"))
    }
}

async fn create_spawned_session(
    agent: &MvpAgent,
    prepared: PreparedSpawn,
    snapshot: ContextSnapshot,
) -> Result<SpawnedDerived, acp::Error> {
    let PreparedSpawn {
        mut params,
        source,
        role_id,
        role,
        isolation,
    } = prepared;
    validate_snapshot_session(&snapshot, &params.session_id)?;

    let source_cwd = PathBuf::from(source.info.cwd.clone());
    let worktree_path = if isolation == "worktree" {
        Some(create_derived_worktree(agent, &source_cwd).await?)
    } else {
        None
    };
    let cwd = worktree_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| source_cwd.clone());
    let mut meta = acp::Meta::new();
    meta.insert(
        "atelier/derivedFrom".to_owned(),
        serde_json::json!(snapshot.id),
    );
    meta.insert("role".to_owned(), serde_json::json!(role_id.as_str()));
    meta.insert(
        "atelier/role".to_owned(),
        serde_json::json!(role_id.as_str()),
    );
    meta.insert(
        "atelier/roleSnapshot".to_owned(),
        serde_json::to_value(&role)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?,
    );
    let new_session = match agent
        .new_session(acp::NewSessionRequest::new(cwd).meta(meta))
        .await
    {
        Ok(session) => session,
        Err(error) => {
            if let Some(path) = worktree_path.as_ref() {
                remove_derived_worktree(path).await;
            }
            return Err(acp::Error::internal_error().data(error.to_string()));
        }
    };
    let child = match agent.session_handle_now(&new_session.session_id.to_string()) {
        Some(child) => child,
        None => {
            agent.request_session_shutdown(&new_session.session_id);
            agent.remove_session(&new_session.session_id);
            if let Some(path) = worktree_path.as_ref() {
                remove_derived_worktree(path).await;
            }
            return Err(acp::Error::internal_error().data("derived session was not registered"));
        }
    };
    let child_conversation = child.chat_state_handle.get_conversation().await;
    child
        .chat_state_handle
        .replace_conversation(compose_derived_conversation(
            child_conversation,
            &snapshot,
            params.append_context.as_deref(),
        ));
    let _ = child.chat_state_handle.get_conversation().await;

    let prompt_route = route_derived_prompt(params.background, std::mem::take(&mut params.prompt));
    let prompt_id = matches!(prompt_route, DerivedPromptRoute::Background(_))
        .then(|| format!("derived-{}", uuid::Uuid::now_v7()));
    Ok(SpawnedDerived {
        session_id: new_session.session_id,
        source_session_id: params.session_id,
        role_id,
        snapshot_id: snapshot.id,
        fresh: params.fresh,
        background: params.background,
        isolation,
        worktree_path,
        prompt_id,
        prompt_route,
    })
}

fn rollback_spawned_session(agent: &MvpAgent, spawned: &SpawnedDerived) {
    if let Some(prompt_id) = spawned.prompt_id.as_deref() {
        agent.clear_detach_waiter(prompt_id);
        agent.forget_retryable_prompt(prompt_id);
        if agent
            .runtime_task(prompt_id)
            .is_some_and(|task| !task.state.is_terminal())
        {
            agent.runtime_finish_task(
                prompt_id,
                crate::runtime_control::RuntimeState::Failed,
                Some("parallel derived-session creation rolled back".to_owned()),
            );
        }
    }
    agent.request_session_shutdown(&spawned.session_id);
    agent.remove_session(&spawned.session_id);
    if let Some(path) = spawned.worktree_path.clone() {
        tokio::task::spawn_local(async move {
            remove_derived_worktree(&path).await;
        });
    }
}

fn activate_spawned_session(agent: &MvpAgent, spawned: SpawnedDerived) -> serde_json::Value {
    let pending_first_prompt = match spawned.prompt_route {
        DerivedPromptRoute::None => None,
        DerivedPromptRoute::PagerAfterLoad(prompt) => Some(prompt),
        DerivedPromptRoute::Background(prompt) => {
            start_background_prompt(
                agent,
                spawned.session_id.clone(),
                spawned
                    .prompt_id
                    .clone()
                    .expect("background routes always allocate a prompt id"),
                spawned.role_id,
                prompt,
            );
            None
        }
    };
    serde_json::json!({
        "sessionId": spawned.session_id,
        "sourceSessionId": spawned.source_session_id,
        "role": spawned.role_id.as_str(),
        "snapshotId": spawned.snapshot_id,
        "fresh": spawned.fresh,
        "background": spawned.background,
        "isolation": spawned.isolation,
        "worktreePath": spawned.worktree_path,
        "promptId": spawned.prompt_id,
        "pendingFirstPrompt": pending_first_prompt,
    })
}

fn start_background_prompt(
    agent: &MvpAgent,
    session_id: acp::SessionId,
    prompt_id: String,
    role_id: atelier_provider::RoleId,
    prompt: String,
) {
    agent.runtime_begin_auxiliary_task(
        &prompt_id,
        session_id.0.as_ref(),
        role_id.as_str(),
        crate::runtime_control::RuntimeState::PreparingContext,
        false,
    );
    let request = acp::PromptRequest::new(
        session_id,
        vec![acp::ContentBlock::Text(acp::TextContent::new(prompt))],
    )
    .meta(
        serde_json::json!({ "promptId": prompt_id.clone(), "role": role_id.as_str() })
            .as_object()
            .cloned(),
    );
    let agent_ref = crate::agent::mvp_agent::LocalRef::new(agent);
    tokio::task::spawn_local(async move {
        let error = agent_ref
            .get()
            .prompt(request)
            .await
            .err()
            .map(|error| error.to_string());
        let task = agent_ref.get().runtime_task(&prompt_id);
        if let Some((state, diagnostic_message)) =
            background_prompt_placeholder_completion(task.as_ref(), error)
        {
            agent_ref
                .get()
                .runtime_finish_task(&prompt_id, state, diagnostic_message);
        }
    });
}

fn background_prompt_placeholder_completion(
    task: Option<&crate::runtime_control::RuntimeTask>,
    error: Option<String>,
) -> Option<(crate::runtime_control::RuntimeState, Option<String>)> {
    let task = task?;
    if task.attachable || task.state.is_terminal() {
        return None;
    }
    Some(match error {
        Some(error) => (crate::runtime_control::RuntimeState::Failed, Some(error)),
        None => (crate::runtime_control::RuntimeState::Completed, None),
    })
}

fn validate_isolation(value: Option<&str>) -> Result<&'static str, acp::Error> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("none") => Ok("none"),
        Some("worktree") => Ok("worktree"),
        Some(value) => Err(acp::Error::invalid_params()
            .data(format!("unsupported isolation mode '{value}'; use 'none'",))),
    }
}

async fn create_derived_worktree(
    agent: &MvpAgent,
    source_cwd: &std::path::Path,
) -> Result<PathBuf, acp::Error> {
    let base =
        crate::session::worktree::worktree_base_dir_for_source(source_cwd).map_err(|error| {
            acp::Error::invalid_params().data(format!(
                "worktree isolation requires a Git workspace: {error}"
            ))
        })?;
    let id = format!("derived-{}", uuid::Uuid::now_v7());
    let destination = base.join(&id);
    let source = source_cwd.to_path_buf();
    let creation_mode: xai_fast_worktree::CreationMode = agent.worktree_type.into();
    let btrfs_delegate = crate::session::worktree::btrfs_delegate_from_env();
    tokio::task::spawn_blocking(move || {
        let mut builder = xai_fast_worktree::WorktreeBuilder::new(&source, &destination)
            .working_tree_mode(xai_fast_worktree::WorkingTreeMode::PreserveWorkingTree)
            .creation_mode(creation_mode)
            .worktree_kind(xai_fast_worktree::WorktreeKind::Subagent)
            .session_id(id);
        if let Some(delegate) = btrfs_delegate {
            builder = builder.btrfs_delegate(delegate);
        }
        builder.create().map(|report| report.worktree_path)
    })
    .await
    .map_err(|error| {
        acp::Error::internal_error().data(format!("worktree creation task failed: {error}"))
    })?
    .map_err(|error| {
        acp::Error::internal_error().data(format!("failed to create derived worktree: {error}"))
    })
}

async fn remove_derived_worktree(path: &std::path::Path) {
    let path = path.to_path_buf();
    match tokio::task::spawn_blocking(move || xai_fast_worktree::remove_worktree(&path)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "failed to remove rolled-back derived worktree"),
        Err(error) => tracing::warn!(%error, "derived worktree cleanup task failed"),
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
    let current_prompt_id = agent
        .session_handle_now(session_id)
        .and_then(|handle| handle.current_prompt_id.lock().ok()?.clone());
    ContextSnapshot::from_items_with_metadata(
        session_id.to_owned(),
        cwd.to_owned(),
        snapshot_items_for_source(items, current_prompt_id.as_deref()),
        source_turn_id,
        source_revision,
    )
}

fn snapshot_items_for_source(
    items: Vec<crate::sampling::ConversationItem>,
    current_prompt_id: Option<&str>,
) -> Vec<crate::sampling::ConversationItem> {
    snapshot_items_at_completed_boundary(items, current_prompt_id.is_some())
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
    use super::{
        DerivedPromptRoute, background_prompt_placeholder_completion, create_all_or_rollback,
        load_inherited_snapshot, prevalidate_all, resolve_role_model_id, route_derived_prompt,
        snapshot_for_source_session, snapshot_items_for_source, validate_isolation,
    };
    use crate::runtime_control::{RuntimeState, RuntimeTask};
    use crate::sampling::{ConversationItem, SyntheticReason};
    use crate::session::context_snapshot::{ContextSnapshot, compose_derived_conversation};
    use crate::session::info::Info;
    use agent_client_protocol as acp;
    use atelier_provider::{RoleConfig, RoleId};
    use std::collections::HashMap;

    fn test_info() -> Info {
        Info {
            id: acp::SessionId::new("context-snapshot-inherit-path-test"),
            cwd: "C:/workspace".to_owned(),
        }
    }

    fn child_runtime_prefix() -> Vec<ConversationItem> {
        vec![
            ConversationItem::system("child system prompt"),
            ConversationItem::project_instructions("child AGENTS.md instructions"),
        ]
    }

    fn assert_child_runtime_prefix(items: &[ConversationItem]) {
        assert!(
            matches!(&items[0], ConversationItem::System(system) if system.content.as_ref() == "child system prompt"),
            "derived session must retain its freshly rendered system prompt"
        );
        assert!(
            matches!(
                &items[1],
                ConversationItem::User(user)
                    if user.synthetic_reason == Some(SyntheticReason::ProjectInstructions)
            ),
            "derived session must retain its freshly discovered project instructions"
        );
    }

    #[test]
    fn fresh_derived_session_retains_its_own_system_prompt_and_project_instructions() {
        let snapshot = ContextSnapshot::from_items("parent", "C:/workspace", Vec::new());

        let items = compose_derived_conversation(child_runtime_prefix(), &snapshot, None);

        assert_child_runtime_prefix(&items);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn inherited_derived_session_retains_its_own_system_prompt_and_project_instructions() {
        let snapshot = ContextSnapshot::from_items(
            "parent",
            "C:/workspace",
            vec![
                ConversationItem::system("parent system prompt"),
                ConversationItem::project_instructions("parent AGENTS.md instructions"),
                ConversationItem::user("parent history"),
            ],
        );

        let items = compose_derived_conversation(child_runtime_prefix(), &snapshot, None);

        assert_child_runtime_prefix(&items);
        assert!(
            matches!(&items[2], ConversationItem::User(user) if user.synthetic_reason.is_none())
        );
        assert_eq!(
            items.len(),
            3,
            "parent runtime prefix must not be inherited"
        );
    }

    #[test]
    fn appended_derived_session_retains_its_own_runtime_prefix_before_appended_context() {
        let snapshot = ContextSnapshot::from_items(
            "parent",
            "C:/workspace",
            vec![ConversationItem::user("parent history")],
        );

        let items = compose_derived_conversation(
            child_runtime_prefix(),
            &snapshot,
            Some("additional derived context"),
        );

        assert_child_runtime_prefix(&items);
        assert_eq!(items.len(), 4);
        assert!(
            matches!(&items[2], ConversationItem::User(user) if user.synthetic_reason.is_none())
        );
        assert!(
            matches!(&items[3], ConversationItem::User(user) if user.synthetic_reason.is_none())
        );
        let inherited = serde_json::to_string(&items[2]).unwrap();
        let appended = serde_json::to_string(&items[3]).unwrap();
        assert!(inherited.contains("parent history"));
        assert!(!inherited.contains("additional derived context"));
        assert!(appended.contains("additional derived context"));
        assert!(!appended.contains("parent history"));
    }

    #[test]
    fn derived_snapshot_excludes_the_source_sessions_running_turn() {
        let items = vec![
            ConversationItem::user("completed question"),
            ConversationItem::assistant("completed answer"),
            ConversationItem::user("running question"),
        ];

        let snapshot_items = snapshot_items_for_source(items, Some("prompt-running"));

        assert_eq!(snapshot_items.len(), 2);
        let wire = serde_json::to_string(&snapshot_items).unwrap();
        assert!(!wire.contains("running question"));
        assert!(wire.contains("completed answer"));
    }

    #[cfg(windows)]
    fn absolute_snapshot_id() -> &'static str {
        r"C:\outside\snapshot"
    }

    #[cfg(not(windows))]
    fn absolute_snapshot_id() -> &'static str {
        "/tmp/outside/snapshot"
    }

    #[test]
    fn inherit_rejects_absolute_traversal_and_invalid_uuid_ids() {
        let info = test_info();

        for snapshot_id in [absolute_snapshot_id(), "../outside", "not-a-uuid"] {
            let error = load_inherited_snapshot(&info, snapshot_id)
                .expect_err("inherit must reject an unsafe snapshot id before filesystem access");
            assert!(
                error.to_string().contains("canonical UUID"),
                "{snapshot_id}: {error}"
            );
        }
    }

    #[test]
    fn omitted_and_none_isolation_are_explicitly_supported() {
        assert_eq!(validate_isolation(None).unwrap(), "none");
        assert_eq!(validate_isolation(Some("none")).unwrap(), "none");
    }

    #[test]
    fn worktree_isolation_is_supported_and_unknown_modes_are_rejected() {
        assert_eq!(validate_isolation(Some("worktree")).unwrap(), "worktree");
        let error = validate_isolation(Some("sandbox")).expect_err("unknown mode");
        assert!(error.to_string().contains("unsupported isolation mode"));
    }

    #[test]
    fn parallel_snapshots_are_selected_per_source_session_and_validated() {
        let first = ContextSnapshot::from_items("session-a", "C:/repo-a", Vec::new());
        let second = ContextSnapshot::from_items("session-b", "C:/repo-b", Vec::new());
        let snapshots = HashMap::from([
            ("session-a".to_owned(), first.clone()),
            ("session-b".to_owned(), second.clone()),
        ]);

        assert_eq!(
            snapshot_for_source_session(&snapshots, "session-a")
                .unwrap()
                .id,
            first.id
        );
        assert_eq!(
            snapshot_for_source_session(&snapshots, "session-b")
                .unwrap()
                .id,
            second.id
        );

        let mismatched = HashMap::from([("session-b".to_owned(), first)]);
        let error = snapshot_for_source_session(&mismatched, "session-b")
            .expect_err("snapshot ownership must be checked for every item");
        assert!(error.to_string().contains("different source session"));
    }

    #[test]
    fn parallel_prevalidation_does_not_expose_partial_results() {
        let mut validated = Vec::new();
        let error = prevalidate_all(vec!["first", "invalid", "never-reached"], |value| {
            validated.push(value);
            if value == "invalid" {
                Err("invalid role")
            } else {
                Ok(value)
            }
        })
        .expect_err("a late validation error must reject the whole batch");

        assert_eq!(error, "invalid role");
        assert_eq!(validated, vec!["first", "invalid"]);
    }

    #[test]
    fn every_unconfigured_role_fails_instead_of_inheriting_source_model() {
        let source_model = acp::ModelId::new("local-provider/current-model");

        let error = resolve_role_model_id(RoleId::Main, None, &source_model)
            .expect_err("main must not inherit the source session model");
        assert!(error.to_string().contains("role main is not configured"));

        let error = resolve_role_model_id(RoleId::Explore, None, &source_model)
            .expect_err("all roles require explicit configuration");
        assert!(error.to_string().contains("role explore is not configured"));

        let configured = RoleConfig::new("configured-provider", "configured-model").unwrap();
        assert_eq!(
            resolve_role_model_id(RoleId::Main, Some(&configured), &source_model)
                .unwrap()
                .0
                .as_ref(),
            "configured-provider/configured-model"
        );
    }

    #[test]
    fn internal_runtime_roles_cannot_be_spawned_as_derived_agents() {
        let source_model = acp::ModelId::new("local-provider/current-model");
        let configured = RoleConfig::new("provider", "model").unwrap();

        for role in [RoleId::Compact, RoleId::Summary, RoleId::Title] {
            let error = resolve_role_model_id(role, Some(&configured), &source_model)
                .expect_err("internal runtime roles must not be derived agents");
            assert!(error.to_string().contains("cannot be spawned"));
        }
    }

    #[test]
    fn foreground_prompt_is_returned_to_the_pager_instead_of_run_in_shell() {
        assert_eq!(
            route_derived_prompt(false, "inspect permissions".to_owned()),
            DerivedPromptRoute::PagerAfterLoad("inspect permissions".to_owned())
        );
        assert_eq!(
            route_derived_prompt(true, "background review".to_owned()),
            DerivedPromptRoute::Background("background review".to_owned())
        );
        assert_eq!(
            route_derived_prompt(false, "   ".to_owned()),
            DerivedPromptRoute::None
        );
    }

    #[tokio::test]
    async fn parallel_creation_failure_rolls_back_every_created_session_before_returning_error() {
        let cleanup = std::cell::RefCell::new(Vec::new());

        let error = create_all_or_rollback(
            vec!["first", "second", "fails"],
            |name| async move {
                if name == "fails" {
                    Err("session spawn failed")
                } else {
                    Ok((format!("session-{name}"), format!("task-{name}")))
                }
            },
            |created| cleanup.borrow_mut().push(created.clone()),
        )
        .await
        .expect_err("a later session failure must reject the whole batch");

        assert_eq!(error, "session spawn failed");
        assert_eq!(
            cleanup.into_inner(),
            vec![
                ("session-second".to_owned(), "task-second".to_owned()),
                ("session-first".to_owned(), "task-first".to_owned()),
            ],
            "every already-created session and task identity must roll back in reverse order"
        );
    }

    #[test]
    fn background_first_prompt_early_failure_becomes_an_observable_failed_task() {
        let placeholder = RuntimeTask {
            id: "derived-prompt".to_owned(),
            session_id: "derived-session".to_owned(),
            turn_id: None,
            agent_id: "derived-session".to_owned(),
            role: "explore".to_owned(),
            state: RuntimeState::PreparingContext,
            started_at_ms: 1,
            last_event_id: 1,
            attachable: false,
            diagnostic_message: None,
        };

        assert_eq!(
            background_prompt_placeholder_completion(
                Some(&placeholder),
                Some("role configuration could not be loaded".to_owned()),
            ),
            Some((
                RuntimeState::Failed,
                Some("role configuration could not be loaded".to_owned())
            ))
        );
    }
}
