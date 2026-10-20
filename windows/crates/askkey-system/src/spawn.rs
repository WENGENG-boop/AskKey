use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{
    DuplicateHandle, SetHandleInformation, DUPLICATE_SAME_ACCESS, GENERIC_READ, GENERIC_WRITE,
    HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetFileType, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_CHAR, FILE_TYPE_DISK,
    FILE_TYPE_PIPE, OPEN_EXISTING,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcess, ResumeThread, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_DUP_HANDLE, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::wide::wide;
use crate::{build_command_line, environment_block, last_error};

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(last_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

/// Opens the `NUL` device for reading and writing, used for targets whose
/// output is discarded (MCP runs).
pub fn null_handle() -> io::Result<OwnedHandle> {
    let name = wide("NUL");
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null::<SECURITY_ATTRIBUTES>(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    owned(handle)
}

/// Duplicates the handle values `handles` from process `pid` into this
/// process. Only files, pipes and character devices are accepted, so a client
/// cannot pass a process or thread handle as a "stream".
pub fn duplicate_stream_handles(pid: u32, handles: &[u64]) -> io::Result<Vec<OwnedHandle>> {
    let source = owned(unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, pid) })?;
    let mut duplicated = Vec::with_capacity(handles.len());
    for &value in handles {
        let mut target: HANDLE = ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                source.as_raw_handle(),
                value as usize as HANDLE,
                GetCurrentProcess(),
                &mut target,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        let handle = owned(target)?;
        let kind = unsafe { GetFileType(handle.as_raw_handle()) };
        if ![FILE_TYPE_DISK, FILE_TYPE_CHAR, FILE_TYPE_PIPE].contains(&kind) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a stream handle",
            ));
        }
        duplicated.push(handle);
    }
    Ok(duplicated)
}

/// An anonymous pipe as `(read, write)`. Neither end is inheritable.
pub fn anonymous_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = ptr::null_mut();
    let mut write: HANDLE = ptr::null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, ptr::null(), 64 * 1024) } == 0 {
        return Err(last_error());
    }
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

/// Bytes waiting in a pipe's read end, without reading or blocking.
pub fn pipe_bytes_available(read: &impl AsRawHandle) -> io::Result<usize> {
    let mut available = 0u32;
    let ok = unsafe {
        PeekNamedPipe(
            read.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            &mut available,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(available as usize)
}

pub struct SpawnSpec<'a> {
    pub executable: &'a Path,
    /// Full argument vector, including `argv[0]`.
    pub arguments: &'a [String],
    pub working_directory: &'a Path,
    pub environment: &'a [(String, String)],
    pub stdin: &'a OwnedHandle,
    pub stdout: &'a OwnedHandle,
    pub stderr: &'a OwnedHandle,
}

/// A target running in its own job object. Dropping it, or
/// [`JobChild::terminate`], ends the target and every process it started.
pub struct JobChild {
    job: OwnedHandle,
    process: OwnedHandle,
    pub pid: u32,
}

impl JobChild {
    /// Blocks until the target exits and returns its exit code.
    pub fn wait(&self) -> io::Result<i32> {
        unsafe {
            if WaitForSingleObject(self.process.as_raw_handle(), INFINITE) != WAIT_OBJECT_0 {
                return Err(last_error());
            }
            let mut code = 0u32;
            if GetExitCodeProcess(self.process.as_raw_handle(), &mut code) == 0 {
                return Err(last_error());
            }
            Ok(code as i32)
        }
    }

    /// Ends the target and its descendants. There is no general graceful stop
    /// on Windows for processes without a shared console.
    pub fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
    }
}

struct AttributeList(Vec<u8>);

impl AttributeList {
    fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) }
    }
}

fn create_kill_on_close_job() -> io::Result<OwnedHandle> {
    let job = owned(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    let ok = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(job)
}

/// Starts a target suspended, places it in a new kill-on-close job, then
/// resumes it. Only the three stream handles are inherited
/// (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), and the target gets no console
/// window of its own.
pub fn spawn_in_job(spec: &SpawnSpec<'_>) -> io::Result<JobChild> {
    let job = create_kill_on_close_job()?;
    let mut inherited: Vec<HANDLE> = [spec.stdin, spec.stdout, spec.stderr]
        .iter()
        .map(|handle| handle.as_raw_handle())
        .collect();
    inherited.sort();
    inherited.dedup();

    let mut size = 0usize;
    unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size) };
    let mut attributes = AttributeList(vec![0u8; size]);
    unsafe {
        if InitializeProcThreadAttributeList(attributes.as_ptr(), 1, 0, &mut size) == 0 {
            // Nothing to delete yet.
            std::mem::forget(attributes);
            return Err(last_error());
        }
        if UpdateProcThreadAttribute(
            attributes.as_ptr(),
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            inherited.as_ptr().cast(),
            inherited.len() * std::mem::size_of::<HANDLE>(),
            ptr::null_mut(),
            ptr::null(),
        ) == 0
        {
            return Err(last_error());
        }
    }

    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = spec.stdin.as_raw_handle();
    startup.StartupInfo.hStdOutput = spec.stdout.as_raw_handle();
    startup.StartupInfo.hStdError = spec.stderr.as_raw_handle();
    startup.lpAttributeList = attributes.as_ptr();

    let application = wide(spec.executable);
    let mut command_line = wide(&build_command_line(spec.arguments));
    let directory = wide(spec.working_directory);
    let environment = environment_block(spec.environment);
    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // Mark the stream handles inheritable only for the duration of the call;
    // the handle list keeps any other inheritable handle out of the target.
    for &handle in &inherited {
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    }
    let ok = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_UNICODE_ENVIRONMENT
                | CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_NEW_PROCESS_GROUP,
            environment.as_ptr().cast(),
            directory.as_ptr(),
            &startup.StartupInfo,
            &mut information,
        )
    };
    let spawn_error = if ok == 0 { Some(last_error()) } else { None };
    for &handle in &inherited {
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
    if let Some(error) = spawn_error {
        return Err(error);
    }

    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    unsafe {
        if AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) == 0 {
            let error = last_error();
            windows_sys::Win32::System::Threading::TerminateProcess(process.as_raw_handle(), 1);
            return Err(error);
        }
        if ResumeThread(thread.as_raw_handle()) == u32::MAX {
            let error = last_error();
            TerminateJobObject(job.as_raw_handle(), 1);
            return Err(error);
        }
    }
    drop(thread);
    Ok(JobChild {
        job,
        process,
        pid: information.dwProcessId,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve_executable;
    use std::io::Read;

    fn pipe() -> (OwnedHandle, OwnedHandle) {
        anonymous_pipe().unwrap()
    }

    fn system_root() -> String {
        std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())
    }

    #[test]
    fn runs_a_target_with_only_the_given_environment() {
        let root = system_root();
        let path = format!(r"{root}\System32");
        let executable = resolve_executable("where", Path::new(&root), Some(&path), None).unwrap();
        let (reader, writer) = pipe();
        let null = null_handle().unwrap();
        let arguments = vec!["where".to_string(), "/?".to_string()];
        let environment = vec![
            ("SystemRoot".to_string(), root.clone()),
            ("ASKKEY_SYNTHETIC".to_string(), "x".into()),
        ];
        let child = spawn_in_job(&SpawnSpec {
            executable: &executable,
            arguments: &arguments,
            working_directory: Path::new(&root),
            environment: &environment,
            stdin: &null,
            stdout: &writer,
            stderr: &null,
        })
        .unwrap();
        drop(writer);
        let code = child.wait().unwrap();
        let mut output = Vec::new();
        let mut file = std::fs::File::from(reader);
        file.read_to_end(&mut output).unwrap();
        assert_eq!(code, 0);
        assert!(!output.is_empty());
    }

    #[test]
    fn terminate_ends_the_target() {
        let root = system_root();
        let path = format!(r"{root}\System32");
        let executable = resolve_executable("ping", Path::new(&root), Some(&path), None).unwrap();
        let null = null_handle().unwrap();
        let arguments: Vec<String> = ["ping", "-n", "30", "127.0.0.1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let child = spawn_in_job(&SpawnSpec {
            executable: &executable,
            arguments: &arguments,
            working_directory: Path::new(&root),
            environment: &[("SystemRoot".to_string(), root.clone())],
            stdin: &null,
            stdout: &null,
            stderr: &null,
        })
        .unwrap();
        let started = std::time::Instant::now();
        child.terminate();
        assert_eq!(child.wait().unwrap(), 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn duplicates_own_stream_handles_and_rejects_others() {
        let null = null_handle().unwrap();
        let value = null.as_raw_handle() as usize as u64;
        let copies = duplicate_stream_handles(std::process::id(), &[value]).unwrap();
        assert_eq!(copies.len(), 1);

        let process = unsafe { GetCurrentProcess() };
        let mut real: HANDLE = ptr::null_mut();
        unsafe {
            DuplicateHandle(
                process,
                process,
                process,
                &mut real,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            );
        }
        let real = unsafe { OwnedHandle::from_raw_handle(real) };
        assert!(duplicate_stream_handles(
            std::process::id(),
            &[real.as_raw_handle() as usize as u64]
        )
        .is_err());
    }
}
