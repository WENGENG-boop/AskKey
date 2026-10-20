//! Bringing another process's window forward.

use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, SetForegroundWindow,
    ShowWindow, SW_RESTORE, SW_SHOW,
};

struct Search {
    pid: u32,
    title: Vec<u16>,
    found: HWND,
}

unsafe extern "system" fn visit(window: HWND, parameter: LPARAM) -> BOOL {
    let search = &mut *(parameter as *mut Search);
    let mut pid = 0u32;
    GetWindowThreadProcessId(window, &mut pid);
    if pid != search.pid {
        return 1;
    }
    let mut buffer = [0u16; 256];
    let length = GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32);
    if length > 0 && buffer[..length as usize] == search.title[..] {
        search.found = window;
        return 0;
    }
    1
}

/// Shows, restores and focuses the top-level window of process `pid` whose
/// title is exactly `title`, even if it is hidden. Returns whether one was
/// found.
pub fn activate_window(pid: u32, title: &str) -> bool {
    let mut search = Search {
        pid,
        title: title.encode_utf16().collect(),
        found: std::ptr::null_mut(),
    };
    // SAFETY: `search` outlives the enumeration, and `visit` only reads and
    // writes it through the pointer passed here.
    unsafe {
        EnumWindows(Some(visit), &mut search as *mut Search as LPARAM);
        if search.found.is_null() {
            return false;
        }
        ShowWindow(search.found, SW_SHOW);
        if IsIconic(search.found) != 0 {
            ShowWindow(search.found, SW_RESTORE);
        }
        SetForegroundWindow(search.found);
    }
    true
}
