//! Broker protocol shared by the Ask Key App and the `askkey` helper.
//!
//! Frames are a 4-byte big-endian length followed by one JSON document. The
//! method names, error codes and limits follow the macOS edition's protocol
//! version 1; the transport is a named pipe instead of a Unix socket.

mod environment;
mod framing;
mod limits;
mod types;

pub use environment::inherited_variable_allowed;
pub use framing::{decode_body, read_frame, write_frame, FrameError};
pub use limits::Limits;
pub use types::*;

/// Protocol version spoken by this build. Incompatible versions fail closed.
pub const PROTOCOL_VERSION: u32 = 1;

/// The named pipe the App's Broker listens on for one Windows user.
///
/// `user_sid` keeps users on a shared machine apart; the pipe's DACL is the
/// actual access control.
pub fn pipe_name(user_sid: &str, development: bool) -> String {
    if development {
        format!(r"\\.\pipe\askkey-broker-dev-{user_sid}")
    } else {
        format!(r"\\.\pipe\askkey-broker-{user_sid}")
    }
}
