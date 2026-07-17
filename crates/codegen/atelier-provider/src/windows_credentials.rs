use super::{CredentialError, SecretString};
use std::ffi::c_void;
use std::ptr;
use std::slice;
use windows::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};
use windows::core::{HSTRING, PWSTR};

pub(super) fn read(service: &str, account: &str) -> Result<SecretString, CredentialError> {
    let credential = read_credential(service, account)?;
    let credential = unsafe { &*credential.0 };

    if credential.CredentialBlobSize == 0 {
        return Ok(SecretString::new(String::new()));
    }
    if credential.CredentialBlob.is_null() {
        return Err(operation_failed_with_code("read", service, account, 0));
    }

    let bytes = unsafe {
        slice::from_raw_parts(
            credential.CredentialBlob.cast_const(),
            credential.CredentialBlobSize as usize,
        )
    };
    let secret = String::from_utf8(bytes.to_vec())
        .map_err(|_| operation_failed_with_code("read", service, account, 0))?;
    Ok(SecretString::new(secret))
}

fn read_credential(service: &str, account: &str) -> Result<CredentialGuard, CredentialError> {
    let target_name = HSTRING::from(service);
    let mut raw_credential = ptr::null_mut();
    unsafe { CredReadW(&target_name, CRED_TYPE_GENERIC, None, &mut raw_credential) }
        .map_err(|error| operation_failed("read", service, account, error))?;

    let credential = CredentialGuard(raw_credential);
    if credential.0.is_null() {
        return Err(operation_failed_with_code("read", service, account, 0));
    }

    let credential_data = unsafe { &*credential.0 };
    let stored_account = read_wide_string(credential_data.UserName.0);
    if !matches!(stored_account.as_deref(), Ok(stored) if stored == account) {
        return Err(CredentialError::SecretStoreAccountMismatch {
            service: service.into(),
            account: account.into(),
        });
    }
    Ok(credential)
}

pub(super) fn write(service: &str, account: &str, secret: &str) -> Result<(), CredentialError> {
    let target_name = wide_string(service);
    let user_name = wide_string(account);
    let blob_size = u32::try_from(secret.len())
        .map_err(|_| operation_failed_with_code("write", service, account, 0))?;
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_name.as_ptr() as *mut u16),
        UserName: PWSTR(user_name.as_ptr() as *mut u16),
        CredentialBlobSize: blob_size,
        CredentialBlob: if secret.is_empty() {
            ptr::null_mut()
        } else {
            secret.as_ptr() as *mut u8
        },
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };

    unsafe { CredWriteW(&credential, 0) }
        .map_err(|error| operation_failed("write", service, account, error))
}

pub(super) fn delete(service: &str, account: &str) -> Result<(), CredentialError> {
    {
        let _credential = read_credential(service, account)?;
    }
    let target_name = HSTRING::from(service);
    unsafe { CredDeleteW(&target_name, CRED_TYPE_GENERIC, None) }
        .map_err(|error| operation_failed("delete", service, account, error))
}

fn operation_failed(
    operation: &'static str,
    service: &str,
    account: &str,
    error: windows::core::Error,
) -> CredentialError {
    operation_failed_with_code(operation, service, account, error.code().0)
}

fn operation_failed_with_code(
    operation: &'static str,
    service: &str,
    account: &str,
    error_code: i32,
) -> CredentialError {
    CredentialError::SecretStoreOperationFailed {
        operation,
        service: service.into(),
        account: account.into(),
        error_code,
    }
}

fn wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn read_wide_string(pointer: *mut u16) -> Result<String, ()> {
    if pointer.is_null() {
        return Err(());
    }

    unsafe {
        let mut length = 0;
        while *pointer.add(length) != 0 {
            length += 1;
        }
        String::from_utf16(slice::from_raw_parts(pointer, length)).map_err(|_| ())
    }
}

struct CredentialGuard(*mut CREDENTIALW);

impl Drop for CredentialGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CredFree(self.0.cast::<c_void>()) };
        }
    }
}
