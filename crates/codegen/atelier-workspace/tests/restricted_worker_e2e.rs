#![cfg(windows)]

use atelier_windows_sandbox::{CommandRequest, SandboxMode, run_command};
use atelier_workspace::worker::{WorkspaceWorkerClient, WorkspaceWorkerSandboxMode};
use base64::Engine as _;
use std::ffi::OsString;
use std::path::PathBuf;

#[test]
fn worker_mode_resolution_preserves_read_only_and_workspace_write() {
    assert_eq!(
        WorkspaceWorkerSandboxMode::from_profile_name("read-only").unwrap(),
        WorkspaceWorkerSandboxMode::ReadOnly
    );
    assert_eq!(
        WorkspaceWorkerSandboxMode::from_profile_name("workspace").unwrap(),
        WorkspaceWorkerSandboxMode::WorkspaceWrite
    );
    assert!(WorkspaceWorkerSandboxMode::from_profile_name("off").is_err());
}

#[tokio::test]
async fn restricted_worker_real_process_round_trips_and_blocks_outside_write() {
    let binary = std::env::var_os("ATE_BINARY").expect("ATE_BINARY must point to ate.exe");
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let client = WorkspaceWorkerClient::spawn(
        root.clone(),
        binary.into(),
        WorkspaceWorkerSandboxMode::WorkspaceWrite,
    )
    .await
    .expect("spawn restricted workspace worker");
    let inside = root.join("inside.txt");
    client
        .call(
            "atelier.worker.write_file",
            serde_json::json!({
                "path": inside,
                "data_base64": base64::engine::general_purpose::STANDARD.encode(b"worker-ok"),
                "create_dirs": true,
            }),
            None,
        )
        .await
        .expect("write inside root");
    let read = client
        .call(
            "atelier.worker.read_file",
            serde_json::json!({ "path": root.join("inside.txt") }),
            None,
        )
        .await
        .expect("read inside root");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(read["data_base64"].as_str().unwrap())
            .unwrap(),
        b"worker-ok"
    );

    let blocked = outside.join("blocked.txt");
    assert!(
        client
            .call(
                "atelier.worker.write_file",
                serde_json::json!({
                    "path": blocked,
                    "data_base64": "YmxvY2tlZA==",
                    "create_dirs": true,
                }),
                None,
            )
            .await
            .is_err()
    );
    assert!(!outside.join("blocked.txt").exists());

    client
        .call(
            "atelier.worker.delete_file",
            serde_json::json!({ "path": inside }),
            None,
        )
        .await
        .expect("workspace-write worker deletes inside root");
    assert!(!root.join("inside.txt").exists());
    client.shutdown().await.expect("worker shutdown");
}

#[tokio::test]
async fn read_only_worker_rejects_write_and_delete_inside_workspace() {
    let binary = std::env::var_os("ATE_BINARY").expect("ATE_BINARY must point to ate.exe");
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let existing = root.join("existing.txt");
    std::fs::write(&existing, b"keep").unwrap();

    let client = WorkspaceWorkerClient::spawn(
        root.clone(),
        binary.into(),
        WorkspaceWorkerSandboxMode::ReadOnly,
    )
    .await
    .expect("spawn read-only workspace worker");

    let created = root.join("created.txt");
    assert!(
        client
            .call(
                "atelier.worker.write_file",
                serde_json::json!({
                    "path": created,
                    "data_base64": "YmxvY2tlZA==",
                    "create_dirs": true,
                }),
                None,
            )
            .await
            .is_err(),
        "read-only worker must reject write_file"
    );
    assert!(!root.join("created.txt").exists());

    assert!(
        client
            .call(
                "atelier.worker.delete_file",
                serde_json::json!({ "path": existing }),
                None,
            )
            .await
            .is_err(),
        "read-only worker must reject delete_file"
    );
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep");

    client.shutdown().await.expect("worker shutdown");
}

#[test]
fn restricted_child_os_boundary_allows_workspace_and_denies_adjacent_directory() {
    let binary = std::env::var_os("ATE_BINARY").expect("ATE_BINARY must point to ate.exe");
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let inside = root.join("inside.txt");
    let blocked = outside.join("blocked.txt");
    let request = CommandRequest::new(
        SandboxMode::WorkspaceWrite,
        vec![root.clone()],
        root,
        PathBuf::from(binary),
        vec![
            OsString::from("--internal-windows-sandbox-boundary-probe"),
            inside.as_os_str().to_owned(),
            blocked.as_os_str().to_owned(),
        ],
    );

    let output = run_command(request).expect("run restricted child");
    assert!(inside.is_file(), "workspace write did not succeed");
    assert_eq!(
        std::fs::read_to_string(&inside).unwrap().trim(),
        "inside-ok"
    );
    assert!(
        !blocked.exists(),
        "OS sandbox allowed a direct write outside the workspace"
    );
    assert_eq!(
        output.exit_code,
        5,
        "outside write must fail with ERROR_ACCESS_DENIED; stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn read_only_child_os_boundary_denies_workspace_write() {
    let binary = std::env::var_os("ATE_BINARY").expect("ATE_BINARY must point to ate.exe");
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let inside = root.join("blocked-inside.txt");
    let outside = outside.join("blocked-outside.txt");
    let request = CommandRequest::new(
        SandboxMode::ReadOnly,
        vec![root.clone()],
        root,
        PathBuf::from(binary),
        vec![
            OsString::from("--internal-windows-sandbox-boundary-probe"),
            inside.as_os_str().to_owned(),
            outside.as_os_str().to_owned(),
        ],
    );

    let output = run_command(request).expect("run read-only restricted child");
    assert_ne!(
        output.exit_code, 0,
        "read-only child unexpectedly wrote to the workspace"
    );
    assert!(!inside.exists());
    assert!(!outside.exists());
}
