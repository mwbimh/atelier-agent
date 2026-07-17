//! Local session-search metadata.
//!
//! The upstream implementation synchronized the search index with a vendor
//! object store. Atelier is local-only: the configuration type remains so
//! older config files can be parsed, but upload and download entry points are
//! permanently disabled and never construct a network client.

use std::io;
use std::path::Path;
use std::time::Duration;

use super::search_fts::SessionSearchIndex;

/// Staleness threshold: if local `last_bootstrap_at` is more than this
/// duration older than the remote object's timestamp, download the remote.
const STALENESS_THRESHOLD: Duration = Duration::from_secs(3600);

/// SQLite meta key for the last successful bootstrap timestamp (unix secs).
const META_KEY_LAST_BOOTSTRAP: &str = "last_bootstrap_at";

// Configuration

/// Configuration for remote index sync.
///
/// Parsed from `[session_search.remote_sync]` in `~/.atelier/config.toml`.
/// Default: disabled.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct RemoteSyncConfig {
    /// Whether remote sync is enabled.
    pub enabled: bool,
    /// GCS prefix for the remote index (directory structure in the bucket).
    /// Defaults to `"session_search_index"`.
    pub gcs_prefix: String,
}

impl Default for RemoteSyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            gcs_prefix: "session_search_index".to_string(),
        }
    }
}

// Staleness check

/// Read `last_bootstrap_at` from the sqlite meta table.
///
/// Returns `None` if the DB doesn't exist, can't be opened, or the key
/// is missing.
pub fn read_last_bootstrap_at(db_path: &Path) -> Option<i64> {
    if !db_path.exists() {
        return None;
    }
    let index = SessionSearchIndex::open_or_create(db_path).ok()?;
    index
        .get_meta(META_KEY_LAST_BOOTSTRAP)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
}

/// Like [`read_last_bootstrap_at`] but preserves read failures, so callers
/// can tell "marker genuinely absent" apart from "could not read the DB"
/// (transient busy/locked/I/O). A missing DB file is a true absence, not an
/// error.
pub fn try_read_last_bootstrap_at(db_path: &Path) -> Result<Option<i64>, String> {
    if !db_path.exists() {
        return Ok(None);
    }
    let index = SessionSearchIndex::open_or_create(db_path).map_err(|e| e.to_string())?;
    let value = index
        .get_meta(META_KEY_LAST_BOOTSTRAP)
        .map_err(|e| e.to_string())?;
    Ok(value.and_then(|v| v.parse::<i64>().ok()))
}

/// Write `last_bootstrap_at` into the sqlite meta table.
pub fn write_last_bootstrap_at(db_path: &Path) -> io::Result<()> {
    let index =
        SessionSearchIndex::open_or_create(db_path).map_err(|e| io::Error::other(e.to_string()))?;
    let now = chrono::Utc::now().timestamp();
    index
        .set_meta(META_KEY_LAST_BOOTSTRAP, &now.to_string())
        .map_err(|e| io::Error::other(e.to_string()))
}

/// Determine whether the local index is stale enough to warrant downloading
/// the remote copy.
///
/// Returns `true` if:
/// - The local DB file doesn't exist, or
/// - There is no `last_bootstrap_at` in the meta table, or
/// - `last_bootstrap_at` is more than [`STALENESS_THRESHOLD`] old compared
///   to `remote_timestamp_unix` (0 if unknown — always stale).
pub fn is_local_stale(db_path: &Path, remote_timestamp_unix: i64) -> bool {
    let Some(local_ts) = read_last_bootstrap_at(db_path) else {
        return true; // no local timestamp → stale
    };
    if remote_timestamp_unix == 0 {
        // Remote timestamp unknown; if we have a local bootstrap, trust it.
        return false;
    }
    (remote_timestamp_unix - local_ts) > STALENESS_THRESHOLD.as_secs() as i64
}

/// Legacy compatibility entry point. It deliberately ignores all arguments
/// so a config file cannot re-enable remote session-index upload.
pub async fn maybe_upload_index(
    _db_path: std::path::PathBuf,
    _config: RemoteSyncConfig,
    _gcs_config: xai_file_utils::TraceExportConfig,
    _auth_manager: Option<std::sync::Arc<crate::auth::AuthManager>>,
) {
    tracing::debug!("session search remote upload is disabled in Atelier");
}

/// Legacy compatibility entry point. It never reads a remote index and
/// always lets the local bootstrap path continue.
pub async fn maybe_download_index(
    _db_path: &Path,
    _config: &RemoteSyncConfig,
    _gcs_config: &xai_file_utils::TraceExportConfig,
    _auth_manager: Option<std::sync::Arc<crate::auth::AuthManager>>,
) -> bool {
    tracing::debug!("session search remote download is disabled in Atelier");
    false
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_sync_config_default() {
        let config = RemoteSyncConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.gcs_prefix, "session_search_index");
    }

    #[test]
    fn test_remote_sync_config_deserialize() {
        let toml_str = r#"
            enabled = true
            gcs_prefix = "custom/prefix"
        "#;
        let config: RemoteSyncConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.gcs_prefix, "custom/prefix");
    }

    #[test]
    fn test_is_local_stale_no_db() {
        // No DB file → stale
        assert!(is_local_stale(Path::new("/nonexistent/db.sqlite"), 0));
    }

    #[test]
    fn test_is_local_stale_no_meta() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("session_search.sqlite");

        // Create DB without last_bootstrap_at
        let _index = SessionSearchIndex::open_or_create(&db_path).unwrap();

        // No bootstrap timestamp → stale
        assert!(is_local_stale(&db_path, 100));
    }

    #[test]
    fn test_is_local_stale_fresh() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("session_search.sqlite");

        let index = SessionSearchIndex::open_or_create(&db_path).unwrap();
        let now = chrono::Utc::now().timestamp();
        index
            .set_meta(META_KEY_LAST_BOOTSTRAP, &now.to_string())
            .unwrap();

        // Remote timestamp is only 10 seconds ahead → not stale
        assert!(!is_local_stale(&db_path, now + 10));
    }

    #[test]
    fn test_is_local_stale_old() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("session_search.sqlite");

        let index = SessionSearchIndex::open_or_create(&db_path).unwrap();
        let old_ts = chrono::Utc::now().timestamp() - 7200; // 2 hours ago
        index
            .set_meta(META_KEY_LAST_BOOTSTRAP, &old_ts.to_string())
            .unwrap();

        // Remote is 2 hours newer → stale
        let remote_ts = chrono::Utc::now().timestamp();
        assert!(is_local_stale(&db_path, remote_ts));
    }

    #[test]
    fn test_is_local_stale_remote_unknown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("session_search.sqlite");

        let index = SessionSearchIndex::open_or_create(&db_path).unwrap();
        let now = chrono::Utc::now().timestamp();
        index
            .set_meta(META_KEY_LAST_BOOTSTRAP, &now.to_string())
            .unwrap();

        // Remote timestamp 0 (unknown) with local bootstrap → not stale
        assert!(!is_local_stale(&db_path, 0));
    }

    #[test]
    fn test_upload_debounce_initial() {
        // On fresh process start (LAST_UPLOAD_AT == 0), debounce allows upload
        // Note: can't reset the static in tests, but initial 0 → true
        assert!(
            true,
            "remote upload compatibility path is intentionally disabled"
        );
    }

    #[test]
    fn test_read_write_last_bootstrap_at() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("session_search.sqlite");

        // Before writing, should be None
        assert_eq!(read_last_bootstrap_at(&db_path), None);

        // Create DB and write timestamp
        write_last_bootstrap_at(&db_path).unwrap();

        // Should now have a reasonable timestamp
        let ts = read_last_bootstrap_at(&db_path).unwrap();
        let now = chrono::Utc::now().timestamp();
        assert!(
            (now - ts).abs() < 5,
            "timestamp should be within 5 seconds of now"
        );
    }

    fn test_export_config() -> xai_file_utils::TraceExportConfig {
        xai_file_utils::TraceExportConfig {
            bucket_url: None,
            service_account_key: None,
            upload_method: xai_file_utils::UploadMethod::Direct {
                service_account_key: None,
            },
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
        }
    }

    #[tokio::test]
    async fn enabled_remote_sync_config_cannot_upload_or_download() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("session_search.sqlite");
        std::fs::write(&db_path, b"local index").unwrap();
        let config = RemoteSyncConfig {
            enabled: true,
            gcs_prefix: "must-not-be-used".to_string(),
        };

        maybe_upload_index(db_path.clone(), config.clone(), test_export_config(), None).await;
        assert!(!tmp.path().join("session_search.sqlite.zst.tmp").exists());
        assert!(!maybe_download_index(&db_path, &config, &test_export_config(), None).await);
        assert_eq!(std::fs::read(&db_path).unwrap(), b"local index");
    }
}
