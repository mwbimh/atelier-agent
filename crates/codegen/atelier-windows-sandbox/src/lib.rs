#![cfg(windows)]
#![allow(unsafe_op_in_unsafe_fn)]

//! First-stage Windows sandbox support for Atelier.
//!
//! The active implementation is a real restricted-token runner with temporary
//! capability ACLs and strict existing-path validation. Elevated provisioning,
//! Windows Filtering Platform networking, and ConPTY are intentionally outside
//! this crate's first stage and are not represented as active capabilities.

mod acl;
mod env;
mod error;
mod path_normalization;
mod process;
mod runner;
mod token;
mod winutil;

pub use error::SandboxError;
pub use path_normalization::canonical_path_key;
pub use path_normalization::ensure_no_reparse_points;
pub use path_normalization::normalize_existing_path;
pub use path_normalization::path_is_within;
pub use runner::{SandboxSession, run_command};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Probe whether this Windows host can create the write-restricted token used
/// by the preview backend. A failed probe is a hard capability failure; callers
/// must not fall back to an unsandboxed child process.
pub fn probe_restricted_token() -> Result<(), SandboxError> {
    let capability = token::new_capability_sid().map_err(SandboxError::Operation)?;
    token::create_restricted_token(&capability)
        .map(|_| ())
        .map_err(SandboxError::Operation)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
}

/// Resolve the helper executable used by the async terminal adapter.
///
/// The helper is shipped beside the Atelier executable in release artifacts.
/// `ATELIER_COMMAND_RUNNER` is an explicit development/packaging override; a
/// missing helper is an error and never falls back to direct process spawning.
pub fn command_runner_path() -> Result<PathBuf, SandboxError> {
    let path = std::env::var_os("ATELIER_COMMAND_RUNNER")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf))
                .map(|dir| dir.join("atelier-command-runner.exe"))
        })
        .ok_or_else(|| {
            SandboxError::Operation(anyhow::anyhow!(
                "cannot resolve atelier-command-runner.exe; set ATELIER_COMMAND_RUNNER"
            ))
        })?;

    if !path.is_file() {
        return Err(SandboxError::Operation(anyhow::anyhow!(
            "Atelier Windows sandbox helper is missing: {}",
            path.display()
        )));
    }
    Ok(path)
}

/// Build the command-line arguments for `atelier-command-runner`.
///
/// Keeping this construction in the sandbox crate gives all callers the same
/// `--` boundary and prevents user command arguments from being interpreted as
/// helper options.
pub fn command_runner_args(
    mode: SandboxMode,
    roots: &[PathBuf],
    cwd: &Path,
    program: &Path,
    args: &[OsString],
) -> Result<Vec<OsString>, SandboxError> {
    if roots.is_empty() {
        return Err(SandboxError::NoRoots);
    }
    if program.as_os_str().is_empty() {
        return Err(SandboxError::EmptyCommand);
    }

    let mode = match mode {
        SandboxMode::ReadOnly => "read-only",
        SandboxMode::WorkspaceWrite => "workspace-write",
    };
    let mut result = vec![OsString::from("--mode"), OsString::from(mode)];
    for root in roots {
        result.push(OsString::from("--root"));
        result.push(root.as_os_str().to_owned());
    }
    result.push(OsString::from("--cwd"));
    result.push(cwd.as_os_str().to_owned());
    result.push(OsString::from("--"));
    result.push(program.as_os_str().to_owned());
    result.extend(args.iter().cloned());
    Ok(result)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoopTelemetry;

#[derive(Clone, Debug)]
pub struct CommandRequest {
    pub mode: SandboxMode,
    pub roots: Vec<std::path::PathBuf>,
    pub cwd: std::path::PathBuf,
    pub program: std::path::PathBuf,
    pub args: Vec<std::ffi::OsString>,
    pub env: std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    pub atelier_home: Option<std::path::PathBuf>,
    pub timeout: Option<std::time::Duration>,
    pub telemetry: Option<NoopTelemetry>,
}

impl CommandRequest {
    pub fn new(
        mode: SandboxMode,
        roots: Vec<std::path::PathBuf>,
        cwd: std::path::PathBuf,
        program: std::path::PathBuf,
        args: Vec<std::ffi::OsString>,
    ) -> Self {
        Self {
            mode,
            roots,
            cwd,
            program,
            args,
            env: std::collections::BTreeMap::new(),
            atelier_home: None,
            timeout: None,
            telemetry: None,
        }
    }

    pub fn validate(&self) -> Result<(), SandboxError> {
        runner::validate_request(self).map(|_| ())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct RunOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

#[cfg(test)]
mod contract_tests {
    use super::{SandboxMode, command_runner_args};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn helper_args_keep_user_arguments_after_separator() {
        let args = command_runner_args(
            SandboxMode::WorkspaceWrite,
            &[PathBuf::from(r"C:\workspace")],
            PathBuf::from(r"C:\workspace\src").as_path(),
            PathBuf::from(r"C:\Windows\System32\cmd.exe").as_path(),
            &[
                OsString::from("/D"),
                OsString::from("/S"),
                OsString::from("/C"),
                OsString::from("echo --root"),
            ],
        )
        .expect("helper args");

        let separator = args
            .iter()
            .position(|arg| arg == "--")
            .expect("command separator");
        assert_eq!(args[separator + 1], r"C:\Windows\System32\cmd.exe");
        assert_eq!(args[separator + 5], "echo --root");
        assert_eq!(args[1], "workspace-write");
    }

    #[test]
    fn helper_args_reject_missing_root_or_program() {
        let error = command_runner_args(
            SandboxMode::ReadOnly,
            &[],
            PathBuf::from(r"C:\workspace").as_path(),
            PathBuf::from("cmd.exe").as_path(),
            &[],
        )
        .expect_err("missing roots");
        assert!(error.to_string().contains("at least one"));

        let error = command_runner_args(
            SandboxMode::ReadOnly,
            &[PathBuf::from(r"C:\workspace")],
            PathBuf::from(r"C:\workspace").as_path(),
            PathBuf::new().as_path(),
            &[],
        )
        .expect_err("missing program");
        assert!(error.to_string().contains("empty"));
    }
}
