use crate::SandboxError;
use crate::logon_runner::PersistentPipedProcess;
use crate::path_normalization::{
    ensure_no_reparse_points, normalize_existing_path, path_is_within,
};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) struct ValidatedRequest {
    pub roots: Vec<PathBuf>,
    pub cwd: PathBuf,
    pub program: PathBuf,
}

pub(crate) fn validate_request(
    request: &crate::CommandRequest,
) -> Result<ValidatedRequest, SandboxError> {
    if request.roots.is_empty() {
        return Err(SandboxError::NoRoots);
    }
    if request.program.as_os_str().is_empty() {
        return Err(SandboxError::EmptyCommand);
    }

    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for root in &request.roots {
        ensure_no_reparse_points(root)?;
        let root = normalize_existing_path(root)?;
        if !root.is_dir() {
            return Err(SandboxError::NotDirectory(root));
        }
        if seen.insert(crate::canonical_path_key(&root)) {
            roots.push(root);
        }
    }
    ensure_no_reparse_points(&request.cwd)?;
    let cwd = normalize_existing_path(&request.cwd)?;
    if !cwd.is_dir() {
        return Err(SandboxError::NotDirectory(cwd));
    }
    if !roots.iter().any(|root| path_is_within(root, &cwd)) {
        return Err(SandboxError::CwdOutsideRoots { cwd });
    }
    let program = resolve_program(&request.program)?;
    ensure_no_reparse_points(&program)?;
    Ok(ValidatedRequest {
        roots,
        cwd,
        program,
    })
}

fn resolve_program(program: &Path) -> Result<PathBuf, SandboxError> {
    if program.components().count() > 1 || program.is_absolute() {
        let path = normalize_existing_path(program)?;
        return if path.is_file() {
            Ok(path)
        } else {
            Err(SandboxError::MissingPath(path))
        };
    }

    let name = program.as_os_str();
    let mut candidates = Vec::new();
    if name.to_string_lossy().contains('.') {
        candidates.push(OsString::from(name));
    } else {
        candidates.push(OsString::from(name));
        candidates.push(OsString::from(format!("{}.exe", name.to_string_lossy())));
        candidates.push(OsString::from(format!("{}.com", name.to_string_lossy())));
    }
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    for directory in std::env::split_paths(&search_path) {
        for candidate in &candidates {
            let path = directory.join(candidate);
            if path.is_file() {
                return normalize_existing_path(&path);
            }
        }
    }
    Err(SandboxError::MissingPath(program.to_path_buf()))
}

pub struct SandboxSession;

trait ProcessControl {
    fn try_wait(&mut self) -> anyhow::Result<Option<i32>>;
    fn cleanup_descendants(&mut self) -> anyhow::Result<()>;
    fn kill(&mut self) -> anyhow::Result<()>;
}

impl ProcessControl for PersistentPipedProcess {
    fn try_wait(&mut self) -> anyhow::Result<Option<i32>> {
        PersistentPipedProcess::try_wait(self)
    }

    fn cleanup_descendants(&mut self) -> anyhow::Result<()> {
        PersistentPipedProcess::cleanup_descendants(self)
    }

    fn kill(&mut self) -> anyhow::Result<()> {
        PersistentPipedProcess::kill(self)
    }
}

fn wait_for_exit_or_timeout(
    child: &mut impl ProcessControl,
    timeout: Option<std::time::Duration>,
) -> Result<(i32, bool), SandboxError> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    loop {
        if let Some(exit_code) = child.try_wait().map_err(SandboxError::Operation)? {
            child
                .cleanup_descendants()
                .map_err(SandboxError::Operation)?;
            return Ok((exit_code, false));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            child.kill().map_err(SandboxError::Operation)?;
            return Ok((124, true));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

impl SandboxSession {
    pub fn new() -> Result<Self, SandboxError> {
        Ok(Self)
    }

    pub fn run(
        &mut self,
        request: crate::CommandRequest,
    ) -> Result<crate::RunOutput, SandboxError> {
        let timeout = request.timeout;
        let mut child = crate::logon_runner::spawn(request).map_err(SandboxError::Operation)?;
        drop(child.take_stdin());
        let stdout = child
            .take_stdout()
            .ok_or_else(|| SandboxError::Operation(anyhow::anyhow!("sandbox stdout missing")))?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| SandboxError::Operation(anyhow::anyhow!("sandbox stderr missing")))?;
        let stdout_thread = std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut stdout = stdout;
            let _ = stdout.read_to_end(&mut output);
            output
        });
        let stderr_thread = std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut stderr = stderr;
            let _ = stderr.read_to_end(&mut output);
            output
        });
        let (exit_code, timed_out) = wait_for_exit_or_timeout(&mut child, timeout)?;
        Ok(crate::RunOutput {
            exit_code,
            stdout: stdout_thread.join().unwrap_or_default(),
            stderr: stderr_thread.join().unwrap_or_default(),
            timed_out,
        })
    }

    pub fn spawn_piped(
        self,
        request: crate::CommandRequest,
    ) -> Result<SandboxedPipedChild, SandboxError> {
        let child = crate::logon_runner::spawn(request).map_err(SandboxError::Operation)?;
        Ok(SandboxedPipedChild { child })
    }
}

pub struct SandboxedPipedChild {
    child: PersistentPipedProcess,
}

// The object owns Windows kernel handles and heap-backed SID/ACL state. It is
// moved as one unit and all mutation is externally serialized by the Worker
// connection mutex; none of its raw pointers borrow thread-local storage.
unsafe impl Send for SandboxedPipedChild {}

impl SandboxedPipedChild {
    pub fn take_stdin(&mut self) -> Option<std::fs::File> {
        self.child.take_stdin()
    }

    pub fn take_stdout(&mut self) -> Option<std::fs::File> {
        self.child.take_stdout()
    }

    pub fn take_stderr(&mut self) -> Option<std::fs::File> {
        self.child.take_stderr()
    }

    pub fn try_wait(&mut self) -> anyhow::Result<Option<i32>> {
        self.child.try_wait()
    }

    pub fn wait(&mut self) -> anyhow::Result<i32> {
        self.child.wait()
    }

    pub fn kill(&mut self) -> anyhow::Result<()> {
        self.child.kill()
    }
}

pub fn run_command(request: crate::CommandRequest) -> Result<crate::RunOutput, SandboxError> {
    SandboxSession::new()?.run(request)
}

pub fn spawn_piped_command(
    request: crate::CommandRequest,
) -> Result<SandboxedPipedChild, SandboxError> {
    SandboxSession::new()?.spawn_piped(request)
}

#[cfg(test)]
mod tests {
    use super::{ProcessControl, validate_request, wait_for_exit_or_timeout};
    use crate::{CommandRequest, SandboxMode};
    use std::path::PathBuf;
    use std::time::Duration;

    #[derive(Default)]
    struct FakeProcess {
        exit_code: Option<i32>,
        cleanup_calls: usize,
        kill_calls: usize,
    }

    impl ProcessControl for FakeProcess {
        fn try_wait(&mut self) -> anyhow::Result<Option<i32>> {
            Ok(self.exit_code)
        }

        fn cleanup_descendants(&mut self) -> anyhow::Result<()> {
            self.cleanup_calls += 1;
            Ok(())
        }

        fn kill(&mut self) -> anyhow::Result<()> {
            self.kill_calls += 1;
            Ok(())
        }
    }

    #[test]
    fn timeout_uses_process_tree_kill_path() {
        let mut process = FakeProcess::default();

        let outcome = wait_for_exit_or_timeout(&mut process, Some(Duration::ZERO)).unwrap();

        assert_eq!(outcome, (124, true));
        assert_eq!(process.kill_calls, 1);
        assert_eq!(process.cleanup_calls, 0);
    }

    #[test]
    fn normal_exit_cleans_remaining_job_descendants() {
        let mut process = FakeProcess {
            exit_code: Some(0),
            ..Default::default()
        };

        let outcome = wait_for_exit_or_timeout(&mut process, None).unwrap();

        assert_eq!(outcome, (0, false));
        assert_eq!(process.kill_calls, 0);
        assert_eq!(process.cleanup_calls, 1);
    }

    #[test]
    fn validate_request_rejects_directory_symlink() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        std::fs::create_dir(&target).expect("target directory");

        if std::os::windows::fs::symlink_dir(&target, &link).is_err() {
            // Symlink creation requires a Windows privilege that is not
            // available in every test runner. The contract remains covered by
            // the path-normalization unit tests when this capability is absent.
            return;
        }

        let request = CommandRequest::new(
            SandboxMode::ReadOnly,
            vec![link.clone()],
            link,
            PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            Vec::new(),
        );
        let error = match validate_request(&request) {
            Ok(_) => panic!("symlink root must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("reparse") || error.to_string().contains("symlink"));
    }
}
