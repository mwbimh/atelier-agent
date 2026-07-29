//! Local type surface retained for session-list and restore code.
//!
//! No method in this module creates a network client or performs remote I/O.

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub session_id: String,
    pub cwd: String,
    pub model_id: Option<String>,
    pub repo_remote_url: Option<String>,
    pub repo_branch: Option<String>,
    pub repo_head_at_start: Option<String>,
    pub hostname: Option<String>,
    pub device_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub session_kind: Option<String>,
    pub subagent_type: Option<String>,
    pub subagent_role: Option<String>,
    pub fork_context_source: Option<String>,
    pub subagent_depth: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRequest {
    pub summary: Option<String>,
    pub first_prompt: Option<String>,
    pub last_turn_number: Option<i32>,
    pub repo_head_at_end: Option<String>,
    pub restorable_turn_number: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub session_id: String,
    pub summary: String,
    pub first_prompt: Option<String>,
    pub model_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_turn_number: i32,
    #[serde(default)]
    pub restorable_turn_number: Option<i32>,
    pub cwd: String,
    pub repo_remote_url: Option<String>,
    pub hostname: Option<String>,
    pub status: String,
    #[serde(default)]
    pub last_active_at: Option<String>,
}

impl From<crate::session::persistence::Summary> for SessionRecord {
    fn from(summary: crate::session::persistence::Summary) -> Self {
        Self {
            session_id: summary.info.id.to_string(),
            summary: summary.session_summary,
            first_prompt: None,
            model_id: Some(summary.current_model_id.to_string()),
            created_at: summary.created_at.to_rfc3339(),
            updated_at: summary.updated_at.to_rfc3339(),
            last_turn_number: summary.num_messages as i32,
            restorable_turn_number: None,
            cwd: summary.info.cwd,
            repo_remote_url: None,
            hostname: None,
            status: "local".into(),
            last_active_at: summary.last_active_at.map(|value| value.to_rfc3339()),
        }
    }
}

#[derive(Clone, Default)]
pub struct LocalSessionCatalog;

impl LocalSessionCatalog {
    pub async fn register(&self, _request: &RegisterRequest) -> Result<()> {
        Ok(())
    }

    pub async fn update(&self, _session_id: &str, _request: &UpdateRequest) -> Result<()> {
        Ok(())
    }

    pub async fn finalize(&self, _session_id: &str) -> Result<()> {
        Ok(())
    }

    pub async fn search(&self, _query: Option<&str>, _limit: i64) -> Result<Vec<SessionRecord>> {
        Ok(Vec::new())
    }

    pub async fn get_session(&self, _session_id: &str) -> Result<SessionRecord> {
        anyhow::bail!("session is not present in the local catalog")
    }

    pub async fn get_download_url(
        &self,
        _session_id: &str,
        _file: &str,
        _turn: i32,
    ) -> Result<String> {
        anyhow::bail!("local catalog has no remote download URL")
    }

    pub async fn download_file(
        &self,
        _session_id: &str,
        _file: &str,
        _turn: i32,
        _destination: &std::path::Path,
    ) -> Result<()> {
        anyhow::bail!("local catalog does not download remote files")
    }
}
