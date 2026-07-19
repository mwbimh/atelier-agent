//! Immutable, completed-conversation snapshots used by derived agents and
//! side queries.

use crate::sampling::ConversationItem;
use crate::session::info::Info;
use atelier_provider::{ProviderRegistry, WireApi, WireApiSource};
use chrono::{DateTime, Utc};
use std::io;
use std::path::{Path, PathBuf};

pub type ContextSnapshotId = String;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideQueryResponse {
    pub btw_id: String,
    pub snapshot_id: String,
    pub answer: String,
    pub provider: Option<String>,
    pub model: String,
    pub wire_api: Option<WireApi>,
    pub wire_api_source: Option<WireApiSource>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshot {
    pub id: ContextSnapshotId,
    pub source_session_id: String,
    pub source_cwd: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub source_turn_id: Option<String>,
    #[serde(default)]
    pub estimated_tokens: u64,
    #[serde(default)]
    pub source_revision: u64,
    pub items: Vec<ConversationItem>,
}

impl ContextSnapshot {
    pub fn from_items(
        source_session_id: impl Into<String>,
        source_cwd: impl Into<String>,
        items: Vec<ConversationItem>,
    ) -> Self {
        Self::from_items_with_metadata(source_session_id, source_cwd, items, None, None)
    }

    pub fn from_items_with_metadata(
        source_session_id: impl Into<String>,
        source_cwd: impl Into<String>,
        items: Vec<ConversationItem>,
        source_turn_id: Option<String>,
        source_revision: Option<u64>,
    ) -> Self {
        let items = snapshot_conversation(items);
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            source_session_id: source_session_id.into(),
            source_cwd: source_cwd.into(),
            created_at: Utc::now(),
            source_turn_id,
            estimated_tokens: items
                .iter()
                .filter_map(|item| serde_json::to_string(item).ok())
                .map(|item| xai_token_estimation::estimate_tokens(&item))
                .sum(),
            source_revision: source_revision.unwrap_or(items.len() as u64),
            items,
        }
    }

    pub fn append_context(&self, append_context: Option<&str>) -> Vec<ConversationItem> {
        let mut items = self.items.clone();
        if let Some(context) = append_context.filter(|value| !value.trim().is_empty()) {
            items.push(ConversationItem::user(context.to_owned()));
        }
        items
    }

    pub fn belongs_to_session(&self, session_id: &str) -> bool {
        self.source_session_id == session_id
    }

    pub fn path_for(info: &Info, snapshot_id: &str) -> PathBuf {
        crate::session::persistence::session_dir(info)
            .join("context_snapshots")
            .join(format!("{snapshot_id}.json"))
    }

    pub fn save(&self, info: &Info) -> io::Result<PathBuf> {
        let path = Self::path_for(info, &self.id);
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("context snapshot has no parent directory"))?;
        std::fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, bytes)?;
        std::fs::rename(&temp, &path)?;
        Ok(path)
    }

    pub fn load(info: &Info, snapshot_id: &str) -> io::Result<Self> {
        let path = Self::path_for(info, snapshot_id);
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn delete(info: &Info, snapshot_id: &str) -> io::Result<bool> {
        let path = Self::path_for(info, snapshot_id);
        match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn list(info: &Info) -> io::Result<Vec<Self>> {
        let directory = crate::session::persistence::session_dir(info).join("context_snapshots");
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut snapshots = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read(entry.path()).and_then(|bytes| {
                serde_json::from_slice::<Self>(&bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            }) {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(error) => tracing::warn!(?error, "skipping invalid context snapshot"),
            }
        }
        snapshots.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(snapshots)
    }
}

/// Keep only completed, model-visible conversation content.  A derived agent
/// builds its own system prompt, tools, permissions, and sandbox; carrying a
/// parent system item or an in-flight reasoning block would duplicate runtime
/// state and can produce malformed tool exchanges.
fn snapshot_conversation(items: Vec<ConversationItem>) -> Vec<ConversationItem> {
    let items = xai_chat_state::compaction_utils::strip_reasoning_blocks(items);
    let items = items
        .into_iter()
        .filter(|item| !matches!(item, ConversationItem::System(_)))
        .collect();
    completed_conversation(items)
}

/// Remove a trailing incomplete assistant/tool exchange from a conversation.
///
/// A snapshot is only allowed to end at a completed boundary. This is the
/// same invariant required by Anthropic Messages and prevents a side query or
/// derived agent from inheriting a half-emitted tool call.
pub fn completed_conversation(mut items: Vec<ConversationItem>) -> Vec<ConversationItem> {
    let Some(last) = items.last() else {
        return items;
    };

    if !matches!(last, ConversationItem::ToolResult(_)) {
        if matches!(
            last,
            ConversationItem::Assistant(assistant) if !assistant.tool_calls.is_empty()
        ) {
            items.pop();
        }
        return items;
    }

    // A tool-result run is complete only when it immediately follows an
    // assistant tool-call item and answers every call made by that item.
    // Otherwise the tail is an orphan/in-flight exchange and must not be
    // injected into a new Messages/Responses request.
    let result_start = items
        .iter()
        .rposition(|item| !matches!(item, ConversationItem::ToolResult(_)))
        .unwrap_or(0);
    let Some(ConversationItem::Assistant(assistant)) = items.get(result_start) else {
        // Preserve the completed non-tool item immediately before an orphan
        // result run (for example a normal user message followed by a stale
        // ToolResult from a cancelled turn).
        if !items.is_empty() {
            items.truncate(result_start + 1);
        }
        return items;
    };
    if assistant.tool_calls.is_empty() {
        items.truncate(result_start + 1);
        return items;
    }

    let answered: std::collections::HashSet<&str> = items[result_start + 1..]
        .iter()
        .filter_map(|item| match item {
            ConversationItem::ToolResult(result) => Some(result.tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    if assistant
        .tool_calls
        .iter()
        .any(|call| !answered.contains(call.id.as_ref()))
    {
        items.truncate(result_start);
    }
    items
}

pub fn snapshot_path_exists(info: &Info, snapshot_id: &str) -> bool {
    Path::new(&ContextSnapshot::path_for(info, snapshot_id)).exists()
}

impl super::SessionActor {
    pub(crate) async fn completed_context_snapshot(&self) -> ContextSnapshot {
        let items = self.chat_state_handle.get_conversation().await;
        ContextSnapshot::from_items(
            self.session_info.id.to_string(),
            self.session_info.cwd.clone(),
            items,
        )
    }

    pub(crate) async fn handle_side_question_snapshot(
        &self,
        question: &str,
        persist: bool,
    ) -> Result<String, String> {
        self.handle_side_question_snapshot_detailed(question, None, persist)
            .await
            .map(|response| response.answer)
    }

    pub(crate) async fn handle_side_question_snapshot_detailed(
        &self,
        question: &str,
        append_context: Option<&str>,
        persist: bool,
    ) -> Result<SideQueryResponse, String> {
        self.handle_side_question_snapshot_detailed_from(None, question, append_context, persist)
            .await
    }

    pub(crate) async fn handle_side_question_snapshot_detailed_from(
        &self,
        snapshot_id: Option<&str>,
        question: &str,
        append_context: Option<&str>,
        persist: bool,
    ) -> Result<SideQueryResponse, String> {
        let snapshot = if let Some(snapshot_id) = snapshot_id {
            let snapshot = ContextSnapshot::load(&self.session_info, snapshot_id)
                .map_err(|error| format!("failed to load context snapshot: {error}"))?;
            if !snapshot.belongs_to_session(self.session_info.id.0.as_ref()) {
                return Err("context snapshot belongs to a different session".to_owned());
            }
            snapshot
        } else {
            self.completed_context_snapshot().await
        };
        let mut items = snapshot.append_context(append_context);
        items = xai_chat_state::compaction_utils::strip_reasoning_blocks(items);
        items.retain(|item| !matches!(item, ConversationItem::System(_)));
        items.push(ConversationItem::user(format!(
            "Answer this side question directly in one response. You have no tools and must not propose actions:\n\n{question}"
        )));

        let configured_main_provider =
            ProviderRegistry::load_or_create(atelier_config::atelier_home().join("providers.toml"))
                .ok()
                .and_then(|registry| {
                    registry
                        .role(atelier_provider::RoleId::Main)
                        .map(|role| role.provider.clone())
                });
        let (sampling_client, provider, model) = if let Some((config, client)) = self
            .prepare_role_chat_completion(atelier_provider::RoleId::Main, false)
            .await
            .map_err(|error| format!("failed to prepare main-role side-query client: {error}"))?
        {
            (client, configured_main_provider.clone(), config.model)
        } else {
            let client = self
                .prepare_chat_completion(false)
                .await
                .map_err(|error| format!("failed to prepare side-query client: {error}"))?;
            let model = self
                .chat_state_handle
                .get_sampling_config()
                .await
                .map(|config| config.model)
                .unwrap_or_default();
            let (provider, model) = model
                .split_once('/')
                .map(|(provider, model)| (Some(provider.to_owned()), model.to_owned()))
                .unwrap_or((None, model));
            (client, provider, model)
        };
        let side_query_id = format!("btw-{}", uuid::Uuid::now_v7());
        let parent_session_id = self.session_info.id.to_string();
        let wire_resolution = provider.as_deref().and_then(|provider| {
            let key = atelier_provider::ModelKey::new(provider.to_owned(), model.clone()).ok()?;
            let registry = ProviderRegistry::load_or_create(
                atelier_config::atelier_home().join("providers.toml"),
            )
            .ok()?;
            registry.resolve_wire_api(&key).ok()
        });
        let persist_entry = |answer: String, success: bool, error: Option<String>| {
            if !persist {
                return;
            }
            let _ = self.notifications.persistence_tx.send(
                crate::session::persistence::PersistenceMsg::Btw(
                    crate::session::persistence::BtwEntry {
                        btw_session_id: side_query_id.clone(),
                        parent_session_id: parent_session_id.clone(),
                        asked_at: Utc::now(),
                        question: question.to_owned(),
                        answer,
                        model: model.clone(),
                        snapshot_id: Some(snapshot.id.clone()),
                        provider: provider.clone(),
                        wire_api: wire_resolution.as_ref().map(|resolved| resolved.wire_api),
                        wire_api_source: wire_resolution.as_ref().map(|resolved| resolved.source),
                        success,
                        error,
                    },
                ),
            );
        };
        let request = crate::sampling::ConversationRequest {
            items,
            tools: Vec::new(),
            model: Some(model.clone()),
            temperature: None,
            x_atelier_conv_id: Some(side_query_id.clone()),
            x_atelier_req_id: Some(format!("xai-btw-{}", uuid::Uuid::now_v7())),
            x_atelier_session_id: Some(parent_session_id.clone()),
            x_atelier_agent_id: Some(atelier_telemetry::id::agent_id()),
            ..Default::default()
        };
        let response = match sampling_client.conversation_collect(request).await {
            Ok(response) => response,
            Err(error) => {
                let message = format!("side query failed: {error}");
                persist_entry(String::new(), false, Some(message.clone()));
                return Err(message);
            }
        };
        let answer = response.assistant_text();
        if answer.trim().is_empty() {
            persist_entry(
                String::new(),
                false,
                Some("No response from model".to_owned()),
            );
            return Err("No response from model".to_owned());
        }
        persist_entry(answer.clone(), true, None);
        Ok(SideQueryResponse {
            btw_id: side_query_id,
            snapshot_id: snapshot.id,
            answer,
            provider,
            model,
            wire_api: wire_resolution.as_ref().map(|resolved| resolved.wire_api),
            wire_api_source: wire_resolution.as_ref().map(|resolved| resolved.source),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::completed_conversation;
    use crate::sampling::{ConversationItem, ToolCall};

    #[test]
    fn snapshot_drops_incomplete_tool_exchange_only_at_the_tail() {
        let items = vec![
            ConversationItem::user("keep"),
            ConversationItem::tool_result("call-1", "done"),
        ];
        let result = completed_conversation(items);
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ConversationItem::User(_)));
    }

    #[test]
    fn snapshot_keeps_a_completed_tool_exchange_at_the_tail() {
        let items = vec![
            ConversationItem::user("inspect"),
            ConversationItem::assistant_tool_calls(vec![ToolCall {
                id: "call-1".into(),
                name: "read_file".to_owned(),
                arguments: "{}".into(),
            }]),
            ConversationItem::tool_result("call-1", "contents"),
        ];
        let result = completed_conversation(items.clone());
        assert_eq!(result.len(), items.len());
        assert!(matches!(result[2], ConversationItem::ToolResult(_)));
    }

    #[test]
    fn snapshot_drops_a_partially_answered_tool_exchange() {
        let items = vec![
            ConversationItem::user("inspect"),
            ConversationItem::assistant_tool_calls(vec![
                ToolCall {
                    id: "call-1".into(),
                    name: "read_file".to_owned(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "call-2".into(),
                    name: "grep".to_owned(),
                    arguments: "{}".into(),
                },
            ]),
            ConversationItem::tool_result("call-1", "contents"),
        ];
        let result = completed_conversation(items);
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ConversationItem::User(_)));
    }

    #[test]
    fn append_context_is_after_the_immutable_items() {
        let snapshot = super::ContextSnapshot::from_items(
            "session",
            "C:/workspace",
            vec![ConversationItem::user("history")],
        );
        let items = snapshot.append_context(Some("focus"));
        assert!(matches!(items[0], ConversationItem::User(_)));
        assert!(matches!(items[1], ConversationItem::User(_)));
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.estimated_tokens > 0, true);
        assert_eq!(snapshot.source_revision, 1);
    }

    #[test]
    fn snapshot_metadata_is_stable_and_system_items_are_not_inherited() {
        let snapshot = super::ContextSnapshot::from_items_with_metadata(
            "session",
            "C:/workspace",
            vec![
                ConversationItem::system("parent system"),
                ConversationItem::user("keep"),
            ],
            Some("turn-7".to_owned()),
            Some(42),
        );
        assert_eq!(snapshot.source_turn_id.as_deref(), Some("turn-7"));
        assert_eq!(snapshot.source_revision, 42);
        assert_eq!(snapshot.items.len(), 1);
        assert!(matches!(snapshot.items[0], ConversationItem::User(_)));
    }

    #[test]
    fn snapshot_can_validate_its_source_session() {
        let snapshot = super::ContextSnapshot::from_items("session-a", "C:/workspace", vec![]);
        assert!(snapshot.belongs_to_session("session-a"));
        assert!(!snapshot.belongs_to_session("session-b"));
    }
}
