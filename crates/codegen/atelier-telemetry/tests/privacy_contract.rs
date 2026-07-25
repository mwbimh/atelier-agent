use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace_root() -> PathBuf {
    crate_root()
        .ancestors()
        .nth(3)
        .expect("telemetry crate must live under the workspace root")
        .to_path_buf()
}

fn assert_absent(relative: &str) {
    let path = crate_root().join(relative);
    assert!(
        !path.exists(),
        "remote telemetry path still exists: {}",
        path.display()
    );
}

fn rust_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()))
    {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "tests") {
                continue;
            }
            rust_sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if file_name == "tests.rs" || file_name.ends_with("_tests.rs") {
                continue;
            }
            files.push(path);
        }
    }
}

fn production_source(contents: &str) -> &str {
    contents
        .split_once("#[cfg(test)]")
        .map_or(contents, |(production, _)| production)
}

#[test]
fn remote_telemetry_modules_and_test_fixture_are_deleted() {
    for relative in [
        "src/external",
        "src/otel_layer",
        "src/sentry.rs",
        "src/http.rs",
        "tests/otlp_collector",
    ] {
        assert_absent(relative);
    }
}

#[test]
fn manifest_has_no_remote_sink_dependencies() {
    let manifest = std::fs::read_to_string(crate_root().join("Cargo.toml"))
        .expect("telemetry manifest must be readable")
        .to_ascii_lowercase();

    for forbidden in [
        "reqwest",
        "atelier-auth",
        "opentelemetry",
        "sentry",
        "tonic",
        "axum",
        "prost",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "telemetry manifest still contains remote sink dependency `{forbidden}`"
        );
    }
}

#[test]
fn source_has_no_remote_telemetry_or_upload_contracts() {
    let mut sources = Vec::new();
    rust_sources(&crate_root().join("src"), &mut sources);

    let forbidden = [
        "mixpanel",
        "trace_upload",
        "TraceUpload",
        "UploadMethod",
        "ExternalOtel",
        "ATELIER_EXTERNAL_OTEL",
        "OTEL_LOG_",
        "OTLP",
        "sentry",
    ];

    for path in sources {
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        for marker in forbidden {
            assert!(
                !contents.contains(marker),
                "{} still contains remote telemetry/upload marker `{marker}`",
                path.display()
            );
        }
    }
}

#[test]
fn runtime_remote_sinks_are_absent() {
    assert!(!atelier_telemetry::is_enabled());
    assert!(!atelier_telemetry::is_session_metrics_enabled());
}

#[test]
fn provider_requests_do_not_send_implicit_tracking_headers() {
    let source = std::fs::read_to_string(
        workspace_root().join("crates/codegen/atelier-sampler/src/client.rs"),
    )
    .expect("sampler client source must be readable");
    let production = production_source(&source);

    for forbidden in [
        "x-atelier-conv-id",
        "x-atelier-req-id",
        "x-atelier-model-override",
        "x-atelier-session-id",
        "x-atelier-turn-idx",
        "x-atelier-agent-id",
        "x-atelier-deployment-id",
        "x-atelier-user-id",
        "AtelierRequestHeaders",
    ] {
        assert!(
            !production.contains(forbidden),
            "sampler still sends implicit Provider tracking header `{forbidden}`"
        );
    }
}

#[test]
fn runtime_has_no_remote_settings_transport() {
    let root = workspace_root();
    let source_roots = [
        "crates/codegen/atelier-http/src",
        "crates/codegen/atelier-shell/src",
        "crates/codegen/atelier-pager/src",
        "apps/cli/src",
    ];
    let forbidden = [
        "/v1/settings",
        "fetch_remote_settings",
        "load_cached_remote_settings",
        "set_remote_campaigns_from_settings",
        "cache_remote_auto_mode",
    ];

    for relative in source_roots {
        let mut sources = Vec::new();
        rust_sources(&root.join(relative), &mut sources);
        for path in sources {
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let production = production_source(&contents);
            for marker in forbidden {
                assert!(
                    !production.contains(marker),
                    "{} still contains remote-settings transport marker `{marker}`",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn gcs_upload_restore_and_queue_surfaces_are_physically_absent() {
    let root = workspace_root();
    let manifest = std::fs::read_to_string(root.join("Cargo.toml"))
        .expect("workspace manifest must be readable");
    assert!(
        !manifest.contains("prod/mc/cli-chat-proxy-types"),
        "legacy proxy types crate is still a workspace member"
    );
    assert!(
        !root.join("prod/mc/cli-chat-proxy-types").exists(),
        "legacy proxy types crate directory still exists"
    );

    for relative in [
        "crates/codegen/atelier-config/Cargo.toml",
        "crates/codegen/atelier-shared/Cargo.toml",
    ] {
        let path = root.join(relative);
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        assert!(
            !contents.contains("prod-mc-cli-chat-proxy-types"),
            "{} still depends on the legacy proxy types crate",
            path.display()
        );
    }

    let checks = [
        (
            "crates/codegen/atelier-http/src/lib.rs",
            &["shared_upload_client"] as &[&str],
        ),
        (
            "crates/codegen/atelier-pager/src/app/actions.rs",
            &["RestoreAndLoadSession"],
        ),
        (
            "crates/codegen/atelier-shell/src/agent/local_session_catalog.rs",
            &["gcs_trace_prefix", "gcs_bucket"],
        ),
        (
            "crates/codegen/atelier-shell/src/session/signals.rs",
            &["RecordGcsQueueSnapshot", "gcs_queue_"],
        ),
        (
            "crates/codegen/atelier-shell/src/leader/server.rs",
            &["ATELIER_WORKSPACE_UPLOAD_QUEUE_ENABLED"],
        ),
        (
            "crates/codegen/atelier-workspace/src/handle.rs",
            &["upload_queue_pending", "_upload_queue_enabled"],
        ),
    ];

    for (relative, forbidden) in checks {
        let path = root.join(relative);
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let production = production_source(&contents);
        for marker in forbidden {
            assert!(
                !production.contains(marker),
                "{} still contains GCS/upload residue `{marker}`",
                path.display()
            );
        }
    }

    for relative in [
        "crates/codegen/atelier-shell/src/local_artifacts",
        "crates/codegen/atelier-memory/src/archive.rs",
    ] {
        assert!(
            root.join(relative).exists(),
            "local trace/archive surface must be retained: {relative}"
        );
    }
}

#[test]
fn runtime_crates_have_no_remote_telemetry_or_vendor_auth_backdoors() {
    let root = workspace_root();
    let source_roots = [
        "crates/codegen/atelier-shell/src",
        "crates/codegen/atelier-pager/src",
        "crates/codegen/atelier-workspace/src",
        "crates/common/atelier-tool-protocol/src",
        "crates/common/atelier-tool-hub-sdk/src",
    ];
    let forbidden = [
        "trace_gcs_config",
        "artifact_upload_ctx",
        "ArtifactUploadContext",
        "ArtifactTracker",
        "TraceExportConfig",
        "UploadMethod",
        "trace_upload",
        "ATELIER_EXTERNAL_OTEL",
        "OTEL_EXPORTER_OTLP",
        "mixpanel_token",
        "events_api_key",
        "ATELIER_XAI_API_BASE_URL",
        "XAI_API_KEY_ENV_VAR",
        "traces.donate",
        "logs.donate",
        "metrics.donate",
        "TracesDonateParams",
        "LogsDonateParams",
        "MetricsDonateParams",
    ];

    for relative in source_roots {
        let mut sources = Vec::new();
        rust_sources(&root.join(relative), &mut sources);
        for path in sources {
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let production = production_source(&contents);
            for marker in forbidden {
                assert!(
                    !production.contains(marker),
                    "{} still contains forbidden runtime marker `{marker}`",
                    path.display()
                );
            }
        }
    }
}
