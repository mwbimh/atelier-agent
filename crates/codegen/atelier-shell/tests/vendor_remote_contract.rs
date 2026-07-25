use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace_root() -> PathBuf {
    crate_root()
        .ancestors()
        .nth(3)
        .expect("shell crate must live under the workspace root")
        .to_path_buf()
}

fn assert_absent(path: impl AsRef<Path>) {
    let path = path.as_ref();
    assert!(
        !path.exists(),
        "removed vendor remote path still exists: {}",
        path.display()
    );
}

fn source(relative: &str) -> String {
    let path = crate_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

#[test]
fn vendor_upload_and_remote_modules_are_physically_absent() {
    let root = crate_root();
    for relative in [
        "src/upload",
        "src/remote",
        "src/agent/feedback_client.rs",
        "src/agent/session_registry_client.rs",
        "src/agent/storage_client_tests.rs",
        "src/session/storage/search_remote_sync.rs",
    ] {
        assert_absent(root.join(relative));
    }

    let workspace = workspace_root();
    for relative in [
        "crates/codegen/atelier-workspace/src/upload",
        "crates/codegen/atelier-workspace/src/recovery.rs",
        "crates/codegen/atelier-workspace/src/bin/workspace_server.rs",
        "crates/codegen/atelier-workspace/src/bin/workspace_server_probe.rs",
        "prod/mc/cli-chat-proxy-types",
    ] {
        assert_absent(workspace.join(relative));
    }
}

#[test]
fn endpoint_config_exposes_no_upload_or_vendor_control_plane_fields() {
    let config = source("src/agent/config.rs");
    let endpoint_section = config
        .split_once("pub struct EndpointsConfig")
        .expect("EndpointsConfig must exist")
        .1
        .split_once("impl Default for EndpointsConfig")
        .expect("EndpointsConfig default must exist")
        .0;

    for forbidden in [
        "trace_upload",
        "upload_bucket",
        "upload_credentials",
        "managed_config_url",
        "remote_settings",
        "events_api_key",
    ] {
        assert!(
            !endpoint_section.contains(forbidden),
            "EndpointsConfig still exposes removed vendor field `{forbidden}`"
        );
    }
}

#[test]
fn runtime_startup_contains_no_vendor_fetch_sync_or_relay_hooks() {
    let checks = [
        (
            "src/lib.rs",
            &["pub mod relay", "pub mod remote"] as &[&str],
        ),
        (
            "src/agent/mvp_agent/acp_agent.rs",
            &[
                "maybe_sync_bundle_in_background",
                "post_login_sync",
                "spawn_managed_gateway_tool_catalog_fetch",
                "fetch_managed_mcps",
            ],
        ),
        (
            "src/agent/mvp_agent/agent_ops.rs",
            &[
                "fetch_local_runtime_settings",
                "refresh_local_runtime_settings",
                "fetch_subagent_bundle",
                "spawn_relay_connection",
            ],
        ),
        (
            "src/agent/app.rs",
            &["spawn_relay_connection", "run_auto_update_checker"],
        ),
        (
            "src/leader/server.rs",
            &[
                "ATELIER_WORKSPACE_UPLOAD_QUEUE_ENABLED",
                "spawn_relay_connection",
                "fetch_remote_settings",
            ],
        ),
        (
            "src/managed_config.rs",
            &[
                "fetch_managed_config",
                "fetch_setup_report",
                "deployment/config",
            ],
        ),
    ];

    for (relative, forbidden) in checks {
        let contents = source(relative);
        for marker in forbidden {
            assert!(
                !contents.contains(marker),
                "{relative} still contains removed vendor runtime marker `{marker}`"
            );
        }
    }
}

#[test]
fn local_artifacts_remain_local_and_available() {
    let root = crate_root();
    assert!(root.join("src/local_artifacts").is_dir());
    assert!(
        workspace_root()
            .join("crates/codegen/atelier-memory/src/archive.rs")
            .is_file()
    );
}
