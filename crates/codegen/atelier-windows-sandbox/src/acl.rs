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
    ACE_FLAGS, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
};

const INHERIT_FLAGS: ACE_FLAGS = OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE;

/// A temporary capability grant. The original DACL is restored when the guard
/// is dropped, so a capability SID never becomes a persistent access path.
pub struct ScopedAclGrant {
    path: PathBuf,
    original_dacl: Option<Vec<u8>>,
    restored: bool,
}

pub fn access_mask_for_mode(mode: crate::SandboxMode) -> u32 {
    match mode {
        crate::SandboxMode::ReadOnly => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        crate::SandboxMode::WorkspaceWrite => FILE_ALL_ACCESS,
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
        restored: false,
    })
}

impl ScopedAclGrant {
    pub fn restore(mut self) -> Result<()> {
        let result = self.restore_inner();
        if result.is_ok() {
            self.restored = true;
        }
        result
    }

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
        if !self.restored {
            let _ = self.restore_inner();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FILE_ALL_ACCESS;
    use super::grant_restricted_sids;
    use crate::token::LocalSid;
    use std::process::Command;

    #[test]
    fn capability_acl_is_inherited_by_new_children() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        let sid_text = "S-1-5-21-111111111-222222222-333333333-444444444";
        let sid = LocalSid::new(sid_text).expect("sid");
        let grant =
            grant_restricted_sids(temp.path(), &[sid.as_ptr()], FILE_ALL_ACCESS).expect("grant");
        std::fs::create_dir(&child).expect("child");
        let output = Command::new("icacls").arg(&child).output().expect("icacls");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(sid_text), "icacls output: {stdout}");
        grant.restore().expect("restore");
    }
}
