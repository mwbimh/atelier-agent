//! Purely local runtime types retained after removing the vendor backend.

/// Local runtime policy/settings payload.
///
/// The storage type still lives in `atelier-config-types`; this neutral alias
/// keeps the shell independent from the removed `remote` module.
pub type LocalRuntimeSettings = atelier_config_types::LocalRuntimeSettings;

/// Legacy chat-session data used only by local serialization tests while the
/// vendor conversation service is absent from the runtime.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    #[serde(default)]
    pub conversation_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub create_time: Option<String>,
    #[serde(default)]
    pub modify_time: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<ConversationWorkspace>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationWorkspace {
    #[serde(default)]
    pub workspace_id: String,
}
