//! Retained configuration types for the removed external telemetry API.
//!
//! All resolvers return `None`. The types remain only until callers migrate to
//! local observability; they cannot enable a network exporter.

use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const ENV_MASTER_SWITCH: &str = "ATELIER_EXTERNAL_OTEL";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OtlpTransport {
    HttpProtobuf,
    Grpc,
}
impl Default for OtlpTransport {
    fn default() -> Self {
        Self::HttpProtobuf
    }
}

impl OtlpTransport {
    pub fn as_protocol_str(self) -> &'static str {
        match self {
            Self::HttpProtobuf => "http/protobuf",
            Self::Grpc => "grpc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExporterSelection {
    None,
    Otlp,
    Console,
}

impl Default for ExporterSelection {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TemporalityPreference {
    Cumulative,
    Delta,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentGates {
    pub log_user_prompts: bool,
    pub log_tool_details: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalClientInfo {
    pub service_version: String,
    pub client_version: String,
    pub app_entrypoint: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalOtelFileConfig {
    pub enabled: Option<bool>,
    pub metrics_exporter: Option<String>,
    pub logs_exporter: Option<String>,
    pub endpoint: Option<String>,
    pub protocol: Option<String>,
    pub log_user_prompts: Option<bool>,
    pub log_tool_details: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalOtelConfig {
    pub metrics_exporter: ExporterSelection,
    pub logs_exporter: ExporterSelection,
    pub transport: OtlpTransport,
    pub logs_endpoint: String,
    pub metrics_endpoint: String,
    pub headers: Vec<(String, String)>,
    pub export_interval: Duration,
    pub timeout: Duration,
    pub gates: ContentGates,
    pub include_session_id_on_metrics: bool,
    pub include_version_on_metrics: bool,
    pub client: ExternalClientInfo,
    pub internal_pipeline_consumed_otel_vars: bool,
    pub enabled_source: &'static str,
}

impl ExternalOtelConfig {
    pub fn resolve(_file: Option<&ExternalOtelFileConfig>) -> Option<Self> {
        None
    }

    pub fn resolve_with(
        _getenv: impl Fn(&str) -> Option<String>,
        _file: Option<&ExternalOtelFileConfig>,
    ) -> Option<Self> {
        None
    }
}

pub fn parse_header_list(_raw: &str) -> Vec<(String, String)> {
    Vec::new()
}
