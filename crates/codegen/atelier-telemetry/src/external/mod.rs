//! Removed external telemetry compatibility API.
//!
//! No exporter, socket, background task, credential header, or remote policy
//! is constructed by Atelier. This module exists only so local runtime call
//! sites can migrate without retaining a network-capable implementation.

pub mod config;

pub use config::{ContentGates, ExternalOtelConfig, ExternalOtelFileConfig};

#[derive(Debug, Clone, Default)]
pub struct IdentityAttrs {
    pub user_id: Option<String>,
    pub organization_id: Option<String>,
    pub team_id: Option<String>,
    pub deployment_id: Option<String>,
}
impl IdentityAttrs {
    pub fn from_snapshot(snapshot: &atelier_auth::CredentialSnapshot) -> Self {
        Self {
            user_id: snapshot.user_id.clone(),
            organization_id: snapshot.organization_id.clone(),
            team_id: snapshot.team_id.clone(),
            deployment_id: snapshot.deployment_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ExternalOtelRemotePolicy {
    pub force_disable: bool,
    pub lock_content_gates: bool,
}

pub fn init(_config: Option<ExternalOtelConfig>) {}

pub fn is_active() -> bool {
    false
}

pub fn emit<T: crate::events::TelemetryEvent>(_event: &T) {}

pub fn set_identity(_attrs: IdentityAttrs) {}

pub fn apply_remote_policy(_policy: ExternalOtelRemotePolicy) {}

pub fn flush() {}

pub fn shutdown() {}
