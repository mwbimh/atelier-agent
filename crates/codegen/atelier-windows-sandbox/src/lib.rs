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
pub use runner::{SandboxSession, SandboxedPipedChild, run_command, spawn_piped_command};
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

/// Resolve the executable used by the async terminal adapter.
///
/// Release builds reuse `atelier.exe` through the hidden
/// `--internal-command-runner` sub-mode, so the release directory no longer
/// needs a second executable. `ATELIER_COMMAND_RUNNER` remains an explicit
/// development/packaging override for a standalone runner.
pub fn command_runner_path() -> Result<PathBuf, SandboxError> {
    let path = if let Some(path) = std::env::var_os("ATELIER_COMMAND_RUNNER") {
        PathBuf::from(path)
    } else {
        let current_exe = std::env::current_exe().map_err(|error| {
            SandboxError::Operation(anyhow::anyhow!(
                "cannot resolve the Atelier command runner executable: {error}"
            ))
        })?;
        // The command runner is an internal mode of the main executable.
        // Aliases and versioned npm filenames must behave exactly like
        // `atelier.exe`, so the filename is deliberately not inspected.
        current_exe
    };

    if !path.is_file() {
        return Err(SandboxError::Operation(anyhow::anyhow!(
            "Atelier Windows sandbox command runner is missing: {}",
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

/// Build runner arguments and add the hidden marker when the runner is the
/// current `atelier.exe`. Keeping this separate preserves compatibility with
/// an explicitly configured standalone `atelier-command-runner.exe`.
pub fn command_runner_args_for(
    runner: &Path,
    current_exe: &Path,
    mode: SandboxMode,
    roots: &[PathBuf],
    cwd: &Path,
    program: &Path,
    args: &[OsString],
) -> Result<Vec<OsString>, SandboxError> {
    let mut result = command_runner_args(mode, roots, cwd, program, args)?;
    if same_executable(runner, current_exe) {
        result.insert(0, OsString::from("--internal-command-runner"));
    }
    Ok(result)
}

fn same_executable(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (
        dunce::canonicalize(left).ok(),
        dunce::canonicalize(right).ok(),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Parsed arguments for the hidden command-runner sub-mode.
#[derive(Debug, PartialEq, Eq)]
pub struct CommandRunnerArgs {
    pub mode: SandboxMode,
    pub roots: Vec<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub atelier_home: Option<PathBuf>,
    pub timeout: Option<std::time::Duration>,
    pub command: Vec<OsString>,
}

/// Parse the arguments accepted by both the standalone runner binary and the
/// hidden runner mode in `atelier.exe`.
pub fn parse_command_runner_args<I, T>(args: I) -> anyhow::Result<CommandRunnerArgs>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into).peekable();
    let mut mode = SandboxMode::ReadOnly;
    let mut roots = Vec::new();
    let mut cwd = None;
    let mut atelier_home = None;
    let mut timeout = None;
    let mut command = Vec::new();

    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--internal-command-runner" => {}
            "--" => {
                command.extend(args);
                break;
            }
            "--help" | "-h" => {
                return Err(anyhow::anyhow!(
                    "help is only available from the standalone command runner"
                ));
            }
            "--mode" => {
                mode = parse_mode(
                    &args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("missing --mode value"))?,
                )?;
            }
            "--root" => roots.push(PathBuf::from(
                args.next()
                    .ok_or_else(|| anyhow::anyhow!("missing --root value"))?,
            )),
            "--cwd" => {
                cwd = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("missing --cwd value"))?,
                ))
            }
            "--atelier-home" => {
                atelier_home =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("missing --atelier-home value")
                    })?));
            }
            "--timeout-ms" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing --timeout-ms value"))?;
                timeout = Some(std::time::Duration::from_millis(
                    value.to_string_lossy().parse()?,
                ));
            }
            other => return Err(anyhow::anyhow!("unknown option: {other}")),
        }
    }

    if roots.is_empty() {
        return Err(anyhow::anyhow!("at least one --root is required"));
    }
    if command.is_empty() {
        return Err(anyhow::anyhow!("missing command after --"));
    }
    Ok(CommandRunnerArgs {
        mode,
        roots,
        cwd,
        atelier_home,
        timeout,
        command,
    })
}

fn parse_mode(value: &OsString) -> anyhow::Result<SandboxMode> {
    match value.to_string_lossy().as_ref() {
        "read-only" => Ok(SandboxMode::ReadOnly),
        "workspace-write" => Ok(SandboxMode::WorkspaceWrite),
        other => Err(anyhow::anyhow!("unsupported sandbox mode: {other}")),
    }
}

/// Execute a command-runner invocation and forward the child streams.
pub fn run_command_runner<I, T>(args: I) -> anyhow::Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    use std::io::Write;

    let parsed = parse_command_runner_args(args)?;
    let cwd = parsed.cwd.unwrap_or(std::env::current_dir()?);
    let program = parsed
        .command
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing command after --"))?;
    let mut request = CommandRequest::new(
        parsed.mode,
        parsed.roots,
        cwd,
        PathBuf::from(program),
        parsed.command.into_iter().skip(1).collect(),
    );
    request.atelier_home = parsed.atelier_home;
    request.timeout = parsed.timeout;
    let output = run_command(request)?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    Ok(if output.timed_out {
        124
    } else {
        output.exit_code
    })
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

    #[test]
    fn embedded_runner_args_have_a_hidden_internal_marker() {
        let args = super::command_runner_args_for(
            PathBuf::from(r"C:\bin\atelier.exe").as_path(),
            PathBuf::from(r"C:\bin\atelier.exe").as_path(),
            SandboxMode::ReadOnly,
            &[PathBuf::from(r"C:\workspace")],
            PathBuf::from(r"C:\workspace").as_path(),
            PathBuf::from(r"C:\Windows\System32\cmd.exe").as_path(),
            &[OsString::from("/C"), OsString::from("echo ok")],
        )
        .expect("embedded runner args");
        assert_eq!(args[0], "--internal-command-runner");
        assert!(args.iter().any(|arg| arg == "--"));
    }

    #[test]
    fn renamed_release_executable_uses_embedded_command_runner_mode() {
        for executable in [
            r"C:\bin\agent.exe",
            r"C:\bin\atelier-0.1.220-alpha.4.exe",
            r"C:\bin\my-company-agent.exe",
        ] {
            let path = PathBuf::from(executable);
            let args = super::command_runner_args_for(
                path.as_path(),
                path.as_path(),
                SandboxMode::WorkspaceWrite,
                &[PathBuf::from(r"C:\workspace")],
                PathBuf::from(r"C:\workspace").as_path(),
                PathBuf::from(r"C:\Windows\System32\cmd.exe").as_path(),
                &[OsString::from("/C"), OsString::from("echo ok")],
            )
            .expect("embedded runner args");
            assert_eq!(args[0], "--internal-command-runner", "{executable}");
        }
    }

    #[test]
    fn command_runner_defaults_to_the_current_executable_regardless_of_name() {
        if std::env::var_os("ATELIER_COMMAND_RUNNER").is_some() {
            return;
        }
        let current = std::env::current_exe().expect("current test executable");
        let runner = super::command_runner_path().expect("embedded command runner");
        assert_eq!(runner, current);
    }

    #[test]
    fn external_runner_args_do_not_have_the_embedded_marker() {
        let args = super::command_runner_args_for(
            PathBuf::from(r"C:\bin\atelier-command-runner.exe").as_path(),
            PathBuf::from(r"C:\bin\atelier.exe").as_path(),
            SandboxMode::ReadOnly,
            &[PathBuf::from(r"C:\workspace")],
            PathBuf::from(r"C:\workspace").as_path(),
            PathBuf::from(r"C:\Windows\System32\cmd.exe").as_path(),
            &[],
        )
        .expect("external runner args");
        assert!(!matches!(
            args.first(),
            Some(arg) if arg == "--internal-command-runner"
        ));
    }

    #[test]
    fn command_runner_parser_requires_separator_and_command() {
        let parsed = super::parse_command_runner_args([
            "--mode",
            "workspace-write",
            "--root",
            r"C:\workspace",
            "--cwd",
            r"C:\workspace",
            "--",
            "cmd.exe",
            "/C",
            "echo ok",
        ])
        .expect("parse embedded command runner args");
        assert_eq!(parsed.mode, SandboxMode::WorkspaceWrite);
        assert_eq!(parsed.roots, vec![PathBuf::from(r"C:\workspace")]);
        assert_eq!(parsed.command[0], "cmd.exe");
    }
}
