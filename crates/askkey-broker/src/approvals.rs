//! Operation and approval bookkeeping.
//!
//! Each run operation is identified by the caller's operation ID and bound to
//! the digest of its whole request. An approval belongs to exactly one
//! operation, can be consumed once, and expires after five minutes. Only an
//! identical retry continues an operation; changed content is rejected.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use askkey_protocol::{ApprovalTicket, ErrorCode, Limits, RequestState};
use rand::RngCore;
use serde::Serialize;

/// What the App shows the person. Derived inside the Broker from the same
/// request the approval binds; values are never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalSummary {
    #[serde(rename = "requestID")]
    pub request_id: String,
    pub command_line: String,
    pub executable: String,
    pub working_directory: String,
    pub credential_names: Vec<String>,
    pub environment_variables: Vec<String>,
    /// Self-declared and unverified.
    pub caller_name: Option<String>,
    /// Self-declared and unverified.
    pub caller_purpose: Option<String>,
    pub created_at: u64,
    pub expires_at: u64,
}

impl ApprovalSummary {
    /// File name of the executable, the only part kept in access records.
    pub fn executable_name(&self) -> String {
        executable_name(&self.executable)
    }
}

pub(crate) fn executable_name(command: &str) -> String {
    command
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(command)
        .to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Credentials are being resolved; retries wait for the outcome.
    Resolving,
    AwaitingApproval,
    Approved,
    Running,
    Completed(i32),
    Refused(RequestState),
    Unknown,
}

impl Status {
    fn is_active(self) -> bool {
        matches!(
            self,
            Self::Resolving | Self::AwaitingApproval | Self::Approved | Self::Running
        )
    }

    fn request_state(self) -> RequestState {
        match self {
            Self::Resolving | Self::AwaitingApproval => RequestState::Pending,
            Self::Approved => RequestState::Approved,
            Self::Running => RequestState::Consumed,
            Self::Completed(_) => RequestState::Completed,
            Self::Refused(state) => state,
            Self::Unknown => RequestState::OutcomeUnknown,
        }
    }
}

struct Ticket {
    request_id: String,
    capability: String,
    deadline: Instant,
    summary: ApprovalSummary,
}

struct Operation {
    digest: String,
    status: Status,
    ticket: Option<Ticket>,
}

#[derive(Default)]
struct State {
    operations: HashMap<String, Operation>,
    order: VecDeque<String>,
}

#[derive(Default)]
pub struct Approvals {
    state: Mutex<State>,
}

/// Where an operation stands when a run request arrives.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Begin {
    /// A new operation, now reserved; resolve credentials and then call
    /// [`Approvals::start`], [`Approvals::request_approval`] or
    /// [`Approvals::abandon`].
    New,
    /// The approval was consumed by this call; launch now.
    Approved,
    /// Still waiting for the person.
    Pending(ApprovalTicket),
    /// Already finished during this App runtime; return its status without
    /// running again or replaying output.
    Completed(i32),
    OutcomeUnknown,
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buffer);
    hex::encode(buffer)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

impl State {
    fn sweep(&mut self, now: Instant) {
        for operation in self.operations.values_mut() {
            let expired = operation
                .ticket
                .as_ref()
                .is_some_and(|ticket| ticket.deadline <= now);
            if expired
                && matches!(
                    operation.status,
                    Status::AwaitingApproval | Status::Approved
                )
            {
                operation.status = Status::Refused(RequestState::Expired);
            }
        }
    }

    fn reserve(&mut self, operation_id: &str, digest: &str) -> Result<(), ErrorCode> {
        while self.operations.len() >= Limits::MAX_RETAINED_OPERATIONS {
            let position = self
                .order
                .iter()
                .position(|id| {
                    self.operations
                        .get(id)
                        .is_none_or(|operation| !operation.status.is_active())
                })
                .ok_or(ErrorCode::ResourceExhausted)?;
            let evicted = self.order.remove(position).expect("position is in range");
            self.operations.remove(&evicted);
        }
        self.operations.insert(
            operation_id.to_owned(),
            Operation {
                digest: digest.to_owned(),
                status: Status::Resolving,
                ticket: None,
            },
        );
        self.order.push_back(operation_id.to_owned());
        Ok(())
    }

    fn by_ticket(
        &mut self,
        request_id: &str,
        capability: &str,
    ) -> Result<&mut Operation, ErrorCode> {
        self.operations
            .values_mut()
            .find(|operation| {
                operation.ticket.as_ref().is_some_and(|ticket| {
                    ticket.request_id == request_id
                        && constant_time_eq(&ticket.capability, capability)
                })
            })
            // A wrong capability looks exactly like an unknown request.
            .ok_or(ErrorCode::RequestNotFound)
    }

    fn set(&mut self, operation_id: &str, status: Status) {
        if let Some(operation) = self.operations.get_mut(operation_id) {
            operation.status = status;
        }
    }
}

impl Approvals {
    fn lock(&self) -> MutexGuard<'_, State> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.sweep(Instant::now());
        state
    }

    pub(crate) fn begin(&self, operation_id: &str, digest: &str) -> Result<Begin, ErrorCode> {
        let mut state = self.lock();
        let Some(operation) = state.operations.get_mut(operation_id) else {
            state.reserve(operation_id, digest)?;
            return Ok(Begin::New);
        };
        if operation.digest != digest {
            return Err(ErrorCode::RequestRejected);
        }
        match operation.status {
            Status::Approved => {
                operation.status = Status::Running;
                Ok(Begin::Approved)
            }
            Status::AwaitingApproval => {
                let ticket = operation
                    .ticket
                    .as_ref()
                    .expect("awaiting operations have a ticket");
                Ok(Begin::Pending(ApprovalTicket {
                    request_id: ticket.request_id.clone(),
                    capability: ticket.capability.clone(),
                }))
            }
            Status::Completed(code) => Ok(Begin::Completed(code)),
            Status::Unknown => Ok(Begin::OutcomeUnknown),
            Status::Resolving | Status::Running | Status::Refused(_) => {
                Err(ErrorCode::RequestRejected)
            }
        }
    }

    /// The reserved operation needs no approval and starts now.
    pub(crate) fn start(&self, operation_id: &str) {
        self.lock().set(operation_id, Status::Running);
    }

    /// Creates the approval ticket for a reserved operation.
    pub(crate) fn request_approval(
        &self,
        operation_id: &str,
        mut summary: ApprovalSummary,
    ) -> Result<ApprovalTicket, ErrorCode> {
        let mut state = self.lock();
        let pending = state
            .operations
            .values()
            .filter(|operation| operation.status == Status::AwaitingApproval)
            .count();
        if pending >= Limits::MAX_PENDING_APPROVALS {
            state.operations.remove(operation_id);
            state.order.retain(|id| id != operation_id);
            return Err(ErrorCode::ResourceExhausted);
        }
        let ticket = ApprovalTicket {
            request_id: random_hex(16),
            capability: random_hex(32),
        };
        let lifetime: Duration = Limits::APPROVAL_LIFETIME;
        summary.request_id = ticket.request_id.clone();
        summary.created_at = unix_now();
        summary.expires_at = summary.created_at + lifetime.as_secs();
        let operation = state
            .operations
            .get_mut(operation_id)
            .ok_or(ErrorCode::InternalError)?;
        operation.status = Status::AwaitingApproval;
        operation.ticket = Some(Ticket {
            request_id: ticket.request_id.clone(),
            capability: ticket.capability.clone(),
            deadline: Instant::now() + lifetime,
            summary,
        });
        Ok(ticket)
    }

    /// The operation never started; a retry with the same ID starts over.
    pub(crate) fn abandon(&self, operation_id: &str) {
        let mut state = self.lock();
        state.operations.remove(operation_id);
        state.order.retain(|id| id != operation_id);
    }

    pub(crate) fn complete(&self, operation_id: &str, code: i32) {
        self.lock().set(operation_id, Status::Completed(code));
    }

    pub(crate) fn mark_unknown(&self, operation_id: &str) {
        self.lock().set(operation_id, Status::Unknown);
    }

    pub(crate) fn status(
        &self,
        request_id: &str,
        capability: &str,
    ) -> Result<RequestState, ErrorCode> {
        Ok(self
            .lock()
            .by_ticket(request_id, capability)?
            .status
            .request_state())
    }

    pub(crate) fn cancel(
        &self,
        request_id: &str,
        capability: &str,
    ) -> Result<RequestState, ErrorCode> {
        let mut state = self.lock();
        let operation = state.by_ticket(request_id, capability)?;
        if matches!(
            operation.status,
            Status::AwaitingApproval | Status::Approved
        ) {
            operation.status = Status::Refused(RequestState::Cancelled);
        }
        Ok(operation.status.request_state())
    }

    pub(crate) fn decide(
        &self,
        request_id: &str,
        approve: bool,
    ) -> Result<ApprovalSummary, ErrorCode> {
        let mut state = self.lock();
        let operation = state
            .operations
            .values_mut()
            .find(|operation| {
                operation
                    .ticket
                    .as_ref()
                    .is_some_and(|ticket| ticket.request_id == request_id)
            })
            .ok_or(ErrorCode::RequestNotFound)?;
        if operation.status != Status::AwaitingApproval {
            return Err(ErrorCode::RequestRejected);
        }
        operation.status = if approve {
            Status::Approved
        } else {
            Status::Refused(RequestState::Denied)
        };
        Ok(operation
            .ticket
            .as_ref()
            .expect("awaiting operations have a ticket")
            .summary
            .clone())
    }

    pub(crate) fn pending(&self) -> Vec<ApprovalSummary> {
        let state = self.lock();
        let mut summaries: Vec<ApprovalSummary> = state
            .operations
            .values()
            .filter(|operation| operation.status == Status::AwaitingApproval)
            .filter_map(|operation| {
                operation
                    .ticket
                    .as_ref()
                    .map(|ticket| ticket.summary.clone())
            })
            .collect();
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.created_at));
        summaries
    }

    pub(crate) fn cancel_all_pending(&self) -> usize {
        let mut state = self.lock();
        let mut cancelled = 0;
        for operation in state.operations.values_mut() {
            if matches!(
                operation.status,
                Status::AwaitingApproval | Status::Approved
            ) {
                operation.status = Status::Refused(RequestState::Cancelled);
                cancelled += 1;
            }
        }
        cancelled
    }

    #[cfg(test)]
    pub(crate) fn expire_all_for_test(&self) {
        let mut state = self.state.lock().unwrap();
        for operation in state.operations.values_mut() {
            if let Some(ticket) = operation.ticket.as_mut() {
                ticket.deadline = Instant::now() - Duration::from_secs(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> ApprovalSummary {
        ApprovalSummary {
            request_id: String::new(),
            command_line: "git.exe push".into(),
            executable: r"C:\Program Files\Git\cmd\git.exe".into(),
            working_directory: r"C:\work".into(),
            credential_names: vec!["GitHub".into()],
            environment_variables: vec!["GH_TOKEN".into()],
            caller_name: None,
            caller_purpose: None,
            created_at: 0,
            expires_at: 0,
        }
    }

    fn pending(approvals: &Approvals, operation: &str) -> ApprovalTicket {
        assert_eq!(approvals.begin(operation, "digest").unwrap(), Begin::New);
        approvals.request_approval(operation, summary()).unwrap()
    }

    #[test]
    fn approval_is_consumed_once() {
        let approvals = Approvals::default();
        let ticket = pending(&approvals, "op");
        assert_eq!(
            approvals.begin("op", "digest").unwrap(),
            Begin::Pending(ticket.clone())
        );
        approvals.decide(&ticket.request_id, true).unwrap();
        assert_eq!(
            approvals
                .status(&ticket.request_id, &ticket.capability)
                .unwrap(),
            RequestState::Approved
        );
        assert_eq!(approvals.begin("op", "digest").unwrap(), Begin::Approved);
        // A concurrent identical retry cannot consume it again.
        assert_eq!(
            approvals.begin("op", "digest"),
            Err(ErrorCode::RequestRejected)
        );
        approvals.complete("op", 7);
        assert_eq!(
            approvals.begin("op", "digest").unwrap(),
            Begin::Completed(7)
        );
    }

    #[test]
    fn changed_payload_is_rejected() {
        let approvals = Approvals::default();
        let ticket = pending(&approvals, "op");
        approvals.decide(&ticket.request_id, true).unwrap();
        assert_eq!(
            approvals.begin("op", "other-digest"),
            Err(ErrorCode::RequestRejected)
        );
    }

    #[test]
    fn denial_and_cancellation_stop_the_operation() {
        let approvals = Approvals::default();
        let denied = pending(&approvals, "denied");
        approvals.decide(&denied.request_id, false).unwrap();
        assert_eq!(
            approvals.begin("denied", "digest"),
            Err(ErrorCode::RequestRejected)
        );
        assert_eq!(
            approvals
                .status(&denied.request_id, &denied.capability)
                .unwrap(),
            RequestState::Denied
        );

        let cancelled = pending(&approvals, "cancelled");
        assert_eq!(
            approvals
                .cancel(&cancelled.request_id, &cancelled.capability)
                .unwrap(),
            RequestState::Cancelled
        );
        assert!(approvals.decide(&cancelled.request_id, true).is_err());
    }

    #[test]
    fn wrong_capability_looks_unknown() {
        let approvals = Approvals::default();
        let ticket = pending(&approvals, "op");
        assert_eq!(
            approvals.status(&ticket.request_id, "guess"),
            Err(ErrorCode::RequestNotFound)
        );
        assert_eq!(
            approvals.cancel(&ticket.request_id, "guess"),
            Err(ErrorCode::RequestNotFound)
        );
    }

    #[test]
    fn approvals_expire() {
        let approvals = Approvals::default();
        let ticket = pending(&approvals, "op");
        approvals.decide(&ticket.request_id, true).unwrap();
        approvals.expire_all_for_test();
        assert_eq!(
            approvals
                .status(&ticket.request_id, &ticket.capability)
                .unwrap(),
            RequestState::Expired
        );
        assert_eq!(
            approvals.begin("op", "digest"),
            Err(ErrorCode::RequestRejected)
        );
        assert!(approvals.pending().is_empty());
    }

    #[test]
    fn abandoned_operations_can_start_over() {
        let approvals = Approvals::default();
        assert_eq!(approvals.begin("op", "digest").unwrap(), Begin::New);
        assert_eq!(
            approvals.begin("op", "digest"),
            Err(ErrorCode::RequestRejected)
        );
        approvals.abandon("op");
        assert_eq!(approvals.begin("op", "digest").unwrap(), Begin::New);
    }

    #[test]
    fn pause_cancels_pending() {
        let approvals = Approvals::default();
        let ticket = pending(&approvals, "op");
        assert_eq!(approvals.cancel_all_pending(), 1);
        assert_eq!(
            approvals
                .status(&ticket.request_id, &ticket.capability)
                .unwrap(),
            RequestState::Cancelled
        );
    }

    #[test]
    fn retention_keeps_active_operations() {
        let approvals = Approvals::default();
        for index in 0..Limits::MAX_RETAINED_OPERATIONS {
            let id = format!("op-{index}");
            assert_eq!(approvals.begin(&id, "d").unwrap(), Begin::New);
            approvals.start(&id);
            approvals.complete(&id, 0);
        }
        assert_eq!(approvals.begin("fresh", "d").unwrap(), Begin::New);
        // The oldest completed operation was evicted.
        assert_eq!(approvals.begin("op-0", "d").unwrap(), Begin::New);
    }
}
