//! Operation-local source selection shared by filesystem and Git-object views.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use walkdir::WalkDir;

use super::Overlay;

#[derive(Debug)]
pub struct SourcePolicy {
    root: PathBuf,
    overlay: Overlay,
    rules: Vec<(PathBuf, Gitignore)>,
    rule_sources: Vec<(PathBuf, Vec<u8>)>,
    files: Vec<PathBuf>,
}

/// Only source-selection inputs belong in inspection identities. Including the
/// whole Overlay would pull unrelated, randomly ordered Git configuration maps in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SourcePolicyIdentity {
    root: PathBuf,
    overlay: PathBuf,
    excludes: Vec<(PathBuf, Option<Vec<String>>)>,
    rules: Vec<(PathBuf, Vec<u8>)>,
}

impl SourcePolicy {
    /// Load only rule files on known Git paths, without discovering untracked trees.
    pub(crate) fn for_inventory(root: &Path, overlay: &Overlay, paths: &[PathBuf]) -> Result<Self> {
        ensure!(
            overlay.root.starts_with(root),
            "overlay is outside the Over repository"
        );
        ensure!(
            !overlay.root.starts_with(root.join(".over")),
            ".over is reserved repository administration"
        );
        if root.join(".over").exists() {
            ensure!(
                !super::mapping::physical(&overlay.root)?
                    .starts_with(super::mapping::physical(&root.join(".over"))?),
                ".over is reserved repository administration"
            );
        }
        let mut policy = Self {
            root: root.to_path_buf(),
            overlay: overlay.clone(),
            rules: Vec::new(),
            rule_sources: Vec::new(),
            files: Vec::new(),
        };
        let mut directories = std::collections::BTreeSet::new();
        for path in paths.iter().chain(std::iter::once(&overlay.root)) {
            for directory in path.ancestors().skip(usize::from(path != &overlay.root)) {
                if directory.starts_with(root) {
                    directories.insert(directory.to_path_buf());
                }
            }
        }
        // Parents sort before children, so excluded subtrees cannot reopen themselves.
        for directory in directories {
            if !policy.is_ignored(&directory, true) {
                policy.load(&directory)?;
            }
        }
        for path in paths.iter().filter(|path| is_control_file(path)) {
            if policy.is_ignored(path, false) {
                continue;
            }
            // Git remembers deleted sidecars; only current declarations define today's layout.
            match path.symlink_metadata() {
                Ok(metadata) if !metadata.is_dir() => policy.files.push(path.clone()),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(policy)
    }

    pub fn new(root: &Path, overlay: &Overlay) -> Result<Self> {
        ensure!(
            !overlay.root.starts_with(root.join(".over")),
            ".over is reserved repository administration"
        );
        if root.join(".over").exists() {
            ensure!(
                !super::mapping::physical(&overlay.root)?
                    .starts_with(super::mapping::physical(&root.join(".over"))?),
                ".over is reserved repository administration"
            );
        }
        ensure!(
            overlay.root.starts_with(root),
            "overlay is outside the Over repository"
        );
        let mut policy = Self {
            root: root.to_path_buf(),
            overlay: overlay.clone(),
            rules: Vec::new(),
            rule_sources: Vec::new(),
            files: Vec::new(),
        };
        let mut ancestors: Vec<_> = overlay
            .root
            .ancestors()
            .take_while(|path| path.starts_with(root))
            .collect();
        ancestors.reverse();
        for directory in ancestors {
            // A nested rule file cannot reopen an excluded parent directory.
            if !policy.is_ignored(directory, true) {
                policy.load(directory)?;
            }
        }
        let mut walker = WalkDir::new(&overlay.root)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter();
        while let Some(entry) = walker.next() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    // Match collection's tolerant handling of inaccessible source directories.
                    tracing::warn!(%error, "skipping inaccessible source directory while discovering ignore rules");
                    continue;
                }
            };
            if entry.file_type().is_dir() {
                if policy.is_ignored(entry.path(), true) {
                    walker.skip_current_dir();
                } else {
                    policy.load(entry.path())?;
                }
            } else if !policy.is_ignored(entry.path(), false) {
                // Sidecars share this discovery; separate walkers multiplied source traversal costs.
                policy.files.push(entry.path().to_path_buf());
            }
        }
        Ok(policy)
    }

    pub(crate) fn files(&self) -> &[PathBuf] {
        &self.files
    }

    fn load(&mut self, directory: &Path) -> Result<()> {
        let path = directory.join(".overignore");
        match path.symlink_metadata() {
            // Like Git, never follow an ignore-file symlink outside the source tree.
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                // An unreadable directory is skipped by the source walker too;
                // an unreadable rule file itself still fails in builder.add below.
                tracing::warn!(path = %path.display(), %error, "cannot inspect ignore rules in inaccessible source directory");
                return Ok(());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect '{}'", path.display()));
            }
        }
        let mut builder = GitignoreBuilder::new(directory);
        if let Some(error) = builder.add(&path) {
            return Err(error).with_context(|| format!("invalid ignore file '{}'", path.display()));
        }
        self.rule_sources.push((
            path.clone(),
            std::fs::read(&path).with_context(|| format!("failed to read '{}'", path.display()))?,
        ));
        self.rules.push((directory.to_path_buf(), builder.build()?));
        Ok(())
    }

    pub(crate) fn identity(&self) -> SourcePolicyIdentity {
        SourcePolicyIdentity {
            root: self.root.clone(),
            overlay: self.overlay.root.clone(),
            excludes: self
                .overlay
                .scopes
                .iter()
                .map(|scope| (scope.root.clone(), scope.exclude.clone()))
                .collect(),
            rules: self.rule_sources.clone(),
        }
    }

    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        // Git administration is never overlay input, including apparent
        // sidecars or nested ignore rules. Negations cannot reopen it.
        if is_git_administration(path) || path.starts_with(self.root.join(".over")) {
            return true;
        }
        if let Ok(relative) = path.strip_prefix(&self.overlay.root)
            && self.overlay.is_excluded(relative)
        {
            return true;
        }
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        let mut candidate = self.root.clone();
        let components: Vec<_> = relative.components().collect();
        for (index, component) in components.iter().enumerate() {
            candidate.push(component);
            let directory = index + 1 < components.len() || is_dir;
            let mut ignored = false;
            for (base, rules) in &self.rules {
                if candidate != *base && candidate.starts_with(base) {
                    let matched = rules.matched(&candidate, directory);
                    if !matched.is_none() {
                        ignored = matched.is_ignore();
                    }
                }
            }
            if ignored {
                return true;
            }
        }
        false
    }

    pub fn includes(&self, source: &Path, is_dir: bool) -> bool {
        !is_control_file(source) && !self.is_ignored(source, is_dir)
    }

    pub fn validate_directory_link(&self, directory: &Path, source: &Path) -> Result<()> {
        crate::secrets::reject_encrypted_directory(directory)?;
        for entry in WalkDir::new(directory).min_depth(1) {
            let entry = entry?;
            let projected = source.join(entry.path().strip_prefix(directory)?);
            ensure!(
                self.includes(&projected, entry.file_type().is_dir()),
                "directory symlink '{}' would expose ignored content or control metadata '{}'; use file-level symlinks or checkout materialization",
                directory.display(),
                projected.display()
            );
        }
        Ok(())
    }
}

pub fn is_control_file(path: &Path) -> bool {
    if is_git_administration(path) {
        return true;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name == ".overignore"
        || super::EXTENSIONS.iter().any(|extension| {
            name == format!("over.{extension}")
                || name.ends_with(&format!(".link.{extension}"))
                || name.ends_with(&format!(".partial.{extension}"))
        })
}

fn is_git_administration(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str();
        name == ".git"
            // Case-insensitive filesystems must not admit a spelling alias.
            || (cfg!(any(windows, target_os = "macos"))
                && name.to_str().is_some_and(|name| name.eq_ignore_ascii_case(".git")))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;
    use crate::overlays::Repository;

    fn write(root: &Path, path: &str, content: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn git_inventory_loads_relevant_ignore_ancestors_but_not_deleted_sidecars() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        write(root, ".overignore", "*.pem\n");
        write(root, "ov/nested/.overignore", "!keep.pem\n");
        write(
            root,
            "ov/unrelated/.overignore",
            "[invalid rule bytes are never read",
        );
        let overlay = Repository::new(root.to_path_buf()).get("ov").unwrap();
        let policy = SourcePolicy::for_inventory(
            root,
            &overlay,
            &[
                root.join("ov/nested/keep.pem"),
                root.join("ov/nested/deleted.link.yaml"),
            ],
        )
        .unwrap();
        assert!(!policy.is_ignored(&root.join("ov/nested/keep.pem"), false));
        assert!(policy.is_ignored(&root.join("ov/nested/other.pem"), false));
        assert!(
            !policy
                .files()
                .contains(&root.join("ov/nested/deleted.link.yaml"))
        );
        assert!(
            policy
                .rule_sources
                .iter()
                .all(|(path, _)| !path.starts_with(root.join("ov/unrelated")))
        );
    }

    #[rstest]
    #[case("# comment\n*.pem\n", "secret.pem", false)]
    #[case("\\#secret\n", "#secret", false)]
    #[case("\\!secret\n", "!secret", false)]
    #[case("name\\ \n", "name ", false)]
    #[case("*.pem\n!keep.pem\n", "keep.pem", false)]
    #[case("!keep.pem\n*.pem\n", "keep.pem", false)]
    #[case("/secret\n", "ov/secret", false)]
    #[case("/ov/secret\n", "ov/secret", false)]
    #[case("cache/\n", "ov/cache", true)]
    #[case("cache/\n", "ov/cache", false)]
    #[case("cache/\n!cache/keep\n", "ov/cache/keep", false)]
    #[case("cache/\n!cache/\ncache/*\n!cache/keep\n", "ov/cache/keep", false)]
    #[case("a?c\n", "ov/abc", false)]
    #[case("**/build/**\n", "ov/deep/build/file", false)]
    #[case("ov/**/secret\n", "ov/secret", false)]
    #[case("ov/**/secret\n", "ov/deep/secret", false)]
    fn representative_patterns_agree_with_git(
        #[case] rules: &str,
        #[case] path: &str,
        #[case] is_dir: bool,
    ) {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("ov")).unwrap();
        write(root, ".overignore", rules);
        write(root, ".gitignore", rules);
        if is_dir {
            fs::create_dir_all(root.join(path)).unwrap();
        } else {
            write(root, path, "content");
        }
        git2::Repository::init(root).unwrap();
        let overlay = Repository::new(root.to_path_buf()).get("ov").unwrap();
        let policy = SourcePolicy::new(root, &overlay).unwrap();
        let output = Command::new("git")
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args([
                "-c",
                "core.excludesFile=/dev/null",
                "check-ignore",
                "--no-index",
                "--quiet",
                "--",
                path,
            ])
            .output()
            .unwrap();
        assert!(matches!(output.status.code(), Some(0 | 1)), "{output:?}");
        assert_eq!(
            policy.is_ignored(&root.join(path), is_dir),
            output.status.success(),
            "rules: {rules:?}, path: {path}"
        );
    }

    #[test]
    fn nested_rules_override_parents_without_affecting_siblings() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        write(root, ".overignore", "*.pem\nblocked/\n");
        write(root, "one/.overignore", "!keep.pem\n/local\n");
        write(root, "one/deep/.overignore", "keep.pem\n");
        write(root, "two/.overignore", "!other.pem\n");
        write(root, "one/blocked/.overignore", "!keep.pem\n");
        write(root, "one/blocked/keep.pem", "secret");
        for name in ["one", "two"] {
            let overlay = Repository::new(root.to_path_buf()).get(name).unwrap();
            let policy = SourcePolicy::new(root, &overlay).unwrap();
            assert!(policy.is_ignored(&overlay.root.join("drop.pem"), false));
            assert_eq!(
                policy.is_ignored(&overlay.root.join("keep.pem"), false),
                name == "two"
            );
            assert_eq!(
                policy.is_ignored(&overlay.root.join("local"), false),
                name == "one"
            );
            if name == "one" {
                assert!(policy.is_ignored(&overlay.root.join("deep/keep.pem"), false));
                assert!(policy.is_ignored(&overlay.root.join("blocked/keep.pem"), false));
                assert!(!policy.is_ignored(&overlay.root.join("deep/local"), false));
            }
        }
    }

    #[test]
    fn an_ignored_overlay_ancestor_cannot_be_reopened_from_inside() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        write(root, ".overignore", "overlays/\n");
        write(root, "overlays/one/.overignore", "!keep\n");
        let overlay = Repository::new(root.to_path_buf())
            .get("overlays/one")
            .unwrap();
        let policy = SourcePolicy::new(root, &overlay).unwrap();
        assert!(policy.is_ignored(&overlay.root.join("keep"), false));
    }

    #[test]
    fn a_new_policy_observes_rule_edits_and_never_uses_unrelated_ignore_sources() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("project");
        write(temp.path(), ".overignore", "*\n");
        write(&root, "ov/.gitignore", "*\n");
        write(&root, "ov/.ignore", "*\n");
        let overlay = Repository::new(root.clone()).get("ov").unwrap();
        let original = SourcePolicy::new(&root, &overlay).unwrap();
        assert!(!original.is_ignored(&overlay.root.join("file"), false));
        write(&root, ".overignore", "file\n");
        let updated = SourcePolicy::new(&root, &overlay).unwrap();
        assert!(updated.is_ignored(&overlay.root.join("file"), false));
        assert!(!original.is_ignored(&overlay.root.join("file"), false));
    }

    #[test]
    fn negation_cannot_reinclude_metadata_or_legacy_exclusions() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        write(root, "ov/.overignore", "!*\n");
        write(root, "ov/over.toml", "exclude = ['secret']\n");
        let overlay = Repository::new(root.to_path_buf()).get("ov").unwrap();
        let policy = SourcePolicy::new(root, &overlay).unwrap();
        for path in [
            ".git",
            ".git/config",
            "nested/.git/HEAD",
            "over.toml",
            ".overignore",
            "nested/.overignore",
            "app.link.yaml",
            "app.partial.toml",
            "secret",
        ] {
            assert!(!policy.includes(&overlay.root.join(path), false), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_only_rules_do_not_follow_symlinks_and_ignore_file_symlinks_are_not_loaded() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        write(root, ".overignore", "cache/\n");
        write(root, "rules", "*\n");
        fs::create_dir(root.join("ov")).unwrap();
        std::os::unix::fs::symlink("../rules", root.join("ov/.overignore")).unwrap();
        std::os::unix::fs::symlink("..", root.join("ov/cache")).unwrap();
        let overlay = Repository::new(root.to_path_buf()).get("ov").unwrap();
        let policy = SourcePolicy::new(root, &overlay).unwrap();
        assert!(!policy.is_ignored(&overlay.root.join("cache"), false));
        assert!(!policy.includes(&overlay.root.join(".overignore"), false));
    }
}
