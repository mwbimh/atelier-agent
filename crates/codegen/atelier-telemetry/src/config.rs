//! Telemetry-engine configuration.
//!
//! Extracted from `atelier-shell::agent::config` so the data-collector
//! engine can construct a [`TelemetryClient`](crate::client::TelemetryClient)
//! without a build-time dependency on the shell.
//!
//! Shell still re-exports these types from their original paths so existing
//! call sites (and `Config` derive impls) compile unchanged.
use serde::{Deserialize, Serialize};
/// Telemetry mode: `true`/`false` (legacy bool) or `"session_metrics"` (string).
///
/// - `Disabled` -- nothing sent (enterprise default)
/// - `SessionMetrics` -- metadata-only lifecycle events, no content
/// - `Enabled` -- full product telemetry (events + Mixpanel)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TelemetryMode {
    #[default]
    Disabled,
    SessionMetrics,
    Enabled,
}
impl TelemetryMode {
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }
    /// True for both `SessionMetrics` and `Enabled`.
    pub fn session_metrics_enabled(&self) -> bool {
        matches!(self, Self::SessionMetrics | Self::Enabled)
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" | "enabled" | "full" => Some(Self::Enabled),
            "0" | "false" | "no" | "off" | "disabled" => Some(Self::Disabled),
            "session-metrics" | "session_metrics" => Some(Self::SessionMetrics),
            _ => None,
        }
    }
}
impl std::fmt::Display for TelemetryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "false"),
            Self::SessionMetrics => write!(f, "session_metrics"),
            Self::Enabled => write!(f, "true"),
        }
    }
}
impl From<bool> for TelemetryMode {
    fn from(b: bool) -> Self {
        if b { Self::Enabled } else { Self::Disabled }
    }
}
impl serde::Serialize for TelemetryMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Disabled => serializer.serialize_bool(false),
            Self::Enabled => serializer.serialize_bool(true),
            Self::SessionMetrics => serializer.serialize_str("session_metrics"),
        }
    }
}
/// Wire format for `[features] telemetry`: accepts `true`, `false`, or `"session_metrics"`.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum TelemetryModeValue {
    Bool(bool),
    Str(String),
}
impl<'de> serde::Deserialize<'de> for TelemetryMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match TelemetryModeValue::deserialize(deserializer)? {
            TelemetryModeValue::Bool(b) => Ok(Self::from(b)),
            TelemetryModeValue::Str(s) => Ok(Self::parse(&s).unwrap_or_else(|| {
                tracing::warn!(
                    value = % s,
                    "TELEMETRY_MODE_UNKNOWN: unrecognized telemetry mode; treating as disabled",
                );
                Self::Disabled
            })),
        }
    }
}
/// Parse an env var as a `TelemetryMode`. Returns `None` if unset or empty.
pub fn env_telemetry_mode(name: &str) -> Option<TelemetryMode> {
    let value = std::env::var(name).ok()?;
    TelemetryMode::parse(&value)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    /// Declared for `serde_ignored`. Actual toggle is `[features] telemetry`.
    #[serde(default)]
    pub enabled: Option<bool>,
    pub events_url: Option<String>,
    pub events_api_key: Option<String>,
    pub mixpanel_token: Option<String>,
    pub mixpanel_enabled: bool,
    /// `None` = inherit from `[features] telemetry`. `Some(false)` = disable GCS uploads only.
    pub trace_upload: Option<bool>,
    /// External OTEL master switch (`= ATELIER_EXTERNAL_OTEL`, env wins).
    pub otel_enabled: Option<bool>,
    /// External OTEL metrics exporter: `otlp` | `console` | `none`.
    pub otel_metrics_exporter: Option<String>,
    /// External OTEL logs/events exporter: `otlp` | `console` | `none`.
    pub otel_logs_exporter: Option<String>,
    /// External OTLP base endpoint (`/v1/logs`, `/v1/metrics` appended for HTTP).
    pub otel_endpoint: Option<String>,
    /// External OTLP transport: `http/protobuf` | `grpc`.
    #[serde(alias = "otel_transport")]
    pub otel_protocol: Option<String>,
    /// External OTEL content gate (admins can pin to `false` via requirements).
    pub otel_log_user_prompts: Option<bool>,
    /// External OTEL content gate (admins can pin to `false` via requirements).
    pub otel_log_tool_details: Option<bool>,
}
fn internal_defaults() -> (Option<String>, Option<String>, Option<String>, bool) {
    (None, None, None, false)
}
fn build_env_default(value: Option<&'static str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}
impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: Some(false),
            events_url: None,
            events_api_key: None,
            mixpanel_token: None,
            mixpanel_enabled: false,
            trace_upload: Some(false),
            otel_enabled: Some(false),
            otel_metrics_exporter: None,
            otel_logs_exporter: None,
            otel_endpoint: None,
            otel_protocol: None,
            otel_log_user_prompts: None,
            otel_log_tool_details: None,
        }
    }
}
impl TelemetryConfig {
    pub fn apply_env_overrides(&mut self) {
        // Keep this loader hook for compatibility, but make the privacy
        // policy irreversible: config and environment cannot enable a remote
        // exporter in an Atelier process.
        self.enabled = Some(false);
        self.events_url = None;
        self.events_api_key = None;
        self.mixpanel_token = None;
        self.mixpanel_enabled = false;
        self.trace_upload = Some(false);
        self.otel_enabled = Some(false);
        self.otel_metrics_exporter = None;
        self.otel_logs_exporter = None;
        self.otel_endpoint = None;
        self.otel_protocol = None;
        self.otel_log_user_prompts = Some(false);
        self.otel_log_tool_details = Some(false);
    }
    fn normalize(&mut self) {
        self.events_url = Self::normalize_optional_string(self.events_url.take());
        self.events_api_key = Self::normalize_optional_string(self.events_api_key.take());
        self.mixpanel_token = Self::normalize_optional_string(self.mixpanel_token.take());
    }
    fn env_override(name: &str) -> Option<Option<String>> {
        match std::env::var(name) {
            Ok(value) => Some(Self::normalize_optional_string(Some(value))),
            Err(_) => None,
        }
    }
    fn normalize_optional_string(value: Option<String>) -> Option<String> {
        value.and_then(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
    }
}
/// Parse an env var as a boolean. Returns `None` if unset or unrecognized.
///
/// Local copy of `atelier_shell::agent::config::env_bool` so this crate
/// stays free of a shell back-edge. Shell keeps its own copy for callers
/// outside the telemetry config path.
fn env_bool(name: &str) -> Option<bool> {
    let value = std::env::var(name).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "1" | "true" | "yes" | "on" | "enabled" => Some(true),
        "0" | "false" | "no" | "off" | "disabled" => Some(false),
        _ => None,
    }
}
/// Derive a stable deployment ID (UUIDv5) from the deployment key.
pub fn deployment_id_from_key(key: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, key.as_bytes()).to_string()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn build_env_default_normalizes() {
        assert_eq!(build_env_default(None), None);
        assert_eq!(build_env_default(Some("")), None);
        assert_eq!(build_env_default(Some(" \t ")), None);
        assert_eq!(build_env_default(Some(" key ")), Some("key".to_owned()));
    }
    #[test]
    fn default_is_build_env_layer_when_feature_off() {
        let cfg = TelemetryConfig::default();
        assert!(!cfg.mixpanel_enabled);
        assert_eq!(cfg.events_url, None);
        assert_eq!(cfg.events_api_key, None);
        assert_eq!(cfg.mixpanel_token, None);
        assert_eq!(cfg.trace_upload, Some(false));
        assert_eq!(cfg.otel_enabled, Some(false));
    }

    #[test]
    fn apply_env_overrides_cannot_enable_remote_sinks() {
        let mut cfg = TelemetryConfig {
            enabled: Some(true),
            events_url: Some("https://collector.invalid".into()),
            events_api_key: Some("secret".into()),
            mixpanel_token: Some("token".into()),
            mixpanel_enabled: true,
            trace_upload: Some(true),
            otel_enabled: Some(true),
            otel_metrics_exporter: Some("otlp".into()),
            otel_logs_exporter: Some("otlp".into()),
            otel_endpoint: Some("https://collector.invalid".into()),
            otel_protocol: Some("grpc".into()),
            otel_log_user_prompts: Some(true),
            otel_log_tool_details: Some(true),
        };
        cfg.apply_env_overrides();
        assert_eq!(cfg.enabled, Some(false));
        assert_eq!(cfg.events_url, None);
        assert_eq!(cfg.events_api_key, None);
        assert_eq!(cfg.mixpanel_token, None);
        assert!(!cfg.mixpanel_enabled);
        assert_eq!(cfg.trace_upload, Some(false));
        assert_eq!(cfg.otel_enabled, Some(false));
        assert_eq!(cfg.otel_metrics_exporter, None);
        assert_eq!(cfg.otel_logs_exporter, None);
        assert_eq!(cfg.otel_endpoint, None);
        assert_eq!(cfg.otel_log_user_prompts, Some(false));
        assert_eq!(cfg.otel_log_tool_details, Some(false));
    }
}
