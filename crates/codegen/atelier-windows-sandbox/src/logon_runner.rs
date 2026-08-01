use crate::acl::{
    ScopedAclGrant, access_mask_for_mode, ensure_persistent_ancestor_traversal_grant,
    ensure_persistent_workspace_grant, grant_restricted_sids,
};
use crate::env::make_environment_block;
use crate::materialize;
use crate::process::spawn_as_user_with_handles;
use crate::setup::{SandboxCreds, ensure_sandbox_creds};
use crate::token::{
    LocalSid, ancestor_traversal_sid, create_restricted_token_for_sandbox_user,
    workspace_capability_sid,
};
use crate::winutil::{argv_to_command_line, path_to_wide, to_wide, win_error};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::ffi::{OsString, c_void};
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessWithLogonW,
    GetExitCodeProcess, LOGON_WITH_PROFILE, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW,
    TerminateProcess, WaitForSingleObject,
};

const PROTOCOL_VERSION: u32 = 2;
const ERROR_PIPE_CONNECTED: u32 = 535;
static PIPE_NONCE: AtomicU64 = AtomicU64::new(1);

fn record_spawn_timing(name: &'static str, started: Instant) {
    tracing::info!(
        target: "atelier_instrumentation",
        event = "timing",
        name,
        elapsed_us = started.elapsed().as_micros() as u64,
    );
}

#[derive(Debug, Serialize, Deserialize)]
struct RunnerRequest {
    version: u32,
    capability_sid: String,
    #[serde(default)]
    ancestor_traversal_sid: String,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    environment: Vec<u16>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum RunnerResponse {
    Ready,
    Error { message: String },
}

struct KillOnCloseJob(Option<HANDLE>);

impl KillOnCloseJob {
    fn new() -> Result<Self> {
        let job = unsafe { CreateJobObjectW(ptr::null_mut(), ptr::null()) };
        if job.is_null() {
            return Err(win_error("CreateJobObjectW"));
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&mut info as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            unsafe { CloseHandle(job) };
            return Err(win_error("SetInformationJobObject"));
        }
        Ok(Self(Some(job)))
    }

    fn assign(&self, process: HANDLE) -> Result<()> {
        let job = self
            .0
            .ok_or_else(|| anyhow!("sandbox Job Object is already closed"))?;
        if unsafe { AssignProcessToJobObject(job, process) } == 0 {
            return Err(win_error("AssignProcessToJobObject"));
        }
        Ok(())
    }

    fn terminate(&mut self, exit_code: u32) -> Result<()> {
        let Some(job) = self.0.take() else {
            return Ok(());
        };
        let terminate_error = if unsafe { TerminateJobObject(job, exit_code) } == 0 {
            Some(win_error("TerminateJobObject"))
        } else {
            None
        };
        let close_error = if unsafe { CloseHandle(job) } == 0 {
            Some(win_error("CloseHandle(Job Object)"))
        } else {
            None
        };
        terminate_error.or(close_error).map_or(Ok(()), Err)
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            unsafe { CloseHandle(job) };
        }
    }
}

struct RunnerProcessGuard(HANDLE);

impl RunnerProcessGuard {
    fn raw(&self) -> HANDLE {
        self.0
    }

    fn disarm(mut self) -> HANDLE {
        let process = self.0;
        self.0 = ptr::null_mut();
        process
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

impl Drop for RunnerProcessGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = TerminateProcess(self.0, 1);
                CloseHandle(self.0);
            }
        }
    }
}

struct ServerPipe(HANDLE);

impl ServerPipe {
    fn into_file(mut self) -> File {
        let handle = self.0;
        self.0 = ptr::null_mut();
        unsafe { File::from_raw_handle(handle.cast()) }
    }
}

impl Drop for ServerPipe {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct PipeNames {
    control: String,
    stdin: String,
    stdout: String,
    stderr: String,
}

struct PipeServers {
    control: ServerPipe,
    stdin: ServerPipe,
    stdout: ServerPipe,
    stderr: ServerPipe,
}

pub struct PersistentPipedProcess {
    process: HANDLE,
    job: KillOnCloseJob,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    _grants: Vec<ScopedAclGrant>,
}

unsafe impl Send for PersistentPipedProcess {}

impl PersistentPipedProcess {
    pub fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    pub fn try_wait(&mut self) -> Result<Option<i32>> {
        let mut exit_code = 0u32;
        if unsafe { GetExitCodeProcess(self.process, &mut exit_code) } == 0 {
            return Err(win_error("GetExitCodeProcess"));
        }
        if exit_code == 259 {
            Ok(None)
        } else {
            Ok(Some(exit_code as i32))
        }
    }

    pub fn wait(&mut self) -> Result<i32> {
        wait_for_process(self.process, u32::MAX)?;
        let exit_code = self
            .try_wait()?
            .ok_or_else(|| anyhow!("sandbox runner remained active after wait"))?;
        self.cleanup_descendants()?;
        Ok(exit_code)
    }

    pub fn kill(&mut self) -> Result<()> {
        let terminate_job = self.job.terminate(1);
        if let Err(wait_error) = wait_for_process(self.process, 5_000) {
            if self.try_wait()?.is_none() {
                if unsafe { TerminateProcess(self.process, 1) } == 0 {
                    let terminate_process = win_error("TerminateProcess");
                    return terminate_job.and(Err(terminate_process));
                }
                wait_for_process(self.process, 5_000)?;
            } else {
                terminate_job?;
                return Err(wait_error);
            }
        }
        terminate_job
    }

    pub(crate) fn cleanup_descendants(&mut self) -> Result<()> {
        self.job.terminate(1)
    }
}

impl Drop for PersistentPipedProcess {
    fn drop(&mut self) {
        let _ = self.kill();
        if !self.process.is_null() {
            unsafe { CloseHandle(self.process) };
        }
    }
}

fn inherited_path_entries_requiring_grant(
    path: &std::ffi::OsStr,
    user_profile: &Path,
    workspace_roots: &[PathBuf],
    protected_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in std::env::split_paths(path) {
        if !entry.is_absolute()
            || entry == user_profile
            || workspace_roots
                .iter()
                .any(|workspace| crate::path_is_within(workspace, &entry))
            || protected_roots
                .iter()
                .any(|protected| crate::path_is_within(protected, &entry))
        {
            continue;
        }
        if seen.insert(crate::canonical_path_key(&entry)) {
            entries.push(entry);
        }
    }
    entries
}

fn request_path(request: &crate::CommandRequest) -> Option<std::ffi::OsString> {
    request
        .env
        .iter()
        .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("Path"))
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var_os("PATH"))
}

fn workspace_ancestors_requiring_traversal(root: &Path) -> Vec<PathBuf> {
    let user_profile = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .and_then(|path| dunce::canonicalize(path).ok());
    if user_profile.as_ref().is_some_and(|profile| root == profile) {
        return Vec::new();
    }
    let profile_boundary = user_profile.filter(|profile| crate::path_is_within(profile, root));
    let mut ancestors = Vec::new();
    for ancestor in root.ancestors().skip(1) {
        if ancestor.parent().is_none() {
            break;
        }
        ancestors.push(ancestor.to_path_buf());
        if profile_boundary
            .as_ref()
            .is_some_and(|profile| ancestor == profile)
        {
            break;
        }
    }
    ancestors
}

pub fn spawn(request: crate::CommandRequest) -> Result<PersistentPipedProcess> {
    let total_started = Instant::now();

    let started = Instant::now();
    let validated = crate::runner::validate_request(&request)?;
    record_spawn_timing("windows_sandbox.validate_request", started);

    let started = Instant::now();
    let source = materialize::runner_source()?;
    record_spawn_timing("windows_sandbox.runner_source", started);

    let started = Instant::now();
    let creds = ensure_sandbox_creds(request.network_policy)?;
    record_spawn_timing("windows_sandbox.ensure_credentials", started);

    let home = crate::setup::atelier_home()?;
    let started = Instant::now();
    let runner = materialize::materialize(&source, &home)?;
    record_spawn_timing("windows_sandbox.materialize_runner", started);
    let program = materialize::remap_program(&validated.program, &source, &runner);

    let started = Instant::now();
    let capability = workspace_capability_sid(&home, &validated.roots, request.mode)?;
    let ancestor_traversal = ancestor_traversal_sid(&home)?;
    let sandbox_user = LocalSid::from_account(&creds.username)?;
    record_spawn_timing("windows_sandbox.resolve_sids", started);

    let access = access_mask_for_mode(request.mode);
    let started = Instant::now();
    let mut acl_changed = false;
    for root in &validated.roots {
        acl_changed |= ensure_persistent_workspace_grant(
            root,
            sandbox_user.as_ptr(),
            capability.as_ptr(),
            access,
        )?;
        for ancestor in workspace_ancestors_requiring_traversal(root) {
            acl_changed |= ensure_persistent_ancestor_traversal_grant(
                &ancestor,
                sandbox_user.as_ptr(),
                ancestor_traversal.as_ptr(),
            )?;
        }
    }
    if let (Some(path), Some(user_profile)) = (
        request_path(&request),
        std::env::var_os("USERPROFILE").map(PathBuf::from),
    ) {
        let mut protected_roots = [
            "SystemRoot",
            "ProgramW6432",
            "ProgramFiles",
            "ProgramFiles(x86)",
        ]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
        if let Some(program_data) = std::env::var_os("ProgramData") {
            protected_roots.push(PathBuf::from(program_data));
        }
        for root in inherited_path_entries_requiring_grant(
            &path,
            &user_profile,
            &validated.roots,
            &protected_roots,
        ) {
            let requested_root = root;
            let Ok(root) = dunce::canonicalize(&requested_root) else {
                continue;
            };
            if !root.is_dir() || crate::path_normalization::ensure_no_reparse_points(&root).is_err()
            {
                continue;
            }
            match ensure_persistent_workspace_grant(
                &root,
                sandbox_user.as_ptr(),
                ancestor_traversal.as_ptr(),
                access_mask_for_mode(crate::SandboxMode::ReadOnly),
            ) {
                Ok(changed) => {
                    acl_changed |= changed;
                    let mut ancestors = workspace_ancestors_requiring_traversal(&requested_root);
                    for ancestor in workspace_ancestors_requiring_traversal(&root) {
                        if !ancestors.iter().any(|existing| {
                            crate::canonical_path_key(existing)
                                == crate::canonical_path_key(&ancestor)
                        }) {
                            ancestors.push(ancestor);
                        }
                    }
                    for ancestor in ancestors {
                        match ensure_persistent_ancestor_traversal_grant(
                            &ancestor,
                            sandbox_user.as_ptr(),
                            ancestor_traversal.as_ptr(),
                        ) {
                            Ok(changed) => acl_changed |= changed,
                            Err(error) => {
                                tracing::warn!(
                                    path = %ancestor.display(),
                                    error = %error,
                                    "could not grant sandbox traversal to inherited PATH root"
                                );
                                break;
                            }
                        }
                    }
                }
                Err(error) => tracing::warn!(
                    path = %root.display(),
                    error = %error,
                    "could not grant sandbox read/execute access to inherited PATH root"
                ),
            }
        }
    }
    record_spawn_timing(
        if acl_changed {
            "windows_sandbox.grant_acl_propagated"
        } else {
            "windows_sandbox.grant_acl_reused"
        },
        started,
    );

    // The restricted worker executes the materialized single-binary runner.
    // Give this one file the current capability; unlike the workspace root,
    // this scoped change has no recursive propagation cost.
    let grants = vec![grant_restricted_sids(
        &runner,
        &[capability.as_ptr()],
        access_mask_for_mode(crate::SandboxMode::ReadOnly),
    )?];

    let names = pipe_names();
    let user_sid = sandbox_user.to_string()?;
    let started = Instant::now();
    let servers = create_servers(&names, &user_sid)?;
    record_spawn_timing("windows_sandbox.create_pipes", started);

    let started = Instant::now();
    let (process, job) = spawn_logon_runner(&runner, &validated.cwd, &creds, &names)?;
    record_spawn_timing("windows_sandbox.create_process_with_logon", started);
    let process = RunnerProcessGuard(process);

    let started = Instant::now();
    connect_server(&servers.control, process.raw())?;
    record_spawn_timing("windows_sandbox.connect_control_pipe", started);
    let started = Instant::now();
    connect_server(&servers.stdin, process.raw())?;
    record_spawn_timing("windows_sandbox.connect_stdin_pipe", started);
    let started = Instant::now();
    connect_server(&servers.stdout, process.raw())?;
    record_spawn_timing("windows_sandbox.connect_stdout_pipe", started);
    let started = Instant::now();
    connect_server(&servers.stderr, process.raw())?;
    record_spawn_timing("windows_sandbox.connect_stderr_pipe", started);

    let mut control = servers.control.into_file();
    let payload = RunnerRequest {
        version: PROTOCOL_VERSION,
        capability_sid: capability.to_string()?,
        ancestor_traversal_sid: ancestor_traversal.to_string()?,
        program,
        args: request
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        cwd: validated.cwd,
        environment: make_environment_block(&request.env, request.atelier_home.as_deref()),
    };
    let started = Instant::now();
    write_frame(&mut control, &payload)?;
    let response: RunnerResponse = read_frame(&mut control)?;
    record_spawn_timing("windows_sandbox.runner_handshake", started);
    match response {
        RunnerResponse::Ready => {}
        RunnerResponse::Error { message } => {
            return Err(anyhow!("persistent sandbox runner failed: {message}"));
        }
    }
    drop(control);
    let process = process.disarm();
    record_spawn_timing("windows_sandbox.spawn_total", total_started);
    Ok(PersistentPipedProcess {
        process,
        job,
        stdin: Some(servers.stdin.into_file()),
        stdout: Some(servers.stdout.into_file()),
        stderr: Some(servers.stderr.into_file()),
        _grants: grants,
    })
}

fn pipe_names() -> PipeNames {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos());
    let nonce = PIPE_NONCE.fetch_add(1, Ordering::Relaxed);
    let base = format!(
        r"\\.\pipe\atelier-sandbox-{}-{now:x}-{nonce:x}",
        std::process::id()
    );
    PipeNames {
        control: format!("{base}-control"),
        stdin: format!("{base}-stdin"),
        stdout: format!("{base}-stdout"),
        stderr: format!("{base}-stderr"),
    }
}

fn create_servers(names: &PipeNames, sandbox_sid: &str) -> Result<PipeServers> {
    Ok(PipeServers {
        control: create_server(&names.control, PIPE_ACCESS_DUPLEX, sandbox_sid)?,
        stdin: create_server(&names.stdin, PIPE_ACCESS_OUTBOUND, sandbox_sid)?,
        stdout: create_server(&names.stdout, PIPE_ACCESS_INBOUND, sandbox_sid)?,
        stderr: create_server(&names.stderr, PIPE_ACCESS_INBOUND, sandbox_sid)?,
    })
}

fn create_server(name: &str, access: u32, sandbox_sid: &str) -> Result<ServerPipe> {
    let sddl = to_wide(format!("D:P(A;;GA;;;{sandbox_sid})"));
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(win_error(
            "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        ));
    }
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let name = to_wide(name);
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            access,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            65_536,
            65_536,
            0,
            &security,
        )
    };
    unsafe { LocalFree(descriptor as HLOCAL) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(win_error("CreateNamedPipeW"));
    }
    Ok(ServerPipe(handle))
}

fn connect_server(pipe: &ServerPipe, expected_process: HANDLE) -> Result<()> {
    let pipe_value = pipe.0 as usize;
    let (sender, receiver) = mpsc::sync_channel(1);
    let connect = thread::spawn(move || {
        let pipe = pipe_value as HANDLE;
        let result = if unsafe { ConnectNamedPipe(pipe, ptr::null_mut()) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_PIPE_CONNECTED {
                Ok(())
            } else {
                Err(anyhow!("ConnectNamedPipe failed: {error}"))
            }
        } else {
            Ok(())
        };
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(Duration::from_secs(15)) {
        Ok(result) => {
            let _ = connect.join();
            result?;
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Err(anyhow!(
                "timed out connecting persistent sandbox runner pipe"
            ));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(anyhow!("persistent sandbox runner pipe thread exited"));
        }
    }
    let mut client_pid = 0u32;
    if unsafe { GetNamedPipeClientProcessId(pipe.0, &mut client_pid) } == 0 {
        return Err(win_error("GetNamedPipeClientProcessId"));
    }
    let expected_pid =
        unsafe { windows_sys::Win32::System::Threading::GetProcessId(expected_process) };
    if client_pid != expected_pid {
        return Err(anyhow!(
            "sandbox pipe client PID mismatch: expected {expected_pid}, got {client_pid}"
        ));
    }
    Ok(())
}

fn spawn_logon_runner(
    runner: &Path,
    cwd: &Path,
    creds: &SandboxCreds,
    names: &PipeNames,
) -> Result<(HANDLE, KillOnCloseJob)> {
    let args = vec![
        OsString::from("--internal-windows-sandbox-runner"),
        OsString::from("--control"),
        OsString::from(&names.control),
        OsString::from("--stdin"),
        OsString::from(&names.stdin),
        OsString::from("--stdout"),
        OsString::from(&names.stdout),
        OsString::from("--stderr"),
        OsString::from(&names.stderr),
    ];
    let app = path_to_wide(runner);
    let mut command_line = to_wide(argv_to_command_line(runner, &args));
    let cwd = path_to_wide(cwd);
    let username = to_wide(&creds.username);
    let domain = to_wide(".");
    let password = to_wide(&creds.password);
    let mut startup = STARTUPINFOW::default();
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process = PROCESS_INFORMATION::default();
    if unsafe {
        CreateProcessWithLogonW(
            username.as_ptr(),
            domain.as_ptr(),
            password.as_ptr(),
            LOGON_WITH_PROFILE,
            app.as_ptr(),
            command_line.as_mut_ptr(),
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            ptr::null::<c_void>(),
            cwd.as_ptr(),
            &startup,
            &mut process,
        )
    } == 0
    {
        return Err(win_error("CreateProcessWithLogonW"));
    }
    let process_guard = RunnerProcessGuard(process.hProcess);
    let thread_guard = OwnedHandle(process.hThread);
    let job = KillOnCloseJob::new()?;
    job.assign(process_guard.raw())?;
    if unsafe { ResumeThread(thread_guard.raw()) } == u32::MAX {
        return Err(win_error("ResumeThread"));
    }
    drop(thread_guard);
    Ok((process_guard.disarm(), job))
}

fn wait_for_process(process: HANDLE, timeout_ms: u32) -> Result<()> {
    match unsafe { WaitForSingleObject(process, timeout_ms) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(anyhow!(
            "timed out waiting for persistent sandbox runner to exit"
        )),
        WAIT_FAILED => Err(win_error("WaitForSingleObject")),
        result => Err(anyhow!(
            "WaitForSingleObject returned unexpected status {result}"
        )),
    }
}

fn open_client(name: &str, access: u32) -> Result<HANDLE> {
    let name = to_wide(name);
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            0,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(win_error("CreateFileW(named pipe)"));
    }
    Ok(handle)
}

pub fn run_runner<I, T>(args: I) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let names = parse_runner_args(args)?;
    let control_handle = open_client(&names.control, FILE_GENERIC_READ | FILE_GENERIC_WRITE)?;
    let stdin_handle = open_client(&names.stdin, FILE_GENERIC_READ)?;
    let stdout_handle = open_client(&names.stdout, FILE_GENERIC_WRITE)?;
    let stderr_handle = open_client(&names.stderr, FILE_GENERIC_WRITE)?;
    let mut control = unsafe { File::from_raw_handle(control_handle.cast()) };
    let request: RunnerRequest = read_frame(&mut control)?;
    if request.version != PROTOCOL_VERSION {
        write_frame(
            &mut control,
            &RunnerResponse::Error {
                message: format!("unsupported runner protocol {}", request.version),
            },
        )?;
        return Ok(2);
    }
    let result = (|| -> Result<_> {
        let capability = LocalSid::new(&request.capability_sid)?;
        let ancestor_traversal = LocalSid::new(&request.ancestor_traversal_sid)?;
        let token = create_restricted_token_for_sandbox_user(&capability, &ancestor_traversal)?;
        let args = request
            .args
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        let environment = crate::env::environment_for_sandbox_child(&request.environment);
        spawn_as_user_with_handles(
            token.raw(),
            &request.program,
            &args,
            &request.cwd,
            &environment,
            stdin_handle,
            stdout_handle,
            stderr_handle,
        )
    })();
    let mut child = match result {
        Ok(child) => child,
        Err(error) => {
            write_frame(
                &mut control,
                &RunnerResponse::Error {
                    message: format!("{error:#}"),
                },
            )?;
            return Ok(2);
        }
    };
    write_frame(&mut control, &RunnerResponse::Ready)?;
    drop(control);
    unsafe {
        CloseHandle(stdin_handle);
        CloseHandle(stdout_handle);
        CloseHandle(stderr_handle);
    }
    child.wait()
}

fn parse_runner_args<I, T>(args: I) -> Result<PipeNames>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    let mut control = None;
    let mut stdin = None;
    let mut stdout = None;
    let mut stderr = None;
    while let Some(arg) = args.next() {
        let target = match arg.to_string_lossy().as_ref() {
            "--control" => &mut control,
            "--stdin" => &mut stdin,
            "--stdout" => &mut stdout,
            "--stderr" => &mut stderr,
            other => return Err(anyhow!("unknown Windows sandbox runner option: {other}")),
        };
        *target = Some(
            args.next()
                .ok_or_else(|| anyhow!("missing value for {}", arg.to_string_lossy()))?
                .to_string_lossy()
                .into_owned(),
        );
    }
    Ok(PipeNames {
        control: control.ok_or_else(|| anyhow!("missing --control"))?,
        stdin: stdin.ok_or_else(|| anyhow!("missing --stdin"))?,
        stdout: stdout.ok_or_else(|| anyhow!("missing --stdout"))?,
        stderr: stderr.ok_or_else(|| anyhow!("missing --stderr"))?,
    })
}

fn write_frame<T: Serialize>(writer: &mut File, value: &T) -> Result<()> {
    let payload = serde_json::to_vec(value)?;
    let length = u32::try_from(payload.len()).context("sandbox runner frame too large")?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

fn read_frame<T: for<'de> Deserialize<'de>>(reader: &mut File) -> Result<T> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > 8 * 1024 * 1024 {
        return Err(anyhow!("sandbox runner frame exceeds 8 MiB"));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;
    Ok(serde_json::from_slice(&payload)?)
}

#[cfg(test)]
mod tests {
    use super::{
        KillOnCloseJob, PersistentPipedProcess, inherited_path_entries_requiring_grant,
        parse_runner_args,
    };
    use std::fs::File;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};
    use std::process::{Child, Command, Stdio};
    use std::ptr;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    const TREE_HELPER_MODE: &str = "ATELIER_WINDOWS_SANDBOX_TREE_HELPER_MODE";
    const TREE_HELPER_SIGNAL: &str = "ATELIER_WINDOWS_SANDBOX_TREE_HELPER_SIGNAL";
    const TREE_HELPER_PID: &str = "ATELIER_WINDOWS_SANDBOX_TREE_HELPER_PID";

    #[test]
    fn inherited_path_grants_user_and_custom_tool_roots_outside_the_workspace() {
        let roots = inherited_path_entries_requiring_grant(
            std::ffi::OsStr::new(
                r"C:\Users\dev\.local\bin;C:\Program Files\Git\cmd;C:\Users\dev\repo\tools;C:\USERS\DEV\.LOCAL\BIN;C:\toolchains\node;relative",
            ),
            std::path::Path::new(r"C:\Users\dev"),
            &[std::path::PathBuf::from(r"C:\Users\dev\repo")],
            &[std::path::PathBuf::from(r"C:\Program Files")],
        );
        assert_eq!(
            roots,
            vec![
                std::path::PathBuf::from(r"C:\Users\dev\.local\bin"),
                std::path::PathBuf::from(r"C:\toolchains\node"),
            ]
        );
    }

    fn spawn_tree_helper(mode: &str, temp: &tempfile::TempDir) -> (Child, std::path::PathBuf) {
        let signal = temp.path().join("start-tree");
        let pid_file = temp.path().join("grandchild.pid");
        let mut child = Command::new(std::env::current_exe().expect("current test executable"));
        child
            .arg("process_tree_helper")
            .arg("--nocapture")
            .env(TREE_HELPER_MODE, mode)
            .env(TREE_HELPER_SIGNAL, &signal)
            .env(TREE_HELPER_PID, &pid_file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        (child.spawn().expect("spawn process-tree helper"), signal)
    }

    fn duplicate_process_handle(child: &Child) -> HANDLE {
        let current = unsafe { GetCurrentProcess() };
        let mut duplicate = ptr::null_mut();
        let duplicated = unsafe {
            DuplicateHandle(
                current,
                child.as_raw_handle().cast(),
                current,
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        assert_ne!(duplicated, 0, "DuplicateHandle failed");
        duplicate
    }

    fn persistent_process(child: &mut Child, job: KillOnCloseJob) -> PersistentPipedProcess {
        let process = duplicate_process_handle(child);
        let stdout = child.stdout.take().expect("helper stdout");
        let stdout = unsafe { File::from_raw_handle(stdout.into_raw_handle()) };
        let stderr = child.stderr.take().expect("helper stderr");
        let stderr = unsafe { File::from_raw_handle(stderr.into_raw_handle()) };
        PersistentPipedProcess {
            process,
            job,
            stdin: None,
            stdout: Some(stdout),
            stderr: Some(stderr),
            _grants: Vec::new(),
        }
    }

    fn wait_for_grandchild(temp: &tempfile::TempDir) -> HANDLE {
        let pid_file = temp.path().join("grandchild.pid");
        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let Ok(contents) = std::fs::read_to_string(&pid_file) {
                break contents.trim().parse::<u32>().expect("grandchild PID");
            }
            assert!(
                Instant::now() < deadline,
                "grandchild PID was not published"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!handle.is_null(), "OpenProcess({pid}) failed");
        handle
    }

    fn assert_process_exited(handle: HANDLE) {
        assert_eq!(
            unsafe { WaitForSingleObject(handle, 5_000) },
            WAIT_OBJECT_0,
            "process remained alive after Job Object termination"
        );
        unsafe { CloseHandle(handle) };
    }

    fn read_pipe_to_end(mut pipe: File) -> mpsc::Receiver<Vec<u8>> {
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let _ = std::io::Read::read_to_end(&mut pipe, &mut output);
            let _ = sender.send(output);
        });
        receiver
    }

    fn read_output_to_end(
        process: &mut PersistentPipedProcess,
    ) -> (mpsc::Receiver<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
        (
            read_pipe_to_end(process.take_stdout().expect("persistent stdout")),
            read_pipe_to_end(process.take_stderr().expect("persistent stderr")),
        )
    }

    #[test]
    fn runner_requires_all_four_pipe_names() {
        assert!(parse_runner_args(["--control", "pipe"]).is_err());
        let parsed = parse_runner_args([
            "--control",
            "control",
            "--stdin",
            "stdin",
            "--stdout",
            "stdout",
            "--stderr",
            "stderr",
        ])
        .unwrap();
        assert_eq!(parsed.control, "control");
        assert_eq!(parsed.stderr, "stderr");
    }

    #[test]
    fn kill_terminates_job_tree_and_unblocks_inherited_pipe_reader() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (mut child, signal) = spawn_tree_helper("hold", &temp);
        let job = KillOnCloseJob::new().expect("job");
        job.assign(child.as_raw_handle().cast())
            .expect("assign job");
        std::fs::write(signal, b"go").expect("release process-tree helper");
        let grandchild = wait_for_grandchild(&temp);
        let mut process = persistent_process(&mut child, job);
        let (stdout, stderr) = read_output_to_end(&mut process);

        process.kill().expect("terminate persistent process tree");
        let stdout_closed = stdout.recv_timeout(Duration::from_secs(2)).is_ok();
        let stderr_closed = stderr.recv_timeout(Duration::from_secs(2)).is_ok();
        drop(process);

        assert!(stdout_closed, "grandchild kept stdout open after kill");
        assert!(stderr_closed, "grandchild kept stderr open after kill");
        assert_process_exited(grandchild);
        assert!(child.try_wait().expect("query helper").is_some());
    }

    #[test]
    fn wait_cleans_descendants_after_runner_exit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (mut child, signal) = spawn_tree_helper("exit", &temp);
        let job = KillOnCloseJob::new().expect("job");
        job.assign(child.as_raw_handle().cast())
            .expect("assign job");
        std::fs::write(signal, b"go").expect("release process-tree helper");
        let grandchild = wait_for_grandchild(&temp);
        let mut process = persistent_process(&mut child, job);
        let (stdout, stderr) = read_output_to_end(&mut process);

        assert_eq!(process.wait().expect("wait persistent process"), 0);
        let stdout_closed = stdout.recv_timeout(Duration::from_secs(2)).is_ok();
        let stderr_closed = stderr.recv_timeout(Duration::from_secs(2)).is_ok();
        drop(process);

        assert!(
            stdout_closed,
            "grandchild kept stdout open after runner exit"
        );
        assert!(
            stderr_closed,
            "grandchild kept stderr open after runner exit"
        );
        assert_process_exited(grandchild);
    }

    #[test]
    fn drop_terminates_job_tree_and_unblocks_inherited_pipe_reader() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (mut child, signal) = spawn_tree_helper("hold", &temp);
        let job = KillOnCloseJob::new().expect("job");
        job.assign(child.as_raw_handle().cast())
            .expect("assign job");
        std::fs::write(signal, b"go").expect("release process-tree helper");
        let grandchild = wait_for_grandchild(&temp);
        let mut process = persistent_process(&mut child, job);
        let (stdout, stderr) = read_output_to_end(&mut process);

        drop(process);

        assert!(
            stdout.recv_timeout(Duration::from_secs(2)).is_ok(),
            "grandchild kept stdout open after PersistentPipedProcess::drop"
        );
        assert!(
            stderr.recv_timeout(Duration::from_secs(2)).is_ok(),
            "grandchild kept stderr open after PersistentPipedProcess::drop"
        );
        assert_process_exited(grandchild);
        assert!(child.try_wait().expect("query helper").is_some());
    }

    #[test]
    fn process_tree_helper() {
        let Ok(mode) = std::env::var(TREE_HELPER_MODE) else {
            return;
        };
        if mode == "grandchild" {
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }

        let signal = std::path::PathBuf::from(
            std::env::var_os(TREE_HELPER_SIGNAL).expect("tree helper signal path"),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !signal.exists() {
            assert!(Instant::now() < deadline, "tree helper signal timed out");
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut grandchild =
            Command::new(std::env::current_exe().expect("current test executable"));
        grandchild
            .arg("process_tree_helper")
            .arg("--nocapture")
            .env(TREE_HELPER_MODE, "grandchild")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let grandchild = grandchild.spawn().expect("spawn grandchild helper");
        let pid_file = std::path::PathBuf::from(
            std::env::var_os(TREE_HELPER_PID).expect("tree helper PID path"),
        );
        std::fs::write(pid_file, grandchild.id().to_string()).expect("publish grandchild PID");

        if mode == "hold" {
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    }
}
