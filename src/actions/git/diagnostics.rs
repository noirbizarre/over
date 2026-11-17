//! Git diagnostics must not echo arbitrary configuration or transport credentials.

pub(crate) fn display_key(key: &str) -> String {
    let Some((section, rest)) = key.split_once('.') else {
        return key.into();
    };
    let Some((subsection, variable)) = rest.rsplit_once('.') else {
        return key.into();
    };
    // Subsections can embed credentials even when the setting value is omitted.
    if matches!(
        section.to_ascii_lowercase().as_str(),
        "url" | "http" | "credential"
    ) || subsection.contains("://")
        || subsection.contains('@')
    {
        format!("{section}.<redacted>.{variable}")
    } else {
        key.into()
    }
}

pub(crate) fn operation_error(operation: &str, error: git2::Error) -> anyhow::Error {
    // libgit2 messages may include rejected URLs, keys, or values. Keep structured causes.
    anyhow::anyhow!("{operation} ({:?}, {:?})", error.class(), error.code())
}
