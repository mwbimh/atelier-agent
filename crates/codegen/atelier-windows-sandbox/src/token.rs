use crate::winutil::to_wide;
use crate::winutil::win_error;
use anyhow::Result;
use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_SUCCESS, GetLastError, HANDLE, HLOCAL, LUID,
    LocalFree,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, AdjustTokenPrivileges, CopySid, CreateRestrictedToken, CreateWellKnownSid,
    DISABLE_MAX_PRIVILEGE, GetLengthSid, GetTokenInformation, LUA_TOKEN, LookupPrivilegeValueW,
    SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ACCESS_MASK, TOKEN_ADJUST_DEFAULT,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
    TOKEN_PRIVILEGES, TOKEN_QUERY, TokenDefaultDacl, TokenGroups,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcessToken,
};

const WORLD_SID: i32 = 1;
const WRITE_RESTRICTED: u32 = 0x08;
const GENERIC_ALL: u32 = 0x1000_0000;
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
static PROCESS_CAPABILITY_SID: OnceLock<Result<String, String>> = OnceLock::new();

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

    pub fn restricting_sids(&self, capability_sid: *mut c_void) -> [*mut c_void; 1] {
        [capability_sid]
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
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        if !self.sid.is_null() {
            unsafe { LocalFree(self.sid as HLOCAL) };
        }
    }
}

pub fn new_capability_sid() -> Result<LocalSid> {
    let value = PROCESS_CAPABILITY_SID.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| format!("system clock before Unix epoch: {err}"))?
            .as_nanos();
        let pid = u128::from(unsafe { GetCurrentProcessId() });
        let a = (nanos as u64 ^ pid as u64) as u32;
        let b = (nanos.rotate_left(17) as u64 ^ (pid >> 32) as u64) as u32;
        let c = (nanos >> 32) as u32;
        let d = (nanos >> 64) as u32;
        Ok(format!("S-1-5-21-{a}-{b}-{c}-{d}"))
    });
    let value = value.as_ref().map_err(|error| anyhow::anyhow!("{error}"))?;
    LocalSid::new(value)
}

fn world_sid() -> Result<Vec<u8>> {
    let mut size = 0u32;
    unsafe {
        let _ = CreateWellKnownSid(WORLD_SID, ptr::null_mut(), ptr::null_mut(), &mut size);
    }
    if size == 0 {
        return Err(win_error("CreateWellKnownSid size query"));
    }
    let mut sid = vec![0u8; size as usize];
    let ok = unsafe {
        CreateWellKnownSid(
            WORLD_SID,
            ptr::null_mut(),
            sid.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if ok == 0 {
        return Err(win_error("CreateWellKnownSid"));
    }
    sid.truncate(size as usize);
    Ok(sid)
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn set_default_dacl(token: HANDLE, sids: &[*mut c_void]) -> Result<()> {
    if sids.is_empty() {
        return Ok(());
    }

    let entries: Vec<EXPLICIT_ACCESS_W> = sids
        .iter()
        .map(|sid| EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
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
    let mut world = world_sid()?;
    let restrictions = [
        SID_AND_ATTRIBUTES {
            Sid: capability.as_ptr(),
            Attributes: 0,
        },
        SID_AND_ATTRIBUTES {
            Sid: logon.as_mut_ptr().cast(),
            Attributes: 0,
        },
    ];
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
    // Logon/world SIDs are present in the token's default DACL so the child
    // process can initialize its standard handles and loader objects. The
    // logon SID is retained in the restricted set because Windows rejects some
    // tokens without it; World/Everyone is deliberately excluded so a broad
    // Everyone ACE cannot satisfy WRITE_RESTRICTED outside granted roots.
    let dacl_sids = [
        capability.as_ptr(),
        logon.as_mut_ptr().cast(),
        world.as_mut_ptr().cast(),
    ];
    unsafe {
        set_default_dacl(token.raw(), &dacl_sids)?;
        enable_change_notify_privilege(token.raw())?;
    }
    Ok(RestrictedToken { handle: token })
}

#[cfg(test)]
mod tests {
    use super::{
        DISABLE_MAX_PRIVILEGE, LUA_TOKEN, WRITE_RESTRICTED, restricted_token_flag_attempts,
    };

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
}
