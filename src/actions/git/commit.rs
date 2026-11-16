//! Native hooks and signing without publishing HEAD before recovery is prepared.

use std::fs;
use std::process::{Command, Output};

use anyhow::{Context as _, Result, ensure};
use git2::{Oid, Repository};

use super::index::read_index;

pub(crate) struct PreparedCommit {
    pub oid: Oid,
    head: Vec<u8>,
    parent: Option<Oid>,
    index: Option<Vec<u8>>,
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
    pub(crate) fn prepare(repo: &Repository, tree: Oid, message: &str) -> Result<Self> {
        let head = fs::read(repo.path().join("HEAD"))?;
        let parent = match repo.head() {
            Ok(head) => Some(head.peel_to_commit()?.id()),
            Err(error) if error.code() == git2::ErrorCode::UnbornBranch => None,
            Err(error) => return Err(error.into()),
        };
        let index = read_index(&repo.path().join("index"))?;
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
            read_index(&repo.path().join("index"))? == self.index,
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
        checked(command(repo)?.args([
            "update-ref",
            "-m",
            "over commit",
            reference,
            &self.oid.to_string(),
            &self.parent.unwrap_or(Oid::ZERO_SHA1).to_string(),
        ]))?;
        Ok(self.oid)
    }
}

pub(crate) fn post_commit(repo: &Repository) {
    // HEAD is authoritative: failure here must never invite a duplicate commit.
    if let Err(error) = command(repo).and_then(|mut command| {
        checked(command.args(["hook", "run", "--ignore-missing", "post-commit"]))
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
        let prepared = PreparedCommit::prepare(&repo, tree, "original").unwrap();
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
        assert!(PreparedCommit::prepare(&repo, tree, "message").is_err());
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
        let prepared = PreparedCommit::prepare(&repo, tree, "chore: signed").unwrap();
        prepared.publish(&repo).unwrap();
        checked(command(&repo).unwrap().args(["verify-commit", "HEAD"])).unwrap();
    }

    #[test]
    fn signing_failure_does_not_publish_head_or_index() {
        let (_directory, repo, tree) = fixture();
        let mut config = repo.config().unwrap();
        config.set_bool("commit.gpgSign", true).unwrap();
        config.set_str("gpg.program", "false").unwrap();
        config.set_str("gpg.format", "openpgp").unwrap();
        assert!(PreparedCommit::prepare(&repo, tree, "chore: cannot sign").is_err());
        assert!(repo.head().is_err());
        assert!(!repo.path().join("index").exists());
    }
}
