#![cfg(windows)]

use atelier_workspace::worker::WorkspaceWorkerClient;
use base64::Engine as _;

#[tokio::test]
async fn restricted_worker_real_process_round_trips_and_blocks_outside_write() {
    let binary = std::env::var_os("ATE_BINARY").expect("ATE_BINARY must point to ate.exe");
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let client = WorkspaceWorkerClient::spawn(root.clone(), binary.into())
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
    client.shutdown().await.expect("worker shutdown");
}
