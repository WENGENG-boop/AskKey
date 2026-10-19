use std::io;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
};

use crate::wide::wide;
use crate::{current_user_sid, last_error};

/// A security descriptor whose protected DACL grants full access to the
/// current user only. Used for the Broker pipe.
pub struct PrivateSecurityAttributes {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

// The descriptor is immutable after creation and owned by this value.
unsafe impl Send for PrivateSecurityAttributes {}
unsafe impl Sync for PrivateSecurityAttributes {}

impl PrivateSecurityAttributes {
    pub fn current_user() -> io::Result<Self> {
        let sid = current_user_sid()?;
        let descriptor = descriptor_from_sddl(&format!("D:P(A;;GA;;;{sid})"))?;
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            },
        })
    }

    /// Pointer suitable for `CreateNamedPipeW` and friends.
    pub fn as_ptr(&self) -> *mut std::ffi::c_void {
        &self.attributes as *const SECURITY_ATTRIBUTES as *mut std::ffi::c_void
    }
}

impl Drop for PrivateSecurityAttributes {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

fn descriptor_from_sddl(sddl: &str) -> io::Result<PSECURITY_DESCRIPTOR> {
    let sddl = wide(sddl);
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(descriptor)
}

/// Replaces the DACL of `path` with one that grants only the current user
/// full control and blocks inherited entries. Directories pass the entry on
/// to their children. This is the Windows counterpart of `0700`/`0600`.
pub fn protect_private_path(path: &Path) -> io::Result<()> {
    let sid = current_user_sid()?;
    let inheritance = if path.is_dir() { "OICI" } else { "" };
    let descriptor = descriptor_from_sddl(&format!("D:P(A;{inheritance};FA;;;{sid})"))?;
    let target = wide(path);
    let ok = unsafe {
        SetFileSecurityW(
            target.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let result = if ok == 0 { Err(last_error()) } else { Ok(()) };
    unsafe {
        LocalFree(descriptor);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_a_directory_and_file() {
        let directory = tempfile::tempdir().unwrap();
        protect_private_path(directory.path()).unwrap();
        let file = directory.path().join("secret.bin");
        std::fs::write(&file, b"synthetic").unwrap();
        protect_private_path(&file).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"synthetic");
    }

    #[test]
    fn builds_pipe_attributes() {
        let attributes = PrivateSecurityAttributes::current_user().unwrap();
        assert!(!attributes.as_ptr().is_null());
    }
}
