//! Windows primitives used by the Ask Key App, Broker and helper.
//!
//! Everything that calls Win32 directly lives here so the other crates stay
//! free of `unsafe`.

mod acl;
mod command_line;
mod dpapi;
mod identity;
mod spawn;
mod wide;
mod window;

pub use acl::{protect_private_path, PrivateSecurityAttributes};
pub use command_line::{build_command_line, environment_block, resolve_executable, ResolveError};
pub use dpapi::{protect, unprotect};
pub use identity::{
    current_user_sid, pipe_client_process_id, pipe_server_process_id, process_image_path,
};
pub use spawn::{
    anonymous_pipe, duplicate_stream_handles, null_handle, pipe_bytes_available, spawn_in_job,
    JobChild, SpawnSpec,
};
pub use window::activate_window;

/// Last Win32 error as an `io::Error`.
pub(crate) fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}
