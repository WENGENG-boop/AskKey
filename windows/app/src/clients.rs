//! Connecting agent clients to the `askkey` helper. Each change touches only
//! Ask Key's own entry, keeps a backup of the previous file, and publishes
//! the new file atomically.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::AppState;

const CLIENTS: [&str; 4] = ["claude", "codex", "cursor", "grok"];

pub fn helper_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|directory| directory.join("askkey.exe")))
        .unwrap_or_else(|| PathBuf::from("askkey.exe"))
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn config_path(client: &str) -> PathBuf {
    match client {
        "claude" => home().join(".claude.json"),
        "codex" => home().join(".codex").join("config.toml"),
        "cursor" => home().join(".cursor").join("mcp.json"),
        _ => home().join(".grok").join("config.toml"),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    client: &'static str,
    config_path: String,
    configured: bool,
    /// What to run by hand when automatic setup is not possible.
    manual: String,
}

fn same_path(value: Option<&str>, helper: &Path) -> bool {
    value.is_some_and(|value| {
        Path::new(value) == helper || value.eq_ignore_ascii_case(&helper.to_string_lossy())
    })
}

fn json_configured(path: &Path, helper: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let args: Option<Vec<&str>> = value
        .pointer("/mcpServers/askkey/args")
        .and_then(Value::as_array)
        .and_then(|args| args.iter().map(Value::as_str).collect());
    same_path(
        value
            .pointer("/mcpServers/askkey/command")
            .and_then(Value::as_str),
        helper,
    ) && args.as_deref() == Some(&["mcp"][..])
}

fn toml_configured(path: &Path, helper: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    let entry = document
        .get("mcp_servers")
        .and_then(|servers| servers.get("askkey"));
    let args: Option<Vec<&str>> = entry
        .and_then(|entry| entry.get("args"))
        .and_then(|args| args.as_array())
        .and_then(|args| args.iter().map(|arg| arg.as_str()).collect());
    same_path(
        entry
            .and_then(|entry| entry.get("command"))
            .and_then(|command| command.as_str()),
        helper,
    ) && args.as_deref() == Some(&["mcp"][..])
}

fn manual(client: &str, helper: &Path) -> String {
    let helper_text = helper.to_string_lossy();
    let server = json!({"type": "stdio", "command": helper_text, "args": ["mcp"]}).to_string();
    match client {
        "claude" => format!("claude mcp add-json askkey '{server}' --scope user"),
        "cursor" => format!("Add to %USERPROFILE%\\.cursor\\mcp.json: {{\"mcpServers\": {{\"askkey\": {{\"command\": {}, \"args\": [\"mcp\"]}}}}}}", json!(helper_text)),
        _ => format!("[mcp_servers.askkey]\ncommand = {}\nargs = [\"mcp\"]", toml_edit::value(helper_text.as_ref())),
    }
}

fn status(client: &'static str, helper: &Path) -> ClientStatus {
    let path = config_path(client);
    let configured = match client {
        "claude" | "cursor" => json_configured(&path, helper),
        _ => toml_configured(&path, helper),
    };
    ClientStatus {
        client,
        config_path: path.to_string_lossy().into_owned(),
        configured,
        manual: manual(client, helper),
    }
}

#[tauri::command]
pub fn client_status() -> Vec<ClientStatus> {
    let helper = helper_path();
    CLIENTS
        .iter()
        .map(|client| status(client, &helper))
        .collect()
}

/// Writes `contents` next to `path` and renames it into place, keeping the
/// previous file as `<name>.askkey-backup`.
fn publish(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    if path.is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to follow a link",
        ));
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if path.exists() {
        fs::copy(path, path.with_file_name(format!("{name}.askkey-backup")))?;
    }
    let temporary = path.with_file_name(format!("{name}.askkey-tmp"));
    fs::write(&temporary, contents)?;
    fs::rename(&temporary, path)
}

fn connect_json(path: &Path, helper: &Path) -> Result<(), String> {
    let mut document: Value = match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|_| "config_unreadable")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(_) => return Err("config_unreadable".into()),
    };
    let root = document.as_object_mut().ok_or("config_unreadable")?;
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().ok_or("config_unreadable")?;
    servers.insert(
        "askkey".into(),
        json!({"type": "stdio", "command": helper.to_string_lossy(), "args": ["mcp"]}),
    );
    let text = serde_json::to_string_pretty(&document).map_err(|_| "config_unreadable")?;
    publish(path, &text).map_err(|_| "config_write_failed".into())
}

fn connect_toml(path: &Path, helper: &Path) -> Result<(), String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err("config_unreadable".into()),
    };
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| "config_unreadable")?;
    let servers = document.entry("mcp_servers").or_insert(toml_edit::table());
    let servers = servers.as_table_mut().ok_or("config_unreadable")?;
    servers.set_implicit(true);
    let mut entry = toml_edit::Table::new();
    entry.insert(
        "command",
        toml_edit::value(helper.to_string_lossy().as_ref()),
    );
    let mut args = toml_edit::Array::new();
    args.push("mcp");
    entry.insert("args", toml_edit::value(args));
    servers.insert("askkey", toml_edit::Item::Table(entry));
    publish(path, &document.to_string()).map_err(|_| "config_write_failed".into())
}

/// Claude Code owns `~/.claude.json` while it runs, so it is configured
/// through its own CLI rather than edited.
fn connect_claude(helper: &Path) -> Result<(), String> {
    let path = std::env::var("PATH").unwrap_or_default();
    let directory = std::env::current_dir().unwrap_or_else(|_| home());
    let claude = askkey_system::resolve_executable("claude", &directory, Some(&path), None)
        .map_err(|_| "claude_cli_not_found")?;
    let server =
        json!({"type": "stdio", "command": helper.to_string_lossy(), "args": ["mcp"]}).to_string();
    let _ = std::process::Command::new(&claude)
        .args(["mcp", "remove", "askkey", "--scope", "user"])
        .output();
    let output = std::process::Command::new(&claude)
        .args(["mcp", "add-json", "askkey", &server, "--scope", "user"])
        .output()
        .map_err(|_| "claude_cli_failed")?;
    if output.status.success() {
        Ok(())
    } else {
        Err("claude_cli_failed".into())
    }
}

#[tauri::command]
pub fn connect_client(state: State<'_, AppState>, client: String) -> Result<ClientStatus, String> {
    crate::commands::require_unlocked(&state)?;
    let client = CLIENTS
        .iter()
        .find(|known| **known == client)
        .ok_or("unknown_client")?;
    let helper = helper_path();
    if !helper.is_file() {
        return Err("helper_missing".into());
    }
    let path = config_path(client);
    match *client {
        "claude" => connect_claude(&helper)?,
        "cursor" => connect_json(&path, &helper)?,
        _ => connect_toml(&path, &helper)?,
    }
    Ok(status(client, &helper))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_merge_keeps_other_servers() {
        let directory =
            std::env::temp_dir().join(format!("askkey-client-test-{}", std::process::id()));
        let path = directory.join("mcp.json");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"other.exe"}},"keep":1}"#,
        )
        .unwrap();
        let helper = Path::new(r"C:\Program Files\Ask Key\askkey.exe");
        connect_json(&path, helper).unwrap();
        let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["mcpServers"]["other"]["command"], "other.exe");
        assert_eq!(value["keep"], 1);
        assert!(json_configured(&path, helper));
        assert!(directory.join("mcp.json.askkey-backup").exists());
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn status_requires_the_mcp_argument() {
        let directory =
            std::env::temp_dir().join(format!("askkey-client-args-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let helper = Path::new(r"C:\Program Files\Ask Key\askkey.exe");
        let json = directory.join("mcp.json");
        let entry = |args: &str| {
            format!(
                r#"{{"mcpServers":{{"askkey":{{"command":{},"args":{args}}}}}}}"#,
                json!(helper.to_string_lossy())
            )
        };
        fs::write(&json, entry(r#"["mcp"]"#)).unwrap();
        assert!(json_configured(&json, helper));
        fs::write(&json, entry(r#"["run"]"#)).unwrap();
        assert!(!json_configured(&json, helper));
        fs::write(&json, entry("[]")).unwrap();
        assert!(!json_configured(&json, helper));

        let toml = directory.join("config.toml");
        let command = toml_edit::value(helper.to_string_lossy().as_ref());
        fs::write(
            &toml,
            format!("[mcp_servers.askkey]\ncommand = {command}\nargs = [\"mcp\"]\n"),
        )
        .unwrap();
        assert!(toml_configured(&toml, helper));
        fs::write(
            &toml,
            format!("[mcp_servers.askkey]\ncommand = {command}\n"),
        )
        .unwrap();
        assert!(!toml_configured(&toml, helper));
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn toml_merge_preserves_comments() {
        let directory =
            std::env::temp_dir().join(format!("askkey-client-toml-{}", std::process::id()));
        let path = directory.join("config.toml");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            &path,
            "# my settings\nmodel = \"x\"\n\n[mcp_servers.other]\ncommand = \"o.exe\"\n",
        )
        .unwrap();
        let helper = Path::new(r"C:\Program Files\Ask Key\askkey.exe");
        connect_toml(&path, helper).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my settings"));
        assert!(text.contains("[mcp_servers.other]"));
        assert!(toml_configured(&path, helper));
        fs::remove_dir_all(&directory).unwrap();
    }
}
