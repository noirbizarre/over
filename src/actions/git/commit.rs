//! Native hooks and signing without publishing HEAD before recovery is prepared.

use std::fs;
use std::process::{Command, Output, Stdio};

use anyhow::{Context as _, Result, ensure};
use git2::{Oid, Repository};

use super::index::IndexSnapshot;

pub(crate) struct PreparedCommit {
    pub oid: Oid,
    head: Vec<u8>,
    parent: Option<Oid>,
    index: IndexSnapshot,
}

/// Keep the canonical Git context for conditional config and branch-sensitive
/// hooks, but quarantine their index/object writes until the tree is validated.
pub(crate) fn command(repo: &Repository) -> Result<Command> {
    let mut command = Command::new("git");
    command.current_dir(repo.workdir().context("repository has no worktree")?);
    for name in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
    ] {
        command.env_remove(name);
    }
    command.arg("--git-dir").arg(repo.path());
    #[cfg(all(test, unix))]
    {
        // Unit tests must not execute a developer's global hooks/signing setup.
        // Per-command overrides avoid racing process-global environment changes.
        command
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        for name in ["GIT_CONFIG", "GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS"] {
            command.env_remove(name);
        }
    }
    Ok(command)
}

fn checked(command: &mut Command) -> Result<Output> {
    let output = command.output().context("could not execute Git")?;
    ensure!(
        output.status.success(),
        "Git commit operation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

impl PreparedCommit {
    pub(crate) fn prepare(
        repo: &Repository,
        tree: Oid,
        message: &str,
        expected_parent: Option<Oid>,
    ) -> Result<Self> {
        let head = fs::read(repo.path().join("HEAD"))?;
        let parent = match repo.head() {
            Ok(head) => Some(head.peel_to_commit()?.id()),
            Err(error) if error.code() == git2::ErrorCode::UnbornBranch => None,
            Err(error) => return Err(error.into()),
        };
        // The tree belongs to the caller's original parent, not a newer tip
        // observed after tree construction; adopting it would revert unrelated changes.
        ensure!(
            parent == expected_parent,
            "branch changed before commit preparation"
        );
        let index = IndexSnapshot::read(&repo.path().join("index"))?;
        let temporary = tempfile::tempdir_in(repo.path())?;
        let objects = temporary.path().join("objects");
        fs::create_dir(&objects)?;
        let common = checked(command(repo)?.args(["rev-parse", "--git-common-dir"]))?;
        let common = String::from_utf8(common.stdout)?;
        let common = repo
            .workdir()
            .context("repository has no worktree")?
            .join(common.trim());
        let private = || -> Result<Command> {
            let mut command = command(repo)?;
            command
                .env("GIT_INDEX_FILE", temporary.path().join("index"))
                .env("GIT_OBJECT_DIRECTORY", &objects)
                .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", common.join("objects"))
                // Recursive secret writes would wait on their parent's mutation lock.
                .env("OVER_COMMIT_REPOSITORY", repo.commondir().canonicalize()?)
                // Like git commit -m, no editor is involved in these hooks.
                .env("GIT_EDITOR", ":");
            Ok(command)
        };
        checked(private()?.args(["read-tree", &tree.to_string()]))?;
        let message_path = temporary.path().join("message");
        fs::write(&message_path, message)?;
        for (hook, arguments) in [
            ("pre-commit", Vec::new()),
            (
                "prepare-commit-msg",
                vec![message_path.as_os_str(), std::ffi::OsStr::new("message")],
            ),
            ("commit-msg", vec![message_path.as_os_str()]),
        ] {
            checked(
                private()?
                    .args(["hook", "run", "--ignore-missing", hook, "--"])
                    .args(arguments),
            )?;
        }
        let actual = checked(private()?.arg("write-tree"))?;
        ensure!(
            String::from_utf8(actual.stdout)?.trim() == tree.to_string(),
            "a commit hook changed the prepared snapshot; inspect its changes and retry"
        );
        let cleanup = command(repo)?
            .args(["config", "--get", "commit.cleanup"])
            .output()?;
        ensure!(
            cleanup.status.success() || cleanup.status.code() == Some(1),
            "could not read commit.cleanup"
        );
        let cleanup = String::from_utf8(cleanup.stdout)?;
        let cleanup = cleanup.trim();
        if cleanup != "verbatim" {
            // Without an editor, default/scissors use whitespace cleanup, just
            // like git commit -F. stripspace reads core.commentChar itself.
            ensure!(
                matches!(
                    cleanup,
                    "" | "default" | "whitespace" | "strip" | "scissors"
                ),
                "invalid commit.cleanup setting"
            );
            let mut strip = command(repo)?;
            strip
                .arg("stripspace")
                .stdin(Stdio::from(fs::File::open(&message_path)?));
            if cleanup == "strip" {
                strip.arg("--strip-comments");
            }
            fs::write(&message_path, checked(&mut strip)?.stdout)?;
        }
        let rendered = fs::read_to_string(&message_path)?;
        ensure!(
            !rendered.trim().is_empty() && !rendered.contains('\0'),
            "commit message must be nonempty and contain no NUL"
        );
        let mut commit = private()?;
        commit
            .arg("commit-tree")
            .arg(tree.to_string())
            .arg("-F")
            .arg(&message_path);
        if let Some(parent) = parent {
            commit.arg("-p").arg(parent.to_string());
        }
        let signing = command(repo)?
            .args(["config", "--type=bool", "--get", "commit.gpgSign"])
            .output()?;
        ensure!(
            signing.status.success() || signing.status.code() == Some(1),
            "could not read commit.gpgSign"
        );
        // commit-tree deliberately ignores commit.gpgSign; porcelain callers must opt in.
        if signing.stdout == b"true\n" {
            commit.arg("-S");
        }
        let output = checked(&mut commit)?;
        let oid = Oid::from_str(String::from_utf8(output.stdout)?.trim())?;
        let prepared = Self {
            oid,
            head,
            parent,
            index,
        };
        prepared.validate(repo)?;
        let bytes = checked(private()?.args(["cat-file", "commit", &oid.to_string()]))?.stdout;
        // Only this verified commit is promoted; hook-created blobs stay quarantined.
        ensure!(
            repo.odb()?.write(git2::ObjectType::Commit, &bytes)? == oid,
            "commit object changed during promotion"
        );
        Ok(prepared)
    }

    pub(crate) fn validate(&self, repo: &Repository) -> Result<()> {
        ensure!(
            fs::read(repo.path().join("HEAD"))? == self.head,
            "HEAD changed while preparing commit"
        );
        ensure!(
            repo.head().ok().and_then(|head| head.target()) == self.parent,
            "branch changed while preparing commit"
        );
        ensure!(
            IndexSnapshot::read(&repo.path().join("index"))? == self.index,
            "index changed while preparing commit"
        );
        Ok(())
    }

    /// Publish the exact signed object recorded by the caller's recovery journal.
    pub(crate) fn publish(&self, repo: &Repository) -> Result<Oid> {
        ensure!(
            fs::read(repo.path().join("HEAD"))? == self.head,
            "HEAD changed before commit publication"
        );
        let reference = std::str::from_utf8(&self.head)?
            .trim()
            .strip_prefix("ref: ")
            .unwrap_or("HEAD");
        let published = checked(
            command(repo)?
                .env("OVER_COMMIT_REPOSITORY", repo.commondir().canonicalize()?)
                .args([
                    "update-ref",
                    "-m",
                    "over commit",
                    reference,
                    &self.oid.to_string(),
                    &self.parent.unwrap_or(Oid::ZERO_SHA1).to_string(),
                ]),
        );
        if let Err(error) = published {
            let reference = match repo.find_reference(reference) {
                Ok(reference) => Some(reference),
                Err(error) if error.code() == git2::ErrorCode::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let current = reference
                .map(|reference| reference.peel_to_commit().map(|commit| commit.id()))
                .transpose()?;
            let authoritative = match current {
                Some(current) => {
                    current == self.oid || repo.graph_descendant_of(current, self.oid)?
                }
                None => false,
            };
            // A reference-transaction hook can kill Git after its ref update.
            // All callers must finalize that published OID instead of rolling it back.
            ensure!(authoritative, "{error:#}");
            tracing::warn!(
                "commit {} succeeded despite Git's publication error: {error:#}",
                self.oid
            );
        }
        Ok(self.oid)
    }
}

pub(crate) fn post_commit(repo: &Repository) {
    // HEAD is authoritative: failure here must never invite a duplicate commit.
    if let Err(error) = command(repo).and_then(|mut command| {
        checked(
            command
                .env("OVER_COMMIT_REPOSITORY", repo.commondir().canonicalize()?)
                .args(["hook", "run", "--ignore-missing", "post-commit"]),
        )
    }) {
        tracing::warn!("commit succeeded, but post-commit failed: {error:#}");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> (tempfile::TempDir, Repository, Oid) {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repository::init(directory.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Commit Test").unwrap();
        config.set_str("user.email", "commit@example.com").unwrap();
        config.set_bool("commit.gpgSign", false).unwrap();
        let tree = repo.treebuilder(None).unwrap().write().unwrap();
        (directory, repo, tree)
    }

    fn hook(repo: &Repository, name: &str, body: &str) {
        let path = repo.path().join("hooks").join(name);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn message_hooks_edit_the_exact_published_commit_and_post_commit_sees_head() {
        let (_directory, repo, tree) = fixture();
        hook(
            &repo,
            "commit-msg",
            "printf 'chore: hook message\\n' > \"$1\"",
        );
        hook(&repo, "post-commit", "git rev-parse HEAD > published");
        let prepared = PreparedCommit::prepare(&repo, tree, "original", None).unwrap();
        assert!(repo.head().is_err());
        assert_eq!(
            repo.find_commit(prepared.oid).unwrap().message().unwrap(),
            "chore: hook message\n"
        );
        prepared.publish(&repo).unwrap();
        post_commit(&repo);
        assert_eq!(
            fs::read_to_string(repo.workdir().unwrap().join("published"))
                .unwrap()
                .trim(),
            prepared.oid.to_string()
        );
    }

    #[test]
    fn hook_staging_is_rejected_without_touching_the_real_index_or_objects() {
        let (_directory, repo, tree) = fixture();
        hook(
            &repo,
            "pre-commit",
            "printf private > hook-file\ngit add hook-file",
        );
        let blob = Oid::hash_object(git2::ObjectType::Blob, b"private").unwrap();
        assert!(PreparedCommit::prepare(&repo, tree, "message", None).is_err());
        assert!(repo.head().is_err());
        assert!(!repo.path().join("index").exists());
        assert!(!repo.odb().unwrap().exists(blob));
        assert_eq!(
            fs::read(repo.workdir().unwrap().join("hook-file")).unwrap(),
            b"private"
        );
    }

    #[test]
    fn configured_ssh_signing_signs_the_published_object() {
        let (directory, repo, tree) = fixture();
        let key = directory.path().join("signing-key");
        assert!(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let mut config = repo.config().unwrap();
        config.set_bool("commit.gpgSign", true).unwrap();
        config.set_str("gpg.format", "ssh").unwrap();
        config
            .set_str("user.signingKey", key.to_str().unwrap())
            .unwrap();
        let allowed = directory.path().join("allowed");
        fs::write(
            &allowed,
            format!(
                "commit@example.com {}",
                fs::read_to_string(key.with_extension("pub")).unwrap()
            ),
        )
        .unwrap();
        config
            .set_str("gpg.ssh.allowedSignersFile", allowed.to_str().unwrap())
            .unwrap();
        let prepared = PreparedCommit::prepare(&repo, tree, "chore: signed", None).unwrap();
        prepared.publish(&repo).unwrap();
        checked(command(&repo).unwrap().args(["verify-commit", "HEAD"])).unwrap();
    }

    #[test]
    fn real_index_refreshes_during_hooks_and_after_preparation_are_accepted() {
        let (directory, repo, tree) = fixture();
        fs::write(directory.path().join("unrelated"), b"staged").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("unrelated")).unwrap();
        index.write().unwrap();
        let before = fs::read(repo.path().join("index")).unwrap();
        hook(
            &repo,
            "pre-commit",
            "touch -m -t 200001010000 unrelated\nGIT_INDEX_FILE=\"$(git rev-parse --absolute-git-dir)/index\" git update-index --refresh",
        );
        let prepared = PreparedCommit::prepare(&repo, tree, "message", None).unwrap();
        assert_ne!(fs::read(repo.path().join("index")).unwrap(), before);
        index.read(true).unwrap();
        index.set_version(4).unwrap();
        index.write().unwrap();
        prepared.validate(&repo).unwrap();
        // A same-tree flag change must still be caught by the final validation.
        let mut entry = index.get(0).unwrap();
        entry.flags |= 0x8000;
        index.add(&entry).unwrap();
        index.write().unwrap();
        assert!(prepared.validate(&repo).is_err());
    }

    #[test]
    fn concurrent_real_index_staging_is_rejected_and_preserved() {
        let (_directory, repo, tree) = fixture();
        hook(
            &repo,
            "pre-commit",
            "printf concurrent > unrelated\nunset GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES\nGIT_INDEX_FILE=\"$(git rev-parse --absolute-git-dir)/index\" git add unrelated",
        );
        let error = PreparedCommit::prepare(&repo, tree, "message", None)
            .err()
            .unwrap();
        assert!(error.to_string().contains("index changed"));
        assert!(repo.head().is_err());
        assert!(
            repo.index()
                .unwrap()
                .get_path(std::path::Path::new("unrelated"), 0)
                .is_some()
        );
        assert!(!repo.path().join("index.lock").exists());
    }

    #[test]
    fn signing_failure_does_not_publish_head_or_index() {
        let (_directory, repo, tree) = fixture();
        let mut config = repo.config().unwrap();
        config.set_bool("commit.gpgSign", true).unwrap();
        config.set_str("gpg.program", "false").unwrap();
        config.set_str("gpg.format", "openpgp").unwrap();
        assert!(PreparedCommit::prepare(&repo, tree, "chore: cannot sign", None).is_err());
        assert!(repo.head().is_err());
        assert!(!repo.path().join("index").exists());
    }

    #[rstest::rstest]
    #[case("strip", "subject\n")]
    #[case("whitespace", "subject\n# comment\n")]
    #[case("verbatim", "\nsubject  \n# comment\n\n")]
    fn configured_cleanup_is_applied_before_signing(#[case] cleanup: &str, #[case] expected: &str) {
        let (_directory, repo, tree) = fixture();
        repo.config()
            .unwrap()
            .set_str("commit.cleanup", cleanup)
            .unwrap();
        let prepared =
            PreparedCommit::prepare(&repo, tree, "\nsubject  \n# comment\n\n", None).unwrap();
        assert_eq!(
            repo.find_commit(prepared.oid)
                .unwrap()
                .message_raw()
                .unwrap(),
            expected
        );
    }

    #[test]
    fn configured_relative_hook_paths_keep_the_real_branch_context() {
        let (directory, repo, tree) = fixture();
        fs::create_dir(directory.path().join("custom-hooks")).unwrap();
        let path = directory.path().join("custom-hooks/commit-msg");
        fs::write(&path, "#!/bin/sh\ngit symbolic-ref --short HEAD > \"$1\"\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        repo.config()
            .unwrap()
            .set_str("core.hooksPath", "custom-hooks")
            .unwrap();
        let branch = checked(
            command(&repo)
                .unwrap()
                .args(["symbolic-ref", "--short", "HEAD"]),
        )
        .unwrap()
        .stdout;
        let prepared = PreparedCommit::prepare(&repo, tree, "original", None).unwrap();
        assert_eq!(
            repo.find_commit(prepared.oid).unwrap().message_bytes(),
            branch
        );
    }

    #[test]
    fn a_newer_parent_cannot_be_adopted_for_an_already_built_tree() {
        let (_directory, repo, tree) = fixture();
        let signature = repo.signature().unwrap();
        let original = repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "initial",
                &repo.find_tree(tree).unwrap(),
                &[],
            )
            .unwrap();
        let blob = repo.blob(b"concurrent unrelated change").unwrap();
        let mut builder = repo
            .treebuilder(Some(&repo.find_tree(tree).unwrap()))
            .unwrap();
        builder.insert("unrelated", blob, 0o100644).unwrap();
        let newer = repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "concurrent",
                &repo.find_tree(builder.write().unwrap()).unwrap(),
                &[&repo.find_commit(original).unwrap()],
            )
            .unwrap();
        assert!(PreparedCommit::prepare(&repo, tree, "selected", Some(original)).is_err());
        assert_eq!(repo.head().unwrap().target(), Some(newer));
        assert_eq!(
            repo.head()
                .unwrap()
                .peel_to_tree()
                .unwrap()
                .get_name("unrelated")
                .unwrap()
                .id(),
            blob
        );
    }

    #[test]
    fn unit_git_commands_disable_external_configuration_without_changing_the_process() {
        let (_directory, repo, _) = fixture();
        let command = command(&repo).unwrap();
        let overrides: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            overrides[std::ffi::OsStr::new("GIT_CONFIG_GLOBAL")],
            Some(std::ffi::OsStr::new("/dev/null"))
        );
        assert_eq!(
            overrides[std::ffi::OsStr::new("GIT_CONFIG_NOSYSTEM")],
            Some(std::ffi::OsStr::new("1"))
        );
        assert_eq!(
            overrides[std::ffi::OsStr::new("GIT_CONFIG_PARAMETERS")],
            None
        );
    }
}
