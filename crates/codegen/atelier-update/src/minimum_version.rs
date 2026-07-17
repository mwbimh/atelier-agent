use crate::version::UpdateConfig;

/// Version floors are part of the removed vendor update control plane.
/// Atelier starts the installed local build without performing a remote
/// minimum-version check or attempting an installation.
pub async fn enforce_minimum_version_or_exit(_update_config: &UpdateConfig) {}
