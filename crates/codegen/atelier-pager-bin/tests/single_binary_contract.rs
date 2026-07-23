#[test]
fn release_helpers_are_not_standalone_binary_targets() {
    let sandbox_manifest = include_str!("../../atelier-windows-sandbox/Cargo.toml");
    let workspace_manifest = include_str!("../../atelier-workspace/Cargo.toml");

    assert!(
        !sandbox_manifest.contains("name = \"atelier-command-runner\""),
        "the command runner must only be exposed through atelier.exe's internal sub-mode"
    );
    assert!(
        sandbox_manifest.contains("autobins = false"),
        "Cargo auto-discovery would still publish src/bin/atelier-command-runner.rs"
    );
    assert!(
        !workspace_manifest.contains("name = \"atelier-workspace-worker\""),
        "the workspace worker must only be exposed through atelier.exe's internal sub-mode"
    );
    assert!(
        workspace_manifest.contains("autobins = false"),
        "Cargo auto-discovery would still publish src/bin/atelier_workspace_worker.rs"
    );
}
