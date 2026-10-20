//! `runtime.run`: authorize an explicit credential selection, then launch the
//! target with approved values in its environment.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use askkey_protocol::{
    inherited_variable_allowed, BrokerPayload, BrokerResponse, ErrorCode, Limits, RunRequest,
    RunResult,
};
use askkey_system::{
    build_command_line, duplicate_stream_handles, null_handle, resolve_executable, spawn_in_job,
    JobChild, SpawnSpec,
};

use crate::approvals::{executable_name, ApprovalSummary, Begin, Binding};
use crate::environment::lookup;
use crate::{Access, AccessEvent, AccessOutcome, Broker, ResolvedCredential};

struct Resolved {
    credentials: Vec<ResolvedCredential>,
    executable: PathBuf,
}

impl Resolved {
    fn binding(&self) -> Binding {
        Binding {
            credentials: self
                .credentials
                .iter()
                .map(|credential| (credential.id.clone(), credential.revision.clone()))
                .collect(),
            executable: self.executable.clone(),
        }
    }

    fn names(&self) -> Vec<String> {
        self.credentials
            .iter()
            .map(|credential| credential.name.clone())
            .collect()
    }
}

fn ok(result: RunResult) -> BrokerResponse {
    BrokerResponse::Ok(BrokerPayload::Run(result))
}

/// Characters that could disguise what an approval shows: controls (Cc),
/// every format character (Cf) such as bidirectional overrides and
/// zero-width joiners, and the line and paragraph separators (Zl, Zp).
fn deceptive(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{ad}'
                | '\u{600}'..='\u{605}'
                | '\u{61c}'
                | '\u{6dd}'
                | '\u{70f}'
                | '\u{890}'..='\u{891}'
                | '\u{8e2}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{110bd}'
                | '\u{110cd}'
                | '\u{13430}'..='\u{1343f}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0001}'
                | '\u{e0020}'..='\u{e007f}'
        )
}

/// Every field an approval shows goes through this, so a crafted argument,
/// path or label cannot disguise the prompt.
pub(crate) fn displayable(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if deceptive(c) {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn all_displayable(texts: impl IntoIterator<Item = String>) -> Vec<String> {
    texts.into_iter().map(|text| displayable(&text)).collect()
}

fn working_directory(request: &RunRequest) -> Result<PathBuf, ErrorCode> {
    let directory = request
        .working_directory
        .as_deref()
        .map(PathBuf::from)
        .ok_or(ErrorCode::InvalidRequest)?;
    if !directory.is_absolute() || !directory.is_dir() {
        return Err(ErrorCode::InvalidRequest);
    }
    Ok(directory)
}

fn resolve_target(request: &RunRequest, directory: &Path) -> Result<PathBuf, ErrorCode> {
    let environment = &request.inherited_environment;
    resolve_executable(
        &request.command[0],
        directory,
        lookup(environment, "PATH"),
        lookup(environment, "PATHEXT"),
    )
    .map_err(|_| ErrorCode::TargetRejected)
}

impl Broker {
    fn resolve_credentials(
        &self,
        request: &RunRequest,
    ) -> Result<Vec<ResolvedCredential>, ErrorCode> {
        let mut credentials: Vec<ResolvedCredential> = Vec::new();
        for name in &request.credential_names {
            let credential = self
                .provider
                .resolve(name)?
                .ok_or(ErrorCode::RequestNotFound)?;
            if credential.expired {
                return Err(ErrorCode::RequestRejected);
            }
            if !credentials
                .iter()
                .any(|existing| existing.id == credential.id)
            {
                credentials.push(credential);
            }
        }
        let mut variables: Vec<String> = Vec::new();
        for variable in credentials
            .iter()
            .flat_map(|credential| &credential.environment_variables)
        {
            let key = variable.to_ascii_uppercase();
            if variables.contains(&key) {
                return Err(ErrorCode::InvalidRequest);
            }
            variables.push(key);
        }
        Ok(credentials)
    }

    fn resolve(&self, request: &RunRequest, directory: &Path) -> Result<Resolved, ErrorCode> {
        Ok(Resolved {
            credentials: self.resolve_credentials(request)?,
            executable: resolve_target(request, directory)?,
        })
    }

    fn summary(
        &self,
        request: &RunRequest,
        directory: &Path,
        resolved: &Resolved,
    ) -> ApprovalSummary {
        ApprovalSummary {
            request_id: String::new(),
            command_line: displayable(&build_command_line(&request.command)),
            executable: displayable(&resolved.executable.to_string_lossy()),
            working_directory: displayable(&directory.to_string_lossy()),
            credential_names: all_displayable(resolved.names()),
            environment_variables: all_displayable(
                resolved
                    .credentials
                    .iter()
                    .flat_map(|credential| credential.environment_variables.clone()),
            ),
            caller_name: request.caller_name.as_deref().map(displayable),
            caller_purpose: request.caller_purpose.as_deref().map(displayable),
            created_at: 0,
            expires_at: 0,
        }
    }

    /// `credential_names` are the resolved names whenever resolution
    /// succeeded, so a record names the credentials that were actually used.
    fn record(&self, credential_names: Vec<String>, request: &RunRequest, outcome: AccessOutcome) {
        self.provider.record(AccessEvent {
            credential_names,
            executable: executable_name(&request.command[0]),
            caller_name: request.caller_name.clone(),
            outcome,
        });
    }

    pub(crate) async fn run<D: Future<Output = ()>>(
        self: &Arc<Self>,
        request: RunRequest,
        client_pid: u32,
        disconnected: D,
    ) -> BrokerResponse {
        if !request.is_bounded() || request.credential_names.is_empty() {
            return BrokerResponse::Error(ErrorCode::InvalidRequest);
        }
        if self.provider.agent_access_paused() {
            return BrokerResponse::Error(ErrorCode::AgentAccessPaused);
        }
        let directory = match working_directory(&request) {
            Ok(directory) => directory,
            Err(code) => return BrokerResponse::Error(code),
        };
        let operation = request.operation_id.clone();
        let begin = self.approvals.begin(&operation, &request.digest());
        if self.approvals.take_expired() {
            self.notify();
        }
        let begin = match begin {
            Ok(begin) => begin,
            Err(code) => return BrokerResponse::Error(code),
        };
        let (binding, approved) = match begin {
            Begin::Pending(ticket) => {
                return ok(RunResult::ApprovalRequired {
                    operation_id: operation,
                    tickets: vec![ticket],
                })
            }
            Begin::Completed(code) => return ok(RunResult::Exited(code)),
            Begin::OutcomeUnknown => return ok(RunResult::OutcomeUnknown),
            Begin::Approved(binding) => (binding, true),
            Begin::New => {
                let resolved = match self.resolve(&request, &directory) {
                    Ok(resolved) => resolved,
                    Err(code) => {
                        self.approvals.abandon(&operation);
                        if code == ErrorCode::RequestNotFound {
                            let requested = request.credential_names.clone();
                            self.record(requested, &request, AccessOutcome::Unavailable);
                        }
                        return BrokerResponse::Error(code);
                    }
                };
                if resolved
                    .credentials
                    .iter()
                    .any(|credential| credential.access == Access::Ask)
                {
                    let summary = self.summary(&request, &directory, &resolved);
                    let binding = resolved.binding();
                    return match self
                        .approvals
                        .request_approval(&operation, summary, binding)
                    {
                        Ok(ticket) => {
                            self.notify_new_request();
                            self.report_expiry_later();
                            ok(RunResult::ApprovalRequired {
                                operation_id: operation,
                                tickets: vec![ticket],
                            })
                        }
                        Err(code) => BrokerResponse::Error(code),
                    };
                }
                let binding = resolved.binding();
                self.approvals.start(&operation, binding.clone());
                (binding, false)
            }
        };
        match self
            .launch(
                &request,
                &directory,
                &binding,
                approved,
                client_pid,
                disconnected,
            )
            .await
        {
            Ok(response) => response,
            Err(code) => {
                // Nothing started: the operation can be retried from scratch.
                self.approvals.abandon(&operation);
                BrokerResponse::Error(code)
            }
        }
    }

    /// Tells the presenter once a new ticket's lifetime has passed, so an
    /// expired request leaves the App even when no other request arrives.
    fn report_expiry_later(self: &Arc<Self>) {
        let broker = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(Limits::APPROVAL_LIFETIME + Duration::from_secs(1)).await;
            if let Some(broker) = broker.upgrade() {
                if broker.approvals.take_expired() {
                    broker.notify();
                }
            }
        });
    }

    /// The final launch boundary: re-check everything the authorization
    /// depended on, consume it, then decrypt, deliver and create exactly the
    /// bound executable, all while holding the gate that pause and credential
    /// changes wait on. Returns the child and the credential names used.
    fn authorize_and_spawn(
        &self,
        request: &RunRequest,
        directory: &Path,
        binding: &Binding,
        approved: bool,
        client_pid: u32,
    ) -> Result<(JobChild, Vec<String>), ErrorCode> {
        let _gate = self
            .gate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.provider.agent_access_paused() {
            return Err(ErrorCode::AgentAccessPaused);
        }
        let resolved = self.resolve(request, directory)?;
        // The authorization covers exactly the credential revisions and the
        // executable it was decided on; an edit, permission change,
        // re-creation or a different program on PATH needs a new one.
        if &resolved.binding() != binding {
            return Err(ErrorCode::RequestRejected);
        }
        if !approved
            && resolved
                .credentials
                .iter()
                .any(|credential| credential.access != Access::Allow)
        {
            return Err(ErrorCode::RequestRejected);
        }
        self.approvals.consume(&request.operation_id)?;

        let mut environment: Vec<(String, String)> = request
            .inherited_environment
            .iter()
            .filter(|(name, _)| inherited_variable_allowed(name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let mut delivered = Vec::new();
        for credential in &resolved.credentials {
            delivered.extend(
                self.provider
                    .deliver(&credential.id, &credential.revision)?,
            );
        }
        // Delivered values replace inherited variables of the same name.
        environment.extend(
            delivered
                .iter()
                .map(|(name, value)| (name.clone(), value.to_string())),
        );
        drop(delivered);

        let streams = match request.streams {
            Some(streams) => duplicate_stream_handles(
                client_pid,
                &[streams.stdin, streams.stdout, streams.stderr],
            )
            .map_err(|_| ErrorCode::InvalidRequest)?,
            None => {
                let null = || null_handle().map_err(|_| ErrorCode::InternalError);
                vec![null()?, null()?, null()?]
            }
        };
        let child = spawn_in_job(&SpawnSpec {
            executable: &binding.executable,
            arguments: &request.command,
            working_directory: directory,
            environment: &environment,
            stdin: &streams[0],
            stdout: &streams[1],
            stderr: &streams[2],
        });
        // Our copies of the values and streams are no longer needed.
        for (_, value) in environment.iter_mut() {
            zeroize::Zeroize::zeroize(value);
        }
        drop(streams);
        let child = child.map_err(|_| ErrorCode::InternalError)?;
        Ok((child, resolved.names()))
    }

    pub(crate) async fn launch<D: Future<Output = ()>>(
        self: &Arc<Self>,
        request: &RunRequest,
        directory: &Path,
        binding: &Binding,
        approved: bool,
        client_pid: u32,
        disconnected: D,
    ) -> Result<BrokerResponse, ErrorCode> {
        let (child, names) =
            self.authorize_and_spawn(request, directory, binding, approved, client_pid)?;
        let child = Arc::new(child);
        self.record(
            names,
            request,
            if approved {
                AccessOutcome::Approved
            } else {
                AccessOutcome::Allowed
            },
        );

        let waiter = {
            let child = child.clone();
            tokio::task::spawn_blocking(move || child.wait())
        };
        tokio::pin!(waiter);
        let exit = tokio::select! {
            exit = &mut waiter => exit,
            () = disconnected => {
                // The helper went away (cancelled or killed): stop the target.
                child.terminate();
                waiter.await
            }
        };
        let operation = &request.operation_id;
        Ok(match exit {
            Ok(Ok(code)) => {
                self.approvals.complete(operation, code);
                ok(RunResult::Exited(code))
            }
            _ => {
                self.approvals.mark_unknown(operation);
                ok(RunResult::OutcomeUnknown)
            }
        })
    }
}
