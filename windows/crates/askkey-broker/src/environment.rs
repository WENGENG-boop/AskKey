/// Looks a variable up case-insensitively.
pub(crate) fn lookup<'a>(
    variables: &'a std::collections::BTreeMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    variables
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
