use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use tokio::fs;

use atelier_shell::env::AtelierBuildEnvironment;
use atelier_shell::util::atelier_home::atelier_home;

pub(crate) const AUTOMATIC_UPDATES_DISABLED: &str = "automatic updates disabled";

/// Configuration kept for the pager's local call boundary.
///
/// The fields are retained so callers can keep constructing the existing
/// configuration object. None of them are used to contact a vendor or to
/// select an update source.
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    pub proxy_base_url: String,
    pub auth_scope: String,
    pub deployment_key: Option<String>,
    pub alpha_test_key: Option<String>,
    pub channel: String,
    pub npm_registry: Option<String>,
}

impl UpdateConfig {
    pub fn from_environment(env: &AtelierBuildEnvironment) -> Self {
        Self {
            proxy_base_url: env.cli_chat_proxy_base_url(),
            auth_scope: atelier_shell::auth::AtelierComConfig::default().auth_scope(),
            deployment_key: None,
            alpha_test_key: None,
            channel: "stable".to_string(),
            npm_registry: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct VersionCache {
    version: String,
    #[serde(default)]
    stable_version: Option<String>,
    checked_at: String,
}

fn disabled<T>() -> Result<T> {
    Err(anyhow!(AUTOMATIC_UPDATES_DISABLED))
}

/// Version discovery is intentionally unavailable. Keep this signature for
/// callers that still route through the old update boundary.
pub async fn fetch_latest_version(_installer: &str, _config: &UpdateConfig) -> Result<String> {
    disabled()
}

/// Legacy npm probe boundary; it must never spawn `npm`.
#[doc(hidden)]
pub async fn fetch_npm_tag_for_test(_tag: &str, _npm_registry: Option<&str>) -> Result<String> {
    disabled()
}

/// Legacy npm version boundary; it must never spawn `npm`.
#[doc(hidden)]
pub async fn fetch_npm_version_for_test(
    _channel: &str,
    _npm_registry: Option<&str>,
) -> Result<String> {
    disabled()
}

/// Legacy GitHub release boundary; it must never spawn `gh`.
#[doc(hidden)]
pub async fn fetch_gh_release_version(_channel: &str) -> Result<String> {
    disabled()
}

/// Legacy GCS channel-pointer boundary; it must never create an HTTP client.
#[doc(hidden)]
pub async fn fetch_gcs_version_from_base(_channel: &str, _base_url: &str) -> Result<String> {
    disabled()
}

/// Legacy cache-writing version lookup boundary. No remote probe is allowed.
pub async fn get_latest_version(_installer: &str, _config: &UpdateConfig) -> Result<String> {
    disabled()
}

/// The cache remains local-only and is used to render the channel label.
pub async fn write_version_cache(version: &str, stable_version: Option<&str>) {
    let version_path = atelier_home().join("version.json");
    let now = time::OffsetDateTime::now_utc();
    let checked_at = now
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| now.to_string());
    let cache = VersionCache {
        version: version.to_string(),
        stable_version: stable_version.map(str::to_owned),
        checked_at,
    };

    if let Some(parent) = version_path.parent()
        && let Err(error) = fs::create_dir_all(parent).await
    {
        tracing::warn!(%error, "failed to create local version cache directory");
        return;
    }

    let data = match serde_json::to_vec_pretty(&cache) {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(%error, "failed to serialize local version cache");
            return;
        }
    };
    let temporary_path = version_path.with_extension("json.tmp");
    if let Err(error) = fs::write(&temporary_path, data).await {
        tracing::warn!(%error, "failed to write local version cache");
        return;
    }
    if let Err(error) = fs::rename(&temporary_path, &version_path).await {
        tracing::warn!(%error, "failed to publish local version cache");
    }
}

/// Automatic update checks are disabled, so a cache never suppresses a check
/// that no longer exists.
pub async fn is_version_cache_fresh() -> bool {
    false
}

pub use atelier_version::installed as get_installed_atelier_version;

/// Read the managed application symlink without contacting any remote source.
pub fn installed_on_disk_version() -> Option<String> {
    #[cfg(unix)]
    {
        let application = atelier_shell::util::atelier_home::atelier_application();
        let target = std::fs::read_link(&application).ok()?;
        std::fs::metadata(&application).ok()?;
        version_from_versioned_binary_name(target.file_name()?.to_str()?, "atelier")
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[allow(dead_code)]
pub(crate) fn version_from_versioned_binary_name(name: &str, bin_prefix: &str) -> Option<String> {
    const PLATFORM_OS: &[&str] = &["macos", "linux", "darwin", "windows"];
    let suffix = name.strip_prefix(bin_prefix)?.strip_prefix('-')?;
    let parts: Vec<&str> = suffix.split('-').collect();
    let platform_start = parts
        .iter()
        .position(|part| PLATFORM_OS.contains(part))
        .unwrap_or(parts.len());
    let version = parts[..platform_start].join("-");
    semver::Version::parse(&version).ok()?;
    Some(version)
}

pub fn cached_stable_version() -> Option<String> {
    let version_path = atelier_home().join("version.json");
    let content = std::fs::read_to_string(version_path).ok()?;
    serde_json::from_str::<VersionCache>(&content)
        .ok()
        .and_then(|cache| cache.stable_version)
}

fn derive_channel(current: &str, stable: &str) -> Option<&'static str> {
    let current = semver::Version::parse(current).ok()?;
    let stable = semver::Version::parse(stable).ok()?;
    Some(if current > stable { "alpha" } else { "stable" })
}

/// Return the local channel name, if a locally written stable pointer exists.
pub fn channel_name() -> Option<&'static str> {
    use std::sync::OnceLock;

    static NAME: OnceLock<Option<&'static str>> = OnceLock::new();
    *NAME.get_or_init(|| {
        let stable = cached_stable_version()?;
        derive_channel(atelier_version::VERSION, &stable)
    })
}

/// Return the local channel label without performing a version check.
pub fn channel_label() -> &'static str {
    use std::sync::OnceLock;

    static LABEL: OnceLock<&'static str> = OnceLock::new();
    LABEL.get_or_init(|| {
        let Some(stable) = cached_stable_version() else {
            return "";
        };
        match derive_channel(atelier_version::VERSION, &stable) {
            Some("alpha") => " [alpha]",
            Some("stable") => " [stable]",
            _ => "",
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_name_parser_is_local_only() {
        assert_eq!(
            version_from_versioned_binary_name("atelier-0.1.220-alpha.4-linux-x86_64", "atelier"),
            Some("0.1.220-alpha.4".to_string())
        );
        assert_eq!(
            version_from_versioned_binary_name("atelier-latest", "atelier"),
            None
        );
    }
}
