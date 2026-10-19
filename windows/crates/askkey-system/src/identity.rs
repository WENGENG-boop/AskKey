use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::last_error;
use crate::wide::from_wide_ptr;

/// String SID of the user running this process, such as `S-1-5-21-...`.
pub fn current_user_sid() -> io::Result<String> {
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(last_error());
        }
        let mut length = 0u32;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length);
        // u64 storage keeps TOKEN_USER suitably aligned.
        let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(last_error());
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut string_sid: *mut u16 = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut string_sid) == 0 {
            return Err(last_error());
        }
        let sid = from_wide_ptr(string_sid);
        LocalFree(string_sid.cast());
        Ok(sid)
    }
}

/// Full Win32 path of the executable image of `pid`.
pub fn process_image_path(pid: u32) -> io::Result<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(last_error());
        }
        let mut buffer = vec![0u16; 32 * 1024];
        let mut size = buffer.len() as u32;
        let ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size);
        CloseHandle(process);
        if ok == 0 {
            return Err(last_error());
        }
        Ok(PathBuf::from(String::from_utf16_lossy(
            &buffer[..size as usize],
        )))
    }
}

/// Process ID of the client connected to a named pipe server handle.
pub fn pipe_client_process_id(pipe: &impl AsRawHandle) -> io::Result<u32> {
    let mut pid = 0u32;
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(last_error());
    }
    Ok(pid)
}

/// Process ID of the server at the other end of a named pipe client handle.
pub fn pipe_server_process_id(pipe: &impl AsRawHandle) -> io::Result<u32> {
    let mut pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(last_error());
    }
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_current_user_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
    }

    #[test]
    fn reads_own_image_path() {
        let path = process_image_path(std::process::id()).unwrap();
        assert_eq!(path, std::env::current_exe().unwrap());
    }
}
