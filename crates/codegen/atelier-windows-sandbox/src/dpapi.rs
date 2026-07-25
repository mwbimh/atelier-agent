use anyhow::{Result, anyhow};
use windows_sys::Win32::Foundation::{GetLastError, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    CryptUnprotectData,
};

fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    }
}

pub fn protect(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut input = blob(bytes);
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(anyhow!("CryptProtectData failed: {}", unsafe {
            GetLastError()
        }));
    }
    let protected =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    if !output.pbData.is_null() {
        unsafe { LocalFree(output.pbData as HLOCAL) };
    }
    Ok(protected)
}

pub fn unprotect(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut input = blob(bytes);
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(anyhow!("CryptUnprotectData failed: {}", unsafe {
            GetLastError()
        }));
    }
    let clear =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    if !output.pbData.is_null() {
        unsafe { LocalFree(output.pbData as HLOCAL) };
    }
    Ok(clear)
}

#[cfg(test)]
mod tests {
    #[test]
    fn machine_scope_round_trip() {
        let protected = super::protect(b"atelier-sandbox-secret").unwrap();
        assert_ne!(protected, b"atelier-sandbox-secret");
        assert_eq!(
            super::unprotect(&protected).unwrap(),
            b"atelier-sandbox-secret"
        );
    }
}
