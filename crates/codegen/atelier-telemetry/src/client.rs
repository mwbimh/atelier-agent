//! Local-only event compatibility layer.
//!
//! Atelier keeps the event call sites used by the runtime, but deliberately
//! contains no product analytics client, HTTP sink, Mixpanel client, or remote
//! profile synchronizer. Local diagnostics are handled by `unified_log` and
//! the tracing modules.

use crate::config::{TelemetryConfig, TelemetryMode};
use crate::http::OriginClientInfo;

/// Event metadata retained for local producers.
pub type Metadata = serde_json::Map<String, serde_json::Value>;

#[derive(Clone, Debug, Default)]
pub struct TelemetryClient;

impl TelemetryClient {
    #[allow(clippy::too_many_arguments)]
    pub fn from_config(
        config: TelemetryConfig,
        mode: TelemetryMode,
        user_id: Option<String>,
        team_id: Option<String>,
        deployment_key: Option<String>,
        origin_client: Option<OriginClientInfo>,
        shell_version: String,
        subscription_tier: Option<String>,
        http_client: reqwest::Client,
    ) -> Self {
        let _ = (
            config,
            mode,
            user_id,
            team_id,
            deployment_key,
            origin_client,
            shell_version,
            subscription_tier,
            http_client,
        );
        Self
    }
}
/// Product analytics are permanently absent from Atelier.
pub fn is_enabled() -> bool {
    false
}

/// Remote session metrics are permanently absent from Atelier.
pub fn is_session_metrics_enabled() -> bool {
    false
}

#[derive(Debug, Clone, Default)]
pub struct UserContext {
    pub country: String,
    pub language: String,
    pub timestamp: String,
}

impl UserContext {
    pub fn collect() -> Self {
        Self::default()
    }
}

pub async fn track(event_name: &str, request_id: &str, context: &UserContext, metadata: Metadata) {
    let _ = (event_name, request_id, context, metadata);
}

pub fn sync_profile() {}

#[allow(clippy::too_many_arguments)]
pub fn init(
    config: TelemetryConfig,
    mode: TelemetryMode,
    user_id: Option<String>,
    team_id: Option<String>,
    deployment_key: Option<String>,
    origin_client: Option<OriginClientInfo>,
    shell_version: String,
    subscription_tier: Option<String>,
    http_client: reqwest::Client,
) {
    let _ = TelemetryClient::from_config(
        config,
        mode,
        user_id,
        team_id,
        deployment_key,
        origin_client,
        shell_version,
        subscription_tier,
        http_client,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn init_if_needed(
    config: TelemetryConfig,
    mode: TelemetryMode,
    user_id: Option<String>,
    team_id: Option<String>,
    deployment_key: Option<String>,
    origin_client: Option<OriginClientInfo>,
    shell_version: String,
    subscription_tier: Option<String>,
    http_client: reqwest::Client,
) {
    init(
        config,
        mode,
        user_id,
        team_id,
        deployment_key,
        origin_client,
        shell_version,
        subscription_tier,
        http_client,
    );
}
