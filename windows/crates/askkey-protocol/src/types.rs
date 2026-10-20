use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Limits, PROTOCOL_VERSION};

/// One request frame from the helper to the Broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerRequest {
    pub version: u32,
    pub method: String,
    #[serde(rename = "requestID", default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(rename = "textRun", default, skip_serializing_if = "Option::is_none")]
    pub text_run: Option<RunRequest>,
}

impl BrokerRequest {
    pub fn simple(method: &str) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            method: method.to_owned(),
            request_id: None,
            capability: None,
            text_run: None,
        }
    }

    pub fn ticket(method: &str, request_id: &str, capability: &str) -> Self {
        Self {
            request_id: Some(request_id.to_owned()),
            capability: Some(capability.to_owned()),
            ..Self::simple(method)
        }
    }

    pub fn run(request: RunRequest) -> Self {
        Self {
            text_run: Some(request),
            ..Self::simple("runtime.run")
        }
    }

    /// Envelope fields fit the per-field limit.
    pub fn fields_fit(&self) -> bool {
        [
            Some(&self.method),
            self.request_id.as_ref(),
            self.capability.as_ref(),
        ]
        .into_iter()
        .flatten()
        .all(|field| field.len() <= Limits::MAX_FIELD_BYTES)
    }
}

/// Handle values in the helper's process that the Broker duplicates for the
/// target's standard streams. Absent for MCP runs, whose output is discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamHandles {
    pub stdin: u64,
    pub stdout: u64,
    pub stderr: u64,
}

/// A request to run one target with an explicit credential selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    #[serde(rename = "operationID")]
    pub operation_id: String,
    pub command: Vec<String>,
    pub credential_names: Vec<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub inherited_environment: BTreeMap<String, String>,
    /// Self-declared caller label. Never an authorization or identity input.
    #[serde(default)]
    pub caller_name: Option<String>,
    /// Self-declared purpose. Never an authorization or identity input.
    #[serde(default)]
    pub caller_purpose: Option<String>,
    /// Transport detail; not part of the approval binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streams: Option<StreamHandles>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunBinding<'a> {
    operation_id: &'a str,
    command: &'a [String],
    credential_names: &'a [String],
    working_directory: &'a Option<String>,
    inherited_environment: &'a BTreeMap<String, String>,
    caller_name: &'a Option<String>,
    caller_purpose: &'a Option<String>,
}

impl RunRequest {
    /// Digest of every field an approval binds. Streams are excluded so a
    /// retry from a new helper connection still matches.
    pub fn digest(&self) -> String {
        let binding = RunBinding {
            operation_id: &self.operation_id,
            command: &self.command,
            credential_names: &self.credential_names,
            working_directory: &self.working_directory,
            inherited_environment: &self.inherited_environment,
            caller_name: &self.caller_name,
            caller_purpose: &self.caller_purpose,
        };
        let encoded = serde_json::to_vec(&binding).expect("run binding encodes");
        hex::encode(Sha256::digest(encoded))
    }

    pub fn is_bounded(&self) -> bool {
        let fits = |value: &str| value.len() <= Limits::MAX_FIELD_BYTES && !value.contains('\0');
        !self.operation_id.is_empty()
            && fits(&self.operation_id)
            && !self.command.is_empty()
            && self.command.len() <= Limits::MAX_COMMAND_ARGUMENTS
            && self.command.iter().all(|argument| fits(argument))
            && self.credential_names.len() <= Limits::MAX_CREDENTIALS_PER_RUN
            && self.credential_names.iter().all(|name| fits(name))
            && self.working_directory.as_deref().is_none_or(fits)
            && self.inherited_environment.len() <= Limits::MAX_INHERITED_VARIABLES
            && self
                .inherited_environment
                .iter()
                .all(|(key, value)| fits(key) && fits(value) && !key.contains('='))
            && self.caller_name.as_deref().is_none_or(fits)
            && self.caller_purpose.as_deref().is_none_or(fits)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrokerResponse {
    Ok(BrokerPayload),
    Error(ErrorCode),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrokerPayload {
    Health {
        version: u32,
        status: String,
    },
    #[serde(rename_all = "camelCase")]
    Version {
        protocol_version: u32,
    },
    Catalog {
        credentials: Vec<CatalogItem>,
        groups: Vec<String>,
    },
    RequestStatus {
        state: RequestState,
    },
    Run(RunResult),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunResult {
    Exited(i32),
    ApprovalRequired {
        #[serde(rename = "operationID")]
        operation_id: String,
        tickets: Vec<ApprovalTicket>,
    },
    OutcomeUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalTicket {
    #[serde(rename = "requestID")]
    pub request_id: String,
    pub capability: String,
}

/// Catalog metadata. Never contains a stored value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogItem {
    #[serde(rename = "credentialID")]
    pub credential_id: String,
    pub name: String,
    pub usage_instructions: String,
    /// `None` is Ungrouped and is encoded as an explicit `null`.
    pub group: Option<String>,
    pub components: Vec<CatalogComponent>,
    pub expired: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogComponent {
    pub name: String,
    pub delivery: Delivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Delivery {
    /// The value is injected as this environment variable.
    #[serde(rename_all = "camelCase")]
    Environment { environment_variable: String },
    /// The component is never injected.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Pending,
    Approved,
    Denied,
    Cancelled,
    Expired,
    Completed,
    Consumed,
    OutcomeUnknown,
}

impl RequestState {
    pub fn is_terminal_refusal(self) -> bool {
        matches!(self, Self::Denied | Self::Cancelled | Self::Expired)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    #[error("unsupported protocol version")]
    UnsupportedVersion,
    #[error("method not allowed")]
    MethodNotAllowed,
    #[error("invalid request")]
    InvalidRequest,
    #[error("request or credential not found")]
    RequestNotFound,
    #[error("resource limit reached")]
    ResourceExhausted,
    #[error("deadline exceeded")]
    DeadlineExceeded,
    #[error("internal error")]
    InternalError,
    #[error("agent access is paused in Ask Key")]
    AgentAccessPaused,
    #[error("request rejected")]
    RequestRejected,
    /// The target is not an `.exe` or `.com` file, or was not found. Batch
    /// files are refused because `cmd.exe` parsing defeats argument quoting.
    #[error("target rejected: name an existing .exe or .com program; run batch files through an interpreter you choose")]
    TargetRejected,
    /// The answer exceeds the 64 KiB frame limit, as on macOS. For the
    /// catalog, the person can hide credentials or shorten their usage
    /// instructions.
    #[error("the response exceeds Ask Key's 64 KiB limit; hide unused credentials or shorten their usage instructions in Ask Key")]
    ResponseTooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RunRequest {
        RunRequest {
            operation_id: "op-1".into(),
            command: vec!["git.exe".into(), "push".into()],
            credential_names: vec!["GitHub".into()],
            working_directory: Some(r"C:\work".into()),
            inherited_environment: BTreeMap::from([("PATH".into(), r"C:\Windows".into())]),
            caller_name: Some("Claude Code".into()),
            caller_purpose: None,
            streams: None,
        }
    }

    #[test]
    fn digest_ignores_streams_but_binds_command() {
        let base = sample();
        let mut with_streams = base.clone();
        with_streams.streams = Some(StreamHandles {
            stdin: 1,
            stdout: 2,
            stderr: 3,
        });
        assert_eq!(base.digest(), with_streams.digest());

        let mut changed = base.clone();
        changed.command.push("--force".into());
        assert_ne!(base.digest(), changed.digest());
    }

    #[test]
    fn encodes_wire_names() {
        let json = serde_json::to_value(BrokerRequest::run(sample())).unwrap();
        assert_eq!(json["method"], "runtime.run");
        assert_eq!(json["textRun"]["operationID"], "op-1");
        assert_eq!(json["textRun"]["credentialNames"][0], "GitHub");

        let response = BrokerResponse::Ok(BrokerPayload::Run(RunResult::ApprovalRequired {
            operation_id: "op-1".into(),
            tickets: vec![ApprovalTicket {
                request_id: "r".into(),
                capability: "c".into(),
            }],
        }));
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(
            json["ok"]["run"]["approvalRequired"]["tickets"][0]["requestID"],
            "r"
        );
        let error =
            serde_json::to_value(BrokerResponse::Error(ErrorCode::AgentAccessPaused)).unwrap();
        assert_eq!(error["error"], "agent_access_paused");
    }

    #[test]
    fn group_null_is_explicit() {
        let item = CatalogItem {
            credential_id: "id".into(),
            name: "n".into(),
            usage_instructions: String::new(),
            group: None,
            components: vec![CatalogComponent {
                name: "token".into(),
                delivery: Delivery::None,
            }],
            expired: false,
        };
        let json = serde_json::to_value(item).unwrap();
        assert!(json.as_object().unwrap().contains_key("group"));
        assert_eq!(json["components"][0]["delivery"]["kind"], "none");
    }

    #[test]
    fn rejects_unbounded_requests() {
        let mut request = sample();
        assert!(request.is_bounded());
        request.command.clear();
        assert!(!request.is_bounded());
        let mut request = sample();
        request.command.push("a\0b".into());
        assert!(!request.is_bounded());
    }
}
