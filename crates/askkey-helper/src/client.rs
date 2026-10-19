//! Connection to the App's Broker.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use askkey_protocol::{read_frame, write_frame, BrokerRequest, BrokerResponse, ErrorCode};
use windows_sys::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;

/// Executable names the official App may have next to this helper.
const APP_NAMES: [&str; 2] = ["Ask Key.exe", "askkey-app.exe"];

#[derive(Debug)]
pub enum ClientError {
    /// The App is not running, or agent access is unavailable.
    Unavailable,
    /// Something answered on the pipe that is not the App next to this helper.
    UntrustedServer(PathBuf),
    Broker(ErrorCode),
    Protocol,
    Io(std::io::Error),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(
                formatter,
                "Ask Key is not running. Start Ask Key and try again."
            ),
            Self::UntrustedServer(path) => {
                write!(
                    formatter,
                    "refusing to talk to an unexpected Broker process: {}",
                    path.display()
                )
            }
            Self::Broker(code) => write!(formatter, "Ask Key: {code}"),
            Self::Protocol => write!(formatter, "Ask Key returned a malformed response"),
            Self::Io(error) => write!(formatter, "Ask Key connection failed: {error}"),
        }
    }
}

fn development() -> bool {
    cfg!(debug_assertions)
}

fn pipe_name() -> Result<String, ClientError> {
    // Development builds may point at a test Broker; release builds cannot.
    if development() {
        if let Ok(name) = std::env::var("ASKKEY_BROKER_PIPE") {
            return Ok(name);
        }
    }
    let sid = askkey_system::current_user_sid().map_err(ClientError::Io)?;
    Ok(askkey_protocol::pipe_name(&sid, development()))
}

fn helper_directory() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

/// The Broker must be the App installed next to this helper.
fn verify_server(pipe: &File) -> Result<(), ClientError> {
    if development() && std::env::var_os("ASKKEY_BROKER_PIPE").is_some() {
        return Ok(());
    }
    let pid = askkey_system::pipe_server_process_id(pipe).map_err(ClientError::Io)?;
    let image = askkey_system::process_image_path(pid).map_err(ClientError::Io)?;
    let directory = helper_directory().ok_or(ClientError::Protocol)?;
    let trusted = APP_NAMES.iter().any(|name| {
        let expected = directory.join(name);
        expected
            .to_string_lossy()
            .eq_ignore_ascii_case(&image.to_string_lossy())
    });
    if trusted {
        Ok(())
    } else {
        Err(ClientError::UntrustedServer(image))
    }
}

pub struct Connection {
    pipe: File,
}

impl Connection {
    pub fn open() -> Result<Self, ClientError> {
        let name = pipe_name()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let opened = OpenOptions::new()
                .read(true)
                .write(true)
                // The Broker may identify, but never impersonate, this client.
                .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
                .open(&name);
            match opened {
                Ok(pipe) => {
                    verify_server(&pipe)?;
                    return Ok(Self { pipe });
                }
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                    return Err(ClientError::Unavailable)
                }
                Err(error) => return Err(ClientError::Io(error)),
            }
        }
    }

    pub fn send(&mut self, request: &BrokerRequest) -> Result<BrokerResponse, ClientError> {
        write_frame(&mut self.pipe, request).map_err(|_| ClientError::Protocol)?;
        read_frame(&mut self.pipe).map_err(|error| match error {
            askkey_protocol::FrameError::Io(error) => ClientError::Io(error),
            _ => ClientError::Protocol,
        })
    }
}

/// One request on a fresh connection.
pub fn request(request: &BrokerRequest) -> Result<askkey_protocol::BrokerPayload, ClientError> {
    match Connection::open()?.send(request)? {
        BrokerResponse::Ok(payload) => Ok(payload),
        BrokerResponse::Error(code) => Err(ClientError::Broker(code)),
    }
}
