use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    let mut current = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let manifest = current.join("Cargo.toml");
        if manifest.is_file()
            && std::fs::read_to_string(&manifest).is_ok_and(|source| source.contains("[workspace]"))
        {
            return current.to_path_buf();
        }
        current = current
            .parent()
            .expect("CLI crate must be located inside the repository");
    }
}

#[test]
fn repository_uses_the_public_monorepo_layout() {
    let root = repository_root();
    for path in [
        "apps/cli/Cargo.toml",
        "crates/codegen/atelier-pager/docs/user-guide/README.md",
        "apps/gui/README.md",
        "packages/sdk/typescript/package.json",
        "packages/sdk/csharp/Atelier.RuntimeSdk.csproj",
        "packages/sdk/fixtures/rpc-contract.json",
    ] {
        assert!(root.join(path).is_file(), "missing monorepo path: {path}");
    }

    assert!(
        !root.join("crates/codegen/atelier-pager-bin").exists(),
        "the CLI composition root must live under apps/cli"
    );
    assert!(
        !root.join("sdk").exists(),
        "SDK packages must live under packages/sdk"
    );
}

#[test]
fn private_and_generated_paths_are_ignored() {
    let root = repository_root();
    let ignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    for pattern in [
        "/.project/",
        "/.tmp-tests/",
        "/release/",
        "/target/",
        "/crates/codegen/atelier-pager/crates/",
    ] {
        assert!(
            ignore.lines().any(|line| line.trim() == pattern),
            "missing ignore rule: {pattern}"
        );
    }
}

#[test]
fn root_readmes_are_bilingual_and_credit_grok_build() {
    let root = repository_root();
    let english = std::fs::read_to_string(root.join("README.md")).unwrap();
    let chinese = std::fs::read_to_string(root.join("README.zh-CN.md")).unwrap();

    assert!(english.contains("README.zh-CN.md"));
    assert!(chinese.contains("README.md"));
    assert!(english.contains("Grok Build"));
    assert!(chinese.contains("Grok Build"));
}
