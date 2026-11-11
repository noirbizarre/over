//! Authoritative runtime Git prerequisites shared by status and doctor.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use git2::{Config, ErrorCode, Repository};

use crate::actions::git::{association, config::GitRepoConfig};
use crate::desired::{DesiredTree, Provenance};
use crate::git_exclude;
use crate::ui::{emojis, style};
use crate::utils::short_path;

/// Severity is independent of a command's exit threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Exclusion,
    Tracked,
    Malformed,
    Configuration,
    Association,
    Inspection,
}

/// Values are deliberately absent: URLs and arbitrary settings can contain secrets.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Issue {
    pub overlay: String,
    pub repository: PathBuf,
    pub worktree: Option<PathBuf>,
    pub severity: Severity,
    pub kind: Kind,
    pub message: String,
    /// Distinguish redacted keys without keeping credentials in diagnostic text.
    pub subject_id: Option<git2::Oid>,
}

pub fn repository_root(common: &Path) -> PathBuf {
    if common.file_name().is_some_and(|name| name == ".git") {
        common.parent().unwrap_or(common).to_path_buf()
    } else {
        common.to_path_buf()
    }
}

pub fn inspect(desired: &DesiredTree) -> Result<Vec<Issue>> {
    let mut issues = BTreeSet::new();
    for diagnosis in git_exclude::diagnose_protection(desired)? {
        let root = repository_root(&diagnosis.common_dir);
        let worktree = (diagnosis.workdir != root).then_some(diagnosis.workdir);
        let base = Issue {
            overlay: diagnosis.overlay,
            repository: root,
            worktree,
            severity: Severity::Warning,
            kind: Kind::Exclusion,
            subject_id: None,
            message: String::new(),
        };
        for (kind, label, paths) in [
            (Kind::Exclusion, "✘ exclude missing", diagnosis.missing),
            (Kind::Tracked, "🔒 tracked", diagnosis.tracked),
        ] {
            for path in paths {
                issues.insert(Issue {
                    message: format!("{label}: {}", path.display()),
                    kind,
                    ..base.clone()
                });
            }
        }
        if diagnosis.malformed {
            issues.insert(Issue {
                worktree: None,
                severity: Severity::Error,
                kind: Kind::Malformed,
                message: "✘ exclude malformed: .git/info/exclude (run over doctor --fix)".into(),
                ..base
            });
        }
    }
    for entry in desired.entries() {
        if let Provenance::Git {
            overlay, config, ..
        } = &entry.provenance
        {
            match inspect_repository(&entry.target, overlay, config) {
                Ok(found) => issues.extend(found),
                Err(_) => {
                    // One damaged config must not hide other repositories' findings;
                    // parser details can contain credentials or arbitrary secret values.
                    issues.insert(Issue {
                        overlay: overlay.clone(),
                        repository: entry.target.clone(),
                        worktree: None,
                        severity: Severity::Error,
                        kind: Kind::Inspection,
                        subject_id: None,
                        message: "✘ cannot inspect Git configuration".into(),
                    });
                }
            }
        }
    }
    Ok(issues.into_iter().collect())
}

#[derive(Clone)]
enum Expected {
    Value(String),
    Exact(Vec<String>),
    Contains(String),
}

fn normalized_key(key: &str) -> String {
    // Git folds section/variable case, but subsection (e.g. remote name) is significant.
    let Some((section, rest)) = key.split_once('.') else {
        return key.to_ascii_lowercase();
    };
    if let Some((subsection, variable)) = rest.rsplit_once('.') {
        format!(
            "{}.{subsection}.{}",
            section.to_ascii_lowercase(),
            variable.to_ascii_lowercase()
        )
    } else {
        key.to_ascii_lowercase()
    }
}

fn display_key(key: &str) -> String {
    let Some((section, rest)) = key.split_once('.') else {
        return key.into();
    };
    let Some((subsection, variable)) = rest.rsplit_once('.') else {
        return key.into();
    };
    // URL rewrite and credential subsections can themselves contain tokens,
    // even though configuration values and remote URLs are never displayed.
    if matches!(section, "url" | "http" | "credential")
        || subsection.contains("://")
        || subsection.contains('@')
    {
        format!("{section}.<redacted>.{variable}")
    } else {
        key.into()
    }
}

fn read_config(path: &Path) -> Result<Config> {
    if path.try_exists()? {
        Ok(Config::open(path)?)
    } else {
        Ok(Config::new()?)
    }
}

fn compare(
    config: &Config,
    expected: BTreeMap<String, Expected>,
    base: &Issue,
    issues: &mut Vec<Issue>,
) -> Result<()> {
    for (key, expected) in expected {
        let mut values = Vec::new();
        match config.multivar(&key, None) {
            Ok(mut entries) => {
                while let Some(entry) = entries.next() {
                    let entry = entry?;
                    values.push(entry.value().unwrap_or("").to_owned());
                }
            }
            Err(error) if error.code() == ErrorCode::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let matches = match expected {
            Expected::Value(value) => values.last() == Some(&value),
            Expected::Exact(value) => values == value,
            Expected::Contains(value) => values.contains(&value),
        };
        if !matches {
            let label = if values.is_empty() {
                "missing"
            } else {
                "incorrect"
            };
            issues.push(Issue {
                subject_id: Some(git2::Oid::hash_object(
                    git2::ObjectType::Blob,
                    key.as_bytes(),
                )?),
                message: format!("✘ config {label}: {}", display_key(&key)),
                ..base.clone()
            });
        }
    }
    Ok(())
}

/// Check configuration only after deployment; presence/content remain status's responsibility.
pub fn inspect_repository(
    target: &Path,
    overlay: &str,
    config: &GitRepoConfig,
) -> Result<Vec<Issue>> {
    let bare = config.worktree || config.worktrees.is_some();
    let path = if bare {
        target.join(".git")
    } else {
        target.to_path_buf()
    };
    if !path.try_exists()? {
        return Ok(Vec::new());
    }
    let opened = if bare {
        Repository::open_bare(&path)
    } else {
        Repository::open(&path)
    };
    let repo = match opened {
        Ok(repo) => repo,
        // Existing broken deployments already receive a Broken entry status.
        Err(_) => return Ok(Vec::new()),
    };
    let common = repo.commondir().canonicalize()?;
    let root = repository_root(&common);
    let base = Issue {
        overlay: overlay.into(),
        repository: root.clone(),
        worktree: None,
        severity: Severity::Warning,
        kind: Kind::Configuration,
        subject_id: None,
        message: String::new(),
    };
    let mut expected = BTreeMap::new();
    if !config.url.is_empty() {
        expected.insert(
            "remote.origin.url".into(),
            Expected::Value(config.url.clone()),
        );
    }
    for (name, remote) in config.remotes.iter().flatten() {
        expected.insert(
            format!("remote.{name}.url"),
            Expected::Value(remote.url.clone()),
        );
        if let Some(value) = &remote.fetch {
            expected.insert(
                format!("remote.{name}.fetch"),
                Expected::Exact(vec![value.clone()]),
            );
        }
        if let Some(value) = &remote.push {
            expected.insert(
                format!("remote.{name}.push"),
                Expected::Contains(value.clone()),
            );
        }
        for (key, value) in &remote.extras {
            expected.insert(
                normalized_key(&format!("remote.{name}.{key}")),
                Expected::Value(value.clone()),
            );
        }
        if let Some(value) = &remote.tagopt {
            expected.insert(
                format!("remote.{name}.tagopt"),
                Expected::Value(value.clone()),
            );
        }
    }
    for (key, value) in config.config.iter().flat_map(|config| &config.entries) {
        expected.insert(normalized_key(key), Expected::Value(value.clone()));
    }
    let named_private_config = config
        .worktrees
        .iter()
        .flat_map(|items| items.values())
        .any(|entry| entry.config.is_some());
    if config.per_worktree_config || named_private_config {
        expected.insert(
            "extensions.worktreeconfig".into(),
            Expected::Value("true".into()),
        );
        if bare && config.per_worktree_config {
            expected.insert("core.bare".into(), Expected::Value("false".into()));
        }
    }
    // Provisioning writes the association last; inspect its effective scope separately.
    expected.retain(|key, _| !key.eq_ignore_ascii_case("over.overlay"));
    let mut issues = Vec::new();
    compare(
        &read_config(&common.join("config"))?,
        expected,
        &base,
        &mut issues,
    )?;
    check_association(&repo, overlay, &base, &mut issues);
    if config.per_worktree_config {
        let mut expected = BTreeMap::new();
        if bare {
            expected.insert("core.bare".into(), Expected::Value("true".into()));
        }
        for (key, value) in config
            .worktree_config
            .iter()
            .flat_map(|config| &config.entries)
        {
            expected.insert(normalized_key(key), Expected::Value(value.clone()));
        }
        let private = Issue {
            worktree: Some(target.canonicalize()?),
            ..base.clone()
        };
        compare(
            &read_config(&repo.path().join("config.worktree"))?,
            expected,
            &private,
            &mut issues,
        )?;
    }
    // Check every existing linked worktree's association, including the automatic default.
    for name in repo.worktrees()?.iter() {
        let Some(name) = name? else {
            continue;
        };
        let worktree = repo.find_worktree(name)?;
        if !worktree.path().try_exists()? {
            continue;
        }
        let linked = Repository::open(worktree.path())?;
        let private = Issue {
            worktree: Some(worktree.path().canonicalize()?),
            ..base.clone()
        };
        let private_config = read_config(&linked.path().join("config.worktree"))?;
        let mut expected_association = overlay;
        if let Some(settings) = config
            .worktrees
            .as_ref()
            .and_then(|items| items.get(name))
            .and_then(|entry| entry.config.as_ref())
        {
            if let Some(value) = settings
                .entries
                .iter()
                .find_map(|(key, value)| (normalized_key(key) == "over.overlay").then_some(value))
            {
                expected_association = value;
            }
            let expected = settings
                .entries
                .iter()
                .map(|(key, value)| (normalized_key(key), Expected::Value(value.clone())))
                .collect();
            compare(&private_config, expected, &private, &mut issues)?;
        }
        // Inherited association drift is already reported on shared configuration.
        if private_config.get_entry("over.overlay").is_ok() {
            check_association(&linked, expected_association, &private, &mut issues);
        }
    }
    Ok(issues)
}

fn check_association(repo: &Repository, overlay: &str, base: &Issue, issues: &mut Vec<Issue>) {
    let label = match association::read(repo) {
        Ok(Some(actual)) if actual == overlay => return,
        Ok(None) => "missing",
        Ok(Some(_)) => "incorrect",
        Err(_) => "invalid",
    };
    if issues.iter().any(|issue| {
        issue.repository == base.repository
            && issue.worktree == base.worktree
            && issue.message.ends_with(": over.overlay")
    }) {
        return;
    }
    issues.push(Issue {
        kind: Kind::Association,
        message: format!("✘ config {label}: over.overlay"),
        ..base.clone()
    });
}

/// Render already collected issues; never reopen a repository for presentation.
pub fn grouped(issues: &[Issue]) -> String {
    let groups = issues
        .iter()
        .fold(BTreeMap::<_, BTreeSet<_>>::new(), |mut groups, issue| {
            groups
                .entry((&issue.overlay, &issue.repository))
                .or_default()
                .insert(issue);
            groups
        });
    let mut lines = Vec::new();
    let mut previous = None;
    for ((overlay, repository), issues) in groups {
        if previous != Some(overlay) {
            lines.push(format!("  {}", style::overlay(overlay)));
            previous = Some(overlay);
        }
        lines.push(format!(
            "    {}",
            style::repository(short_path(&repository.to_string_lossy()))
        ));
        let mut previous_worktree = None;
        for issue in issues {
            let indent = if let Some(worktree) = &issue.worktree {
                if previous_worktree != Some(worktree) {
                    let relative = worktree.strip_prefix(repository).unwrap_or(worktree);
                    lines.push(format!(
                        "      {}",
                        style::path(if relative.as_os_str().is_empty() {
                            ".".into()
                        } else {
                            relative.display().to_string()
                        })
                    ));
                    previous_worktree = Some(worktree);
                }
                "        "
            } else {
                "      "
            };
            let severity = match issue.severity {
                Severity::Warning => style::Severity::Warning,
                Severity::Error => style::Severity::Error,
            };
            let (glyph, text) = if let Some(text) = issue.message.strip_prefix("✘ ") {
                (emojis::crossmark().to_string(), text)
            } else if let Some(text) = issue.message.strip_prefix("🔒 ") {
                (emojis::LOCK.to_string(), text)
            } else {
                (String::new(), issue.message.as_str())
            };
            let detail = if let Some((label, subject)) = text.split_once(": ") {
                let subject = match issue.kind {
                    Kind::Configuration | Kind::Association => style::key(subject).to_string(),
                    Kind::Exclusion | Kind::Tracked => style::path(subject).to_string(),
                    _ => subject.to_owned(),
                };
                format!(
                    "{} {subject}",
                    style::severity(format!("{label}:"), severity)
                )
            } else {
                style::severity(text, severity).to_string()
            };
            lines.push(format!("{indent}{glyph} {detail}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::actions::symlink::LinkType;
    use crate::desired::{DesiredEntry, MaterializationIntent};
    use assert_fs::TempDir;

    #[test]
    fn credential_bearing_key_subsections_are_redacted_without_merging_distinct_findings() {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        repo.remote("origin", "https://example.com/repo.git")
            .unwrap();
        repo.config()
            .unwrap()
            .set_str("over.overlay", "owner")
            .unwrap();
        let config = configuration(
            "[config]\n'url.https://user:secret-one@example.com/.insteadOf' = 'one'\n'url.https://user:secret-two@example.com/.insteadOf' = 'two'\n",
        );
        let issues = inspect_repository(temp.path(), "owner", &config).unwrap();
        assert_eq!(issues.len(), 2);
        assert_ne!(issues[0].subject_id, issues[1].subject_id);
        let rendered = grouped(&issues);
        assert_eq!(rendered.matches("url.<redacted>.insteadof").count(), 2);
        assert!(!format!("{issues:?}{rendered}").contains("secret-"));
    }

    #[test]
    fn git_findings_use_common_overlay_repository_path_key_and_severity_colors() {
        if crate::cli::common::run_in_isolated_process() {
            return;
        }
        console::set_colors_enabled(true);
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        Repository::init(&root).unwrap();
        fs::write(root.join("file"), "managed").unwrap();
        let issues = inspect(&artifacts(&root, &["file"])).unwrap();
        let output = grouped(&issues);
        assert!(output.contains("\x1b[36mworkspaces/demo\x1b[0m"));
        assert!(
            output.contains(
                &style::repository(&root.display())
                    .force_styling(true)
                    .to_string()
            )
        );
        assert!(output.contains("\x1b[31m✘\x1b[0m"));
        assert!(output.contains("\x1b[33mexclude missing:\x1b[0m"));
        assert!(output.contains("\x1b[34mfile\x1b[0m"));
        let config = inspect_repository(&root, "owner", &configuration("")).unwrap();
        assert!(
            grouped(&config).contains(
                &style::key("remote.origin.url")
                    .force_styling(true)
                    .to_string()
            )
        );
    }

    fn artifacts(root: &Path, names: &[&str]) -> DesiredTree {
        DesiredTree::from_entries(
            names
                .iter()
                .map(|name| DesiredEntry {
                    target: root.join(name),
                    provenance: Provenance::Overlay {
                        overlay: "workspaces/demo".into(),
                        source: root.join("source"),
                    },
                    intent: MaterializationIntent::SymlinkFile {
                        source: root.join("source"),
                        link_type: LinkType::Soft,
                    },
                    permissions: None,
                })
                .collect(),
        )
    }

    fn configuration(extra: &str) -> GitRepoConfig {
        toml::from_str(&format!("url = 'https://example.com/repo.git'\n{extra}")).unwrap()
    }

    #[test]
    fn effective_ignore_rules_and_negations_report_only_unprotected_artifacts() {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        fs::write(
            temp.path().join(".gitignore"),
            "ignored\n*.toml\n!visible.toml\n",
        )
        .unwrap();
        fs::write(repo.path().join("info/exclude"), "handwritten\n").unwrap();
        for name in [
            "ignored",
            "handwritten",
            "mise.toml",
            "visible.toml",
            "tracked",
        ] {
            fs::write(temp.path().join(name), "content").unwrap();
        }
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("tracked")).unwrap();
        index.write().unwrap();
        let before = fs::read(repo.path().join("index")).unwrap();
        let desired = artifacts(
            temp.path(),
            &[
                "ignored",
                "handwritten",
                "mise.toml",
                "visible.toml",
                "tracked",
                "absent",
            ],
        );
        let issues = inspect(&desired).unwrap();
        assert_eq!(issues.len(), 2);
        assert!(
            issues
                .iter()
                .any(|issue| issue.message == "✘ exclude missing: visible.toml")
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.message == "🔒 tracked: tracked")
        );
        assert_eq!(fs::read(repo.path().join("index")).unwrap(), before);
        git_exclude::reconcile_desired(&desired).unwrap();
        let exclude = fs::read(repo.path().join("info/exclude")).unwrap();
        // .gitignore negation wins over info/exclude, including our managed rule.
        assert_eq!(inspect(&desired).unwrap().len(), 2);
        fs::write(temp.path().join(".gitignore"), "ignored\nmise.toml\n").unwrap();
        assert_eq!(inspect(&desired).unwrap().len(), 1);
        git_exclude::reconcile_desired(&desired).unwrap();
        assert_eq!(fs::read(repo.path().join("info/exclude")).unwrap(), exclude);
    }

    #[test]
    fn every_declared_remote_and_scoped_setting_is_checked_without_exposing_values() {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        repo.config()
            .unwrap()
            .set_str("over.overlay", "owner")
            .unwrap();
        repo.remote("extra", "https://example.com/extra.git")
            .unwrap();
        let config = configuration(
            "[config]\n'user.name' = 'secret-name'\n[remotes.upstream]\nurl = 'https://secret@example.com/upstream'\nfetch = '+refs/heads/*:refs/remotes/upstream/*'\npush = 'refs/heads/main'\ntagopt = '--tags'\npushurl = 'secret-push'\n",
        );
        let issues = inspect_repository(temp.path(), "owner", &config).unwrap();
        assert_eq!(issues.len(), 7);
        let output = grouped(&issues);
        for key in [
            "remote.origin.url",
            "remote.upstream.url",
            "remote.upstream.fetch",
            "remote.upstream.push",
            "remote.upstream.tagopt",
            "remote.upstream.pushurl",
            "user.name",
        ] {
            assert!(output.contains(key), "{output}");
        }
        assert!(!output.contains("secret"));
        assert!(!output.contains("remote.extra"));
    }

    #[test]
    fn extra_push_entries_are_allowed_but_explicit_fetch_is_an_exact_list() {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        repo.remote("origin", "https://example.com/repo.git")
            .unwrap();
        repo.config()
            .unwrap()
            .set_str("over.overlay", "owner")
            .unwrap();
        repo.remote("upstream", "https://example.com/upstream.git")
            .unwrap();
        repo.remote_add_push("upstream", "refs/heads/main").unwrap();
        repo.remote_add_push("upstream", "refs/heads/other")
            .unwrap();
        let config = configuration(
            "[remotes.upstream]\nurl = 'https://example.com/upstream.git'\nfetch = '+refs/heads/*:refs/remotes/upstream/*'\npush = 'refs/heads/main'\n",
        );
        assert!(
            inspect_repository(temp.path(), "owner", &config)
                .unwrap()
                .is_empty()
        );
        repo.remote_add_fetch("upstream", "+refs/tags/*:refs/tags/*")
            .unwrap();
        let issues = inspect_repository(temp.path(), "owner", &config).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0]
                .message
                .contains("incorrect: remote.upstream.fetch")
        );
    }

    fn flat_repository(root: &Path) -> Repository {
        let repo = Repository::init_bare(root.join(".git")).unwrap();
        let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        {
            let head = repo.head().unwrap();
            let mut options = git2::WorktreeAddOptions::new();
            options.reference(Some(&head));
            repo.worktree("main", &root.join("main"), Some(&options))
                .unwrap();
        }
        repo.worktree("other", &root.join("other"), None).unwrap();
        let mut shared = repo.config().unwrap();
        shared.set_bool("extensions.worktreeConfig", true).unwrap();
        shared.set_bool("core.bare", false).unwrap();
        shared.set_str("over.overlay", "owner").unwrap();
        shared.set_str("user.email", "private@example.com").unwrap();
        fs::write(repo.path().join("config.worktree"), "[core]\nbare = true\n").unwrap();
        repo.remote("origin", "https://example.com/repo.git")
            .unwrap();
        repo
    }

    #[test]
    fn flat_worktrees_share_a_heading_but_keep_indexes_ignores_and_private_config() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = flat_repository(&root);
        fs::write(root.join("main/opencode.jsonc"), "{}").unwrap();
        fs::write(root.join("other/opencode.jsonc"), "{}").unwrap();
        fs::write(root.join("other/other.jsonc"), "{}").unwrap();
        fs::write(root.join("other/.gitignore"), "opencode.jsonc\n").unwrap();
        let desired = artifacts(
            &root,
            &[
                "main/opencode.jsonc",
                "other/opencode.jsonc",
                "other/other.jsonc",
            ],
        );
        let issues = inspect(&desired).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].repository, root);
        assert_eq!(
            issues[0].worktree.as_deref(),
            Some(root.join("main").as_path())
        );
        assert_eq!(issues[0].message, "✘ exclude missing: opencode.jsonc");
        let config = configuration(
            "worktrees = { main = { branch = 'main', config = { 'user.email' = 'private@example.com' } } }\n",
        );
        let config_issues = inspect_repository(&root, "owner", &config).unwrap();
        assert_eq!(config_issues.len(), 1, "{config_issues:?}");
        assert!(
            config_issues[0]
                .message
                .contains("config missing: user.email")
        );
        fs::write(
            repo.path().join("worktrees/main/config.worktree"),
            "[user]\nemail = private@example.com\n",
        )
        .unwrap();
        assert!(
            inspect_repository(&root, "owner", &config)
                .unwrap()
                .is_empty()
        );
        let before = fs::read(repo.path().join("config")).unwrap();
        fs::write(
            repo.path().join("info/exclude"),
            "# >>> over: exclude:workspaces/demo >>>\n/stale\n",
        )
        .unwrap();
        git_exclude::repair_existing_batch(&[&desired]).unwrap();
        assert!(inspect(&desired).unwrap().is_empty());
        assert!(
            fs::read_to_string(repo.path().join("info/exclude"))
                .unwrap()
                .contains("/opencode.jsonc")
        );
        assert!(
            fs::read_to_string(repo.path().join("info/exclude"))
                .unwrap()
                .contains("/other.jsonc")
        );
        assert_eq!(fs::read(repo.path().join("config")).unwrap(), before);
    }

    #[test]
    fn private_associations_follow_declared_overrides_without_duplicating_shared_drift() {
        let temp = TempDir::new().unwrap();
        let repo = flat_repository(temp.path());
        let config = configuration("worktrees = { main = 'main' }\n");
        repo.config().unwrap().remove("over.overlay").unwrap();
        let issues = inspect_repository(temp.path(), "owner", &config).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].worktree, None);
        repo.config()
            .unwrap()
            .set_str("over.overlay", "owner")
            .unwrap();
        fs::write(
            repo.path().join("worktrees/main/config.worktree"),
            "[over]\noverlay = other\n",
        )
        .unwrap();
        let issues = inspect_repository(temp.path(), "owner", &config).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].worktree.as_ref().unwrap().ends_with("main"));
        let config = configuration(
            "worktrees = { main = { branch = 'main', config = { 'over.overlay' = 'other' } } }\n",
        );
        assert!(
            inspect_repository(temp.path(), "owner", &config)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn explicit_settings_override_remote_defaults_using_git_key_case_rules() {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        repo.remote("origin", "https://example.com/final.git")
            .unwrap();
        repo.config()
            .unwrap()
            .set_str("over.overlay", "owner")
            .unwrap();
        let config = configuration(
            "[config]\n'Remote.origin.URL' = 'https://example.com/final.git'\n'over.overlay' = 'overwritten-by-provisioning'\n",
        );
        assert!(
            inspect_repository(temp.path(), "owner", &config)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn root_declarations_receive_config_findings_while_absent_deployments_only_need_creation() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("mapped-root");
        let repo = Repository::init(&target).unwrap();
        let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        repo.remote("origin", "https://example.com/repo.git")
            .unwrap();
        repo.config()
            .unwrap()
            .set_str("over.overlay", "owner")
            .unwrap();
        let config = configuration("[config]\n'over.test' = 'required'\n");
        let desired = DesiredTree::from_entries(
            [target.clone(), temp.path().join("absent")]
                .into_iter()
                .map(|target| DesiredEntry {
                    target,
                    provenance: Provenance::Git {
                        overlay: "owner".into(),
                        repo_key: ".".into(),
                        config: Box::new(config.clone()),
                    },
                    intent: MaterializationIntent::Checkout,
                    permissions: None,
                })
                .collect(),
        );
        let report = crate::status::Report::build(&desired).unwrap();
        assert_eq!(report.entries()[0].status, crate::status::Status::Applied);
        assert_eq!(report.entries()[1].status, crate::status::Status::Missing);
        assert_eq!(report.git_configuration().len(), 1);
        assert_eq!(
            report.git_configuration()[0].message,
            "✘ config missing: over.test"
        );
        assert!(
            report
                .health_issues()
                .iter()
                .all(|(_, severity)| *severity == Severity::Warning)
        );
    }

    #[test]
    fn dangling_artifacts_are_checked_without_following_their_referent() {
        let temp = TempDir::new().unwrap();
        Repository::init(temp.path()).unwrap();
        symlink::symlink_file(temp.path().join("absent"), temp.path().join("dangling")).unwrap();
        let issues = inspect(&artifacts(temp.path(), &["dangling"])).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].message, "✘ exclude missing: dangling");
    }
}
