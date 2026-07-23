use atelier_shell::agent::config::EndpointsConfig;
use std::path::PathBuf;

fn crate_source(path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

#[test]
fn local_trace_modules_cannot_upload_auth_diagnostics_or_use_a_default_bucket() {
    let source = include_str!("../src/upload/gcs.rs");
    assert!(
        source.contains("SESSION_TRACES_BUCKET: Option<&str> = None"),
        "a compiled-in trace bucket would re-enable remote export"
    );

    let diagnostics = source
        .split_once("pub(crate) async fn upload_to_auth_diagnostics")
        .expect("diagnostic compatibility function must remain discoverable")
        .1;
    assert!(
        !diagnostics.contains("xai_file_utils::gcs::upload_bytes"),
        "auth diagnostics still invoke remote object storage"
    );
}

#[test]
fn trace_upload_configuration_is_ignored_and_resolves_to_disabled() {
    let config: EndpointsConfig = toml::from_str(
        r#"
trace_upload_url = "https://collector.example"
trace_upload_bucket = "s3://private-bucket"
trace_upload_region = "test-region"
trace_upload_credentials_file = "credentials.json"
trace_upload_credentials = "secret"
trace_upload_endpoint_url = "https://storage.example"
"#,
    )
    .expect("unknown legacy keys must not make the whole config unreadable");

    assert!(config.trace_upload_url.is_none());
    assert!(config.trace_upload_bucket.is_none());
    assert!(config.trace_upload_region.is_none());
    assert!(config.trace_upload_credentials_file.is_none());
    assert!(config.trace_upload_credentials.is_none());
    assert!(config.trace_upload_endpoint_url.is_none());
    assert!(config.resolve_trace_upload_url().is_empty());
    assert!(config.resolve_trace_credentials().is_none());
    assert!(config.resolve_trace_bucket_url().is_none());
    assert!(config.resolve_upload_method(None).is_none());
    assert!(config.resolve_direct_upload_method().is_none());
    assert!(!config.has_noninteractive_upload_auth());
}

#[test]
fn vendor_remote_settings_fetch_pipeline_is_not_compiled() {
    let lib = crate_source("src/lib.rs");
    let agent = crate_source("src/agent/mvp_agent/mod.rs");
    let operations = crate_source("src/agent/mvp_agent/agent_ops.rs");
    let models = crate_source("src/agent/models.rs");
    let remote = crate_source("src/remote/mod.rs");
    let remote_client = crate_source("src/remote/client.rs");

    for (name, source) in [
        ("lib.rs", lib.as_str()),
        ("mvp_agent/mod.rs", agent.as_str()),
        ("mvp_agent/agent_ops.rs", operations.as_str()),
        ("agent/models.rs", models.as_str()),
        ("remote/mod.rs", remote.as_str()),
        ("remote/client.rs", remote_client.as_str()),
    ] {
        assert!(
            !source.contains("fetch_remote_settings")
                && !source.contains("maybe_fetch_post_auth_settings")
                && !source.contains("refresh_remote_settings")
                && !source.contains("resolve_remote_fetch_enabled")
                && !source.contains("fetch_settings_blocking")
                && !source.contains("fetch_login_device_flow")
                && !source.contains("login-config"),
            "{name} still compiles the removed vendor remote-settings pipeline"
        );
    }
}

#[test]
fn vendor_websocket_relay_is_not_compiled() {
    let lib = crate_source("src/lib.rs");
    let app = crate_source("src/agent/app.rs");
    let leader = crate_source("src/leader/server.rs");

    assert!(
        !lib.contains("pub mod relay"),
        "the vendor relay module is still exported"
    );
    for (name, source) in [("agent/app.rs", app), ("leader/server.rs", leader)] {
        assert!(
            !source.contains("spawn_relay_connection")
                && !source.contains("RelayConfig")
                && !source.contains("RelayHandle"),
            "{name} still compiles the vendor WebSocket relay startup path"
        );
    }
}

#[test]
fn vendor_session_writeback_is_not_compiled() {
    let persistence = crate_source("src/session/persistence.rs");
    let remote = crate_source("src/remote/mod.rs");

    for forbidden in ["RemoteSync", "init_remote_sync", "StorageMode::Writeback"] {
        assert!(
            !persistence.contains(forbidden),
            "session persistence still compiles vendor writeback symbol {forbidden}"
        );
    }
    assert!(
        !remote.contains("pub use sync::RemoteSync") && !remote.contains("mod sync"),
        "the vendor HTTP session sync module is still compiled"
    );
}

#[test]
fn acp_startup_and_auth_compile_only_local_provider_paths() {
    let acp_agent = crate_source("src/agent/mvp_agent/acp_agent.rs");

    for forbidden in [
        "run_auth_flow(",
        "run_auth_flow_with_stderr_bridge",
        "maybe_sync_bundle_in_background",
        "post_login_sync",
        "spawn_managed_gateway_tool_catalog_fetch",
        "fetch_managed_mcps",
    ] {
        assert!(
            !acp_agent.contains(forbidden),
            "ACP startup/auth still compiles vendor service path {forbidden}"
        );
    }
}

#[test]
fn vendor_bundle_sync_is_not_compiled() {
    let extensions = crate_source("src/extensions/mod.rs");
    let acp_agent = crate_source("src/agent/mvp_agent/acp_agent.rs");
    let agent = crate_source("src/agent/mvp_agent/mod.rs");
    let operations = crate_source("src/agent/mvp_agent/agent_ops.rs");
    let remote = crate_source("src/remote/mod.rs");
    let remote_client = crate_source("src/remote/client.rs");

    assert!(
        !extensions.contains("pub mod bundle"),
        "the vendor bundle extension is still compiled"
    );
    assert!(
        !PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/extensions/bundle.rs")
            .exists(),
        "the vendor bundle extension source still exists"
    );
    for (name, source) in [
        ("mvp_agent/acp_agent.rs", acp_agent.as_str()),
        ("mvp_agent/mod.rs", agent.as_str()),
        ("mvp_agent/agent_ops.rs", operations.as_str()),
        ("remote/mod.rs", remote.as_str()),
        ("remote/client.rs", remote_client.as_str()),
    ] {
        for forbidden in [
            "atelier/bundle/",
            "maybe_sync_bundle_in_background",
            "bundle_sync_in_flight",
            "fetch_subagent_bundle",
            "fetch_bundle",
            "FetchedBundle",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} still compiles vendor bundle symbol {forbidden}"
            );
        }
    }
}

#[test]
fn vendor_managed_mcp_fetch_is_not_compiled() {
    let operations = crate_source("src/agent/mvp_agent/agent_ops.rs");
    let doctor = crate_source("src/mcp_doctor.rs");
    let session_mcp = crate_source("src/session/acp_session_impl/mcp.rs");
    let spawn = crate_source("src/session/acp_session_impl/spawn.rs");
    let support = crate_source("../atelier-shell-session-support/src/managed_mcp.rs");

    for (name, source) in [
        ("mvp_agent/agent_ops.rs", operations.as_str()),
        ("mcp_doctor.rs", doctor.as_str()),
        ("acp_session_impl/mcp.rs", session_mcp.as_str()),
        ("acp_session_impl/spawn.rs", spawn.as_str()),
        (
            "atelier-shell-session-support/managed_mcp.rs",
            support.as_str(),
        ),
    ] {
        for forbidden in [
            "can_fetch_managed_mcps",
            "can_fetch_managed_mcp_gateway_tools",
            "get_managed_mcp_configs",
            "get_managed_mcp_gateway_tool_catalog",
            "spawn_managed_gateway_tool_catalog_fetch",
            "fetch_managed_mcp_configs",
            "fetch_managed_configs",
            "get_or_fetch_gateway_tool_catalog",
            "refresh_managed_mcp_if_stale",
            "ShellManagedGatewayToolClient",
            "managed_mcp_proxy_base_url",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} still compiles vendor managed-MCP symbol {forbidden}"
            );
        }
    }
}

#[test]
fn vendor_managed_config_sync_is_not_compiled() {
    let managed = crate_source("src/managed_config.rs");
    let init = crate_source("src/agent/init.rs");
    let models = crate_source("src/agent/models.rs");
    let config = crate_source("src/agent/config.rs");

    for forbidden in [
        "fetch_managed_config",
        "fetch_managed_config_once",
        "spawn_sync",
        "post_login_sync",
        "fetch_setup_report",
        "run_setup",
        "deployment/config",
        "resolve_managed_config_url",
    ] {
        assert!(
            !managed.contains(forbidden),
            "managed_config.rs still compiles vendor control-plane symbol {forbidden}"
        );
    }
    assert!(
        !init.contains("managed_config::spawn_sync"),
        "agent init still starts vendor managed-config synchronization"
    );
    assert!(
        !models.contains("managed_config::sync"),
        "model prefetch still performs vendor managed-config synchronization"
    );
    assert!(
        !config.contains("resolve_managed_config_url") && !config.contains("managed_config_url"),
        "endpoint config still exposes the removed vendor managed-config service"
    );
}
