use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("the executable was not found")]
    NotFound,
    /// `.bat` and `.cmd` files run through `cmd.exe`, whose argument parsing
    /// defeats quoting. Users who need one must name an interpreter explicitly.
    #[error("batch files cannot be targets; run them through an interpreter you choose")]
    BatchFile,
    #[error("only .exe and .com files can be targets")]
    NotExecutable,
}

const RUNNABLE: [&str; 2] = ["exe", "com"];

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
}

fn accept(candidate: PathBuf) -> Result<PathBuf, ResolveError> {
    match extension_of(&candidate).as_deref() {
        Some("bat" | "cmd") => Err(ResolveError::BatchFile),
        Some(extension) if RUNNABLE.contains(&extension) => Ok(candidate),
        _ => Err(ResolveError::NotExecutable),
    }
}

/// Finds the program `command` names, the way a user expects from a shell but
/// without searching the working directory for bare names, and without
/// running anything that `cmd.exe` would interpret.
///
/// `path` and `path_ext` are the caller's `PATH` and `PATHEXT`.
pub fn resolve_executable(
    command: &str,
    working_directory: &Path,
    path: Option<&str>,
    path_ext: Option<&str>,
) -> Result<PathBuf, ResolveError> {
    let named = Path::new(command);
    let explicit = named.is_absolute() || command.contains(['\\', '/']);
    let directories: Vec<PathBuf> = if explicit {
        vec![if named.is_absolute() {
            PathBuf::new()
        } else {
            working_directory.to_path_buf()
        }]
    } else {
        path.unwrap_or_default()
            .split(';')
            .filter(|entry| !entry.trim().is_empty())
            .map(PathBuf::from)
            .collect()
    };
    let extensions: Vec<String> = path_ext
        .unwrap_or(".COM;.EXE;.BAT;.CMD")
        .split(';')
        .map(|extension| {
            extension
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase()
        })
        .filter(|extension| !extension.is_empty())
        .collect();

    let mut saw_batch = false;
    for directory in directories {
        let base = directory.join(named);
        if extension_of(&base).is_some() && base.is_file() {
            return accept(base);
        }
        for extension in &extensions {
            let mut candidate = base.clone().into_os_string();
            candidate.push(".");
            candidate.push(extension);
            let candidate = PathBuf::from(candidate);
            if candidate.is_file() {
                match accept(candidate) {
                    Ok(found) => return Ok(found),
                    Err(ResolveError::BatchFile) => saw_batch = true,
                    Err(_) => {}
                }
            }
        }
    }
    Err(if saw_batch {
        ResolveError::BatchFile
    } else {
        ResolveError::NotFound
    })
}

/// Quotes one argument so `CommandLineToArgvW` and the MSVC runtime parse it
/// back unchanged.
fn quote_argument(argument: &str, output: &mut String) {
    if !argument.is_empty() && !argument.contains([' ', '\t', '\n', '\x0b', '"']) {
        output.push_str(argument);
        return;
    }
    output.push('"');
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                output.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                output.push('"');
                backslashes = 0;
            }
            other => {
                output.extend(std::iter::repeat_n('\\', backslashes));
                output.push(other);
                backslashes = 0;
            }
        }
    }
    output.extend(std::iter::repeat_n('\\', backslashes * 2));
    output.push('"');
}

/// Builds a Windows command line from an argument vector.
pub fn build_command_line(arguments: &[String]) -> String {
    let mut line = String::new();
    for (index, argument) in arguments.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        quote_argument(argument, &mut line);
    }
    line
}

/// A `CREATE_UNICODE_ENVIRONMENT` block: `NAME=value` entries sorted
/// case-insensitively, each NUL-terminated, with a final NUL.
///
/// Later entries replace earlier ones with the same name, compared
/// case-insensitively, as Windows treats variable names.
pub fn environment_block(variables: &[(String, String)]) -> Vec<u16> {
    let mut merged: Vec<(String, &str, &str)> = Vec::new();
    for (name, value) in variables {
        let key = name.to_uppercase();
        merged.retain(|(existing, _, _)| *existing != key);
        merged.push((key, name, value));
    }
    merged.sort_by(|a, b| a.0.cmp(&b.0));
    let mut block = Vec::new();
    for (_, name, value) in merged {
        block.extend(format!("{name}={value}").encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(arguments: &[&str]) -> String {
        build_command_line(
            &arguments
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn quotes_like_the_msvc_runtime() {
        assert_eq!(line(&["git.exe", "status"]), "git.exe status");
        assert_eq!(line(&["a b", ""]), r#""a b" """#);
        assert_eq!(line(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(line(&[r"C:\dir with space\"]), r#""C:\dir with space\\""#);
        assert_eq!(line(&[r"C:\plain\path"]), r"C:\plain\path");
        assert_eq!(line(&[r#"back\\"quote"#]), r#""back\\\\\"quote""#);
    }

    #[test]
    fn environment_names_are_case_insensitive() {
        let block = environment_block(&[
            ("Path".into(), "old".into()),
            ("TOKEN".into(), "a".into()),
            ("PATH".into(), "new".into()),
        ]);
        let text = String::from_utf16(&block).unwrap();
        assert_eq!(text, "PATH=new\0TOKEN=a\0\0");
    }

    #[test]
    fn resolves_and_rejects_batch_files() {
        let directory = tempfile::tempdir().unwrap();
        let tool = directory.path().join("tool.exe");
        std::fs::write(&tool, b"").unwrap();
        std::fs::write(directory.path().join("shim.cmd"), b"").unwrap();
        std::fs::write(directory.path().join("notes.txt"), b"").unwrap();
        let path = directory.path().to_string_lossy().to_string();

        let resolved = resolve_executable("tool", Path::new(r"C:\"), Some(&path), None).unwrap();
        assert_eq!(resolved, tool);
        assert_eq!(
            resolve_executable("shim", Path::new(r"C:\"), Some(&path), None),
            Err(ResolveError::BatchFile)
        );
        assert_eq!(
            resolve_executable(
                &tool
                    .with_extension("cmd")
                    .to_string_lossy()
                    .replace("tool", "shim"),
                Path::new(r"C:\"),
                None,
                None
            ),
            Err(ResolveError::BatchFile)
        );
        assert_eq!(
            resolve_executable(r".\notes.txt", directory.path(), None, None),
            Err(ResolveError::NotExecutable)
        );
        assert_eq!(
            resolve_executable("missing", Path::new(r"C:\"), Some(&path), None),
            Err(ResolveError::NotFound)
        );
    }

    #[test]
    fn bare_names_do_not_search_the_working_directory() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("planted.exe"), b"").unwrap();
        assert_eq!(
            resolve_executable("planted", directory.path(), Some(""), None),
            Err(ResolveError::NotFound)
        );
        assert!(resolve_executable(r".\planted", directory.path(), Some(""), None).is_ok());
    }
}
