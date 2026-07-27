use crate::winutil::path_to_wide;
use crate::winutil::win32_error;
use anyhow::Result;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr;
use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, SE_FILE_OBJECT, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_FLAGS, ACE_HEADER, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
    EqualSid, GetAce, OBJECT_INHERIT_ACE,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};

const INHERIT_FLAGS: ACE_FLAGS = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;
const ACCESS_ALLOWED_ACE_KIND: u8 = 0;
const INHERIT_HEADER_FLAGS: u8 = INHERIT_FLAGS as u8;

/// A temporary capability grant. The original DACL is restored when the guard
/// is dropped, so a capability SID never becomes a persistent access path.
pub struct ScopedAclGrant {
    path: PathBuf,
    original_dacl: Option<Vec<u8>>,
}

pub fn access_mask_for_mode(mode: crate::SandboxMode) -> u32 {
    match mode {
        crate::SandboxMode::ReadOnly => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        // Match the pinned Codex workspace grant: workspace-write permits
        // ordinary file operations and deletion, but never ACL or ownership
        // control such as WRITE_DAC/WRITE_OWNER.
        crate::SandboxMode::WorkspaceWrite => {
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE
        }
    }
}

pub fn grant_restricted_sids(
    path: &Path,
    restricted_sids: &[*mut c_void],
    access_mask: u32,
) -> Result<ScopedAclGrant> {
    if restricted_sids.is_empty() {
        return Err(anyhow::anyhow!("no restricting SIDs supplied"));
    }
    let path_wide = path_to_wide(path);
    let mut dacl = ptr::null_mut();
    let mut security_descriptor = ptr::null_mut();
    let code = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut security_descriptor,
        )
    };
    if code != 0 {
        return Err(win32_error("GetNamedSecurityInfoW", code));
    }

    let original_dacl = unsafe {
        if dacl.is_null() {
            None
        } else {
            let size = usize::from((*dacl).AclSize);
            if size < std::mem::size_of::<ACL>() {
                if !security_descriptor.is_null() {
                    LocalFree(security_descriptor as HLOCAL);
                }
                return Err(anyhow::anyhow!("invalid ACL size for {}", path.display()));
            }
            Some(std::slice::from_raw_parts(dacl.cast::<u8>(), size).to_vec())
        }
    };

    let entries = restricted_sids
        .iter()
        .map(|sid| EXPLICIT_ACCESS_W {
            grfAccessPermissions: access_mask,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: INHERIT_FLAGS,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: (*sid).cast(),
            },
        })
        .collect::<Vec<_>>();
    let mut new_dacl = ptr::null_mut();
    let code =
        unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), dacl, &mut new_dacl) };
    if !security_descriptor.is_null() {
        unsafe { LocalFree(security_descriptor as HLOCAL) };
    }
    if code != 0 {
        return Err(win32_error("SetEntriesInAclW", code));
    }

    let code = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            new_dacl,
            ptr::null_mut(),
        )
    };
    unsafe { LocalFree(new_dacl as HLOCAL) };
    if code != 0 {
        return Err(win32_error("SetNamedSecurityInfoW", code));
    }

    Ok(ScopedAclGrant {
        path: path.to_path_buf(),
        original_dacl,
    })
}

/// Persist an inheritable ACL grant. This is used only for the materialized
/// internal runner directory, which the dedicated sandbox account must be able
/// to read and execute across sessions.
fn dacl_has_inheritable_grant(dacl: *mut ACL, sid: *mut c_void, access_mask: u32) -> bool {
    if dacl.is_null() {
        return false;
    }
    let ace_count = unsafe { (*dacl).AceCount };
    for index in 0..ace_count {
        let mut ace = ptr::null_mut();
        if unsafe { GetAce(dacl, u32::from(index), &mut ace) } == 0 || ace.is_null() {
            continue;
        }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType != ACCESS_ALLOWED_ACE_KIND
            || header.AceFlags & INHERIT_HEADER_FLAGS != INHERIT_HEADER_FLAGS
        {
            continue;
        }
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let ace_sid = (&allowed.SidStart as *const u32)
            .cast_mut()
            .cast::<c_void>();
        if unsafe { EqualSid(ace_sid, sid) } != 0 && allowed.Mask & access_mask == access_mask {
            return true;
        }
    }
    false
}

/// Ensure the workspace root has stable account and capability grants. The
/// expensive inheritable ACL propagation runs only when either exact ACE is
/// missing; subsequent launches verify the root DACL and reuse it.
pub fn ensure_persistent_workspace_grant(
    path: &Path,
    sandbox_user: *mut c_void,
    capability: *mut c_void,
    capability_access: u32,
) -> Result<bool> {
    let path_wide = path_to_wide(path);
    let mut dacl = ptr::null_mut();
    let mut security_descriptor = ptr::null_mut();
    let code = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut security_descriptor,
        )
    };
    if code != 0 {
        return Err(win32_error("GetNamedSecurityInfoW", code));
    }
    let account_access = access_mask_for_mode(crate::SandboxMode::WorkspaceWrite);
    let already_present = dacl_has_inheritable_grant(dacl, sandbox_user, account_access)
        && dacl_has_inheritable_grant(dacl, capability, capability_access);
    if already_present {
        if !security_descriptor.is_null() {
            unsafe { LocalFree(security_descriptor as HLOCAL) };
        }
        return Ok(false);
    }

    let entries = [
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: account_access,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: INHERIT_FLAGS,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sandbox_user.cast(),
            },
        },
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: capability_access,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: INHERIT_FLAGS,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: capability.cast(),
            },
        },
    ];
    let mut new_dacl = ptr::null_mut();
    let code =
        unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), dacl, &mut new_dacl) };
    if !security_descriptor.is_null() {
        unsafe { LocalFree(security_descriptor as HLOCAL) };
    }
    if code != 0 {
        return Err(win32_error("SetEntriesInAclW", code));
    }
    let code = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            new_dacl,
            ptr::null_mut(),
        )
    };
    unsafe { LocalFree(new_dacl as HLOCAL) };
    if code != 0 {
        return Err(win32_error("SetNamedSecurityInfoW", code));
    }
    Ok(true)
}

pub fn grant_persistent_sids(path: &Path, sids: &[*mut c_void], access_mask: u32) -> Result<()> {
    if sids.is_empty() {
        return Err(anyhow::anyhow!("no SIDs supplied"));
    }
    let path_wide = path_to_wide(path);
    let mut dacl = ptr::null_mut();
    let mut security_descriptor = ptr::null_mut();
    let code = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut security_descriptor,
        )
    };
    if code != 0 {
        return Err(win32_error("GetNamedSecurityInfoW", code));
    }
    let entries = sids
        .iter()
        .map(|sid| EXPLICIT_ACCESS_W {
            grfAccessPermissions: access_mask,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: INHERIT_FLAGS,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: (*sid).cast(),
            },
        })
        .collect::<Vec<_>>();
    let mut new_dacl = ptr::null_mut();
    let code =
        unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), dacl, &mut new_dacl) };
    if !security_descriptor.is_null() {
        unsafe { LocalFree(security_descriptor as HLOCAL) };
    }
    if code != 0 {
        return Err(win32_error("SetEntriesInAclW", code));
    }
    let code = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            new_dacl,
            ptr::null_mut(),
        )
    };
    unsafe { LocalFree(new_dacl as HLOCAL) };
    if code != 0 {
        return Err(win32_error("SetNamedSecurityInfoW", code));
    }
    Ok(())
}

impl ScopedAclGrant {
    fn restore_inner(&self) -> Result<()> {
        let path_wide = path_to_wide(&self.path);
        let original_dacl = self
            .original_dacl
            .as_ref()
            .map_or(ptr::null(), |dacl| dacl.as_ptr().cast::<ACL>());
        let code = unsafe {
            SetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                original_dacl,
                ptr::null_mut(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(win32_error("restore SetNamedSecurityInfoW", code))
        }
    }
}

impl Drop for ScopedAclGrant {
    fn drop(&mut self) {
        let _ = self.restore_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::access_mask_for_mode;
    use super::{ensure_persistent_workspace_grant, grant_restricted_sids};
    use crate::SandboxMode;
    use crate::token::LocalSid;
    use std::process::Command;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        WRITE_DAC, WRITE_OWNER,
    };

    #[test]
    fn workspace_write_mask_excludes_acl_and_owner_control() {
        let mask = access_mask_for_mode(SandboxMode::WorkspaceWrite);

        assert_eq!(
            mask,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE
        );
        assert_eq!(mask & WRITE_DAC, 0);
        assert_eq!(mask & WRITE_OWNER, 0);
        assert_ne!(mask, FILE_ALL_ACCESS);
    }

    #[test]
    fn persistent_workspace_grant_is_verified_and_reused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let user =
            LocalSid::new("S-1-5-21-111111111-222222222-333333333-444444441").expect("user sid");
        let capability = LocalSid::new("S-1-5-21-111111111-222222222-333333333-444444442")
            .expect("capability sid");
        let access = access_mask_for_mode(SandboxMode::WorkspaceWrite);

        assert!(
            ensure_persistent_workspace_grant(
                temp.path(),
                user.as_ptr(),
                capability.as_ptr(),
                access,
            )
            .expect("first grant")
        );
        assert!(
            !ensure_persistent_workspace_grant(
                temp.path(),
                user.as_ptr(),
                capability.as_ptr(),
                access,
            )
            .expect("reused grant")
        );
    }

    #[test]
    fn capability_acl_is_inherited_by_new_children() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        let sid_text = "S-1-5-21-111111111-222222222-333333333-444444444";
        let sid = LocalSid::new(sid_text).expect("sid");
        let grant = grant_restricted_sids(
            temp.path(),
            &[sid.as_ptr()],
            access_mask_for_mode(SandboxMode::WorkspaceWrite),
        )
        .expect("grant");
        std::fs::create_dir(&child).expect("child");
        let output = Command::new("icacls").arg(&child).output().expect("icacls");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(sid_text), "icacls output: {stdout}");
        drop(grant);
    }
}
