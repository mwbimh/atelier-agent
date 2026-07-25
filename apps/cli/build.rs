use std::path::{Path, PathBuf};
use std::process::Command;

fn repository_root(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(manifest_dir)
        .to_path_buf()
}

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = repository_root(&manifest_dir);
    println!(
        "cargo:rerun-if-changed={}",
        repository_root.join(".git/HEAD").display()
    );
    println!("cargo:rerun-if-env-changed=ATELIER_VERSION");

    let commit = Command::new("git")
        .current_dir(&repository_root)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let version = std::env::var("ATELIER_VERSION")
        .or_else(|_| std::env::var("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| "0.0.0".to_string());

    println!(
        "cargo:rustc-env=VERSION_WITH_COMMIT={} ({})",
        version, commit
    );
}
