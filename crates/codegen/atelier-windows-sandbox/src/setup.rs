use crate::acl::grant_persistent_sids;
use crate::dpapi;
use crate::network::{
    self, NetworkPolicy, SANDBOX_NETWORK_ALLOWED_USERNAME, SANDBOX_NETWORK_DISABLED_USERNAME,
};
use crate::token::LocalSid;
use crate::winutil::{quote_windows_arg, to_wide};
use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
use windows_sys::Win32::NetworkManagement::NetManagement::{
    NERR_Success, NetUserAdd, NetUserDel, NetUserSetInfo, UF_DONT_EXPIRE_PASSWD, UF_SCRIPT,
    USER_INFO_1, USER_INFO_1003, USER_PRIV_USER,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};

pub const SETUP_VERSION: u32 = 2;

#[derive(Clone, Debug)]
pub struct SandboxCreds {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    NotSetup,
    Incomplete,
    Ready,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SetupStatus {
    pub state: SetupState,
    pub account_exists: bool,
    pub network_allowed_account_exists: bool,
    pub network_disabled_account_exists: bool,
    pub marker_valid: bool,
    pub credentials_valid: bool,
    pub wfp_filters_ready: bool,
    pub atelier_home: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct SetupMarker {
    version: u32,
    network_allowed_username: String,
    network_disabled_username: String,
    network_disabled_sid: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SandboxUserRecord {
    username: String,
    password: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SandboxUsersFile {
    version: u32,
    network_allowed: SandboxUserRecord,
    network_disabled: SandboxUserRecord,
}

#[derive(Debug, Serialize, Deserialize)]
struct SetupRequest {
    version: u32,
    atelier_home: PathBuf,
    action: ElevatedAction,
}

#[derive(Clone, Debug)]
struct SandboxCredentialSet {
    network_allowed: SandboxCreds,
    network_disabled: SandboxCreds,
}

impl SandboxCredentialSet {
    fn select(&self, policy: NetworkPolicy) -> SandboxCreds {
        match policy {
            NetworkPolicy::AllowAll => self.network_allowed.clone(),
            NetworkPolicy::Disabled => self.network_disabled.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ElevatedAction {
    Setup,
    Reset,
}

pub fn atelier_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("ATELIER_HOME") {
        return Ok(PathBuf::from(home));
    }
    crate::env::default_atelier_home()
        .ok_or_else(|| anyhow!("cannot resolve ATELIER_HOME or the current user profile"))
}

pub fn sandbox_bin_dir(home: &Path) -> PathBuf {
    home.join(".sandbox-bin")
}

fn sandbox_secrets_dir(home: &Path) -> PathBuf {
    home.join(".sandbox-secrets")
}

fn marker_path(home: &Path) -> PathBuf {
    home.join(".sandbox").join("setup-marker.json")
}

fn users_path(home: &Path) -> PathBuf {
    sandbox_secrets_dir(home).join("sandbox-users.json")
}

pub fn ensure_sandbox_creds(policy: NetworkPolicy) -> Result<SandboxCreds> {
    let home = atelier_home()?;
    ensure_sandbox_creds_at(&home, policy)
}

fn ensure_sandbox_creds_at(home: &Path, policy: NetworkPolicy) -> Result<SandboxCreds> {
    let status = inspect_status_at(&home);
    if status.state != SetupState::Ready {
        return Err(anyhow!(
            "Windows sandbox is not set up (state: {}). Run `ate sandbox setup`, approve the Windows UAC prompt once, then retry.",
            status.state.as_str()
        ));
    }
    let creds = load_credential_set(&home)?.ok_or_else(|| {
        anyhow!(
            "Windows sandbox setup is incomplete under {}. Run `ate sandbox reset --yes`, then `ate sandbox setup`.",
            home.display(),
        )
    })?;
    ensure_runner_acl(&home, &creds)?;
    Ok(creds.select(policy))
}

impl SetupState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSetup => "not_setup",
            Self::Incomplete => "incomplete",
            Self::Ready => "ready",
        }
    }
}

pub fn inspect_status() -> Result<SetupStatus> {
    Ok(inspect_status_at(&atelier_home()?))
}

/// Verify that the persistent Windows sandbox setup is ready and that the
/// complete persistent-account launch chain can start a restricted child.
///
/// This is intentionally stronger than checking setup artifacts or creating a
/// restricted token in the current user process. It exercises the same
/// `CreateProcessWithLogonW -> persistent runner -> CreateProcessAsUserW`
/// chain used for real sandboxed commands, with a read-only no-op child.
pub fn probe_ready_launch_chain() -> Result<()> {
    let home = atelier_home()?;
    probe_ready_launch_chain_for(&home, NetworkPolicy::Disabled)?;
    probe_ready_launch_chain_for(&home, NetworkPolicy::AllowAll)
}

fn probe_ready_launch_chain_for(home: &Path, network_policy: NetworkPolicy) -> Result<()> {
    ensure_sandbox_creds_at(home, network_policy)?;
    let root = sandbox_bin_dir(&home);
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("SystemRoot is not set; cannot locate cmd.exe for sandbox probe"))?;
    let cmd = system_root.join("System32").join("cmd.exe");
    if !cmd.is_file() {
        return Err(anyhow!(
            "Windows sandbox launch probe cannot find {}",
            cmd.display()
        ));
    }

    let mut request = crate::CommandRequest::new(
        crate::SandboxMode::ReadOnly,
        vec![root.clone()],
        root,
        cmd,
        vec!["/D".into(), "/S".into(), "/C".into(), "exit 0".into()],
    );
    request.network_policy = network_policy;
    request.atelier_home = Some(home.to_path_buf());
    request.timeout = Some(Duration::from_secs(10));
    let output = crate::runner::run_command(request)
        .map_err(|error| anyhow!("persistent sandbox launch chain failed: {error}"))?;
    if output.timed_out {
        return Err(anyhow!(
            "persistent sandbox launch chain timed out after 10 seconds"
        ));
    }
    if output.exit_code != 0 {
        return Err(anyhow!(
            "persistent sandbox launch chain exited with code {}: {}",
            output.exit_code,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn inspect_status_at(home: &Path) -> SetupStatus {
    let network_allowed = LocalSid::from_account(SANDBOX_NETWORK_ALLOWED_USERNAME).ok();
    let network_disabled = LocalSid::from_account(SANDBOX_NETWORK_DISABLED_USERNAME).ok();
    let network_disabled_sid = network_disabled
        .as_ref()
        .and_then(|sid| sid.to_string().ok());
    let wfp_filters_ready = network_disabled_sid.as_deref().is_some_and(|sid| {
        network::disabled_network_filters_installed_for_sid(sid).unwrap_or(false)
    });
    inspect_artifacts(
        home,
        network_allowed.is_some(),
        network_disabled.is_some(),
        network_disabled_sid.as_deref(),
        wfp_filters_ready,
    )
}

fn inspect_artifacts(
    home: &Path,
    network_allowed_account_exists: bool,
    network_disabled_account_exists: bool,
    network_disabled_sid: Option<&str>,
    wfp_filters_ready: bool,
) -> SetupStatus {
    let marker_valid = std::fs::read(marker_path(home))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SetupMarker>(&bytes).ok())
        .is_some_and(|marker| {
            marker.version == SETUP_VERSION
                && marker.network_allowed_username == SANDBOX_NETWORK_ALLOWED_USERNAME
                && marker.network_disabled_username == SANDBOX_NETWORK_DISABLED_USERNAME
                && network_disabled_sid == Some(marker.network_disabled_sid.as_str())
        });
    let credentials_valid = load_credential_set(home).ok().flatten().is_some();
    let account_exists = network_allowed_account_exists && network_disabled_account_exists;
    let any_artifact = marker_path(home).exists() || users_path(home).exists() || wfp_filters_ready;
    let state = if account_exists && marker_valid && credentials_valid && wfp_filters_ready {
        SetupState::Ready
    } else if !account_exists && !any_artifact {
        SetupState::NotSetup
    } else {
        SetupState::Incomplete
    };
    SetupStatus {
        state,
        account_exists,
        network_allowed_account_exists,
        network_disabled_account_exists,
        marker_valid,
        credentials_valid,
        wfp_filters_ready,
        atelier_home: home.to_path_buf(),
    }
}

#[cfg(test)]
fn inspect_artifacts_for_test(
    home: &Path,
    network_allowed_account_exists: bool,
    network_disabled_account_exists: bool,
    network_disabled_sid: Option<&str>,
    wfp_filters_ready: bool,
) -> SetupStatus {
    inspect_artifacts(
        home,
        network_allowed_account_exists,
        network_disabled_account_exists,
        network_disabled_sid,
        wfp_filters_ready,
    )
}

pub fn setup_public(setup_executable: &Path) -> Result<bool> {
    let home = atelier_home()?;
    if inspect_status_at(&home).state == SetupState::Ready {
        return Ok(false);
    }
    std::fs::create_dir_all(home.join(".sandbox"))
        .with_context(|| format!("create Atelier sandbox directory under {}", home.display()))?;
    run_elevated_action(setup_executable, &home, ElevatedAction::Setup)?;
    if inspect_status_at(&home).state != SetupState::Ready {
        return Err(anyhow!(
            "Windows sandbox setup finished but status is not ready. Run `ate sandbox status` for details."
        ));
    }
    Ok(true)
}

pub fn reset_public(setup_executable: &Path) -> Result<bool> {
    let home = atelier_home()?;
    if inspect_status_at(&home).state == SetupState::NotSetup {
        return Ok(false);
    }
    run_elevated_action(setup_executable, &home, ElevatedAction::Reset)?;
    if inspect_status_at(&home).state != SetupState::NotSetup {
        return Err(anyhow!(
            "Windows sandbox reset finished but AtelierSandbox artifacts remain. Run `ate sandbox status` for details."
        ));
    }
    Ok(true)
}

fn load_credential_set(home: &Path) -> Result<Option<SandboxCredentialSet>> {
    let marker = match std::fs::read(marker_path(home)) {
        Ok(bytes) => serde_json::from_slice::<SetupMarker>(&bytes).ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let Some(marker) = marker.filter(|marker| {
        marker.version == SETUP_VERSION
            && marker.network_allowed_username == SANDBOX_NETWORK_ALLOWED_USERNAME
            && marker.network_disabled_username == SANDBOX_NETWORK_DISABLED_USERNAME
    }) else {
        return Ok(None);
    };
    let users = match std::fs::read(users_path(home)) {
        Ok(bytes) => serde_json::from_slice::<SandboxUsersFile>(&bytes).ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let Some(users) = users.filter(|users| {
        users.version == SETUP_VERSION
            && users.network_allowed.username == marker.network_allowed_username
            && users.network_disabled.username == marker.network_disabled_username
    }) else {
        return Ok(None);
    };
    Ok(Some(SandboxCredentialSet {
        network_allowed: decode_user_record(users.network_allowed)?,
        network_disabled: decode_user_record(users.network_disabled)?,
    }))
}

fn decode_user_record(record: SandboxUserRecord) -> Result<SandboxCreds> {
    let encrypted = BASE64
        .decode(record.password.as_bytes())
        .context("decode sandbox password")?;
    let password = String::from_utf8(dpapi::unprotect(&encrypted)?)
        .context("sandbox password is not UTF-8")?;
    Ok(SandboxCreds {
        username: record.username,
        password,
    })
}

fn ensure_runner_acl(home: &Path, creds: &SandboxCredentialSet) -> Result<()> {
    let bin = sandbox_bin_dir(home);
    std::fs::create_dir_all(&bin)
        .with_context(|| format!("create sandbox runner directory {}", bin.display()))?;
    let network_allowed = LocalSid::from_account(&creds.network_allowed.username)?;
    let network_disabled = LocalSid::from_account(&creds.network_disabled.username)?;
    grant_persistent_sids(
        &bin,
        &[network_allowed.as_ptr(), network_disabled.as_ptr()],
        FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
    )
}

fn run_elevated_action(setup_executable: &Path, home: &Path, action: ElevatedAction) -> Result<()> {
    let request = SetupRequest {
        version: SETUP_VERSION,
        atelier_home: home.to_path_buf(),
        action,
    };
    let payload = BASE64.encode(serde_json::to_vec(&request)?);
    let parameters = format!(
        "--internal-windows-sandbox-setup {}",
        quote_windows_arg(&payload)
    );
    let exe = to_wide(setup_executable.as_os_str());
    let params = to_wide(parameters);
    let verb = to_wide("runas");
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = exe.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = 0;
    if unsafe { ShellExecuteExW(&mut info) } == 0 || info.hProcess.is_null() {
        return Err(anyhow!(
            "ShellExecuteExW failed to launch Atelier Windows sandbox helper: {}",
            unsafe { GetLastError() }
        ));
    }
    unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
    let mut exit_code = 1u32;
    unsafe {
        GetExitCodeProcess(info.hProcess, &mut exit_code);
        CloseHandle(info.hProcess);
    }
    if exit_code != 0 {
        let detail = std::fs::read_to_string(home.join(".sandbox").join("setup-error.txt"))
            .unwrap_or_else(|_| "no setup error report was written".to_owned());
        return Err(anyhow!(
            "Atelier Windows sandbox helper exited with status {exit_code}: {detail}"
        ));
    }
    Ok(())
}

pub fn run_setup_helper<I, T>(args: I) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    let payload = args
        .next()
        .ok_or_else(|| anyhow!("missing Windows sandbox setup payload"))?;
    if args.next().is_some() {
        return Err(anyhow!("unexpected Windows sandbox setup arguments"));
    }
    let request: SetupRequest = serde_json::from_slice(
        &BASE64
            .decode(payload.to_string_lossy().as_bytes())
            .context("decode Windows sandbox setup payload")?,
    )?;
    if request.version != SETUP_VERSION {
        return Err(anyhow!("unsupported Windows sandbox setup request"));
    }
    let result = match request.action {
        ElevatedAction::Setup => provision(&request),
        ElevatedAction::Reset => reset(&request),
    };
    if let Err(error) = result {
        let sandbox = request.atelier_home.join(".sandbox");
        let _ = std::fs::create_dir_all(&sandbox);
        let _ = std::fs::write(sandbox.join("setup-error.txt"), format!("{error:#}"));
        return Err(error);
    }
    let _ = std::fs::remove_file(
        request
            .atelier_home
            .join(".sandbox")
            .join("setup-error.txt"),
    );
    Ok(0)
}

fn provision(request: &SetupRequest) -> Result<()> {
    let network_allowed_password = random_password()?;
    let network_disabled_password = random_password()?;
    if let Err(error) = provision_inner(
        request,
        &network_allowed_password,
        &network_disabled_password,
    ) {
        let cleanup_errors = rollback_provision(request);
        return Err(match cleanup_errors {
            Ok(()) => error,
            Err(cleanup) => anyhow!("{error:#}; rollback also failed: {cleanup:#}"),
        });
    }
    Ok(())
}

fn provision_inner(
    request: &SetupRequest,
    network_allowed_password: &str,
    network_disabled_password: &str,
) -> Result<()> {
    ensure_local_user(SANDBOX_NETWORK_ALLOWED_USERNAME, network_allowed_password)?;
    ensure_local_user(SANDBOX_NETWORK_DISABLED_USERNAME, network_disabled_password)?;
    let home = &request.atelier_home;
    let sandbox = home.join(".sandbox");
    let secrets = sandbox_secrets_dir(home);
    let bin = sandbox_bin_dir(home);
    std::fs::create_dir_all(&sandbox)?;
    std::fs::create_dir_all(&secrets)?;
    std::fs::create_dir_all(&bin)?;

    let network_allowed_protected = dpapi::protect(network_allowed_password.as_bytes())?;
    let network_disabled_protected = dpapi::protect(network_disabled_password.as_bytes())?;
    let users = SandboxUsersFile {
        version: SETUP_VERSION,
        network_allowed: SandboxUserRecord {
            username: SANDBOX_NETWORK_ALLOWED_USERNAME.to_owned(),
            password: BASE64.encode(network_allowed_protected),
        },
        network_disabled: SandboxUserRecord {
            username: SANDBOX_NETWORK_DISABLED_USERNAME.to_owned(),
            password: BASE64.encode(network_disabled_protected),
        },
    };
    let network_allowed_sid = LocalSid::from_account(SANDBOX_NETWORK_ALLOWED_USERNAME)?;
    let network_disabled_sid = LocalSid::from_account(SANDBOX_NETWORK_DISABLED_USERNAME)?;
    let network_disabled_sid_string = network_disabled_sid.to_string()?;
    let installed =
        network::install_disabled_network_filters_for_account(SANDBOX_NETWORK_DISABLED_USERNAME)?;
    if installed == 0
        || !network::disabled_network_filters_installed_for_sid(&network_disabled_sid_string)?
    {
        return Err(anyhow!(
            "Atelier WFP setup did not produce a verifiable network block"
        ));
    }
    grant_persistent_sids(
        &bin,
        &[network_allowed_sid.as_ptr(), network_disabled_sid.as_ptr()],
        FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
    )?;
    std::fs::write(users_path(home), serde_json::to_vec_pretty(&users)?)?;
    let marker = SetupMarker {
        version: SETUP_VERSION,
        network_allowed_username: SANDBOX_NETWORK_ALLOWED_USERNAME.to_owned(),
        network_disabled_username: SANDBOX_NETWORK_DISABLED_USERNAME.to_owned(),
        network_disabled_sid: network_disabled_sid_string,
    };
    std::fs::write(marker_path(home), serde_json::to_vec_pretty(&marker)?)?;
    Ok(())
}

fn reset(request: &SetupRequest) -> Result<()> {
    network::remove_disabled_network_filters()
        .context("remove Atelier Windows sandbox WFP filters")?;
    delete_local_user(SANDBOX_NETWORK_DISABLED_USERNAME)?;
    delete_local_user(SANDBOX_NETWORK_ALLOWED_USERNAME)?;
    remove_setup_files(&request.atelier_home)
}

fn rollback_provision(request: &SetupRequest) -> Result<()> {
    let mut errors = Vec::new();
    if let Err(error) = network::remove_disabled_network_filters() {
        errors.push(error.context("remove WFP filters"));
    }
    for username in [
        SANDBOX_NETWORK_DISABLED_USERNAME,
        SANDBOX_NETWORK_ALLOWED_USERNAME,
    ] {
        if let Err(error) = delete_local_user(username) {
            errors.push(error);
        }
    }
    if let Err(error) = remove_setup_files(&request.atelier_home) {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "{}",
            errors
                .into_iter()
                .map(|error| format!("{error:#}"))
                .collect::<Vec<_>>()
                .join("; ")
        ))
    }
}

fn delete_local_user(username: &str) -> Result<()> {
    const NERR_USER_NOT_FOUND: u32 = 2221;
    let username_w = to_wide(OsStr::new(username));
    let status = unsafe { NetUserDel(std::ptr::null(), username_w.as_ptr()) };
    if status != NERR_Success && status != NERR_USER_NOT_FOUND {
        return Err(anyhow!(
            "failed to delete local sandbox user {username}: NetUserDel={status}"
        ));
    }
    Ok(())
}

fn remove_setup_files(home: &Path) -> Result<()> {
    for path in [
        users_path(home),
        marker_path(home),
        home.join(".sandbox").join("setup-error.txt"),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("remove {}", path.display()));
            }
        }
    }
    Ok(())
}

fn random_password() -> Result<String> {
    const ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()-_=+";
    let mut random = [0u8; 32];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            random.as_mut_ptr(),
            random.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        return Err(anyhow!("BCryptGenRandom failed: {status}"));
    }
    Ok(random
        .iter()
        .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
        .collect())
}

fn ensure_local_user(username: &str, password: &str) -> Result<()> {
    let username = to_wide(OsStr::new(username));
    let password = to_wide(OsStr::new(password));
    let info = USER_INFO_1 {
        usri1_name: username.as_ptr().cast_mut(),
        usri1_password: password.as_ptr().cast_mut(),
        usri1_password_age: 0,
        usri1_priv: USER_PRIV_USER,
        usri1_home_dir: std::ptr::null_mut(),
        usri1_comment: std::ptr::null_mut(),
        usri1_flags: UF_SCRIPT | UF_DONT_EXPIRE_PASSWD,
        usri1_script_path: std::ptr::null_mut(),
    };
    let status = unsafe {
        NetUserAdd(
            std::ptr::null(),
            1,
            (&info as *const USER_INFO_1).cast_mut().cast(),
            std::ptr::null_mut(),
        )
    };
    if status == NERR_Success {
        return Ok(());
    }
    let update = USER_INFO_1003 {
        usri1003_password: password.as_ptr().cast_mut(),
    };
    let update_status = unsafe {
        NetUserSetInfo(
            std::ptr::null(),
            username.as_ptr(),
            1003,
            (&update as *const USER_INFO_1003).cast_mut().cast(),
            std::ptr::null_mut(),
        )
    };
    if update_status != NERR_Success {
        return Err(anyhow!(
            "failed to create or update local sandbox user: NetUserAdd={status}, NetUserSetInfo={update_status}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        SETUP_VERSION, SandboxCredentialSet, SandboxCreds, SandboxUserRecord, SandboxUsersFile,
        SetupMarker, SetupState, inspect_artifacts_for_test,
    };
    use crate::network::{
        NetworkPolicy, SANDBOX_NETWORK_ALLOWED_USERNAME, SANDBOX_NETWORK_DISABLED_USERNAME,
    };
    use base64::Engine as _;

    const OFFLINE_SID: &str = "S-1-5-21-1-2-3-1002";

    #[test]
    fn setup_marker_is_versioned_and_bound_to_both_fixed_accounts_and_offline_sid() {
        let marker = SetupMarker {
            version: SETUP_VERSION,
            network_allowed_username: SANDBOX_NETWORK_ALLOWED_USERNAME.to_owned(),
            network_disabled_username: SANDBOX_NETWORK_DISABLED_USERNAME.to_owned(),
            network_disabled_sid: OFFLINE_SID.to_owned(),
        };
        let json = serde_json::to_value(marker).unwrap();
        assert_eq!(json["version"], SETUP_VERSION);
        assert_eq!(
            json["network_allowed_username"],
            SANDBOX_NETWORK_ALLOWED_USERNAME
        );
        assert_eq!(
            json["network_disabled_username"],
            SANDBOX_NETWORK_DISABLED_USERNAME
        );
        assert_eq!(json["network_disabled_sid"], OFFLINE_SID);
    }

    #[test]
    fn setup_status_distinguishes_missing_incomplete_and_ready_artifacts() {
        let home = tempfile::tempdir().unwrap();
        let missing = inspect_artifacts_for_test(home.path(), false, false, None, false);
        assert_eq!(missing.state, SetupState::NotSetup);
        assert!(!missing.account_exists);

        std::fs::create_dir_all(home.path().join(".sandbox")).unwrap();
        std::fs::write(
            home.path().join(".sandbox").join("setup-marker.json"),
            serde_json::to_vec(&SetupMarker {
                version: SETUP_VERSION,
                network_allowed_username: SANDBOX_NETWORK_ALLOWED_USERNAME.to_owned(),
                network_disabled_username: SANDBOX_NETWORK_DISABLED_USERNAME.to_owned(),
                network_disabled_sid: OFFLINE_SID.to_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        let incomplete =
            inspect_artifacts_for_test(home.path(), true, true, Some(OFFLINE_SID), false);
        assert_eq!(incomplete.state, SetupState::Incomplete);
        assert!(incomplete.marker_valid);
        assert!(!incomplete.credentials_valid);

        let secrets = home.path().join(".sandbox-secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        let protected = crate::dpapi::protect(b"test-password").unwrap();
        std::fs::write(
            secrets.join("sandbox-users.json"),
            serde_json::to_vec(&SandboxUsersFile {
                version: SETUP_VERSION,
                network_allowed: SandboxUserRecord {
                    username: SANDBOX_NETWORK_ALLOWED_USERNAME.to_owned(),
                    password: base64::engine::general_purpose::STANDARD.encode(&protected),
                },
                network_disabled: SandboxUserRecord {
                    username: SANDBOX_NETWORK_DISABLED_USERNAME.to_owned(),
                    password: base64::engine::general_purpose::STANDARD.encode(protected),
                },
            })
            .unwrap(),
        )
        .unwrap();
        let ready = inspect_artifacts_for_test(home.path(), true, true, Some(OFFLINE_SID), true);
        assert_eq!(ready.state, SetupState::Ready);
        assert!(ready.credentials_valid);
        assert!(ready.wfp_filters_ready);
    }

    #[test]
    fn disabled_network_identity_is_not_ready_when_wfp_filters_are_missing() {
        let home = tempfile::tempdir().unwrap();
        let status = inspect_artifacts_for_test(home.path(), true, true, Some(OFFLINE_SID), false);
        assert_ne!(status.state, SetupState::Ready);
        assert!(!status.wfp_filters_ready);
    }

    #[test]
    fn credential_selection_follows_network_policy() {
        let credentials = SandboxCredentialSet {
            network_allowed: SandboxCreds {
                username: SANDBOX_NETWORK_ALLOWED_USERNAME.to_owned(),
                password: "allowed".to_owned(),
            },
            network_disabled: SandboxCreds {
                username: SANDBOX_NETWORK_DISABLED_USERNAME.to_owned(),
                password: "disabled".to_owned(),
            },
        };
        assert_eq!(
            credentials.select(NetworkPolicy::AllowAll).username,
            SANDBOX_NETWORK_ALLOWED_USERNAME
        );
        assert_eq!(
            credentials.select(NetworkPolicy::Disabled).username,
            SANDBOX_NETWORK_DISABLED_USERNAME
        );
    }

    #[test]
    fn missing_setup_error_points_to_the_public_setup_command() {
        let home = tempfile::tempdir().unwrap();
        let error =
            super::ensure_sandbox_creds_at(home.path(), NetworkPolicy::Disabled).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("ate sandbox setup"), "{message}");
        assert!(message.contains("UAC"), "{message}");
    }
}
