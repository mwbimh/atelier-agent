//! Immutable, completed-conversation snapshots used by derived agents and
//! side queries.

use crate::sampling::{ConversationItem, SyntheticReason};
use crate::session::info::Info;
use atelier_provider::{ProviderRegistry, WireApi, WireApiSource};
use chrono::{DateTime, Utc};
use std::io;
use std::io::Write as _;
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

fn build_side_query_request(
    snapshot: &ContextSnapshot,
    append_context: Option<&str>,
    question: &str,
    model: &str,
    side_query_id: &str,
    parent_session_id: &str,
) -> crate::sampling::ConversationRequest {
    let mut items = snapshot.append_context(append_context);
    items = xai_chat_state::compaction_utils::strip_reasoning_blocks(items);
    items.retain(|item| !matches!(item, ConversationItem::System(_)));
    items.push(ConversationItem::user(format!(
        "Answer this side question directly in one response. You have no tools and must not propose actions:\n\n{question}"
    )));
    crate::sampling::ConversationRequest {
        items,
        tools: Vec::new(),
        model: Some(model.to_owned()),
        temperature: None,
        x_atelier_conv_id: Some(side_query_id.to_owned()),
        x_atelier_req_id: Some(format!("xai-btw-{}", uuid::Uuid::now_v7())),
        x_atelier_session_id: Some(parent_session_id.to_owned()),
        x_atelier_agent_id: Some(atelier_telemetry::id::agent_id()),
        ..Default::default()
    }
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

    pub fn path_for(info: &Info, snapshot_id: &str) -> io::Result<PathBuf> {
        let parsed_id = uuid::Uuid::parse_str(snapshot_id).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "context snapshot id must be a canonical UUID",
            )
        })?;
        if parsed_id.to_string() != snapshot_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "context snapshot id must be a canonical UUID",
            ));
        }

        let directory = crate::session::persistence::session_dir(info).join("context_snapshots");
        let path = directory.join(format!("{parsed_id}.json"));
        if path.parent() != Some(directory.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "context snapshot path must remain inside context_snapshots",
            ));
        }
        Ok(path)
    }

    pub fn save(&self, info: &Info) -> io::Result<PathBuf> {
        let path = Self::path_for(info, &self.id)?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("context snapshot has no parent directory"))?;
        std::fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        write_snapshot_once(&path, &bytes)?;
        Ok(path)
    }

    pub fn load(info: &Info, snapshot_id: &str) -> io::Result<Self> {
        let path = Self::path_for(info, snapshot_id)?;
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn delete(info: &Info, snapshot_id: &str) -> io::Result<bool> {
        let path = Self::path_for(info, snapshot_id)?;
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

fn write_snapshot_once(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn compose_derived_conversation(
    child_conversation: Vec<ConversationItem>,
    snapshot: &ContextSnapshot,
    append_context: Option<&str>,
) -> Vec<ConversationItem> {
    let mut conversation = child_conversation
        .into_iter()
        .filter(is_derived_runtime_prefix)
        .collect::<Vec<_>>();
    conversation.extend(
        snapshot
            .append_context(append_context)
            .into_iter()
            .filter(|item| !is_derived_runtime_prefix(item)),
    );
    conversation
}

fn is_derived_runtime_prefix(item: &ConversationItem) -> bool {
    matches!(item, ConversationItem::System(_))
        || matches!(
            item,
            ConversationItem::User(user)
                if user.synthetic_reason == Some(SyntheticReason::ProjectInstructions)
        )
}

fn is_snapshot_inheritable(item: &ConversationItem) -> bool {
    match item {
        ConversationItem::System(_) => false,
        ConversationItem::User(user) => match user.synthetic_reason.as_ref() {
            None | Some(SyntheticReason::CompactionMeta | SyntheticReason::Interjection) => true,
            Some(
                SyntheticReason::SystemReminder
                | SyntheticReason::ProjectInstructions
                | SyntheticReason::AutoContinue
                | SyntheticReason::AutoRecovery
                | SyntheticReason::TaskCompleted
                | SyntheticReason::SubagentCompleted
                | SyntheticReason::NotificationDrain
                | SyntheticReason::GoalSummary
                | SyntheticReason::GoalClassifierNudge
                | SyntheticReason::SchedulerFired
                | SyntheticReason::Unknown,
            ) => false,
        },
        _ => true,
    }
}

/// Keep only completed, model-visible conversation content.  A derived agent
/// builds its own system prompt, project instructions, tools, permissions, and
/// sandbox; carrying a parent runtime prefix or an in-flight reasoning block
/// would duplicate runtime state and can produce malformed tool exchanges.
fn snapshot_conversation(items: Vec<ConversationItem>) -> Vec<ConversationItem> {
    let items = xai_chat_state::compaction_utils::strip_reasoning_blocks(items);
    let items = items.into_iter().filter(is_snapshot_inheritable).collect();
    completed_conversation(items)
}

pub(crate) fn snapshot_items_at_completed_boundary(
    mut items: Vec<ConversationItem>,
    has_in_flight_turn: bool,
) -> Vec<ConversationItem> {
    if has_in_flight_turn
        && let Some(turn_start) = items.iter().rposition(|item| {
            matches!(
                item,
                ConversationItem::User(user)
                    if user.synthetic_reason.is_none()
                        || user
                            .synthetic_reason
                            .as_ref()
                            .is_some_and(SyntheticReason::starts_prompt_turn)
            )
        })
    {
        items.truncate(turn_start);
    }
    items
}

/// Remove a trailing incomplete assistant/tool exchange from a conversation.
///
/// A snapshot is only allowed to end at a completed boundary. This is the
/// same invariant required by Anthropic Messages and prevents a side query or
/// derived agent from inheriting a half-emitted tool call.
pub fn completed_conversation(items: Vec<ConversationItem>) -> Vec<ConversationItem> {
    let mut completed = Vec::with_capacity(items.len());
    let mut index = 0;
    while index < items.len() {
        match &items[index] {
            ConversationItem::Assistant(assistant) if !assistant.tool_calls.is_empty() => {
                let result_start = index + 1;
                let mut result_end = result_start;
                while matches!(items.get(result_end), Some(ConversationItem::ToolResult(_))) {
                    result_end += 1;
                }

                let expected = assistant
                    .tool_calls
                    .iter()
                    .map(|call| call.id.as_ref())
                    .collect::<std::collections::HashSet<_>>();
                let answered = items[result_start..result_end]
                    .iter()
                    .filter_map(|item| match item {
                        ConversationItem::ToolResult(result) => Some(result.tool_call_id.as_str()),
                        _ => None,
                    })
                    .collect::<std::collections::HashSet<_>>();

                if !expected.is_empty() && expected == answered {
                    completed.extend(items[index..result_end].iter().cloned());
                }
                index = result_end;
            }
            ConversationItem::ToolResult(_) => {
                // Orphaned results cannot be sent to any supported Provider.
                index += 1;
            }
            item => {
                completed.push(item.clone());
                index += 1;
            }
        }
    }
    completed
}

pub fn snapshot_path_exists(info: &Info, snapshot_id: &str) -> bool {
    ContextSnapshot::path_for(info, snapshot_id)
        .map(|path| path.exists())
        .unwrap_or(false)
}

impl super::SessionActor {
    pub(crate) async fn completed_context_snapshot(&self) -> ContextSnapshot {
        let items = snapshot_items_at_completed_boundary(
            self.chat_state_handle.get_conversation().await,
            self.session_turn_active
                .load(std::sync::atomic::Ordering::Acquire),
        );
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
        self.handle_side_question_snapshot_detailed_from(
            None,
            None,
            question,
            append_context,
            persist,
        )
        .await
    }

    pub(crate) async fn handle_side_question_snapshot_detailed_from(
        &self,
        side_query_id: Option<&str>,
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
        let configured_main_provider =
            ProviderRegistry::load_or_create(atelier_config::atelier_home().join("providers.toml"))
                .ok()
                .and_then(|registry| {
                    registry
                        .role(atelier_provider::RoleId::Main)
                        .map(|role| role.provider.clone())
                });
        let (config, sampling_client) = self
            .prepare_role_chat_completion(atelier_provider::RoleId::Main, false)
            .await
            .map_err(|error| format!("failed to prepare main-role side-query client: {error}"))?;
        let provider = configured_main_provider;
        let model = config.model;
        let side_query_id = side_query_id
            .map(str::to_owned)
            .unwrap_or_else(|| format!("btw-{}", uuid::Uuid::now_v7()));
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
        let request = build_side_query_request(
            &snapshot,
            append_context,
            question,
            &model,
            &side_query_id,
            &parent_session_id,
        );
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
    use super::{
        ContextSnapshot, build_side_query_request, completed_conversation,
        snapshot_items_at_completed_boundary, write_snapshot_once,
    };
    use crate::sampling::{ConversationItem, SyntheticReason, ToolCall};
    use crate::session::info::Info;
    use std::io::ErrorKind;

    fn test_info() -> Info {
        Info {
            id: agent_client_protocol::SessionId::new("context-snapshot-path-test"),
            cwd: "C:/workspace".to_owned(),
        }
    }

    #[cfg(windows)]
    fn absolute_snapshot_id() -> &'static str {
        r"C:\outside\snapshot"
    }

    #[cfg(not(windows))]
    fn absolute_snapshot_id() -> &'static str {
        "/tmp/outside/snapshot"
    }

    fn invalid_snapshot_ids() -> [&'static str; 3] {
        [absolute_snapshot_id(), "../outside", "not-a-uuid"]
    }

    #[test]
    fn snapshot_get_rejects_absolute_traversal_and_invalid_uuid_ids() {
        let info = test_info();

        for snapshot_id in invalid_snapshot_ids() {
            let error = ContextSnapshot::load(&info, snapshot_id)
                .expect_err("get must reject an unsafe snapshot id before filesystem access");
            assert_eq!(error.kind(), ErrorKind::InvalidInput, "{snapshot_id}");
        }
    }

    #[test]
    fn snapshot_delete_rejects_absolute_traversal_and_invalid_uuid_ids() {
        let info = test_info();

        for snapshot_id in invalid_snapshot_ids() {
            let error = ContextSnapshot::delete(&info, snapshot_id)
                .expect_err("delete must reject an unsafe snapshot id before filesystem access");
            assert_eq!(error.kind(), ErrorKind::InvalidInput, "{snapshot_id}");
        }
    }

    #[test]
    fn snapshot_path_is_a_direct_child_of_context_snapshots() {
        let info = test_info();
        let snapshot_id = uuid::Uuid::now_v7().to_string();
        let directory = crate::session::persistence::session_dir(&info).join("context_snapshots");

        let path = ContextSnapshot::path_for(&info, &snapshot_id).unwrap();

        assert_eq!(path.parent(), Some(directory.as_path()));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(format!("{snapshot_id}.json").as_str())
        );
    }

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
    fn snapshot_drops_an_incomplete_tool_exchange_before_a_later_user_turn() {
        let items = vec![
            ConversationItem::user("first turn"),
            ConversationItem::assistant_tool_calls(vec![ToolCall {
                id: "call-1".into(),
                name: "read_file".to_owned(),
                arguments: "{}".into(),
            }]),
            ConversationItem::user("later completed turn"),
            ConversationItem::assistant("later answer"),
        ];

        let result = completed_conversation(items);

        assert_eq!(result.len(), 3);
        assert!(matches!(&result[0], ConversationItem::User(_)));
        assert!(matches!(&result[1], ConversationItem::User(_)));
        assert!(
            matches!(&result[2], ConversationItem::Assistant(assistant) if assistant.tool_calls.is_empty())
        );
    }

    #[test]
    fn running_turn_is_excluded_from_a_completed_snapshot_boundary() {
        let completed = vec![
            ConversationItem::user("completed question"),
            ConversationItem::assistant("completed answer"),
            ConversationItem::user("currently running question"),
        ];

        let result = snapshot_items_at_completed_boundary(completed, true);

        assert_eq!(result.len(), 2);
        let wire = serde_json::to_string(&result).unwrap();
        assert!(!wire.contains("currently running question"));
        assert!(wire.contains("completed answer"));
    }

    #[test]
    fn idle_snapshot_keeps_the_latest_completed_turn() {
        let completed = vec![
            ConversationItem::user("completed question"),
            ConversationItem::assistant("completed answer"),
        ];

        let result = snapshot_items_at_completed_boundary(completed.clone(), false);

        assert_eq!(result.len(), completed.len());
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
    fn snapshot_serialization_excludes_parent_runtime_secrets() {
        let runtime_only_reasons = [
            SyntheticReason::SystemReminder,
            SyntheticReason::ProjectInstructions,
            SyntheticReason::AutoContinue,
            SyntheticReason::AutoRecovery,
            SyntheticReason::TaskCompleted,
            SyntheticReason::SubagentCompleted,
            SyntheticReason::NotificationDrain,
            SyntheticReason::GoalSummary,
            SyntheticReason::GoalClassifierNudge,
            SyntheticReason::SchedulerFired,
            SyntheticReason::Unknown,
        ];
        let mut items = vec![
            ConversationItem::system("Authorization: Bearer parent-secret"),
            ConversationItem::user("safe history"),
            ConversationItem::user_meta("compaction context"),
            ConversationItem::interjection("user steering"),
        ];
        for (index, reason) in runtime_only_reasons.into_iter().enumerate() {
            let mut item = ConversationItem::user(format!("runtime-secret-{index}"));
            let ConversationItem::User(user) = &mut item else {
                unreachable!("ConversationItem::user must create a user item");
            };
            user.synthetic_reason = Some(reason);
            items.push(item);
        }
        let snapshot = ContextSnapshot::from_items("session", "C:/workspace", items);

        let wire = serde_json::to_string(&snapshot).unwrap();

        assert!(!wire.contains("parent-secret"));
        assert!(!wire.contains("runtime-secret"));
        assert!(wire.contains("safe history"));
        assert!(wire.contains("compaction context"));
        assert!(wire.contains("user steering"));
    }

    #[test]
    fn snapshot_file_is_create_once_and_cannot_be_overwritten() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("snapshot.json");

        write_snapshot_once(&path, b"first").unwrap();
        let error = write_snapshot_once(&path, b"second")
            .expect_err("an immutable snapshot must reject a second write");

        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }

    #[test]
    fn side_query_request_is_one_tool_free_call_and_does_not_mutate_snapshot() {
        let snapshot = ContextSnapshot::from_items(
            "parent-session",
            "C:/workspace",
            vec![ConversationItem::user("parent history")],
        );
        let original_items = serde_json::to_value(&snapshot.items).unwrap();

        let request = build_side_query_request(
            &snapshot,
            Some("temporary context"),
            "what is happening?",
            "test-model",
            "btw-1",
            "parent-session",
        );

        assert!(request.tools.is_empty());
        assert!(request.hosted_tools.is_empty());
        assert_eq!(request.model.as_deref(), Some("test-model"));
        assert_eq!(request.x_atelier_conv_id.as_deref(), Some("btw-1"));
        assert_eq!(
            request.x_atelier_session_id.as_deref(),
            Some("parent-session")
        );
        assert_eq!(
            serde_json::to_value(&snapshot.items).unwrap(),
            original_items
        );
        let wire = serde_json::to_string(&request.items).unwrap();
        assert!(wire.contains("parent history"));
        assert!(wire.contains("temporary context"));
        assert!(wire.contains("what is happening?"));
    }

    #[test]
    fn snapshot_can_validate_its_source_session() {
        let snapshot = super::ContextSnapshot::from_items("session-a", "C:/workspace", vec![]);
        assert!(snapshot.belongs_to_session("session-a"));
        assert!(!snapshot.belongs_to_session("session-b"));
    }
}
