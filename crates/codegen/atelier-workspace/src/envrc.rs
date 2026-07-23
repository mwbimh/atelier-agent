//! Parse .envrc files and extract environment variables.
//!
//! This module provides a way to load environment variables from `.envrc` files.
//! It uses a two-tier approach:
//!
//! 1. **Try `direnv export json`** - If direnv is installed, use it for full compatibility
//! 2. **Fallback to bash** - Run .envrc in a bash subshell with direnv stubs
//!
//! This approach handles:
//! - Variable expansion ($HOME, ${VAR:-default})
//! - Command substitution ($(git rev-parse ...))
//! - Conditional logic (if/then/else)
//! - direnv helper functions (source_up_if_exists, PATH_add, etc.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Stub implementations of common direnv helper functions.
/// These are prepended to the .envrc before execution when direnv is not available.
const DIRENV_STUBS: &str = r#"
# Stub direnv helper functions
source_up_if_exists() { :; }
source_up() { :; }
source_env_if_exists() {
    if [ -f "$1" ]; then
        . "$1"
    fi
}
source_env() {
    if [ -f "$1" ]; then
        . "$1"
    fi
}
PATH_add() {
    export PATH="$PWD/$1:$PATH"
}
path_add() {
    PATH_add "$@"
}
layout() { :; }
use() { :; }
watch_file() { :; }
"#;

/// Load environment variables from .envrc file in the given directory.
///
/// Returns a HashMap of environment variables that were set/modified by the .envrc.
/// Returns None if no .envrc exists or if parsing fails.
pub fn load_envrc(dir: &Path) -> Option<HashMap<String, String>> {
    let envrc_path = dir.join(".envrc");
    if !envrc_path.exists() {
        tracing::debug!(?dir, ".envrc not found");
        return None;
    }

    // Try direnv first (most reliable if installed)
    if let Some(env) = try_direnv_export(dir) {
        return Some(env);
    }

    // Fall back to bash subshell approach
    load_envrc_via_bash(dir)
}

/// Try to use `direnv export json` to load environment variables.
/// Returns None if direnv is not installed or fails.
fn try_direnv_export(dir: &Path) -> Option<HashMap<String, String>> {
    let mut cmd = Command::new("direnv");
    cmd.args(["export", "json"])
        .current_dir(dir)
        .stdin(std::process::Stdio::null());
    atelier_tools::util::detach_std_command(&mut cmd);
    let output = cmd.output().ok()?;

    if !output.status.success() {
        // direnv not allowed, or other error
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("not allowed") {
            tracing::debug!(?dir, %stderr, "direnv export failed");
        }
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        // No changes from direnv
        return None;
    }

    // Parse JSON output: {"VAR": "value", ...}
    // Note: direnv also outputs null for vars to unset, but we ignore those
    match serde_json::from_str::<HashMap<String, serde_json::Value>>(&stdout) {
        Ok(json) => {
            let env: HashMap<String, String> = json
                .into_iter()
                .filter_map(|(k, v)| {
                    if let serde_json::Value::String(s) = v {
                        Some((k, s))
                    } else {
                        None // Skip null values (unset)
                    }
                })
                .collect();

            if env.is_empty() {
                None
            } else {
                tracing::info!(?dir, count = env.len(), "Loaded environment via direnv");
                Some(env)
            }
        }
        Err(e) => {
            tracing::warn!(?dir, ?e, "Failed to parse direnv JSON output");
            None
        }
    }
}

/// Load environment by running .envrc in a bash subshell.
/// This is the fallback when direnv is not installed.
fn load_envrc_via_bash(dir: &Path) -> Option<HashMap<String, String>> {
    let envrc_path = dir.join(".envrc");
    let bash = resolve_bash_program()?;

    // Build a script that:
    // 1. Includes direnv stubs
    // 2. Sources the .envrc
    // 3. Outputs all env vars as KEY=VALUE pairs (null-separated for safety)
    #[cfg(windows)]
    let pwd_override = format!(
        "export PWD={}\n",
        bash_single_quote(&dir.to_string_lossy().replace('\\', "/"))
    );
    #[cfg(not(windows))]
    let pwd_override = String::new();
    let script = format!(
        r#"
set -e
{pwd_override}
{stubs}
. ./.envrc
# Output all environment variables, null-separated
env -0
"#,
        pwd_override = pwd_override,
        stubs = DIRENV_STUBS,
    );

    // Capture baseline environment (before running .envrc)
    let baseline: HashMap<String, String> = std::env::vars().collect();

    // Run the script and capture output
    let mut bash_cmd = Command::new(&bash);
    bash_cmd
        .arg("-c")
        .arg(&script)
        .current_dir(dir)
        .stdin(std::process::Stdio::null());
    atelier_tools::util::detach_std_command(&mut bash_cmd);
    let output = bash_cmd.output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        Ok(_) => {
            tracing::warn!(?envrc_path, "Failed to execute .envrc via bash");
            return None;
        }
        Err(e) => {
            tracing::warn!(?envrc_path, ?e, "Failed to run bash for .envrc");
            return None;
        }
    };

    // Parse the null-separated KEY=VALUE pairs
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut result: HashMap<String, String> = HashMap::new();

    for entry in stdout.split('\0') {
        if entry.is_empty() {
            continue;
        }
        if let Some((key, value)) = entry.split_once('=') {
            // Skip internal/noise variables
            let ignored_keys = ["_", "SHLVL", "PWD", "OLDPWD"];
            if ignored_keys.contains(&key) {
                continue;
            }
            // Only include vars that are new or changed from baseline
            match baseline.get(key) {
                Some(baseline_value) if baseline_value == value => {
                    // Unchanged, skip
                }
                _ => {
                    // New or changed
                    result.insert(key.to_string(), value.to_string());
                }
            }
        }
    }

    if result.is_empty() {
        tracing::debug!(?envrc_path, "No environment changes from .envrc");
        None
    } else {
        tracing::info!(
            ?envrc_path,
            count = result.len(),
            "Loaded environment from .envrc via bash"
        );
        Some(result)
    }
}

fn bash_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn resolve_bash_program() -> Option<PathBuf> {
    Some(PathBuf::from(atelier_config::shell::unix_shell_path(
        atelier_config::shell::UnixShellKind::Bash,
    )))
}

#[cfg(windows)]
fn resolve_bash_program() -> Option<PathBuf> {
    use atelier_config::shell::WindowsShell;

    if let WindowsShell::GitBash(path) = atelier_config::shell::detect_windows_shell() {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let mut candidates = Vec::new();
    let mut git = Command::new("git.exe");
    git.arg("--exec-path").stdin(std::process::Stdio::null());
    atelier_tools::util::detach_std_command(&mut git);
    if let Ok(output) = git.output()
        && output.status.success()
    {
        let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        if let Some(git_root) = exec_path.ancestors().find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("Git"))
        }) {
            candidates.push(git_root.join("bin").join("bash.exe"));
        }
    }

    for env_name in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(env_name) {
            let root = PathBuf::from(root);
            candidates.push(if env_name == "LOCALAPPDATA" {
                root.join("Programs")
                    .join("Git")
                    .join("bin")
                    .join("bash.exe")
            } else {
                root.join("Git").join("bin").join("bash.exe")
            });
        }
    }

    let mut where_git = Command::new("where.exe");
    where_git.arg("git.exe").stdin(std::process::Stdio::null());
    atelier_tools::util::detach_std_command(&mut where_git);
    if let Ok(output) = where_git.output()
        && output.status.success()
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let git = PathBuf::from(line.trim());
            if git.parent().and_then(Path::parent).is_some_and(|root| {
                root.file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("Git"))
            }) && let Some(root) = git.parent().and_then(Path::parent)
            {
                candidates.push(root.join("bin").join("bash.exe"));
            }
        }
    }

    candidates.into_iter().find(|path| path.is_file())
}

/// Load .envrc and return the environment, or empty HashMap on failure.
pub fn load_envrc_or_empty(dir: &Path) -> HashMap<String, String> {
    load_envrc(dir).unwrap_or_default()
}

/// [`load_envrc_or_empty`] gated on folder-trust: loads the repo-local `.envrc`
/// (executed in a bash subshell) only when `trusted`, else returns an empty map.
/// The shell call sites pass the `project_scope_allowed` verdict so the "run a
/// cloned repo's `.envrc` only when the folder is trusted" rule lives in ONE
/// place (mirrors `permission::claude_settings::load_claude_env_with_project`).
pub fn load_envrc_or_empty_when_trusted(dir: &Path, trusted: bool) -> HashMap<String, String> {
    if trusted {
        load_envrc_or_empty(dir)
    } else {
        HashMap::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_simple_export() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".envrc"), "export FOO=bar\n").unwrap();

        let env = load_envrc(dir.path()).unwrap();
        assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn test_variable_expansion() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".envrc"), "export MY_DIR=$PWD/subdir\n").unwrap();

        let env = load_envrc(dir.path()).unwrap();
        let expected = format!("{}/subdir", dir.path().display());
        #[cfg(windows)]
        let expected = expected.replace('\\', "/");
        assert_eq!(env.get("MY_DIR"), Some(&expected));
    }

    #[test]
    fn test_no_envrc() {
        let dir = TempDir::new().unwrap();
        assert!(load_envrc(dir.path()).is_none());
    }

    #[test]
    fn test_path_add() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".envrc"), "PATH_add bin\n").unwrap();

        let env = load_envrc(dir.path()).unwrap();
        let path = env.get("PATH").unwrap();
        let expected = format!("{}/bin", dir.path().display());
        #[cfg(windows)]
        let expected = expected.replace('\\', "/");
        assert!(path.contains(&expected));
    }

    #[test]
    fn test_conditional() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".envrc"),
            r#"
if [ -d "$PWD" ]; then
    export EXISTS=yes
else
    export EXISTS=no
fi
"#,
        )
        .unwrap();

        let env = load_envrc(dir.path()).unwrap();
        assert_eq!(env.get("EXISTS"), Some(&"yes".to_string()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_bash_resolver_finds_real_git_bash_not_wsl_alias() {
        let bash = resolve_bash_program().expect("Git Bash must be resolved on Windows");
        let normalized = bash
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        assert!(
            normalized.ends_with("/git/bin/bash.exe"),
            "must resolve Git Bash, not the System32/WindowsApps WSL alias: {}",
            bash.display()
        );
        let output = Command::new(&bash)
            .arg("--version")
            .output()
            .expect("resolved Git Bash must be executable");
        assert!(output.status.success());
    }
}
