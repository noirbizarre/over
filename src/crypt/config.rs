//! Root-scoped messages for automatically committed access operations.

use anyhow::{Result, ensure};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CryptConfig {
    pub commit_messages: AccessCommitMessages,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AccessCommitMessages {
    pub user_add: String,
    pub user_delete: String,
    pub user_revoke: String,
    pub key_add: String,
    pub key_detach: String,
    pub key_revoke: String,
    pub key_rotate: String,
    pub migrate: String,
}

impl Default for AccessCommitMessages {
    fn default() -> Self {
        Self {
            user_add: "chore(crypt): added user {{ user }}".into(),
            user_delete: "chore(crypt): deleted user {{ user }}".into(),
            user_revoke: "chore(crypt): revoked user {{ user }}".into(),
            key_add: "chore(crypt): added keys for {{ user }}".into(),
            key_detach: "chore(crypt): detached {{ user }}/{{ key }}".into(),
            key_revoke: "chore(crypt): revoked {{ user }}/{{ key }}".into(),
            key_rotate: "chore(crypt): rotated repository key".into(),
            migrate: "chore(crypt): migrated repository metadata".into(),
        }
    }
}

pub(crate) fn message(
    repo: &git2::Repository,
    operation: &str,
    context: serde_json::Value,
) -> Result<String> {
    let messages = crate::overlays::Repository::new(super::access::root(repo)?.to_path_buf())
        .crypt_config()?
        .commit_messages;
    let template = match operation {
        "user_add" => messages.user_add,
        "user_delete" => messages.user_delete,
        "user_revoke" => messages.user_revoke,
        "key_add" => messages.key_add,
        "key_detach" => messages.key_detach,
        "key_revoke" => messages.key_revoke,
        "key_rotate" => messages.key_rotate,
        "migrate" => messages.migrate,
        _ => anyhow::bail!("unknown access mutation"),
    };
    // Omit secret functions, environment variables and credential data from this context.
    let message = crate::exec::templates::render_string(&template, &context)?;
    ensure!(
        !message.trim().is_empty() && !message.contains('\0'),
        "access commit message must be nonempty and contain no NUL"
    );
    Ok(message)
}

#[cfg(test)]
mod tests {
    #[rstest::rstest]
    #[case(
        "toml",
        "[crypt.commit_messages]\nuser_add = 'chore: enrolled {{ user }}'\n"
    )]
    #[case(
        "yaml",
        "crypt:\n  commit_messages:\n    user_add: 'chore: enrolled {{ user }}'\n"
    )]
    fn access_messages_load_in_both_descriptor_formats(
        #[case] extension: &str,
        #[case] contents: &str,
    ) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(format!("over.{extension}")), contents).unwrap();
        let config = crate::overlays::Repository::new(directory.path().to_path_buf())
            .crypt_config()
            .unwrap();
        assert_eq!(
            config.commit_messages.user_add,
            "chore: enrolled {{ user }}"
        );
        assert_eq!(
            config.commit_messages.key_rotate,
            super::AccessCommitMessages::default().key_rotate
        );
    }

    #[test]
    fn access_messages_reject_numeric_configuration_instead_of_coercing_it() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("over.toml"),
            "crypt.commit_messages.user_add = 42",
        )
        .unwrap();
        assert!(
            crate::overlays::Repository::new(directory.path().to_path_buf())
                .crypt_config()
                .is_err()
        );
    }
}
