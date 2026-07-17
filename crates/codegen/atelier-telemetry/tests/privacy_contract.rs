use atelier_telemetry::config::TelemetryConfig;
use atelier_telemetry::external::{ExternalOtelConfig, ExternalOtelFileConfig};

#[test]
fn external_otel_cannot_be_enabled_by_environment_or_config() {
    let file = ExternalOtelFileConfig {
        enabled: Some(true),
        metrics_exporter: Some("otlp".into()),
        logs_exporter: Some("otlp".into()),
        endpoint: Some("https://collector.invalid".into()),
        protocol: Some("grpc".into()),
        log_user_prompts: Some(true),
        log_tool_details: Some(true),
    };

    assert!(ExternalOtelConfig::resolve(Some(&file)).is_none());
    assert!(
        ExternalOtelConfig::resolve_with(
            |name| match name {
                "ATELIER_EXTERNAL_OTEL" => Some("1".into()),
                "OTEL_METRICS_EXPORTER" | "OTEL_LOGS_EXPORTER" => Some("otlp".into()),
                _ => None,
            },
            Some(&file),
        )
        .is_none()
    );
}

#[test]
fn telemetry_config_is_local_only_after_normalization() {
    let mut config = TelemetryConfig {
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
    config.apply_env_overrides();

    assert_eq!(config.enabled, Some(false));
    assert_eq!(config.events_url, None);
    assert_eq!(config.events_api_key, None);
    assert_eq!(config.mixpanel_token, None);
    assert!(!config.mixpanel_enabled);
    assert_eq!(config.trace_upload, Some(false));
    assert_eq!(config.otel_enabled, Some(false));
    assert_eq!(config.otel_metrics_exporter, None);
    assert_eq!(config.otel_logs_exporter, None);
    assert_eq!(config.otel_endpoint, None);
}

#[test]
fn runtime_telemetry_sinks_are_always_disabled() {
    assert!(!atelier_telemetry::is_enabled());
    assert!(!atelier_telemetry::is_session_metrics_enabled());
    assert!(!atelier_telemetry::external::is_active());
}
