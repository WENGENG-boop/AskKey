use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

/// NUL-terminated UTF-16 for Win32 `PCWSTR` parameters.
pub(crate) fn wide<S: AsRef<OsStr> + ?Sized>(value: &S) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Reads a NUL-terminated UTF-16 string returned by Win32.
///
/// # Safety
/// `pointer` must point to a valid NUL-terminated UTF-16 string.
pub(crate) unsafe fn from_wide_ptr(pointer: *const u16) -> String {
    let mut length = 0;
    while *pointer.add(length) != 0 {
        length += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
}
