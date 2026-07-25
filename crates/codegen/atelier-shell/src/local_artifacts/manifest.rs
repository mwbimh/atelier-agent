use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, serde::Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactStatus {
    Written,
    Failed,
    Skipped,
}

#[derive(Debug, serde::Serialize, Clone)]
pub(crate) struct FailureDetail {
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct LocalArtifactManifest {
    pub schema_version: u32,
    pub complete: bool,
    pub completed_at: DateTime<Utc>,
    pub artifacts: HashMap<String, ArtifactStatus>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub failure_details: HashMap<String, FailureDetail>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub skip_details: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub(crate) struct LocalArtifactStateInner {
    pub statuses: HashMap<String, ArtifactStatus>,
    pub failures: HashMap<String, FailureDetail>,
    pub skips: HashMap<String, String>,
}

pub(crate) type LocalArtifactState = Arc<parking_lot::Mutex<LocalArtifactStateInner>>;

pub(crate) fn new_local_artifact_state() -> LocalArtifactState {
    Arc::new(parking_lot::Mutex::new(LocalArtifactStateInner::default()))
}

pub(crate) enum ArtifactResult<'a> {
    Written,
    Failed {
        reason: &'a str,
        error: Option<&'a str>,
    },
}

pub(crate) fn record_artifact(
    state: &LocalArtifactState,
    filename: &str,
    result: ArtifactResult<'_>,
) {
    let key = filename.to_owned();
    let mut inner = state.lock();
    match result {
        ArtifactResult::Written => {
            inner.statuses.insert(key.clone(), ArtifactStatus::Written);
            inner.failures.remove(&key);
            inner.skips.remove(&key);
        }
        ArtifactResult::Failed { reason, error } => {
            inner.statuses.insert(key.clone(), ArtifactStatus::Failed);
            inner.skips.remove(&key);
            inner.failures.insert(
                key,
                FailureDetail {
                    reason: reason.to_owned(),
                    error: error.map(truncate).map(str::to_owned),
                },
            );
        }
    }
}

pub(crate) fn skip_artifact(state: &LocalArtifactState, filename: &str, reason: &str) {
    let key = filename.to_owned();
    let mut inner = state.lock();
    inner.statuses.insert(key.clone(), ArtifactStatus::Skipped);
    inner.failures.remove(&key);
    inner.skips.insert(key, reason.to_owned());
}

pub(crate) fn build_manifest(state: &LocalArtifactState) -> LocalArtifactManifest {
    let inner = state.lock();
    let artifacts = inner.statuses.clone();
    LocalArtifactManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        complete: !artifacts
            .values()
            .any(|status| matches!(status, ArtifactStatus::Failed)),
        completed_at: Utc::now(),
        failure_details: inner
            .failures
            .iter()
            .filter(|(name, _)| matches!(artifacts.get(*name), Some(ArtifactStatus::Failed)))
            .map(|(name, detail)| (name.clone(), detail.clone()))
            .collect(),
        skip_details: inner
            .skips
            .iter()
            .filter(|(name, _)| matches!(artifacts.get(*name), Some(ArtifactStatus::Skipped)))
            .map(|(name, detail)| (name.clone(), detail.clone()))
            .collect(),
        artifacts,
    }
}

fn truncate(value: &str) -> &str {
    match value.char_indices().nth(512) {
        Some((index, _)) => &value[..index],
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_local_and_terminal() {
        let state = new_local_artifact_state();
        record_artifact(&state, "prompt.json", ArtifactResult::Written);
        skip_artifact(&state, "memory.tar.gz", "disabled");
        let manifest = build_manifest(&state);
        assert!(manifest.complete);
        assert_eq!(manifest.artifacts["prompt.json"], ArtifactStatus::Written);
        assert_eq!(manifest.artifacts["memory.tar.gz"], ArtifactStatus::Skipped);
    }

    #[test]
    fn failure_is_recorded_without_queue_state() {
        let state = new_local_artifact_state();
        record_artifact(
            &state,
            "turn.json",
            ArtifactResult::Failed {
                reason: "write_failed",
                error: Some("disk full"),
            },
        );
        let manifest = build_manifest(&state);
        assert!(!manifest.complete);
        assert_eq!(manifest.failure_details["turn.json"].reason, "write_failed");
    }
}
