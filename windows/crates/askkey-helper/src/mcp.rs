//! MCP over stdio: newline-delimited JSON-RPC 2.0.

use std::io::{BufRead, Write};

use askkey_protocol::{BrokerPayload, BrokerRequest, Limits, RunRequest, RunResult};
use serde_json::{json, Value};

use crate::client::{self, ClientError};
use crate::guide;
use crate::run::inherited_environment;

const LATEST_PROTOCOL: &str = "2025-06-18";

fn tools() -> Value {
    let ticket = json!({
        "type": "object",
        "properties": {
            "request_id": {"type": "string", "description": "requestID from approvalRequired."},
            "capability": {"type": "string", "description": "capability from approvalRequired."}
        },
        "required": ["request_id", "capability"]
    });
    json!([
        {
            "name": "list_credentials",
            "description": format!("{} {}", guide::DISCOVERY, guide::DELIVERY),
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "run",
            "description": format!("Run a program with the selected credentials delivered as environment variables. Output is discarded; only the exit status returns. {} {} {}", guide::WINDOWS, guide::RESUME, guide::cli()),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "credentials": {"type": "array", "items": {"type": "string"}, "description": "Exact visible credential names from list_credentials. Select only those this task needs."},
                    "command": {"type": "array", "items": {"type": "string"}, "description": "Program and arguments, not a shell string."},
                    "cwd": {"type": "string", "description": "Existing absolute working directory. Keep unchanged when retrying after approval."},
                    "operation_id": {"type": "string", "description": "Unique ID for this task; reuse it, with the identical request, when retrying after approval."},
                    "caller_name": {"type": "string", "description": "Your agent name, shown to the user as unverified."},
                    "caller_purpose": {"type": "string", "description": "The user task, shown to the user as unverified."}
                },
                "required": ["credentials", "command", "cwd", "operation_id"]
            }
        },
        {
            "name": "request_status",
            "description": "Check an approval ticket. Does not execute or consume it.",
            "inputSchema": ticket
        },
        {
            "name": "request_cancel",
            "description": "Cancel one incomplete approval request.",
            "inputSchema": ticket
        },
        {
            "name": "connection_status",
            "description": "Check that this helper reaches the local Ask Key Broker.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
}

fn string(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn strings(arguments: &Value, key: &str) -> Option<Vec<String>> {
    arguments
        .get(key)?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn call(name: &str, arguments: &Value) -> Result<Value, String> {
    let broker = |request: BrokerRequest| {
        client::request(&request).map_err(|error: ClientError| error.to_string())
    };
    match name {
        "list_credentials" => match broker(BrokerRequest::simple("catalog"))? {
            BrokerPayload::Catalog {
                credentials,
                groups,
            } => Ok(json!({"credentials": credentials, "groups": groups})),
            _ => Err("unexpected response".into()),
        },
        "run" => {
            let invalid = || "run needs credentials, command, cwd and operation_id".to_owned();
            let request = RunRequest {
                operation_id: string(arguments, "operation_id").ok_or_else(invalid)?,
                command: strings(arguments, "command").ok_or_else(invalid)?,
                credential_names: strings(arguments, "credentials").ok_or_else(invalid)?,
                working_directory: Some(string(arguments, "cwd").ok_or_else(invalid)?),
                inherited_environment: inherited_environment(),
                caller_name: string(arguments, "caller_name"),
                caller_purpose: string(arguments, "caller_purpose"),
                streams: None,
            };
            match broker(BrokerRequest::run(request))? {
                BrokerPayload::Run(RunResult::Exited(code)) => {
                    Ok(json!({"status": "exited", "exitCode": code}))
                }
                BrokerPayload::Run(RunResult::OutcomeUnknown) => {
                    Ok(json!({"status": "outcomeUnknown"}))
                }
                BrokerPayload::Run(RunResult::ApprovalRequired {
                    operation_id,
                    tickets,
                }) => Ok(json!({
                    "status": "approvalRequired",
                    "operationID": operation_id,
                    "tickets": tickets,
                    "next": "The command has not run. After the user approves in Ask Key, call run again with the identical request and this operation_id."
                })),
                _ => Err("unexpected response".into()),
            }
        }
        "request_status" | "request_cancel" => {
            let (Some(id), Some(capability)) = (
                string(arguments, "request_id"),
                string(arguments, "capability"),
            ) else {
                return Err("request_id and capability are required".into());
            };
            let method = if name == "request_status" {
                "request.status"
            } else {
                "request.cancel"
            };
            match broker(BrokerRequest::ticket(method, &id, &capability))? {
                BrokerPayload::RequestStatus { state } => Ok(json!({"state": state})),
                _ => Err("unexpected response".into()),
            }
        }
        "connection_status" => match broker(BrokerRequest::simple("health"))? {
            BrokerPayload::Health { version, status } => {
                Ok(json!({"status": status, "protocolVersion": version}))
            }
            _ => Err("unexpected response".into()),
        },
        _ => Err(format!("unknown tool {name}")),
    }
}

fn result_for(method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(LATEST_PROTOCOL);
            Ok(json!({
                "protocolVersion": requested,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "askkey", "version": env!("CARGO_PKG_VERSION")},
                "instructions": guide::instructions()
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Ok(match call(name, &arguments) {
                Ok(value) => {
                    json!({"content": [{"type": "text", "text": value.to_string()}], "isError": false})
                }
                Err(message) => {
                    json!({"content": [{"type": "text", "text": message}], "isError": true})
                }
            })
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

pub fn serve() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = if line.len() > Limits::MAX_FRAME_BYTES {
            Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "message too large"}}),
            )
        } else {
            match serde_json::from_str::<Value>(&line) {
                Err(_) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}})),
                // Notifications carry no id and get no response.
                Ok(message) => message.get("id").cloned().map(|id| {
                    let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
                    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
                    match result_for(method, &params) {
                        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                        Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
                    }
                }),
            }
        };
        if let Some(response) = response {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_tools_and_initializes() {
        let tools = result_for("tools/list", &json!({})).unwrap();
        let names: Vec<&str> = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "list_credentials",
                "run",
                "request_status",
                "request_cancel",
                "connection_status"
            ]
        );
        let init = result_for("initialize", &json!({"protocolVersion": "2025-03-26"})).unwrap();
        assert_eq!(init["protocolVersion"], "2025-03-26");
        assert_eq!(init["serverInfo"]["name"], "askkey");
        assert!(result_for("resources/list", &json!({})).is_err());
    }

    #[test]
    fn run_validates_arguments() {
        let result = result_for(
            "tools/call",
            &json!({"name": "run", "arguments": {"command": ["git.exe"]}}),
        )
        .unwrap();
        assert_eq!(result["isError"], true);
    }
}
