use std::path::{Path, PathBuf};
use std::process::Command;

fn repository_root(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(manifest_dir)
        .to_path_buf()
}

fn git_dir(repository_root: &Path) -> PathBuf {
    Command::new("git")
        .current_dir(repository_root)
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| PathBuf::from(value.trim()))
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                repository_root.join(path)
            }
        })
        .unwrap_or_else(|| repository_root.join(".git"))
}

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = repository_root(&manifest_dir);
    let git_dir = git_dir(&repository_root);
    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    if let Ok(head) = std::fs::read_to_string(&head_path)
        && let Some(reference) = head.trim().strip_prefix("ref: ")
    {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(reference).display()
        );
    }
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );
    println!("cargo:rerun-if-env-changed=ATELIER_VERSION");
    println!("cargo:rerun-if-env-changed=ATELIER_BUILD_COMMIT");

    let commit = std::env::var("ATELIER_BUILD_COMMIT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .or_else(|| {
            Command::new("git")
                .current_dir(&repository_root)
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|value| value.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    let version = std::env::var("ATELIER_VERSION")
        .or_else(|_| std::env::var("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| "0.0.0".to_string());

    println!(
        "cargo:rustc-env=VERSION_WITH_COMMIT={} ({})",
        version, commit
    );
}
