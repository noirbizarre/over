//! Root-scoped secret commit policy, independent of overlay selection.

use anyhow::{Context as _, Result, ensure};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SecretsConfig {
    pub commit_messages: CommitMessages,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CommitMessages {
    pub add: String,
    pub replace: String,
    pub remove: String,
}

impl Default for CommitMessages {
    fn default() -> Self {
        Self {
            add: "chore(secrets): added {{ name }}".into(),
            replace: "chore(secrets): replaced {{ name }}".into(),
            remove: "chore(secrets): removed {{ name }}".into(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SecretOperation {
    Add,
    Replace,
    Remove,
}

impl CommitMessages {
    pub fn render(&self, operation: SecretOperation, name: &super::SecretName) -> Result<String> {
        let template = match operation {
            SecretOperation::Add => &self.add,
            SecretOperation::Replace => &self.replace,
            SecretOperation::Remove => &self.remove,
        };
        // A minimal context prevents values or secret-resolution capabilities leaking into history.
        let message = crate::exec::templates::render_string(
            template,
            &serde_json::json!({ "name": name.as_str() }),
        )
        .context("invalid secret commit-message template")?;
        ensure!(
            !message.trim().is_empty() && !message.contains('\0'),
            "secret commit message must be nonempty and contain no NUL"
        );
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case("toml", "[secrets.commit_messages]\nadd = 'chore: saved {{ name }}'\n")]
    #[case(
        "yaml",
        "secrets:\n  commit_messages:\n    add: 'chore: saved {{ name }}'\n"
    )]
    fn repository_templates_load_in_each_descriptor_format(
        #[case] extension: &str,
        #[case] content: &str,
    ) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(format!("over.{extension}")), content).unwrap();
        let config = crate::overlays::Repository::new(directory.path().to_path_buf())
            .secrets_config()
            .unwrap();
        assert_eq!(config.commit_messages.add, "chore: saved {{ name }}");
        assert_eq!(
            config.commit_messages.remove,
            CommitMessages::default().remove
        );
    }

    #[test]
    fn numeric_message_configuration_is_not_coerced_to_text() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("over.toml"),
            "secrets.commit_messages.add = 42",
        )
        .unwrap();
        assert!(
            crate::overlays::Repository::new(directory.path().to_path_buf())
                .secrets_config()
                .is_err()
        );
    }

    #[rstest::rstest]
    #[case(SecretOperation::Add, "added")]
    #[case(SecretOperation::Replace, "replaced")]
    #[case(SecretOperation::Remove, "removed")]
    fn default_messages_describe_the_operation(
        #[case] operation: SecretOperation,
        #[case] verb: &str,
    ) {
        assert_eq!(
            CommitMessages::default()
                .render(
                    operation,
                    &super::super::SecretName::parse("token").unwrap()
                )
                .unwrap(),
            format!("chore(secrets): {verb} token")
        );
    }

    #[rstest::rstest]
    #[case("{{ missing }}")]
    #[case("{{ secret('token') }}")]
    #[case("   ")]
    #[case("message\0")]
    fn unusable_templates_are_rejected(#[case] template: &str) {
        let messages = CommitMessages {
            add: template.into(),
            ..Default::default()
        };
        assert!(
            messages
                .render(
                    SecretOperation::Add,
                    &super::super::SecretName::parse("token").unwrap()
                )
                .is_err()
        );
    }
}
