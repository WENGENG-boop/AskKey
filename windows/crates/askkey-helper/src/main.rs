//! `askkey`: the restricted helper agents use to reach Ask Key.
//!
//! It has no access to the vault or its key. It forwards requests to the
//! App's Broker, which decides permission, asks the person and launches
//! targets with approved values.

mod client;
mod guide;
mod mcp;
mod run;
mod stdin;

use askkey_protocol::{BrokerPayload, BrokerRequest};

const USAGE: &str = "usage:
  askkey mcp
  askkey run [--wait-for-approval] --credential <name> [--credential <name>...]
             [--operation-id <id>] [--caller-name <name>] [--caller-purpose <text>] -- <program> [args...]
  askkey health | version | status
  askkey open";

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

fn parse_run(arguments: &[String]) -> Option<run::RunOptions> {
    let mut options = run::RunOptions {
        credentials: Vec::new(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        caller_name: None,
        caller_purpose: None,
        wait_for_approval: false,
        command: Vec::new(),
    };
    let mut index = 0;
    while index < arguments.len() && arguments[index] != "--" {
        if arguments[index] == "--wait-for-approval" {
            options.wait_for_approval = true;
            index += 1;
            continue;
        }
        let value = arguments.get(index + 1)?.clone();
        match arguments[index].as_str() {
            "--credential" => options.credentials.push(value),
            "--operation-id" => options.operation_id = value,
            "--caller-name" => options.caller_name = Some(value),
            "--caller-purpose" => options.caller_purpose = Some(value),
            _ => return None,
        }
        index += 2;
    }
    if arguments.get(index).map(String::as_str) != Some("--") {
        return None;
    }
    options.command = arguments[index + 1..].to_vec();
    (!options.command.is_empty()).then_some(options)
}

fn open_app() -> i32 {
    let directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::PathBuf::from));
    for name in ["Ask Key.exe", "askkey-app.exe"] {
        if let Some(path) = directory
            .as_ref()
            .map(|directory| directory.join(name))
            .filter(|path| path.is_file())
        {
            return match std::process::Command::new(path).spawn() {
                Ok(_) => 0,
                Err(error) => {
                    eprintln!("Ask Key could not be started: {error}");
                    1
                }
            };
        }
    }
    eprintln!("Ask Key was not found next to this helper.");
    1
}

fn simple(method: &str) -> i32 {
    match client::request(&BrokerRequest::simple(method)) {
        Ok(BrokerPayload::Health { version, status }) => {
            println!(
                "{}",
                serde_json::json!({"status": status, "protocolVersion": version})
            );
            0
        }
        Ok(BrokerPayload::Version { protocol_version }) => {
            println!(
                "{}",
                serde_json::json!({"protocolVersion": protocol_version, "helperVersion": env!("CARGO_PKG_VERSION")})
            );
            0
        }
        Ok(_) => 1,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let code = match arguments.first().map(String::as_str) {
        Some("mcp") if arguments.len() == 1 => match mcp::serve() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        Some("run") => match parse_run(&arguments[1..]) {
            Some(options) => run::run(options),
            None => usage(),
        },
        Some("health" | "status") if arguments.len() == 1 => simple("health"),
        Some("version") if arguments.len() == 1 => simple("version"),
        Some("open") if arguments.len() == 1 => open_app(),
        _ => usage(),
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_run_arguments() {
        let options = parse_run(&args(&[
            "--wait-for-approval",
            "--credential",
            "GitHub",
            "--operation-id",
            "op-1",
            "--",
            "git.exe",
            "--credential",
        ]))
        .unwrap();
        assert!(options.wait_for_approval);
        assert_eq!(options.credentials, ["GitHub"]);
        assert_eq!(options.operation_id, "op-1");
        assert_eq!(options.command, ["git.exe", "--credential"]);
    }

    #[test]
    fn rejects_malformed_run_arguments() {
        assert!(parse_run(&args(&["--credential", "x"])).is_none());
        assert!(parse_run(&args(&["--credential", "x", "--"])).is_none());
        assert!(parse_run(&args(&["--unknown", "x", "--", "a.exe"])).is_none());
        assert!(parse_run(&args(&["--credential"])).is_none());
    }
}
