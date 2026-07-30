use crate::winutil::to_wide;
use crate::winutil::win_error;
use anyhow::Result;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_SUCCESS, GENERIC_ALL, GetLastError, HANDLE, HLOCAL,
    LUID, LocalFree,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, AdjustTokenPrivileges, CopySid, CreateRestrictedToken, DISABLE_MAX_PRIVILEGE,
    GetLengthSid, GetTokenInformation, LUA_TOKEN, LookupPrivilegeValueW, SID_AND_ATTRIBUTES,
    SetTokenInformation, TOKEN_ACCESS_MASK, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_PRIVILEGES, TOKEN_QUERY,
    TOKEN_USER, TokenDefaultDacl, TokenGroups, TokenUser,
};
use windows_sys::Win32::Security::{LookupAccountNameW, SID_NAME_USE};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcessToken,
};

const WRITE_RESTRICTED: u32 = 0x08;
const DEFAULT_DACL_ACCESS_MASK: u32 = GENERIC_ALL;
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
static PROCESS_CAPABILITY_SID: OnceLock<Result<String, String>> = OnceLock::new();
static CAPABILITY_NONCE: AtomicU64 = AtomicU64::new(1);

#[repr(C)]
struct TokenDefaultDaclInfo {
    default_dacl: *mut ACL,
}

pub struct OwnedHandle(HANDLE);

impl OwnedHandle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub struct RestrictedToken {
    handle: OwnedHandle,
}

impl RestrictedToken {
    pub fn raw(&self) -> HANDLE {
        self.handle.raw()
    }
}

pub struct LocalSid {
    sid: *mut c_void,
}

fn restricted_token_flag_attempts() -> [u32; 2] {
    [
        DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED,
        DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED,
    ]
}

fn sandbox_user_restricted_token_flags() -> u32 {
    DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED
}

fn standard_policy_sids(capability: *mut c_void, logon: *mut c_void) -> [*mut c_void; 2] {
    [capability, logon]
}

fn sandbox_user_policy_sids(capability: *mut c_void, logon: *mut c_void) -> [*mut c_void; 2] {
    [capability, logon]
}

fn sandbox_user_restriction_sids(
    workspace: *mut c_void,
    ancestor_traversal: *mut c_void,
    logon: *mut c_void,
    users: *mut c_void,
    world: *mut c_void,
) -> [*mut c_void; 5] {
    [workspace, ancestor_traversal, logon, users, world]
}

fn sandbox_user_default_dacl_sids(
    workspace: *mut c_void,
    user: *mut c_void,
    logon: *mut c_void,
    world: *mut c_void,
) -> [*mut c_void; 4] {
    [workspace, user, logon, world]
}

fn restricting_entries<const N: usize>(sids: [*mut c_void; N]) -> [SID_AND_ATTRIBUTES; N] {
    sids.map(|sid| SID_AND_ATTRIBUTES {
        Sid: sid,
        Attributes: 0,
    })
}

impl LocalSid {
    pub fn new(value: &str) -> Result<Self> {
        let mut sid = ptr::null_mut();
        let wide = to_wide(value);
        let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) };
        if ok == 0 || sid.is_null() {
            return Err(win_error("ConvertStringSidToSidW"));
        }
        Ok(Self { sid })
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.sid
    }

    pub fn from_account(name: &str) -> Result<Self> {
        let name = to_wide(name);
        let mut sid_len = 0u32;
        let mut domain_len = 0u32;
        let mut use_type: SID_NAME_USE = 0;
        unsafe {
            let _ = LookupAccountNameW(
                ptr::null(),
                name.as_ptr(),
                ptr::null_mut(),
                &mut sid_len,
                ptr::null_mut(),
                &mut domain_len,
                &mut use_type,
            );
        }
        if sid_len == 0 {
            return Err(win_error("LookupAccountNameW size query"));
        }
        let mut sid = vec![0u8; sid_len as usize];
        let mut domain = vec![0u16; domain_len as usize];
        let ok = unsafe {
            LookupAccountNameW(
                ptr::null(),
                name.as_ptr(),
                sid.as_mut_ptr().cast(),
                &mut sid_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                &mut use_type,
            )
        };
        if ok == 0 {
            return Err(win_error("LookupAccountNameW"));
        }
        let length = unsafe { GetLengthSid(sid.as_mut_ptr().cast()) };
        let mut owned = ptr::null_mut();
        let text = unsafe {
            let mut value = ptr::null_mut();
            if ConvertSidToStringSidW(sid.as_mut_ptr().cast(), &mut value) == 0 {
                return Err(win_error("ConvertSidToStringSidW"));
            }
            let mut len = 0usize;
            while *value.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(value, len));
            LocalFree(value as HLOCAL);
            text
        };
        let wide = to_wide(text);
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut owned) } == 0 || owned.is_null() {
            return Err(win_error("ConvertStringSidToSidW"));
        }
        debug_assert_eq!(length, unsafe { GetLengthSid(owned) });
        Ok(Self { sid: owned })
    }

    pub fn to_string(&self) -> Result<String> {
        let mut value = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(self.sid, &mut value) } == 0 || value.is_null() {
            return Err(win_error("ConvertSidToStringSidW"));
        }
        let mut len = 0usize;
        unsafe {
            while *value.add(len) != 0 {
                len += 1;
            }
        }
        let text = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(value, len)) };
        unsafe { LocalFree(value as HLOCAL) };
        Ok(text)
    }
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        if !self.sid.is_null() {
            unsafe { LocalFree(self.sid as HLOCAL) };
        }
    }
}

fn generate_capability_sid_text() -> Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| anyhow::anyhow!("system clock before Unix epoch: {err}"))?
        .as_nanos();
    let pid = u128::from(unsafe { GetCurrentProcessId() });
    let nonce = u128::from(CAPABILITY_NONCE.fetch_add(1, Ordering::Relaxed));
    let mixed = nanos ^ pid.rotate_left(29) ^ nonce.rotate_left(61);
    let a = mixed as u32;
    let b = (mixed >> 32) as u32;
    let c = (mixed >> 64) as u32;
    let d = (mixed >> 96) as u32;
    Ok(format!("S-1-5-21-{a}-{b}-{c}-{d}"))
}

pub fn new_capability_sid() -> Result<LocalSid> {
    let value = PROCESS_CAPABILITY_SID
        .get_or_init(|| generate_capability_sid_text().map_err(|error| error.to_string()));
    let value = value.as_ref().map_err(|error| anyhow::anyhow!("{error}"))?;
    LocalSid::new(value)
}

fn capability_path(home: &Path, roots: &[PathBuf], mode: crate::SandboxMode) -> PathBuf {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(match mode {
        crate::SandboxMode::ReadOnly => b"read-only".as_slice(),
        crate::SandboxMode::WorkspaceWrite => b"workspace-write".as_slice(),
    });
    for root in roots {
        digest.update([0]);
        digest.update(crate::canonical_path_key(root).as_bytes());
    }
    let hash = digest.finalize();
    let mut name = String::with_capacity(hash.len() * 2 + 4);
    for byte in hash {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").expect("write capability hash");
    }
    name.push_str(".sid");
    home.join(".sandbox").join("capabilities").join(name)
}

fn load_or_create_capability_sid(path: &Path, label: &str) -> Result<LocalSid> {
    if let Ok(value) = std::fs::read_to_string(path) {
        return LocalSid::new(value.trim());
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{label} SID path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let value = generate_capability_sid_text()?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(value.as_bytes())?;
            file.sync_all()?;
            LocalSid::new(&value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            LocalSid::new(std::fs::read_to_string(path)?.trim())
        }
        Err(error) => Err(error.into()),
    }
}

/// Load the stable restricted-token capability used only for non-inheritable
/// read/traverse grants on workspace ancestor directories.
pub fn ancestor_traversal_sid(home: &Path) -> Result<LocalSid> {
    load_or_create_capability_sid(
        &home
            .join(".sandbox")
            .join("capabilities")
            .join("ancestor-traversal.sid"),
        "ancestor traversal",
    )
}

/// Load the stable capability identity for one exact root set and access mode.
/// Persisting this identity lets an already-propagated workspace ACL be reused
/// across Atelier launches instead of recursively rewriting the repository on
/// every Session startup.
pub fn workspace_capability_sid(
    home: &Path,
    roots: &[PathBuf],
    mode: crate::SandboxMode,
) -> Result<LocalSid> {
    if roots.is_empty() {
        return Err(anyhow::anyhow!(
            "workspace capability requires at least one root"
        ));
    }
    load_or_create_capability_sid(&capability_path(home, roots, mode), "workspace capability")
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn set_default_dacl(token: HANDLE, sids: &[*mut c_void]) -> Result<()> {
    if sids.is_empty() {
        return Ok(());
    }

    let entries: Vec<EXPLICIT_ACCESS_W> = sids
        .iter()
        .map(|sid| EXPLICIT_ACCESS_W {
            grfAccessPermissions: DEFAULT_DACL_ACCESS_MASK,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: 0,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: (*sid).cast(),
            },
        })
        .collect();

    let mut new_dacl = ptr::null_mut();
    let result = SetEntriesInAclW(
        entries.len() as u32,
        entries.as_ptr(),
        ptr::null_mut(),
        &mut new_dacl,
    );
    if result != ERROR_SUCCESS {
        return Err(anyhow::anyhow!("SetEntriesInAclW failed: {result}"));
    }

    let mut info = TokenDefaultDaclInfo {
        default_dacl: new_dacl,
    };
    let ok = SetTokenInformation(
        token,
        TokenDefaultDacl,
        (&mut info as *mut TokenDefaultDaclInfo).cast(),
        std::mem::size_of::<TokenDefaultDaclInfo>() as u32,
    );
    if ok == 0 {
        let error = GetLastError();
        if !new_dacl.is_null() {
            LocalFree(new_dacl as HLOCAL);
        }
        return Err(anyhow::anyhow!(
            "SetTokenInformation(TokenDefaultDacl) failed: {error}"
        ));
    }

    if !new_dacl.is_null() {
        LocalFree(new_dacl as HLOCAL);
    }
    Ok(())
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn find_logon_sid(groups: HANDLE) -> Option<Vec<u8>> {
    let mut required = 0u32;
    let _ = GetTokenInformation(groups, TokenGroups, ptr::null_mut(), 0, &mut required);
    if required == 0 {
        return None;
    }

    let mut buffer = vec![0u8; required as usize];
    if GetTokenInformation(
        groups,
        TokenGroups,
        buffer.as_mut_ptr().cast(),
        required,
        &mut required,
    ) == 0
        || (required as usize) < std::mem::size_of::<u32>()
    {
        return None;
    }

    let count = ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) as usize;
    let groups_start = buffer.as_ptr().add(std::mem::size_of::<u32>()) as usize;
    let alignment = std::mem::align_of::<SID_AND_ATTRIBUTES>();
    let aligned_groups = (groups_start + alignment - 1) & !(alignment - 1);
    let entries = aligned_groups as *const SID_AND_ATTRIBUTES;

    for index in 0..count {
        let entry = ptr::read_unaligned(entries.add(index));
        if entry.Attributes & SE_GROUP_LOGON_ID != SE_GROUP_LOGON_ID {
            continue;
        }
        let length = GetLengthSid(entry.Sid);
        if length == 0 {
            return None;
        }
        let mut sid = vec![0u8; length as usize];
        if CopySid(length, sid.as_mut_ptr().cast(), entry.Sid) == 0 {
            return None;
        }
        return Some(sid);
    }
    None
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn logon_sid_bytes(token: HANDLE) -> Result<Vec<u8>> {
    if let Some(sid) = find_logon_sid(token) {
        return Ok(sid);
    }

    #[repr(C)]
    struct TokenLinkedToken {
        linked_token: HANDLE,
    }
    const TOKEN_LINKED_TOKEN: i32 = 19;

    let mut required = 0u32;
    let _ = GetTokenInformation(token, TOKEN_LINKED_TOKEN, ptr::null_mut(), 0, &mut required);
    if required >= std::mem::size_of::<TokenLinkedToken>() as u32 {
        let mut buffer = vec![0u8; required as usize];
        if GetTokenInformation(
            token,
            TOKEN_LINKED_TOKEN,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        ) != 0
        {
            let linked = ptr::read_unaligned(buffer.as_ptr().cast::<TokenLinkedToken>());
            if !linked.linked_token.is_null() {
                let sid = find_logon_sid(linked.linked_token);
                CloseHandle(linked.linked_token);
                if let Some(sid) = sid {
                    return Ok(sid);
                }
            }
        }
    }

    Err(anyhow::anyhow!("Logon SID not present on token"))
}

unsafe fn user_sid_bytes(token: HANDLE) -> Result<Vec<u8>> {
    let mut required = 0u32;
    let _ = GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut required);
    if required < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(win_error("GetTokenInformation(TokenUser) size query"));
    }
    let mut buffer = vec![0u8; required as usize];
    if GetTokenInformation(
        token,
        TokenUser,
        buffer.as_mut_ptr().cast(),
        required,
        &mut required,
    ) == 0
    {
        return Err(win_error("GetTokenInformation(TokenUser)"));
    }
    let token_user = ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_USER>());
    let length = GetLengthSid(token_user.User.Sid);
    if length == 0 {
        return Err(win_error("GetLengthSid(TokenUser)"));
    }
    let mut sid = vec![0u8; length as usize];
    if CopySid(length, sid.as_mut_ptr().cast(), token_user.User.Sid) == 0 {
        return Err(win_error("CopySid(TokenUser)"));
    }
    Ok(sid)
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn enable_change_notify_privilege(token: HANDLE) -> Result<()> {
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    if LookupPrivilegeValueW(
        ptr::null(),
        to_wide("SeChangeNotifyPrivilege").as_ptr(),
        &mut luid,
    ) == 0
    {
        return Err(win_error("LookupPrivilegeValueW"));
    }

    let mut privileges: TOKEN_PRIVILEGES = std::mem::zeroed();
    privileges.PrivilegeCount = 1;
    privileges.Privileges[0].Luid = luid;
    privileges.Privileges[0].Attributes = 0x0000_0002;
    if AdjustTokenPrivileges(token, 0, &privileges, 0, ptr::null_mut(), ptr::null_mut()) == 0 {
        return Err(win_error("AdjustTokenPrivileges"));
    }
    let error = GetLastError();
    if error != 0 {
        return Err(anyhow::anyhow!("AdjustTokenPrivileges failed: {error}"));
    }
    Ok(())
}

pub fn create_restricted_token(capability: &LocalSid) -> Result<RestrictedToken> {
    let desired: TOKEN_ACCESS_MASK = TOKEN_DUPLICATE
        | TOKEN_QUERY
        | TOKEN_ASSIGN_PRIMARY
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_SESSIONID
        | TOKEN_ADJUST_PRIVILEGES;
    let mut base = ptr::null_mut();
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), desired, &mut base) };
    if ok == 0 || base.is_null() {
        return Err(win_error("OpenProcessToken"));
    }
    let base = OwnedHandle(base);
    let mut logon = unsafe { logon_sid_bytes(base.raw()) }?;
    let system_read = LocalSid::new("S-1-5-32-545")?;
    let world_read = LocalSid::new("S-1-1-0")?;
    let policy_sids = standard_policy_sids(capability.as_ptr(), logon.as_mut_ptr().cast());
    let restrictions = restricting_entries([
        policy_sids[0],
        policy_sids[1],
        system_read.as_ptr(),
        world_read.as_ptr(),
    ]);
    let attempts = restricted_token_flag_attempts();
    let mut restricted = ptr::null_mut();
    let mut last_error = ERROR_SUCCESS;
    for (index, flags) in attempts.into_iter().enumerate() {
        let ok = unsafe {
            CreateRestrictedToken(
                base.raw(),
                flags,
                0,
                ptr::null(),
                0,
                ptr::null(),
                restrictions.len() as u32,
                restrictions.as_ptr(),
                &mut restricted,
            )
        };
        if ok != 0 {
            last_error = ERROR_SUCCESS;
            break;
        }

        last_error = unsafe { GetLastError() };
        let tried_lua = flags & LUA_TOKEN != 0;
        let has_retry = index + 1 < attempts.len();
        if !(tried_lua && has_retry && last_error == ERROR_INVALID_PARAMETER) {
            break;
        }
    }
    if last_error != ERROR_SUCCESS || restricted.is_null() {
        return Err(anyhow::anyhow!(
            "CreateRestrictedToken failed: {}",
            if last_error == ERROR_SUCCESS {
                ERROR_INVALID_PARAMETER
            } else {
                last_error
            }
        ));
    }
    let token = OwnedHandle(restricted);
    // The logon SID is retained because Windows uses it for session-scoped
    // objects. BUILTIN\Users is restricted-check-only. Everyone must also be
    // present in the default DACL so loader/session objects can initialize;
    // the normal token access check still gates every resource.
    let default_dacl_sids = [policy_sids[0], policy_sids[1], world_read.as_ptr()];
    unsafe {
        set_default_dacl(token.raw(), &default_dacl_sids)?;
        enable_change_notify_privilege(token.raw())?;
    }
    Ok(RestrictedToken { handle: token })
}

/// Derive a WRITE_RESTRICTED primary token from the dedicated sandbox
/// account's current logon token. The normal token supplies the sandbox-account
/// side of the access check. Workspace resources still require the exact
/// per-workspace capability; the separate traversal capability is granted only
/// on non-inheriting ancestor-directory ACEs needed to establish the cwd.
pub fn create_restricted_token_for_sandbox_user(
    capability: &LocalSid,
    ancestor_traversal: &LocalSid,
) -> Result<RestrictedToken> {
    let desired: TOKEN_ACCESS_MASK = TOKEN_DUPLICATE
        | TOKEN_QUERY
        | TOKEN_ASSIGN_PRIMARY
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_SESSIONID
        | TOKEN_ADJUST_PRIVILEGES;
    let mut base = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), desired, &mut base) } == 0 || base.is_null() {
        return Err(win_error("OpenProcessToken"));
    }
    let base = OwnedHandle(base);
    let mut user = unsafe { user_sid_bytes(base.raw()) }?;
    let mut logon = unsafe { logon_sid_bytes(base.raw()) }?;
    let system_read = LocalSid::new("S-1-5-32-545")?;
    let world_read = LocalSid::new("S-1-1-0")?;
    let policy_sids = sandbox_user_policy_sids(capability.as_ptr(), logon.as_mut_ptr().cast());
    let restrictions = restricting_entries(sandbox_user_restriction_sids(
        policy_sids[0],
        ancestor_traversal.as_ptr(),
        policy_sids[1],
        system_read.as_ptr(),
        world_read.as_ptr(),
    ));
    let mut restricted = ptr::null_mut();
    if unsafe {
        CreateRestrictedToken(
            base.raw(),
            sandbox_user_restricted_token_flags(),
            0,
            ptr::null(),
            0,
            ptr::null(),
            restrictions.len() as u32,
            restrictions.as_ptr(),
            &mut restricted,
        )
    } == 0
        || restricted.is_null()
    {
        return Err(win_error("CreateRestrictedToken(sandbox user)"));
    }
    let token = OwnedHandle(restricted);
    let default_dacl_sids = sandbox_user_default_dacl_sids(
        policy_sids[0],
        user.as_mut_ptr().cast(),
        policy_sids[1],
        world_read.as_ptr(),
    );
    unsafe {
        set_default_dacl(token.raw(), &default_dacl_sids)?;
        enable_change_notify_privilege(token.raw())?;
    }
    Ok(RestrictedToken { handle: token })
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_DACL_ACCESS_MASK, DISABLE_MAX_PRIVILEGE, LUA_TOKEN, WRITE_RESTRICTED,
        restricted_token_flag_attempts, sandbox_user_default_dacl_sids, sandbox_user_policy_sids,
        sandbox_user_restricted_token_flags, sandbox_user_restriction_sids, standard_policy_sids,
    };
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::GENERIC_ALL;

    #[test]
    fn restricted_token_retries_without_lua_after_invalid_parameter() {
        assert_eq!(
            restricted_token_flag_attempts(),
            [
                DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED,
                DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED,
            ]
        );
    }

    #[test]
    fn dedicated_sandbox_user_does_not_apply_lua_token() {
        assert_eq!(
            sandbox_user_restricted_token_flags(),
            DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED
        );
    }

    #[test]
    fn sandbox_user_restrictions_require_the_capability_not_the_account_sid() {
        let capability = 1usize as *mut c_void;
        let logon = 3usize as *mut c_void;

        assert_eq!(standard_policy_sids(capability, logon), [capability, logon]);
        assert_eq!(
            sandbox_user_policy_sids(capability, logon),
            [capability, logon]
        );
    }

    #[test]
    fn traversal_capability_is_restricting_only_and_never_inherited_by_created_files() {
        let workspace = 1usize as *mut c_void;
        let traversal = 2usize as *mut c_void;
        let logon = 3usize as *mut c_void;
        let users = 4usize as *mut c_void;
        let world = 5usize as *mut c_void;
        let user = 6usize as *mut c_void;

        assert!(
            sandbox_user_restriction_sids(workspace, traversal, logon, users, world)
                .contains(&traversal)
        );
        assert!(
            !sandbox_user_default_dacl_sids(workspace, user, logon, world).contains(&traversal)
        );
    }

    #[test]
    fn workspace_capability_is_stable_per_root_and_mode() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let roots = vec![root.path().to_path_buf()];

        let first = super::workspace_capability_sid(
            home.path(),
            &roots,
            crate::SandboxMode::WorkspaceWrite,
        )
        .unwrap()
        .to_string()
        .unwrap();
        let second = super::workspace_capability_sid(
            home.path(),
            &roots,
            crate::SandboxMode::WorkspaceWrite,
        )
        .unwrap()
        .to_string()
        .unwrap();
        let read_only =
            super::workspace_capability_sid(home.path(), &roots, crate::SandboxMode::ReadOnly)
                .unwrap()
                .to_string()
                .unwrap();

        assert_eq!(first, second);
        assert_ne!(first, read_only);
    }

    #[test]
    fn ancestor_traversal_capability_is_stable_and_not_a_workspace_capability() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let roots = vec![root.path().to_path_buf()];

        let first = super::ancestor_traversal_sid(home.path())
            .unwrap()
            .to_string()
            .unwrap();
        let second = super::ancestor_traversal_sid(home.path())
            .unwrap()
            .to_string()
            .unwrap();
        let workspace = super::workspace_capability_sid(
            home.path(),
            &roots,
            crate::SandboxMode::WorkspaceWrite,
        )
        .unwrap()
        .to_string()
        .unwrap();

        assert_eq!(first, second);
        assert_ne!(first, workspace);
    }

    #[test]
    fn default_dacl_allows_child_created_kernel_objects_to_initialize() {
        assert_eq!(DEFAULT_DACL_ACCESS_MASK, GENERIC_ALL);
    }
}
