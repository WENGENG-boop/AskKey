//! `askkey run`: the CLI route. The target's streams are anonymous pipes
//! that the Broker duplicates from this process; the helper copies their
//! bytes to its own standard streams without inspecting them.

use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use askkey_protocol::{
    inherited_variable_allowed, ApprovalTicket, BrokerPayload, BrokerRequest, BrokerResponse,
    Limits, RequestState, RunRequest, RunResult, StreamHandles,
};

use crate::client::{self, ClientError, Connection};

pub struct RunOptions {
    pub credentials: Vec<String>,
    pub operation_id: String,
    pub caller_name: Option<String>,
    pub caller_purpose: Option<String>,
    pub wait_for_approval: bool,
    pub command: Vec<String>,
}

/// The caller's environment, reduced to what a target may inherit.
pub fn inherited_environment() -> std::collections::BTreeMap<String, String> {
    std::env::vars()
        .filter(|(name, _)| inherited_variable_allowed(name))
        .collect()
}

fn copy_thread<R: Read + Send + 'static, W: Write + Send + 'static>(
    mut from: R,
    mut to: W,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            match from.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if to.write_all(&buffer[..count]).is_err() {
                        break;
                    }
                    let _ = to.flush();
                }
            }
        }
    })
}

/// Sends one run request with fresh stream pipes and relays the target's
/// output until it exits.
fn run_once(request: &RunRequest) -> Result<RunResult, ClientError> {
    let pipe = || askkey_system::anonymous_pipe().map_err(ClientError::Io);
    let (child_stdin, stdin_writer) = pipe()?;
    let (stdout_reader, child_stdout) = pipe()?;
    let (stderr_reader, child_stderr) = pipe()?;
    let value = |handle: &OwnedHandle| handle.as_raw_handle() as usize as u64;
    let mut request = request.clone();
    request.streams = Some(StreamHandles {
        stdin: value(&child_stdin),
        stdout: value(&child_stdout),
        stderr: value(&child_stderr),
    });

    // Not joined: a read of an idle console would block forever.
    copy_thread(std::io::stdin(), std::fs::File::from(stdin_writer));
    let stdout = copy_thread(std::fs::File::from(stdout_reader), std::io::stdout());
    let stderr = copy_thread(std::fs::File::from(stderr_reader), std::io::stderr());

    let response =
        Connection::open().and_then(|mut connection| connection.send(&BrokerRequest::run(request)));
    // The target and the Broker's copies are gone once the response arrives;
    // closing ours lets the relays reach end of file.
    drop((child_stdin, child_stdout, child_stderr));
    let _ = stdout.join();
    let _ = stderr.join();
    match response? {
        BrokerResponse::Ok(BrokerPayload::Run(result)) => Ok(result),
        BrokerResponse::Error(code) => Err(ClientError::Broker(code)),
        BrokerResponse::Ok(_) => Err(ClientError::Protocol),
    }
}

fn status(ticket: &ApprovalTicket) -> Result<RequestState, ClientError> {
    match client::request(&BrokerRequest::ticket(
        "request.status",
        &ticket.request_id,
        &ticket.capability,
    ))? {
        BrokerPayload::RequestStatus { state } => Ok(state),
        _ => Err(ClientError::Protocol),
    }
}

/// Waits in this process until every ticket is approved, then reruns the
/// identical request. Gives up on refusal or after the approval lifetime.
fn wait_and_retry(
    request: &RunRequest,
    tickets: &[ApprovalTicket],
) -> Result<RunResult, ClientError> {
    eprintln!("Ask Key: waiting for approval in the Ask Key window (up to 5 minutes). Keep this process running.");
    let deadline = Instant::now() + Limits::APPROVAL_LIFETIME + Duration::from_secs(5);
    loop {
        let mut all_approved = true;
        for ticket in tickets {
            match status(ticket)? {
                RequestState::Approved => {}
                RequestState::Pending => all_approved = false,
                state => {
                    eprintln!(
                        "Ask Key: the request was not approved ({}).",
                        serde_json::to_string(&state).unwrap_or_default()
                    );
                    return Err(ClientError::Broker(
                        askkey_protocol::ErrorCode::RequestRejected,
                    ));
                }
            }
        }
        if all_approved {
            return run_once(request);
        }
        if Instant::now() > deadline {
            return Err(ClientError::Broker(
                askkey_protocol::ErrorCode::DeadlineExceeded,
            ));
        }
        std::thread::sleep(Duration::from_millis(750));
    }
}

pub fn run(options: RunOptions) -> i32 {
    let request = RunRequest {
        operation_id: options.operation_id,
        command: options.command,
        credential_names: options.credentials,
        working_directory: std::env::current_dir()
            .ok()
            .map(|directory| directory.to_string_lossy().into_owned()),
        inherited_environment: inherited_environment(),
        caller_name: options.caller_name,
        caller_purpose: options.caller_purpose,
        streams: None,
    };
    let mut result = run_once(&request);
    if let (true, Ok(RunResult::ApprovalRequired { tickets, .. })) =
        (options.wait_for_approval, &result)
    {
        let tickets = tickets.clone();
        result = wait_and_retry(&request, &tickets);
    }
    match result {
        Ok(RunResult::Exited(code)) => code,
        Ok(RunResult::OutcomeUnknown) => {
            eprintln!(
                "Ask Key could not determine whether the target started; it was not retried."
            );
            1
        }
        Ok(pending @ RunResult::ApprovalRequired { .. }) => {
            println!("{}", serde_json::to_string(&pending).unwrap_or_default());
            eprintln!(
                "Ask Key: approval required. Approve it in Ask Key, then rerun the identical command with the same --operation-id."
            );
            1
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}
