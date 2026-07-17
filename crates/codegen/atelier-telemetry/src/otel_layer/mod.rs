//! Local tracing compatibility layer.
//!
//! Atelier records local tracing only. No OpenTelemetry provider, exporter,
//! credential header, endpoint, background exporter task, or socket is built.

use atelier_auth::AuthCredentialProvider;
use std::sync::Arc;
use tracing_subscriber::registry::LookupSpan;

pub struct OtelLayerConfig {
    pub credentials: Arc<dyn AuthCredentialProvider>,
    pub token_header_value: String,
    pub alpha_test_key: Option<String>,
    pub exporter: OtelExporterConfig,
}

#[derive(Debug, Default, Clone)]
pub struct OtelClientInfo {
    pub client_name: &'static str,
    pub client_version: &'static str,
    pub service_version: &'static str,
    pub app_entrypoint: &'static str,
}

#[derive(Debug, Default, Clone)]
pub struct OtelExporterConfig {
    pub traces_url: String,
    pub extra_headers: Vec<(String, String)>,
    pub export_interval: Option<std::time::Duration>,
    pub timeout: Option<std::time::Duration>,
    pub enabled: bool,
}

pub fn build_otel_layer<S>(
    client: OtelClientInfo,
    config: OtelLayerConfig,
) -> impl tracing_subscriber::layer::Layer<S>
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    let _ = (client, config);
    tracing_subscriber::layer::Identity::new()
}

pub fn shutdown_otel() {}

/// RAII compatibility guard for callers that used to own the remote OTLP
/// exporter.  Atelier has no external exporter, so dropping the guard only
/// preserves the lifecycle contract without creating a network resource.
#[must_use = "the guard keeps the existing shutdown lifecycle alive"]
pub struct OtelGuard;

impl Drop for OtelGuard {
    fn drop(&mut self) {
        shutdown_otel();
    }
}

pub fn otel_guard() -> OtelGuard {
    OtelGuard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otel_guard_is_a_local_noop() {
        let guard = otel_guard();
        drop(guard);
    }
}
