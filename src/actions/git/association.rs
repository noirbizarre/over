//! Repository-local overlay associations shared by provisioning and selection.

use std::collections::BTreeSet;

use anyhow::{Context as _, Result, ensure};
use git2::{Config, ConfigLevel, Repository};

/// Read the effective explicit association, rejecting conflicting values.
pub fn read(repo: &Repository) -> Result<Option<String>> {
    let config = repo
        .config()
        .context("failed to read repository Git config")?;
    read_config(&config)
}

fn read_config(config: &Config) -> Result<Option<String>> {
    let mut entries = config.entries(Some(r"^over\.overlay$"))?;
    let mut local = Vec::new();
    let mut worktree = Vec::new();
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let values = match entry.level() {
            ConfigLevel::Local => &mut local,
            ConfigLevel::Worktree => &mut worktree,
            // A global setting does not establish a repository-to-overlay relationship.
            _ => continue,
        };
        // Validate only the effective level: a worktree can override a broken shared value.
        values.push(if entry.has_value() {
            Some(entry.value_bytes().to_vec())
        } else {
            None
        });
    }
    let entries = if worktree.is_empty() { local } else { worktree };
    let mut values = BTreeSet::new();
    for entry in entries {
        let bytes = entry.context("over.overlay has no value; set it to an overlay name")?;
        let value = String::from_utf8(bytes).context("over.overlay is not valid UTF-8")?;
        ensure!(
            !value.trim().is_empty(),
            "over.overlay is empty; set it to an overlay name"
        );
        values.insert(value);
    }
    ensure!(
        values.len() <= 1,
        "ambiguous over.overlay Git association: {}; specify an overlay explicitly or fix the repository config",
        values.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    Ok(values.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use rstest::rstest;

    #[rstest]
    #[case("[over]\noverlay = one\noverlay = one", Some("one"), None)]
    #[case("[over]\noverlay = two\noverlay = one", None, Some("ambiguous"))]
    #[case("[over]\noverlay = \"\"", None, Some("empty"))]
    #[case("[over]\noverlay", None, Some("no value"))]
    #[case("[over]\nother = one", None, None)]
    fn local_associations_require_one_distinct_nonempty_name(
        #[case] content: &str,
        #[case] expected: Option<&str>,
        #[case] error: Option<&str>,
    ) {
        let temp = assert_fs::TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        // Append rather than replace so Git's repository-format settings remain intact.
        let config = repo.path().join("config");
        let before = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, format!("{before}\n{content}\n")).unwrap();
        let result = read(&repo);
        if let Some(error) = error {
            assert!(result.unwrap_err().to_string().contains(error));
        } else {
            assert_eq!(result.unwrap().as_deref(), expected);
        }
    }

    #[test]
    fn global_associations_are_ignored_and_worktree_values_override_local_values() {
        let temp = assert_fs::TempDir::new().unwrap();
        temp.child("global")
            .write_str("[over]\noverlay = global\n")
            .unwrap();
        temp.child("local")
            .write_str("[over]\noverlay =\n")
            .unwrap();
        temp.child("worktree")
            .write_str("[over]\noverlay = worktree\n")
            .unwrap();
        let mut config = Config::new().unwrap();
        config
            .add_file(&temp.path().join("global"), ConfigLevel::Global, false)
            .unwrap();
        assert_eq!(read_config(&config).unwrap(), None);
        config
            .add_file(&temp.path().join("local"), ConfigLevel::Local, false)
            .unwrap();
        config
            .add_file(&temp.path().join("worktree"), ConfigLevel::Worktree, false)
            .unwrap();
        assert_eq!(read_config(&config).unwrap().as_deref(), Some("worktree"));
    }
}
