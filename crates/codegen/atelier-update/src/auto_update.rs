use std::path::Path;

use anyhow::{Result, anyhow};
use serde::Serialize;
use tokio::process::Child;

use crate::version::{AUTOMATIC_UPDATES_DISABLED, UpdateConfig, get_installed_atelier_version};

fn disabled<T>() -> Result<T> {
    Err(anyhow!(AUTOMATIC_UPDATES_DISABLED))
}

#[derive(Clone, Copy, Debug)]
pub enum UpdateRunMode {
    Blocking,
    NonBlocking,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub installer: Option<String>,
    pub channel: String,
    pub auto_update: Option<bool>,
    pub error: Option<String>,
}

pub fn print_update_status(status: &UpdateStatus, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string(status)?);
        return Ok(());
    }

    println!("Atelier - v{} [{}]", status.current_version, status.channel);
    if let Some(error) = status.error.as_deref() {
        println!("Update check failed: {error}");
    }
    Ok(())
}

pub async fn check_update_status(update_config: &UpdateConfig) -> UpdateStatus {
    UpdateStatus {
        current_version: get_installed_atelier_version(),
        latest_version: None,
        update_available: false,
        installer: None,
        channel: update_config.channel.clone(),
        auto_update: None,
        error: Some(AUTOMATIC_UPDATES_DISABLED.to_string()),
    }
}

pub async fn auto_update_target(_update_config: &UpdateConfig) -> Option<(&'static str, String)> {
    None
}

#[derive(Debug, Default)]
pub struct EnsureLatestOutcome {
    pub installed: Option<String>,
    pub relaunch_needed: bool,
}

pub async fn ensure_latest_on_disk(_update_config: &UpdateConfig) -> Result<EnsureLatestOutcome> {
    Ok(EnsureLatestOutcome::default())
}

pub async fn get_installer() -> Option<&'static str> {
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAvailable {
    pub latest_version: String,
}

pub struct BackgroundUpdateCheck {
    pub update: Option<UpdateAvailable>,
    pub download: Option<Child>,
}

impl BackgroundUpdateCheck {
    fn none() -> Self {
        Self {
            update: None,
            download: None,
        }
    }
}

pub async fn check_update_background(_update_config: &UpdateConfig) -> BackgroundUpdateCheck {
    BackgroundUpdateCheck::none()
}

pub async fn run_update_if_available(
    _run_mode: UpdateRunMode,
    _interactive: bool,
    _update_config: &UpdateConfig,
) -> Result<bool> {
    disabled()
}

pub fn restart_atelier() -> Result<()> {
    disabled()
}

pub async fn run_install_script(
    _installer: &str,
    _target: Option<&str>,
    _update_config: &UpdateConfig,
) -> Result<()> {
    disabled()
}

#[doc(hidden)]
pub async fn download_with_progress(_url: &str, _dest: &Path) -> Result<()> {
    disabled()
}

#[doc(hidden)]
pub async fn download_silent(_url: &str, _dest: &Path) -> Result<()> {
    disabled()
}

#[doc(hidden)]
pub async fn install_internal_from_bases(
    _target: Option<&str>,
    _update_config: &UpdateConfig,
    _bases: &[&str],
) -> Result<()> {
    disabled()
}

#[doc(hidden)]
pub async fn install_internal_from_base(
    _target: Option<&str>,
    _update_config: &UpdateConfig,
    _base_url: &str,
) -> Result<()> {
    disabled()
}

#[doc(hidden)]
pub fn install_npm_for_test(
    _target: Option<&str>,
    _channel: &str,
    _npm_registry: Option<&str>,
) -> Result<()> {
    disabled()
}

/// Apply only the local channel selection. It does not persist a remote
/// setting or trigger a version probe.
pub async fn apply_channel_switch(channel_switch: Option<&str>, update_config: &mut UpdateConfig) {
    if let Some(channel) = channel_switch {
        update_config.channel = channel.to_string();
    }
}

pub async fn run_update(
    _force: bool,
    _pinned_version: Option<&str>,
    channel_switch: Option<&str>,
    update_config: &mut UpdateConfig,
) -> Result<Option<String>> {
    apply_channel_switch(channel_switch, update_config).await;
    disabled()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> UpdateConfig {
        UpdateConfig {
            proxy_base_url: String::new(),
            auth_scope: String::new(),
            deployment_key: None,
            alpha_test_key: None,
            channel: "stable".to_string(),
            npm_registry: None,
        }
    }

    #[tokio::test]
    async fn update_paths_are_disabled_without_spawning_work() {
        let config = config();
        let outcome = ensure_latest_on_disk(&config).await.unwrap();
        assert!(outcome.installed.is_none());
        assert!(!outcome.relaunch_needed);
        assert!(check_update_background(&config).await.update.is_none());
        assert_eq!(check_update_status(&config).await.latest_version, None);
        assert_eq!(
            run_update_if_available(UpdateRunMode::Blocking, false, &config)
                .await
                .unwrap_err()
                .to_string(),
            AUTOMATIC_UPDATES_DISABLED
        );
    }
}
