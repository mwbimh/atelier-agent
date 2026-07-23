//! Ephemeral side-query control plane.

use agent_client_protocol as acp;
use serde::Deserialize;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::session::SessionCommand;
use crate::session::info::Info;
use crate::session::persistence::{BtwEntry, list_summaries, session_dir};
use crate::session::storage::{JsonlStorageAdapter, StorageAdapter};
use crate::util::atelier_home::atelier_home;

pub const BTW_ASK: &str = "_atelier/btw/ask";
pub const BTW_GET: &str = "_atelier/btw/get";
pub const BTW_LIST: &str = "_atelier/btw/list";
pub const BTW_DELETE: &str = "_atelier/btw/delete";
pub const BTW_PERSIST: &str = "_atelier/btw/persist";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskParams {
    session_id: String,
    #[serde(default)]
    snapshot_id: Option<String>,
    question: String,
    #[serde(default)]
    append_context: Option<String>,
    #[serde(default)]
    persist: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryParams {
    session_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    btw_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistParams {
    session_id: String,
    btw_id: String,
    question: String,
    answer: String,
    #[serde(default)]
    snapshot_id: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    model: String,
    #[serde(default)]
    wire_api: Option<atelier_provider::WireApi>,
    #[serde(default)]
    wire_api_source: Option<atelier_provider::WireApiSource>,
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        BTW_ASK | "atelier/btw/ask" => ask(agent, args).await,
        BTW_GET | "atelier/btw/get" => get(args).await,
        BTW_LIST | "atelier/btw/list" => list(args).await,
        BTW_DELETE | "atelier/btw/delete" => delete(args).await,
        BTW_PERSIST | "atelier/btw/persist" => persist(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn ask(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: AskParams = parse_params(args)?;
    if params.question.trim().is_empty() {
        return Err(acp::Error::invalid_params().data("question must not be empty"));
    }
    let session_id = acp::SessionId::new(params.session_id.clone());
    let session = agent
        .session_handle_now(&params.session_id)
        .ok_or_else(|| acp::Error::invalid_params().data("session not found"))?;
    let side_query_id = format!("btw-{}", uuid::Uuid::now_v7());
    agent.runtime_begin_auxiliary_task(
        &side_query_id,
        session_id.0.as_ref(),
        atelier_provider::RoleId::Main.as_str(),
        xai_acp_lib::RuntimeState::PreparingContext,
        false,
    );
    let (respond_to, response) = tokio::sync::oneshot::channel();
    if session
        .cmd_tx
        .send(SessionCommand::SideQuestionDetailed {
            side_query_id: side_query_id.clone(),
            snapshot_id: params.snapshot_id,
            question: params.question,
            append_context: params.append_context,
            persist: params.persist,
            respond_to,
        })
        .is_err()
    {
        agent.runtime_finish_task(
            &side_query_id,
            xai_acp_lib::RuntimeState::Failed,
            Some("failed to dispatch side query".to_owned()),
        );
        return Err(acp::Error::internal_error().data("failed to dispatch side query"));
    }
    let result = match response.await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            agent.runtime_finish_task(
                &side_query_id,
                xai_acp_lib::RuntimeState::Failed,
                Some(error.clone()),
            );
            return Err(acp::Error::internal_error().data(error));
        }
        Err(_) => {
            agent.runtime_finish_task(
                &side_query_id,
                xai_acp_lib::RuntimeState::Failed,
                Some("side query did not respond".to_owned()),
            );
            return Err(acp::Error::internal_error().data("side query did not respond"));
        }
    };
    if result.btw_id != side_query_id {
        agent.runtime_finish_task(
            &side_query_id,
            xai_acp_lib::RuntimeState::Failed,
            Some("side query returned a mismatched task id".to_owned()),
        );
        return Err(acp::Error::internal_error().data("side query returned a mismatched task id"));
    }
    agent.runtime_finish_task(&side_query_id, xai_acp_lib::RuntimeState::Completed, None);
    to_raw_response(&serde_json::json!({
        "sessionId": session_id,
        "btwId": result.btw_id,
        "snapshotId": result.snapshot_id,
        "answer": result.answer,
        "provider": result.provider,
        "model": result.model,
        "wireApi": result.wire_api,
        "wireApiSource": result.wire_api_source,
        "persisted": params.persist,
    }))
}

async fn persist(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: PersistParams = parse_params(args)?;
    if params.btw_id.trim().is_empty()
        || params.question.trim().is_empty()
        || params.answer.trim().is_empty()
        || params.model.trim().is_empty()
    {
        return Err(acp::Error::invalid_params()
            .data("btwId, question, answer, and model must not be empty"));
    }
    let session = agent
        .session_handle_now(&params.session_id)
        .ok_or_else(|| acp::Error::invalid_params().data("session not found"))?;
    let existing = read_entries(&session.info)?
        .into_iter()
        .find(|entry| entry.btw_session_id == params.btw_id);
    if existing.is_some() {
        return to_raw_response(&serde_json::json!({
            "sessionId": params.session_id,
            "btwId": params.btw_id,
            "persisted": true,
            "alreadyPersisted": true,
        }));
    }
    let entry = BtwEntry {
        btw_session_id: params.btw_id.clone(),
        parent_session_id: params.session_id.clone(),
        asked_at: chrono::Utc::now(),
        question: params.question,
        answer: params.answer,
        model: params.model,
        snapshot_id: params.snapshot_id,
        provider: params.provider,
        wire_api: params.wire_api,
        wire_api_source: params.wire_api_source,
        success: true,
        error: None,
    };
    JsonlStorageAdapter::with_root(atelier_home())
        .append_btw(&session.info, &entry)
        .await
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    to_raw_response(&serde_json::json!({
        "sessionId": params.session_id,
        "btwId": entry.btw_session_id,
        "persisted": true,
        "alreadyPersisted": false,
    }))
}

async fn get(args: &acp::ExtRequest) -> ExtResult {
    let params: HistoryParams = parse_params(args)?;
    let id = params
        .btw_id
        .clone()
        .ok_or_else(|| acp::Error::invalid_params().data("btwId is required"))?;
    let entries = read_entries(&resolve_info(&params).await?)?;
    let entry = entries.into_iter().find(|entry| entry.btw_session_id == id);
    to_raw_response(&serde_json::json!({ "btwId": id, "entry": entry }))
}

async fn list(args: &acp::ExtRequest) -> ExtResult {
    let params: HistoryParams = parse_params(args)?;
    let entries = read_entries(&resolve_info(&params).await?)?;
    to_raw_response(&serde_json::json!({ "entries": entries }))
}

async fn delete(args: &acp::ExtRequest) -> ExtResult {
    let params: HistoryParams = parse_params(args)?;
    let id = params
        .btw_id
        .clone()
        .ok_or_else(|| acp::Error::invalid_params().data("btwId is required"))?;
    let info = resolve_info(&params).await?;
    let path = session_dir(&info).join("btw_history.jsonl");
    let mut entries = read_entries(&info)?;
    let before = entries.len();
    entries.retain(|entry| entry.btw_session_id != id);
    if entries.len() != before {
        let content = entries
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?
            .join("\n");
        tokio::fs::write(
            path,
            if content.is_empty() {
                content
            } else {
                format!("{content}\n")
            },
        )
        .await
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    }
    to_raw_response(&serde_json::json!({ "btwId": id, "deleted": entries.len() != before }))
}

async fn resolve_info(params: &HistoryParams) -> Result<Info, acp::Error> {
    if let Some(cwd) = &params.cwd {
        return Ok(Info {
            id: acp::SessionId::new(params.session_id.clone()),
            cwd: cwd.clone(),
        });
    }
    let summaries = list_summaries(None)
        .await
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    summaries
        .into_iter()
        .find(|summary| summary.info.id.0.as_ref() == params.session_id)
        .map(|summary| summary.info)
        .ok_or_else(|| acp::Error::invalid_params().data("session cwd is required"))
}

fn read_entries(info: &Info) -> Result<Vec<BtwEntry>, acp::Error> {
    let path = session_dir(info).join("btw_history.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path)
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .map_err(|error| acp::Error::internal_error().data(error.to_string()))
        })
        .collect()
}
