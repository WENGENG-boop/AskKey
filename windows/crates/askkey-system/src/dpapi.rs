use std::io;
use std::ptr;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

use crate::last_error;

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

unsafe fn take(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
    // Clear before freeing: unprotected output is key material.
    ptr::write_bytes(output.pbData, 0, output.cbData as usize);
    LocalFree(output.pbData.cast());
    bytes
}

/// Encrypts `data` with DPAPI for the current user. Never shows UI.
///
/// Any process running as the same user can call [`unprotect`]; DPAPI
/// protects against other users and offline disk access, not same-user code.
pub fn protect(data: &[u8], entropy: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(data);
    let entropy = blob(entropy);
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(unsafe { take(output) })
}

/// Decrypts DPAPI output produced by [`protect`] with the same entropy.
pub fn unprotect(data: &[u8], entropy: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(data);
    let entropy = blob(entropy);
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(unsafe { take(output) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_binds_entropy() {
        let sealed = protect(b"synthetic key material", b"entropy-a").unwrap();
        assert_ne!(sealed, b"synthetic key material");
        assert_eq!(
            unprotect(&sealed, b"entropy-a").unwrap(),
            b"synthetic key material"
        );
        assert!(unprotect(&sealed, b"entropy-b").is_err());
    }
}
