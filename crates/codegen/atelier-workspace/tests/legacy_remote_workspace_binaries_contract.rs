use std::fs;
use std::path::Path;

const REMOVED_BINARIES: [&str; 2] = ["xai-workspace-server", "workspace-server-probe"];
const REMOVED_SOURCES: [&str; 2] = [
    "src/bin/workspace_server.rs",
    "src/bin/workspace_server_probe.rs",
];

#[test]
fn removed_remote_workspace_binaries_are_not_declared_or_present() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_text = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("atelier-workspace Cargo.toml must be readable");
    let manifest: toml::Value =
        toml::from_str(&manifest_text).expect("atelier-workspace Cargo.toml must parse");

    let declared_bins = manifest
        .get("bin")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|bin| bin.get("name"))
        .filter_map(toml::Value::as_str)
        .collect::<Vec<_>>();

    for removed_binary in REMOVED_BINARIES {
        assert!(
            !declared_bins.contains(&removed_binary),
            "removed remote binary `{removed_binary}` must not be declared: {declared_bins:?}"
        );
    }

    assert!(
        declared_bins.contains(&"ate-workspace-worker"),
        "the local Workspace Worker binary must remain declared"
    );

    for removed_source in REMOVED_SOURCES {
        assert!(
            !crate_root.join(removed_source).exists(),
            "removed remote binary source must not remain: {removed_source}"
        );
    }
}
