/// Inherited variables a target may receive from the helper's environment.
/// Everything else the agent's shell has is dropped. Compared
/// case-insensitively, as Windows does.
const ALLOWED: &[&str] = &[
    "ALLUSERSPROFILE",
    "APPDATA",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "COMMONPROGRAMW6432",
    "COMPUTERNAME",
    "COMSPEC",
    "HOMEDRIVE",
    "HOMEPATH",
    "LANG",
    "LOCALAPPDATA",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "PATH",
    "PATHEXT",
    "PROCESSOR_ARCHITECTURE",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "TEMP",
    "TERM",
    "TMP",
    "USERDOMAIN",
    "USERNAME",
    "USERPROFILE",
    "WINDIR",
];

pub fn inherited_variable_allowed(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ALLOWED.contains(&upper.as_str()) || upper.starts_with("LC_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_only_listed_variables() {
        assert!(inherited_variable_allowed("Path"));
        assert!(inherited_variable_allowed("SystemRoot"));
        assert!(inherited_variable_allowed("LC_ALL"));
        assert!(!inherited_variable_allowed("GITHUB_TOKEN"));
        assert!(!inherited_variable_allowed("ANTHROPIC_API_KEY"));
    }
}
