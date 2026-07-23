use crate::SandboxError;
use crate::acl::{ScopedAclGrant, access_mask_for_mode, grant_restricted_sids};
use crate::env::make_environment_block;
use crate::path_normalization::{
    ensure_no_reparse_points, normalize_existing_path, path_is_within,
};
use crate::process::{RestrictedPipedProcess, run_as_user, spawn_as_user_piped};
use crate::token::{RestrictedToken, create_restricted_token, new_capability_sid};
use anyhow::Result;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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

pub struct SandboxSession {
    capability: crate::token::LocalSid,
    token: RestrictedToken,
    grants: Vec<ScopedAclGrant>,
    roots: Vec<(String, crate::SandboxMode)>,
}

impl SandboxSession {
    pub fn new() -> Result<Self, SandboxError> {
        let capability = new_capability_sid().map_err(SandboxError::Operation)?;
        let token = create_restricted_token(&capability).map_err(SandboxError::Operation)?;
        Ok(Self {
            capability,
            token,
            grants: Vec::new(),
            roots: Vec::new(),
        })
    }

    pub fn run(
        &mut self,
        request: crate::CommandRequest,
    ) -> Result<crate::RunOutput, SandboxError> {
        let validated = validate_request(&request)?;
        let restricting_sids = self.token.restricting_sids(self.capability.as_ptr());
        let access_mask = access_mask_for_mode(request.mode);
        for root in &validated.roots {
            let key = crate::canonical_path_key(root);
            let existing_mode = self
                .roots
                .iter()
                .find(|(existing_key, _)| existing_key == &key)
                .map(|(_, mode)| *mode);
            let needs_grant = match existing_mode {
                None => true,
                Some(crate::SandboxMode::ReadOnly) => {
                    request.mode == crate::SandboxMode::WorkspaceWrite
                }
                Some(crate::SandboxMode::WorkspaceWrite) => false,
            };
            if needs_grant {
                let grant = grant_restricted_sids(root, &restricting_sids, access_mask)
                    .map_err(SandboxError::Operation)?;
                self.grants.push(grant);
                match existing_mode {
                    Some(_) => {
                        if let Some((_, mode)) = self
                            .roots
                            .iter_mut()
                            .find(|(existing_key, _)| existing_key == &key)
                        {
                            *mode = request.mode;
                        }
                    }
                    None => self.roots.push((key, request.mode)),
                }
            }
        }

        let environment = make_environment_block(&request.env, request.atelier_home.as_deref());
        run_as_user(
            self.token.raw(),
            &validated.program,
            &request.args,
            &validated.cwd,
            &environment,
            request.timeout,
        )
        .map_err(SandboxError::Operation)
    }

    pub fn spawn_piped(
        mut self,
        request: crate::CommandRequest,
    ) -> Result<SandboxedPipedChild, SandboxError> {
        let validated = validate_request(&request)?;
        let restricting_sids = self.token.restricting_sids(self.capability.as_ptr());
        let access_mask = access_mask_for_mode(request.mode);
        for root in &validated.roots {
            let grant = grant_restricted_sids(root, &restricting_sids, access_mask)
                .map_err(SandboxError::Operation)?;
            self.grants.push(grant);
        }
        let environment = make_environment_block(&request.env, request.atelier_home.as_deref());
        let child = spawn_as_user_piped(
            self.token.raw(),
            &validated.program,
            &request.args,
            &validated.cwd,
            &environment,
        )
        .map_err(SandboxError::Operation)?;
        Ok(SandboxedPipedChild {
            child,
            _session: self,
        })
    }
}

pub struct SandboxedPipedChild {
    child: RestrictedPipedProcess,
    _session: SandboxSession,
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

impl Drop for SandboxSession {
    fn drop(&mut self) {
        let grants = std::mem::take(&mut self.grants);
        let _ = restore_grants(grants);
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

fn restore_grants(grants: Vec<ScopedAclGrant>) -> Result<()> {
    let mut first_error = None;
    for grant in grants.into_iter().rev() {
        if let Err(err) = grant.restore() {
            first_error.get_or_insert(err);
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::validate_request;
    use crate::{CommandRequest, SandboxMode};
    use std::path::PathBuf;

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
