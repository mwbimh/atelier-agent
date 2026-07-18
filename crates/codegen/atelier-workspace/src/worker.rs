//! Local, typed RPC transport for the workspace worker.
//!
//! The worker is deliberately a thin process boundary around the existing
//! [`WorkspaceRpcHandler`].  Keeping the operation implementation in the
//! workspace crate means the in-process, hub-proxy, and worker paths share the
//! same path-confinement and VCS code.

use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

use crate::{WorkspaceError, WorkspaceResult};

/// Version of the local worker protocol.
pub const WORKER_PROTOCOL_VERSION: u32 = 1;

/// Maximum encoded JSON frame, including the trailing newline.
///
/// This is intentionally smaller than the largest hub frame.  File contents
/// should use the streaming/ranged workspace APIs rather than one unbounded
/// worker message.
pub const MAX_WORKER_FRAME_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_WORKER_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const WORKER_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Methods accepted by a worker connection.
pub fn is_worker_method(method: &str) -> bool {
    method.starts_with("workspace.") || method.starts_with("atelier.worker.")
}

/// One newline-delimited worker request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerRequest {
    Hello {
        protocol_version: u32,
        nonce: String,
        workspace_root: String,
    },
    Call {
        protocol_version: u32,
        nonce: String,
        request_id: String,
        method: String,
        #[serde(default)]
        params: Value,
        #[serde(default)]
        bound_session: Option<String>,
    },
    Shutdown {
        protocol_version: u32,
        nonce: String,
        request_id: String,
    },
}

/// One newline-delimited worker response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerResponse {
    Ready {
        protocol_version: u32,
        workspace_root: String,
    },
    Result {
        request_id: String,
        result: Value,
    },
    Error {
        request_id: Option<String>,
        code: String,
        message: String,
    },
    Bye {
        request_id: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerProtocolError {
    #[error("worker protocol I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("worker protocol JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("worker protocol frame exceeds {MAX_WORKER_FRAME_BYTES} bytes")]
    FrameTooLarge,
    #[error("worker protocol frame is empty")]
    EmptyFrame,
    #[error("worker protocol version mismatch: expected {expected}, got {actual}")]
    VersionMismatch { expected: u32, actual: u32 },
    #[error("worker protocol nonce mismatch")]
    NonceMismatch,
    #[error("worker protocol invalid request: {0}")]
    InvalidRequest(String),
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
}

/// Read one bounded newline-delimited JSON frame.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, WorkerProtocolError>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut bytes = Vec::new();
    let count = reader.read_until(b'\n', &mut bytes).await?;
    if count == 0 {
        return Ok(None);
    }
    if count > MAX_WORKER_FRAME_BYTES {
        return Err(WorkerProtocolError::FrameTooLarge);
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(WorkerProtocolError::EmptyFrame);
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

/// Write one bounded newline-delimited JSON frame.
pub async fn write_frame<W, T>(writer: &mut W, frame: &T) -> Result<(), WorkerProtocolError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(frame)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_WORKER_FRAME_BYTES {
        return Err(WorkerProtocolError::FrameTooLarge);
    }
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

struct WorkerConnection {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    nonce: String,
    next_request_id: u64,
    root: PathBuf,
}

/// Client for a single `atelier-workspace-worker` process.
#[derive(Clone)]
pub struct WorkspaceWorkerClient {
    inner: Arc<Mutex<WorkerConnection>>,
}

impl WorkspaceWorkerClient {
    /// Spawn a worker binary and complete the protocol handshake.
    pub async fn spawn(root: PathBuf, worker_path: PathBuf) -> WorkspaceResult<Self> {
        let root = canonical_root(&root).await?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let (program, args) = worker_process_command(&root, &worker_path)?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| WorkspaceError::HubError(format!("spawn workspace worker: {e}")))?;
        let stdin = child.stdin.take().ok_or_else(|| {
            WorkspaceError::HubError("workspace worker stdin was not piped".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            WorkspaceError::HubError("workspace worker stdout was not piped".into())
        })?;
        let mut connection = WorkerConnection {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            nonce: nonce.clone(),
            next_request_id: 0,
            root: root.clone(),
        };
        write_frame(
            &mut connection.stdin,
            &WorkerRequest::Hello {
                protocol_version: WORKER_PROTOCOL_VERSION,
                nonce,
                workspace_root: root.to_string_lossy().into_owned(),
            },
        )
        .await
        .map_err(worker_protocol_to_workspace)?;
        match tokio::time::timeout(
            WORKER_HANDSHAKE_TIMEOUT,
            read_frame::<_, WorkerResponse>(&mut connection.stdout),
        )
        .await
        .map_err(|_| WorkspaceError::HubError("workspace worker handshake timed out".into()))?
        .map_err(worker_protocol_to_workspace)?
        {
            Some(WorkerResponse::Ready {
                protocol_version,
                workspace_root,
            }) if protocol_version == WORKER_PROTOCOL_VERSION
                && workspace_root == root.to_string_lossy() => {}
            Some(other) => {
                return Err(WorkspaceError::HubError(format!(
                    "workspace worker handshake failed: {other:?}"
                )));
            }
            None => {
                return Err(WorkspaceError::HubError(
                    "workspace worker exited during handshake".into(),
                ));
            }
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(connection)),
        })
    }

    /// Resolve the sibling worker binary used by packaged builds.
    pub fn default_worker_path() -> WorkspaceResult<PathBuf> {
        let exe = std::env::current_exe()
            .map_err(|e| WorkspaceError::HubError(format!("resolve current executable: {e}")))?;
        find_worker_binary(
            &exe,
            std::env::var_os("ATELIER_WORKSPACE_WORKER").as_deref(),
        )
    }

    /// Workspace root bound by the handshake.
    pub async fn root(&self) -> PathBuf {
        self.inner.lock().await.root.clone()
    }

    /// Call a workspace RPC method through the worker.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        bound_session: Option<&str>,
    ) -> WorkspaceResult<Value> {
        let mut connection = self.inner.lock().await;
        Self::call_locked(&mut connection, method, params, bound_session).await
    }

    /// Call a workspace method with a hard deadline.
    ///
    /// A timed-out request terminates the worker process before releasing the
    /// connection lock. This deliberately treats the worker as poisoned: a
    /// late response must never be allowed to desynchronise the next request,
    /// and callers must recreate the session/worker instead of falling back to
    /// host filesystem access.
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        bound_session: Option<&str>,
        timeout: std::time::Duration,
    ) -> WorkspaceResult<Value> {
        let mut connection = self.inner.lock().await;
        match tokio::time::timeout(
            timeout,
            Self::call_locked(&mut connection, method, params, bound_session),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let _ = connection.child.kill().await;
                Err(WorkspaceError::HubError(format!(
                    "workspace worker request timed out after {} ms: {method}",
                    timeout.as_millis()
                )))
            }
        }
    }

    /// Terminate the worker without attempting a graceful protocol exchange.
    /// Used by cancellation and teardown paths; subsequent calls fail closed.
    pub async fn terminate(&self) {
        let mut connection = self.inner.lock().await;
        let _ = connection.child.kill().await;
    }

    async fn call_locked(
        connection: &mut WorkerConnection,
        method: &str,
        params: Value,
        bound_session: Option<&str>,
    ) -> WorkspaceResult<Value> {
        if !is_worker_method(method) {
            return Err(WorkspaceError::HubError(format!(
                "workspace worker rejected method namespace: {method}"
            )));
        }
        connection.next_request_id = connection.next_request_id.saturating_add(1);
        let request_id = connection.next_request_id.to_string();
        let nonce = connection.nonce.clone();
        write_frame(
            &mut connection.stdin,
            &WorkerRequest::Call {
                protocol_version: WORKER_PROTOCOL_VERSION,
                nonce,
                request_id: request_id.clone(),
                method: method.to_owned(),
                params,
                bound_session: bound_session.map(str::to_owned),
            },
        )
        .await
        .map_err(worker_protocol_to_workspace)?;
        match read_frame::<_, WorkerResponse>(&mut connection.stdout)
            .await
            .map_err(worker_protocol_to_workspace)?
        {
            Some(WorkerResponse::Result {
                request_id: id,
                result,
            }) if id == request_id => Ok(result),
            Some(WorkerResponse::Error {
                request_id: Some(id),
                code,
                message,
            }) if id == request_id => Err(WorkspaceError::HubError(format!(
                "workspace worker [{code}]: {message}"
            ))),
            Some(other) => Err(WorkspaceError::HubError(format!(
                "workspace worker returned an unexpected response: {other:?}"
            ))),
            None => Err(worker_crashed(&mut connection.child).await),
        }
    }

    /// Call a typed workspace RPC request.
    pub async fn call_typed<R>(
        &self,
        request: &R,
        bound_session: Option<&str>,
    ) -> WorkspaceResult<R::Response>
    where
        R: atelier_workspace_types::rpc::WorkspaceRpc,
    {
        let params = serde_json::to_value(request)
            .map_err(|e| WorkspaceError::HubError(format!("serialize worker request: {e}")))?;
        let value = self.call(R::METHOD, params, bound_session).await?;
        serde_json::from_value(value)
            .map_err(|e| WorkspaceError::HubError(format!("decode worker response: {e}")))
    }

    pub async fn call_typed_with_timeout<R>(
        &self,
        request: &R,
        bound_session: Option<&str>,
        timeout: std::time::Duration,
    ) -> WorkspaceResult<R::Response>
    where
        R: atelier_workspace_types::rpc::WorkspaceRpc,
    {
        let params = serde_json::to_value(request)
            .map_err(|e| WorkspaceError::HubError(format!("serialize worker request: {e}")))?;
        let value = self
            .call_with_timeout(R::METHOD, params, bound_session, timeout)
            .await?;
        serde_json::from_value(value)
            .map_err(|e| WorkspaceError::HubError(format!("decode worker response: {e}")))
    }

    /// List a workspace directory through the worker boundary.
    pub async fn read_dir(
        &self,
        path: &Path,
        bound_session: Option<&str>,
    ) -> WorkspaceResult<atelier_workspace_types::rpc::fs::ClientFsListRes> {
        let value = self
            .call_with_timeout(
                "atelier.worker.read_dir",
                serde_json::json!({ "path": path }),
                bound_session,
                worker_call_timeout(),
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|e| WorkspaceError::HubError(format!("decode worker read_dir response: {e}")))
    }

    /// Stat a workspace path through the worker boundary.
    pub async fn stat(
        &self,
        path: &Path,
        bound_session: Option<&str>,
    ) -> WorkspaceResult<atelier_workspace_types::rpc::fs::ClientFsStatRes> {
        let value = self
            .call_with_timeout(
                "atelier.worker.stat",
                serde_json::json!({ "path": path }),
                bound_session,
                worker_call_timeout(),
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|e| WorkspaceError::HubError(format!("decode worker stat response: {e}")))
    }

    /// Shut down the worker and wait for its process to exit.
    pub async fn shutdown(&self) -> WorkspaceResult<()> {
        let mut connection = self.inner.lock().await;
        connection.next_request_id = connection.next_request_id.saturating_add(1);
        let request_id = connection.next_request_id.to_string();
        let nonce = connection.nonce.clone();
        write_frame(
            &mut connection.stdin,
            &WorkerRequest::Shutdown {
                protocol_version: WORKER_PROTOCOL_VERSION,
                nonce,
                request_id: request_id.clone(),
            },
        )
        .await
        .map_err(worker_protocol_to_workspace)?;
        match read_frame::<_, WorkerResponse>(&mut connection.stdout)
            .await
            .map_err(worker_protocol_to_workspace)?
        {
            Some(WorkerResponse::Bye { request_id: id }) if id == request_id => {}
            Some(other) => {
                return Err(WorkspaceError::HubError(format!(
                    "workspace worker shutdown failed: {other:?}"
                )));
            }
            None => {}
        }
        let status =
            tokio::time::timeout(std::time::Duration::from_secs(2), connection.child.wait()).await;
        if status.is_err() {
            connection
                .child
                .kill()
                .await
                .map_err(|e| WorkspaceError::HubError(format!("kill workspace worker: {e}")))?;
        }
        Ok(())
    }
}

fn find_worker_binary(
    current_exe: &Path,
    explicit_path: Option<&std::ffi::OsStr>,
) -> WorkspaceResult<PathBuf> {
    let name = if cfg!(windows) {
        "atelier-workspace-worker.exe"
    } else {
        "atelier-workspace-worker"
    };

    if let Some(path) = explicit_path.map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
        return Err(WorkspaceError::HubError(format!(
            "workspace worker binary is unavailable: {}",
            path.display()
        )));
    }

    if is_atelier_main_executable(current_exe) && current_exe.is_file() {
        return Ok(current_exe.to_path_buf());
    }

    let parent = current_exe.parent().ok_or_else(|| {
        WorkspaceError::HubError("current executable has no parent directory".into())
    })?;
    let mut candidates = vec![parent.join(name)];

    // `cargo test` places the test executable in `target/<profile>/deps` while
    // the binary target is in `target/<profile>`.  Treat this as a discovery
    // path only; the final `is_file` check keeps production fail-closed.
    if parent.file_name().is_some_and(|file| file == "deps")
        && let Some(profile_dir) = parent.parent()
    {
        candidates.push(profile_dir.join(name));
    }

    if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
        return Ok(path);
    }

    Err(WorkspaceError::HubError(format!(
        "workspace worker binary is unavailable; checked beside {} and Cargo target locations",
        current_exe.display()
    )))
}

fn is_atelier_main_executable(path: &Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("atelier"))
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

fn worker_args_for_path(
    worker_path: &Path,
    current_exe: &Path,
    root: &Path,
) -> Vec<std::ffi::OsString> {
    let mut args = Vec::new();
    if same_executable(worker_path, current_exe) {
        args.push(std::ffi::OsString::from("--internal-workspace-worker"));
    }
    args.push(std::ffi::OsString::from("--root"));
    args.push(root.as_os_str().to_owned());
    args
}

fn worker_process_command(
    root: &Path,
    worker_path: &Path,
) -> WorkspaceResult<(PathBuf, Vec<std::ffi::OsString>)> {
    let current_exe = std::env::current_exe().map_err(|error| {
        WorkspaceError::HubError(format!("resolve current executable: {error}"))
    })?;
    let worker_args = worker_args_for_path(worker_path, &current_exe, root);

    #[cfg(windows)]
    {
        if atelier_sandbox::diagnostics().backend == atelier_sandbox::SandboxBackendKind::Unsafe {
            return Ok((worker_path.to_path_buf(), worker_args));
        }
        let mode = match atelier_sandbox::windows_child_sandbox_mode() {
            Some("read-only") => atelier_windows_sandbox::SandboxMode::ReadOnly,
            Some("workspace-write") => atelier_windows_sandbox::SandboxMode::WorkspaceWrite,
            Some(other) => {
                return Err(WorkspaceError::HubError(format!(
                    "unsupported Windows worker sandbox mode: {other}"
                )));
            }
            None => {
                return Err(WorkspaceError::HubError(
                    "Windows workspace worker sandbox is not active; refusing unsandboxed worker"
                        .into(),
                ));
            }
        };
        let runner = atelier_windows_sandbox::command_runner_path()
            .map_err(|error| WorkspaceError::HubError(error.to_string()))?;
        let args = atelier_windows_sandbox::command_runner_args_for(
            &runner,
            &current_exe,
            mode,
            &[root.to_path_buf()],
            root,
            worker_path,
            &worker_args,
        )
        .map_err(|error| WorkspaceError::HubError(error.to_string()))?;
        return Ok((runner, args));
    }

    #[cfg(not(windows))]
    {
        Ok((worker_path.to_path_buf(), worker_args))
    }
}

/// Parse the command line for the workspace-worker sub-mode.
pub fn parse_worker_args<I, T>(args: I) -> WorkspaceResult<PathBuf>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    let mut root = None;
    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--internal-workspace-worker" => {}
            "--root" => {
                root = Some(PathBuf::from(args.next().ok_or_else(|| {
                    WorkspaceError::HubError("missing --root value".into())
                })?));
            }
            "--help" | "-h" => {
                return Err(WorkspaceError::HubError(
                    "workspace worker requires --root PATH".into(),
                ));
            }
            other => {
                return Err(WorkspaceError::HubError(format!(
                    "unknown workspace worker option: {other}"
                )));
            }
        }
    }
    root.ok_or_else(|| WorkspaceError::HubError("workspace worker requires --root PATH".into()))
}

async fn worker_crashed(child: &mut Child) -> WorkspaceError {
    match child.try_wait() {
        Ok(Some(status)) => WorkspaceError::HubError(format!(
            "workspace worker exited unexpectedly with {status}"
        )),
        Ok(None) => WorkspaceError::HubError("workspace worker pipe closed".into()),
        Err(error) => WorkspaceError::HubError(format!(
            "workspace worker pipe closed and exit status unavailable: {error}"
        )),
    }
}

fn worker_protocol_to_workspace(error: WorkerProtocolError) -> WorkspaceError {
    WorkspaceError::HubError(error.to_string())
}

async fn canonical_root(root: &Path) -> WorkspaceResult<PathBuf> {
    let root = dunce::canonicalize(root)
        .map_err(|e| WorkspaceError::HubError(format!("canonicalize worker root: {e}")))?;
    if !root.is_dir() {
        return Err(WorkspaceError::HubError(format!(
            "workspace worker root is not a directory: {}",
            root.display()
        )));
    }
    Ok(root)
}

/// Worker-backed filesystem shared by workspace and tool contexts.
#[derive(Clone)]
pub struct WorkspaceWorkerFs {
    root: PathBuf,
    client: WorkspaceWorkerClient,
}

impl WorkspaceWorkerFs {
    pub fn new(root: PathBuf, client: WorkspaceWorkerClient) -> Self {
        Self { root, client }
    }

    pub fn client(&self) -> &WorkspaceWorkerClient {
        &self.client
    }

    fn absolute_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }

    async fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, WorkspaceError> {
        let response = self
            .client
            .call_with_timeout(
                "atelier.worker.read_file",
                serde_json::json!({
                    "path": self.absolute_path(path),
                }),
                None,
                worker_call_timeout(),
            )
            .await?;
        let encoded = response
            .get("data_base64")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                WorkspaceError::HubError("worker read response lacks data_base64".into())
            })?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| WorkspaceError::HubError(format!("decode worker file: {e}")))
    }

    async fn write_bytes(&self, path: &Path, data: &[u8]) -> Result<(), WorkspaceError> {
        self.client
            .call_with_timeout(
                "atelier.worker.write_file",
                serde_json::json!({
                    "path": self.absolute_path(path),
                    "data_base64": base64::engine::general_purpose::STANDARD.encode(data),
                    "create_dirs": true,
                }),
                None,
                worker_call_timeout(),
            )
            .await
            .map(|_| ())
    }

    async fn delete_bytes(&self, path: &Path) -> Result<(), WorkspaceError> {
        self.client
            .call_with_timeout(
                "atelier.worker.delete_file",
                serde_json::json!({ "path": self.absolute_path(path) }),
                None,
                worker_call_timeout(),
            )
            .await
            .map(|_| ())
    }

    async fn grep(
        &self,
        request: atelier_tools::computer::types::GrepRequest,
    ) -> Result<Option<atelier_tools::types::output::GrepSearchOutput>, WorkspaceError> {
        let value = self
            .client
            .call_with_timeout(
                "atelier.worker.grep",
                serde_json::to_value(request)
                    .map_err(|error| WorkspaceError::HubError(error.to_string()))?,
                None,
                worker_call_timeout(),
            )
            .await?;
        let output = serde_json::from_value(value).map_err(|error| {
            WorkspaceError::HubError(format!("decode worker grep response: {error}"))
        })?;
        Ok(Some(output))
    }

    pub async fn read_dir(
        &self,
        path: &Path,
    ) -> Result<atelier_workspace_types::rpc::fs::ClientFsListRes, WorkspaceError> {
        self.client.read_dir(&self.absolute_path(path), None).await
    }

    pub async fn stat(
        &self,
        path: &Path,
    ) -> Result<atelier_workspace_types::rpc::fs::ClientFsStatRes, WorkspaceError> {
        self.client.stat(&self.absolute_path(path), None).await
    }
}

#[async_trait]
impl crate::file_system::AsyncFileSystem for WorkspaceWorkerFs {
    fn root(&self) -> &Path {
        &self.root
    }

    async fn exists(&self, path: &Path) -> Result<bool, crate::file_system::FsError> {
        self.stat(path)
            .await
            .map(|response| response.exists)
            .map_err(|e| crate::file_system::FsError::Other(e.to_string()))
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, crate::file_system::FsError> {
        self.read_bytes(path)
            .await
            .map_err(|e| crate::file_system::FsError::Other(e.to_string()))
    }

    async fn try_read_file(
        &self,
        path: &Path,
    ) -> Result<Option<Vec<u8>>, crate::file_system::FsError> {
        match self.read_bytes(path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(WorkspaceError::HubError(message))
                if message.contains("No such file") || message.contains("not found") =>
            {
                Ok(None)
            }
            Err(error) => Err(crate::file_system::FsError::Other(error.to_string())),
        }
    }

    async fn write_file(
        &self,
        path: &Path,
        data: &[u8],
    ) -> Result<(), crate::file_system::FsError> {
        self.write_bytes(path, data)
            .await
            .map_err(|e| crate::file_system::FsError::Other(e.to_string()))
    }

    async fn delete_file(&self, path: &Path) -> Result<(), crate::file_system::FsError> {
        self.delete_bytes(path)
            .await
            .map_err(|e| crate::file_system::FsError::Other(e.to_string()))
    }
}

fn worker_call_timeout() -> std::time::Duration {
    std::env::var("ATELIER_WORKSPACE_WORKER_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(std::time::Duration::from_millis)
        .unwrap_or(DEFAULT_WORKER_CALL_TIMEOUT)
}

#[async_trait]
impl atelier_tools::computer::types::AsyncFileSystem for WorkspaceWorkerFs {
    async fn read_file(
        &self,
        path: &Path,
    ) -> Result<Vec<u8>, atelier_tools::computer::types::ComputerError> {
        self.read_bytes(path)
            .await
            .map_err(|e| atelier_tools::computer::types::ComputerError::io(e.to_string()))
    }

    async fn write_file(
        &self,
        path: &Path,
        data: &[u8],
    ) -> Result<(), atelier_tools::computer::types::ComputerError> {
        self.write_bytes(path, data)
            .await
            .map_err(|e| atelier_tools::computer::types::ComputerError::io(e.to_string()))
    }

    async fn delete_file(
        &self,
        path: &Path,
    ) -> Result<(), atelier_tools::computer::types::ComputerError> {
        self.delete_bytes(path)
            .await
            .map_err(|e| atelier_tools::computer::types::ComputerError::io(e.to_string()))
    }

    async fn grep(
        &self,
        request: atelier_tools::computer::types::GrepRequest,
    ) -> Result<
        Option<atelier_tools::types::output::GrepSearchOutput>,
        atelier_tools::computer::types::ComputerError,
    > {
        self.grep(request)
            .await
            .map_err(|error| atelier_tools::computer::types::ComputerError::io(error.to_string()))
    }
}

/// Run the worker protocol on stdin/stdout.
pub async fn run_worker(root: PathBuf) -> Result<(), WorkerProtocolError> {
    let root = canonical_root(&root)
        .await
        .map_err(WorkerProtocolError::Workspace)?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut writer = BufWriter::new(stdout);

    let hello = read_frame::<_, WorkerRequest>(&mut reader)
        .await?
        .ok_or_else(|| WorkerProtocolError::InvalidRequest("missing hello frame".into()))?;
    let (nonce, requested_root) = match hello {
        WorkerRequest::Hello {
            protocol_version,
            nonce,
            workspace_root,
        } => {
            if protocol_version != WORKER_PROTOCOL_VERSION {
                write_frame(
                    &mut writer,
                    &WorkerResponse::Error {
                        request_id: None,
                        code: "protocol_version_mismatch".into(),
                        message: format!(
                            "expected {}, got {protocol_version}",
                            WORKER_PROTOCOL_VERSION
                        ),
                    },
                )
                .await?;
                return Err(WorkerProtocolError::VersionMismatch {
                    expected: WORKER_PROTOCOL_VERSION,
                    actual: protocol_version,
                });
            }
            (nonce, workspace_root)
        }
        _ => {
            return Err(WorkerProtocolError::InvalidRequest(
                "first frame must be hello".into(),
            ));
        }
    };
    let requested_root = canonical_root(Path::new(&requested_root))
        .await
        .map_err(WorkerProtocolError::Workspace)?;
    if requested_root != root {
        return Err(WorkerProtocolError::InvalidRequest(
            "hello workspace root does not match worker root".into(),
        ));
    }

    let handle = crate::WorkspaceHandle::new_minimal_confined(
        root.clone(),
        crate::upload::environment::WorkspaceIdentity::default(),
        true,
    )
    .map_err(WorkerProtocolError::Workspace)?;
    handle
        .create_session("main")
        .map_err(WorkerProtocolError::Workspace)?;
    let handler = crate::hub_server::WorkspaceRpcHandler::new(handle);
    write_frame(
        &mut writer,
        &WorkerResponse::Ready {
            protocol_version: WORKER_PROTOCOL_VERSION,
            workspace_root: root.to_string_lossy().into_owned(),
        },
    )
    .await?;

    while let Some(request) = read_frame::<_, WorkerRequest>(&mut reader).await? {
        match request {
            WorkerRequest::Call {
                protocol_version,
                nonce: request_nonce,
                request_id,
                method,
                params,
                bound_session,
            } => {
                if let Err(error) = validate_call(protocol_version, &request_nonce, &nonce) {
                    write_frame(
                        &mut writer,
                        &WorkerResponse::Error {
                            request_id: Some(request_id),
                            code: "protocol_error".into(),
                            message: error.to_string(),
                        },
                    )
                    .await?;
                    continue;
                }
                if !is_worker_method(&method) {
                    write_frame(
                        &mut writer,
                        &WorkerResponse::Error {
                            request_id: Some(request_id),
                            code: "method_not_allowed".into(),
                            message: format!("method is outside worker namespace: {method}"),
                        },
                    )
                    .await?;
                    continue;
                }
                let result = dispatch_worker_method(&handler, &method, params, bound_session).await;
                let response = match result {
                    Ok(result) => WorkerResponse::Result { request_id, result },
                    Err(error) => WorkerResponse::Error {
                        request_id: Some(request_id),
                        code: crate::rpc_envelope::error_code(&error).into(),
                        message: error.to_string(),
                    },
                };
                write_frame(&mut writer, &response).await?;
            }
            WorkerRequest::Shutdown {
                protocol_version,
                nonce: request_nonce,
                request_id,
            } => {
                validate_call(protocol_version, &request_nonce, &nonce)?;
                write_frame(&mut writer, &WorkerResponse::Bye { request_id }).await?;
                break;
            }
            WorkerRequest::Hello { .. } => {
                write_frame(
                    &mut writer,
                    &WorkerResponse::Error {
                        request_id: None,
                        code: "invalid_request".into(),
                        message: "hello is only valid as the first frame".into(),
                    },
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn dispatch_worker_method(
    handler: &crate::hub_server::WorkspaceRpcHandler,
    method: &str,
    params: Value,
    bound_session: Option<String>,
) -> WorkspaceResult<Value> {
    if method.starts_with("atelier.worker.") {
        return handler.dispatch_worker_file_method(method, params).await;
    }
    handler
        .dispatch(method, params, bound_session.as_deref())
        .await
}

fn validate_call(
    protocol_version: u32,
    request_nonce: &str,
    expected_nonce: &str,
) -> Result<(), WorkerProtocolError> {
    if protocol_version != WORKER_PROTOCOL_VERSION {
        return Err(WorkerProtocolError::VersionMismatch {
            expected: WORKER_PROTOCOL_VERSION,
            actual: protocol_version,
        });
    }
    if request_nonce != expected_nonce {
        return Err(WorkerProtocolError::NonceMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[test]
    fn worker_method_namespace_is_closed() {
        assert!(is_worker_method("workspace.fs_read_file"));
        assert!(is_worker_method("atelier.worker.write_file"));
        assert!(!is_worker_method("provider.list"));
        assert!(!is_worker_method("x.ai/remote"));
    }

    #[test]
    fn request_round_trip_preserves_binary_safe_call_shape() {
        let request = WorkerRequest::Call {
            protocol_version: WORKER_PROTOCOL_VERSION,
            nonce: "nonce".into(),
            request_id: "7".into(),
            method: "atelier.worker.write_file".into(),
            params: serde_json::json!({"data_base64": "AP8="}),
            bound_session: Some("main".into()),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: WorkerRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, request);
    }

    #[tokio::test]
    async fn frame_helpers_round_trip() {
        let mut bytes = Vec::new();
        write_frame(
            &mut bytes,
            &WorkerResponse::Bye {
                request_id: "1".into(),
            },
        )
        .await
        .unwrap();
        let mut reader = BufReader::new(std::io::Cursor::new(bytes));
        let decoded: WorkerResponse = read_frame(&mut reader).await.unwrap().unwrap();
        assert_eq!(
            decoded,
            WorkerResponse::Bye {
                request_id: "1".into()
            }
        );
    }

    #[tokio::test]
    async fn frame_helpers_reject_oversized_frames() {
        let input = vec![b'x'; MAX_WORKER_FRAME_BYTES + 1];
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let error = read_frame::<_, WorkerRequest>(&mut reader)
            .await
            .unwrap_err();
        assert!(matches!(error, WorkerProtocolError::FrameTooLarge));
    }

    #[test]
    fn validate_call_rejects_version_and_nonce_mismatch() {
        assert!(matches!(
            validate_call(WORKER_PROTOCOL_VERSION + 1, "n", "n"),
            Err(WorkerProtocolError::VersionMismatch { .. })
        ));
        assert!(matches!(
            validate_call(WORKER_PROTOCOL_VERSION, "bad", "n"),
            Err(WorkerProtocolError::NonceMismatch)
        ));
    }

    #[test]
    fn worker_binary_discovery_finds_cargo_profile_sibling() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("debug");
        let deps = profile.join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        let test_exe = deps.join("workspace_test.exe");
        std::fs::write(&test_exe, b"test").unwrap();
        let worker = profile.join(if cfg!(windows) {
            "atelier-workspace-worker.exe"
        } else {
            "atelier-workspace-worker"
        });
        std::fs::write(&worker, b"worker").unwrap();

        assert_eq!(
            find_worker_binary(&test_exe, None).unwrap(),
            worker,
            "Cargo test executables must discover the sibling worker binary"
        );
    }

    #[test]
    fn worker_binary_discovery_rejects_missing_explicit_override() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-worker");
        let current_exe = temp.path().join("atelier.exe");
        let error = find_worker_binary(&current_exe, Some(missing.as_os_str())).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("workspace worker binary is unavailable")
        );
        assert!(error.to_string().contains(&missing.display().to_string()));
    }

    #[test]
    fn embedded_worker_invocation_has_a_hidden_internal_marker() {
        let args = worker_args_for_path(
            Path::new(r"C:\bin\atelier.exe"),
            Path::new(r"C:\bin\atelier.exe"),
            Path::new(r"C:\workspace"),
        );
        assert_eq!(args[0], "--internal-workspace-worker");
        assert_eq!(args[1], "--root");
    }

    #[test]
    fn external_worker_invocation_keeps_the_public_worker_arguments() {
        let args = worker_args_for_path(
            Path::new(r"C:\bin\atelier-workspace-worker.exe"),
            Path::new(r"C:\bin\atelier.exe"),
            Path::new(r"C:\workspace"),
        );
        assert_eq!(args[0], "--root");
        assert!(!args.iter().any(|arg| arg == "--internal-workspace-worker"));
    }

    #[test]
    fn worker_parser_requires_root() {
        assert!(parse_worker_args(std::iter::empty::<std::ffi::OsString>()).is_err());
        let root = parse_worker_args([
            std::ffi::OsString::from("--root"),
            std::ffi::OsString::from(r"C:\workspace"),
        ])
        .expect("worker root");
        assert_eq!(root, PathBuf::from(r"C:\workspace"));
    }

    #[tokio::test]
    async fn worker_grep_is_confined_and_uses_the_workspace_worker_path() {
        let handle = crate::handle::tests::make_confining_handle();
        let root = handle.root_cwd().unwrap();
        std::fs::write(root.join("search.txt"), "worker needle\n").unwrap();
        let handler = crate::hub_server::WorkspaceRpcHandler::new(handle);

        let result = handler
            .dispatch_worker_file_method(
                "atelier.worker.grep",
                serde_json::json!({
                    "path": root,
                    "pattern": "needle",
                    "output_mode": "content",
                    "case_insensitive": false,
                    "glob": null,
                    "file_type": null,
                    "before_context": null,
                    "after_context": null,
                    "context": null,
                    "multiline": false,
                    "deny_read_globs": [],
                    "head_limit": 20,
                    "max_output_bytes": 4096,
                    "max_chars_per_line": 1000,
                    "display_path": "/workspace"
                }),
            )
            .await
            .expect("worker grep should succeed");
        let output: atelier_tools::types::output::GrepSearchOutput =
            serde_json::from_value(result).unwrap();
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.match_count, 1);
        assert!(String::from_utf8_lossy(&output.stdout).contains("needle"));

        let outside = tempfile::tempdir().unwrap();
        let error = handler
            .dispatch_worker_file_method(
                "atelier.worker.grep",
                serde_json::json!({
                    "path": outside.path(),
                    "pattern": "needle",
                    "output_mode": "content",
                    "case_insensitive": false,
                    "glob": null,
                    "file_type": null,
                    "before_context": null,
                    "after_context": null,
                    "context": null,
                    "multiline": false,
                    "deny_read_globs": [],
                    "head_limit": 20,
                    "max_output_bytes": 4096,
                    "max_chars_per_line": 1000,
                    "display_path": "/workspace"
                }),
            )
            .await
            .expect_err("worker grep must reject paths outside the workspace");
        assert!(
            error.to_string().contains("path")
                || error.to_string().contains("workspace")
                || error.to_string().contains("root"),
            "unexpected confinement error: {error}"
        );
    }

    #[tokio::test]
    async fn binary_file_methods_are_confined_to_worker_root() {
        let handle = crate::handle::tests::make_confining_handle();
        let root = handle.root_cwd().unwrap();
        let handler = crate::hub_server::WorkspaceRpcHandler::new(handle);
        let payload = base64::engine::general_purpose::STANDARD.encode(b"worker-bytes");
        handler
            .dispatch_worker_file_method(
                "atelier.worker.write_file",
                serde_json::json!({
                    "path": root.join("worker.bin"),
                    "data_base64": payload.clone(),
                    "create_dirs": true,
                }),
            )
            .await
            .expect("in-root worker write");
        let read = handler
            .dispatch_worker_file_method(
                "atelier.worker.read_file",
                serde_json::json!({ "path": root.join("worker.bin") }),
            )
            .await
            .expect("in-root worker read");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(read["data_base64"].as_str().unwrap())
                .unwrap(),
            b"worker-bytes"
        );

        let outside = tempfile::tempdir().unwrap();
        let error = handler
            .dispatch_worker_file_method(
                "atelier.worker.write_file",
                serde_json::json!({
                    "path": outside.path().join("escape.bin"),
                    "data_base64": payload,
                    "create_dirs": true,
                }),
            )
            .await
            .expect_err("outside-root worker write must fail");
        assert!(error.to_string().contains("workspace root"));
        assert!(!outside.path().join("escape.bin").exists());
    }

    #[tokio::test]
    async fn worker_read_dir_and_stat_are_confined_operations() {
        let handle = crate::handle::tests::make_confining_handle();
        let root = handle.root_cwd().unwrap();
        std::fs::write(root.join("visible.txt"), "visible").unwrap();
        let handler = crate::hub_server::WorkspaceRpcHandler::new(handle);

        let listed = handler
            .dispatch_worker_file_method(
                "atelier.worker.read_dir",
                serde_json::json!({ "path": root }),
            )
            .await
            .expect("worker read_dir should succeed");
        let listed: atelier_workspace_types::rpc::fs::ClientFsListRes =
            serde_json::from_value(listed).unwrap();
        assert!(listed.nodes.iter().any(|node| node.name == "visible.txt"));

        let stat = handler
            .dispatch_worker_file_method(
                "atelier.worker.stat",
                serde_json::json!({ "path": root.join("visible.txt") }),
            )
            .await
            .expect("worker stat should succeed");
        let stat: atelier_workspace_types::rpc::fs::ClientFsStatRes =
            serde_json::from_value(stat).unwrap();
        assert!(stat.exists);

        let outside = tempfile::tempdir().unwrap();
        for method in ["atelier.worker.read_dir", "atelier.worker.stat"] {
            let error = handler
                .dispatch_worker_file_method(method, serde_json::json!({ "path": outside.path() }))
                .await
                .expect_err("worker filesystem operations must reject root escapes");
            assert!(
                error.to_string().contains("workspace")
                    || error.to_string().contains("root")
                    || error.to_string().contains("path"),
                "unexpected confinement error: {error}"
            );
        }
    }
}
