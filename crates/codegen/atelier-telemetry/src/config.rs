//! Local observability configuration shared with runtime config parsing.

use serde::{Deserialize, Serialize};

/// Legacy local event gate retained while call sites migrate to file logs and
/// tracing. No variant creates a network sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TelemetryMode {
    #[default]
    Disabled,
    SessionMetrics,
    Enabled,
}

impl TelemetryMode {
    pub fn is_disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }

    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }

    pub fn session_metrics_enabled(self) -> bool {
        matches!(self, Self::SessionMetrics | Self::Enabled)
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
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
    fn from(value: bool) -> Self {
        if value { Self::Enabled } else { Self::Disabled }
    }
}

impl Serialize for TelemetryMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Disabled => serializer.serialize_bool(false),
            Self::Enabled => serializer.serialize_bool(true),
            Self::SessionMetrics => serializer.serialize_str("session_metrics"),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TelemetryModeValue {
    Bool(bool),
    Str(String),
}

impl<'de> Deserialize<'de> for TelemetryMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match TelemetryModeValue::deserialize(deserializer)? {
            TelemetryModeValue::Bool(value) => Ok(Self::from(value)),
            TelemetryModeValue::Str(value) => Ok(Self::parse(&value).unwrap_or_default()),
        }
    }
}

pub fn env_telemetry_mode(name: &str) -> Option<TelemetryMode> {
    TelemetryMode::parse(&std::env::var(name).ok()?)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    /// Local legacy event gate. Network event sinks do not exist.
    pub enabled: Option<bool>,
}

pub fn deployment_id_from_key(key: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, key.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_has_only_the_local_event_gate() {
        let value = serde_json::to_value(TelemetryConfig::default()).unwrap();
        assert_eq!(value, serde_json::json!({ "enabled": null }));
    }
}
