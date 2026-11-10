//! Restore must change only the explicitly requested destinations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pretty_assertions::assert_eq;
use rstest::rstest;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_descriptor("mode = 'checkout'\n")
    }

    fn with_descriptor(descriptor: &str) -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::create_dir_all(source.join("ov/sub")).unwrap();
        fs::write(source.join("ov/over.toml"), descriptor).unwrap();
        for name in ["file", "deleted", "sub/file", "literal[1]"] {
            fs::write(source.join("ov").join(name), "historical\n").unwrap();
        }
        fs::write(source.join("unrelated"), "unrelated\n").unwrap();
        let fixture = Self {
            temp,
            source,
            target,
        };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.name", "Test"]);
        fixture.git(&["config", "user.email", "test@example.com"]);
        fixture.git(&["config", "core.autocrlf", "false"]);
        // Background maintenance would make repository inventories race with Git,
        // obscuring whether restore itself changed source state.
        fixture.git(&["config", "gc.auto", "0"]);
        fixture.git(&["config", "maintenance.auto", "false"]);
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "historical"]);
        fs::write(fixture.source.join("ov/file"), "HEAD\n").unwrap();
        fs::write(fixture.source.join("ov/new-in-head"), "new\n").unwrap();
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "current"]);
        checked(
            fixture
                .command("apply")
                .args(["ov", "--no-prompt"])
                .output()
                .unwrap(),
        );
        fixture
    }

    fn environment(&self, command: &mut Command) {
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .env("XDG_STATE_HOME", self.temp.path().join("state"));
        for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            command.env_remove(name);
        }
    }

    fn git(&self, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        self.environment(&mut command);
        checked(
            command
                .current_dir(&self.source)
                .args(args)
                .output()
                .unwrap(),
        )
    }

    fn command(&self, name: &str) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        self.environment(&mut command);
        command
            .arg("--home")
            .arg(&self.source)
            .arg(name)
            .arg("--root")
            .arg(&self.target);
        command
    }

    fn restore(&self, options: &[&str], files: &[&str]) -> Output {
        let mut command = self.command("restore");
        command.args(["--overlay", "ov"]).args(options);
        for file in files {
            command.arg(self.target.join(file));
        }
        command.output().unwrap()
    }

    fn staged_and_unstaged(&self) {
        fs::write(self.source.join("ov/file"), "index\n").unwrap();
        self.git(&["add", "ov/file"]);
        // Source-worktree edits are independent of restore's target destination.
        fs::write(self.source.join("ov/file"), "source edits\n").unwrap();
        fs::write(self.target.join("file"), "worktree\n").unwrap();
        fs::write(self.source.join("unrelated"), "other staged\n").unwrap();
        self.git(&["add", "unrelated"]);
        fs::write(self.target.join("deleted"), "other worktree\n").unwrap();
    }
}

fn checked(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry.path().strip_prefix(root).unwrap().to_path_buf(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[rstest]
#[case(&[], "index\n", "index\n")]
#[case(&["--worktree"], "index\n", "index\n")]
#[case(&["--staged"], "HEAD\n", "worktree\n")]
#[case(&["--staged", "--worktree"], "HEAD\n", "HEAD\n")]
#[case(&["--source", "HEAD~1"], "index\n", "historical\n")]
#[case(&["--source", "HEAD~1", "--worktree"], "index\n", "historical\n")]
#[case(&["--source", "HEAD~1", "--staged"], "historical\n", "worktree\n")]
#[case(&["--source", "HEAD~1", "--staged", "--worktree"], "historical\n", "historical\n")]
fn restore_changes_only_the_requested_destinations(
    #[case] options: &[&str],
    #[case] index: &str,
    #[case] target: &str,
) {
    let fixture = Fixture::new();
    fixture.staged_and_unstaged();
    let source_before = inventory(&fixture.source);
    let state_before = inventory(&fixture.temp.path().join("state"));
    checked(fixture.restore(options, &["file"]));
    assert_eq!(fixture.git(&["show", ":ov/file"]).stdout, index.as_bytes());
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        target.as_bytes()
    );
    assert_eq!(
        fs::read(fixture.source.join("ov/file")).unwrap(),
        b"source edits\n"
    );
    assert_eq!(
        fixture.git(&["show", ":unrelated"]).stdout,
        b"other staged\n"
    );
    assert_eq!(
        fs::read(fixture.target.join("deleted")).unwrap(),
        b"other worktree\n"
    );
    let mut source_after = inventory(&fixture.source);
    let mut source_before = source_before;
    if options.contains(&"--staged") {
        source_after.remove(Path::new(".git/index"));
        source_before.remove(Path::new(".git/index"));
    }
    assert_eq!(source_after, source_before);
    assert_eq!(inventory(&fixture.temp.path().join("state")), state_before);
}

#[test]
fn deleted_indexed_files_are_recreated_without_changing_the_index() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.target.join("deleted")).unwrap();
    let before = inventory(&fixture.source);
    checked(fixture.restore(&[], &["deleted"]));
    assert_eq!(
        fs::read(fixture.target.join("deleted")).unwrap(),
        b"historical\n"
    );
    assert_eq!(inventory(&fixture.source), before);
}

#[rstest]
#[case(false)]
#[case(true)]
fn unstaging_a_new_file_removes_only_requested_destinations(#[case] worktree: bool) {
    let fixture = Fixture::new();
    fs::write(fixture.source.join("ov/added"), "added\n").unwrap();
    fixture.git(&["add", "ov/added"]);
    fs::write(fixture.target.join("added"), "added locally\n").unwrap();
    let flags = if worktree {
        vec!["--staged", "--worktree"]
    } else {
        vec!["--staged"]
    };
    checked(fixture.restore(&flags, &["added"]));
    assert!(fixture.git(&["ls-files", "ov/added"]).stdout.is_empty());
    assert_eq!(fixture.target.join("added").exists(), !worktree);
    assert_eq!(
        fs::read(fixture.source.join("ov/added")).unwrap(),
        b"added\n"
    );
}

#[rstest]
#[case(&[], true, false)]
#[case(&["--staged"], false, true)]
#[case(&["--staged", "--worktree"], false, false)]
fn historical_absence_removes_only_selected_destinations(
    #[case] flags: &[&str],
    #[case] indexed: bool,
    #[case] deployed: bool,
) {
    let fixture = Fixture::new();
    let mut options = vec!["--source", "HEAD~1"];
    options.extend(flags);
    checked(fixture.restore(&options, &["new-in-head"]));
    assert_eq!(
        !fixture
            .git(&["ls-files", "ov/new-in-head"])
            .stdout
            .is_empty(),
        indexed
    );
    assert_eq!(fixture.target.join("new-in-head").exists(), deployed);
}

#[test]
fn historical_only_files_can_be_recovered_but_untracked_collisions_are_preserved() {
    let fixture = Fixture::new();
    fixture.git(&["rm", "ov/deleted"]);
    fixture.git(&["commit", "-qm", "remove historical file"]);
    fs::remove_file(fixture.target.join("deleted")).unwrap();
    let before = inventory(&fixture.source);
    checked(fixture.restore(&["--source", "HEAD~1"], &["deleted"]));
    assert_eq!(
        fs::read(fixture.target.join("deleted")).unwrap(),
        b"historical\n"
    );
    assert_eq!(inventory(&fixture.source), before);
    fs::write(fixture.target.join("deleted"), "untracked replacement\n").unwrap();
    assert!(
        !fixture
            .restore(&["--source", "HEAD~1"], &["deleted"])
            .status
            .success()
    );
    assert_eq!(
        fs::read(fixture.target.join("deleted")).unwrap(),
        b"untracked replacement\n"
    );
}

#[rstest]
#[case(&["--source", "not-a-revision"], "file")]
#[case(&[], "untracked")]
#[case(&["--staged", "--worktree"], "../outside")]
#[case(&[], "over.toml")]
#[case(&[], "sub")]
fn invalid_requests_preflight_every_path_before_writing(
    #[case] options: &[&str],
    #[case] invalid: &str,
) {
    let fixture = Fixture::new();
    fixture.staged_and_unstaged();
    fs::write(fixture.target.join("untracked"), "keep me\n").unwrap();
    fs::write(fixture.temp.path().join("outside"), "outside\n").unwrap();
    let source = inventory(&fixture.source);
    let target = inventory(&fixture.target);
    assert!(
        !fixture
            .restore(options, &["file", invalid])
            .status
            .success()
    );
    assert_eq!(inventory(&fixture.source), source);
    assert_eq!(inventory(&fixture.target), target);
}

#[test]
fn default_restore_does_not_clean_a_staged_deletion() {
    let fixture = Fixture::new();
    fixture.git(&["rm", "--cached", "ov/file"]);
    let source = inventory(&fixture.source);
    let target = inventory(&fixture.target);
    assert!(!fixture.restore(&[], &["file"]).status.success());
    assert_eq!(inventory(&fixture.source), source);
    assert_eq!(inventory(&fixture.target), target);
}

#[test]
fn dry_run_and_existing_index_locks_leave_all_inputs_unchanged() {
    let fixture = Fixture::new();
    fixture.staged_and_unstaged();
    let source = inventory(&fixture.source);
    let target = inventory(&fixture.target);
    checked(fixture.restore(&["--dry-run", "--staged", "--worktree"], &["file"]));
    assert_eq!(inventory(&fixture.source), source);
    assert_eq!(inventory(&fixture.target), target);
    fs::write(fixture.source.join(".git/index.lock"), "another writer\n").unwrap();
    assert!(
        !fixture
            .restore(&["--staged", "--worktree"], &["file"])
            .status
            .success()
    );
    assert_eq!(
        fs::read(fixture.source.join(".git/index.lock")).unwrap(),
        b"another writer\n"
    );
    fs::remove_file(fixture.source.join(".git/index.lock")).unwrap();
    assert_eq!(inventory(&fixture.source), source);
    assert_eq!(inventory(&fixture.target), target);
}

#[test]
fn ownership_is_inferred_and_literal_metacharacters_are_not_pathspecs() {
    let fixture = Fixture::new();
    fs::write(fixture.target.join("literal[1]"), "edited\n").unwrap();
    checked(
        fixture
            .command("restore")
            .current_dir(&fixture.target)
            .arg("literal[1]")
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(fixture.target.join("literal[1]")).unwrap(),
        b"historical\n"
    );
}

#[cfg(unix)]
#[test]
fn symlink_ancestors_and_nested_repositories_cannot_redirect_restore() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let outside = fixture.temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file"), "outside\n").unwrap();
    fs::remove_dir_all(fixture.target.join("sub")).unwrap();
    symlink(&outside, fixture.target.join("sub")).unwrap();
    assert!(!fixture.restore(&[], &["sub/file"]).status.success());
    assert_eq!(fs::read(outside.join("file")).unwrap(), b"outside\n");
    fs::remove_file(fixture.target.join("sub")).unwrap();
    fs::create_dir(fixture.target.join("sub")).unwrap();
    fs::create_dir(fixture.target.join("sub/.git")).unwrap();
    assert!(!fixture.restore(&[], &["sub/file"]).status.success());
}

#[cfg(unix)]
#[test]
fn restoring_private_hardlinked_files_preserves_access_and_other_links() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let fixture = Fixture::new();
    fs::write(fixture.target.join("file"), "local\n").unwrap();
    fs::set_permissions(
        fixture.target.join("file"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let other = fixture.temp.path().join("other-link");
    fs::hard_link(fixture.target.join("file"), &other).unwrap();
    checked(fixture.restore(&[], &["file"]));
    assert_eq!(fs::read(&other).unwrap(), b"local\n");
    assert_eq!(fs::read(fixture.target.join("file")).unwrap(), b"HEAD\n");
    assert_ne!(
        fs::metadata(&other).unwrap().ino(),
        fs::metadata(fixture.target.join("file")).unwrap().ino()
    );
    assert_eq!(
        fs::metadata(fixture.target.join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(target_os = "macos")]
#[test]
fn restoring_macos_hardlinks_preserves_exact_acls_xattrs_resource_forks_and_ownership() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let fixture = Fixture::new();
    let file = fixture.target.join("file");
    let native = |program: &str, arguments: &[&str], path: &Path| {
        checked(
            Command::new(program)
                .args(arguments)
                .arg(path)
                .output()
                .unwrap(),
        )
    };
    // Give the original an inherited ACL, then change the parent's inheritable
    // ACL so a naive CLONE_ACL merge would alter the restored file's access.
    native(
        "/bin/chmod",
        &[
            "+a",
            "everyone allow readattr,file_inherit,directory_inherit",
        ],
        &fixture.target,
    );
    fs::remove_file(&file).unwrap();
    fs::write(&file, "local\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    native("/bin/chmod", &["+a", "everyone allow readextattr"], &file);
    native("/bin/chmod", &["-N"], &fixture.target);
    native(
        "/bin/chmod",
        &[
            "+a",
            "everyone allow readsecurity,file_inherit,directory_inherit",
        ],
        &fixture.target,
    );
    native(
        "/usr/bin/xattr",
        &["-w", "over.restore", "native attribute"],
        &file,
    );
    fs::write(file.join("..namedfork/rsrc"), b"resource fork").unwrap();
    let acl = |path: &Path| {
        String::from_utf8(native("/bin/ls", &["-lde"], path).stdout)
            .unwrap()
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let expected_acl = acl(&file);
    assert!(!expected_acl.is_empty());
    let original = fs::metadata(&file).unwrap();
    let other = fixture.temp.path().join("other-link");
    fs::hard_link(&file, &other).unwrap();

    checked(fixture.restore(&[], &["file"]));

    let restored = fs::metadata(&file).unwrap();
    assert_eq!(
        (restored.uid(), restored.gid(), restored.mode()),
        (original.uid(), original.gid(), original.mode())
    );
    assert_ne!(restored.ino(), original.ino());
    assert_eq!(fs::metadata(&other).unwrap().ino(), original.ino());
    for path in [&file, &other] {
        assert_eq!(acl(path), expected_acl);
        assert_eq!(
            native("/usr/bin/xattr", &["-p", "over.restore"], path).stdout,
            b"native attribute\n"
        );
        assert_eq!(
            fs::read(path.join("..namedfork/rsrc")).unwrap(),
            b"resource fork"
        );
    }
    assert_eq!(fs::read(file).unwrap(), b"HEAD\n");
    assert_eq!(fs::read(other).unwrap(), b"local\n");
}

#[test]
fn linked_worktrees_restore_their_own_index_only() {
    let mut fixture = Fixture::new();
    let primary_index = fs::read(fixture.source.join(".git/index")).unwrap();
    let linked = fixture.temp.path().join("linked");
    fixture.git(&["worktree", "add", "-qb", "linked", linked.to_str().unwrap()]);
    fixture.source = linked;
    fixture.target = fixture.temp.path().join("linked-target");
    checked(
        fixture
            .command("apply")
            .args(["ov", "--no-prompt"])
            .output()
            .unwrap(),
    );
    fixture.staged_and_unstaged();
    checked(fixture.restore(&["--staged"], &["file"]));
    assert_eq!(fixture.git(&["show", ":ov/file"]).stdout, b"HEAD\n");
    assert_eq!(
        fs::read(fixture.temp.path().join("source/.git/index")).unwrap(),
        primary_index
    );
}

#[test]
fn unrelated_conflicts_survive_but_selected_conflicts_are_rejected() {
    let fixture = Fixture::new();
    let oid = String::from_utf8(fixture.git(&["rev-parse", "HEAD:ov/file"]).stdout).unwrap();
    // Populate real merge stages so the test also checks preservation of index extensions.
    let add_conflict = |path: &str| {
        let mut command = Command::new("git");
        fixture.environment(&mut command);
        let mut child = command
            .current_dir(&fixture.source)
            .args(["update-index", "--index-info"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write as _;
        write!(
            child.stdin.take().unwrap(),
            "0 {}\t{path}\n100644 {} 1\t{path}\n100644 {} 2\t{path}\n",
            "0".repeat(40),
            oid.trim(),
            oid.trim()
        )
        .unwrap();
        assert!(child.wait().unwrap().success());
    };
    add_conflict("unrelated");
    fs::write(fixture.target.join("file"), "edited\n").unwrap();
    let conflict_before = fixture.git(&["ls-files", "--unmerged"]).stdout;
    checked(fixture.restore(&["--staged", "--worktree"], &["file"]));
    assert_eq!(
        fixture.git(&["ls-files", "--unmerged"]).stdout,
        conflict_before
    );
    add_conflict("ov/file");
    let before = inventory(&fixture.source);
    assert!(
        !fixture
            .restore(&["--staged", "--worktree"], &["file"])
            .status
            .success()
    );
    assert_eq!(inventory(&fixture.source), before);
}

#[test]
fn current_ignore_policy_cannot_be_bypassed_by_a_historical_source() {
    let fixture = Fixture::new();
    fs::write(fixture.source.join("ov/.overignore"), "file\n").unwrap();
    fs::write(fixture.target.join("file"), "preserved\n").unwrap();
    let before = inventory(&fixture.source);
    assert!(
        !fixture
            .restore(&["--source", "HEAD~1", "--staged", "--worktree"], &["file"])
            .status
            .success()
    );
    assert_eq!(inventory(&fixture.source), before);
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        b"preserved\n"
    );
}

#[test]
fn configured_defaults_select_an_overlay_and_overlapping_inference_is_ambiguous() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.source.join("other")).unwrap();
    fs::write(
        fixture.source.join("other/over.toml"),
        "mode = 'checkout'\n",
    )
    .unwrap();
    fs::write(fixture.source.join("other/file"), "other\n").unwrap();
    fixture.git(&["add", "other"]);
    fixture.git(&["commit", "-qm", "add overlapping overlay"]);
    fs::write(fixture.target.join("file"), "preserved\n").unwrap();
    assert!(
        !fixture
            .command("restore")
            .arg(fixture.target.join("file"))
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        b"preserved\n"
    );
    fs::write(fixture.source.join("over.toml"), "default_overlay = 'ov'\n").unwrap();
    checked(
        fixture
            .command("restore")
            .arg(fixture.target.join("file"))
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read(fixture.target.join("file")).unwrap(), b"HEAD\n");
}

#[test]
fn help_explains_destructive_destinations_and_missing_patch_support() {
    let fixture = Fixture::new();
    let help = checked(fixture.command("restore").arg("--help").output().unwrap());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("Discard selected changes permanently"));
    assert!(help.contains("historical recovery remains unstaged"));
    assert!(!fixture.restore(&["--patch"], &["file"]).status.success());
}

#[test]
fn independent_materializations_and_other_overlays_cannot_be_restored() {
    let fixture =
        Fixture::with_descriptor("mode = 'symlink'\nrules = [{path = 'sub', mode = 'checkout'}]\n");
    assert!(fixture.target.join("file").is_symlink());
    fs::write(fixture.target.join("sub/file"), "worktree\n").unwrap();
    let source = inventory(&fixture.source);
    let target = inventory(&fixture.target);
    assert!(
        !fixture
            .restore(&["--staged", "--worktree"], &["sub/file", "file"])
            .status
            .success()
    );
    assert_eq!(inventory(&fixture.source), source);
    assert_eq!(inventory(&fixture.target), target);
    fs::create_dir(fixture.source.join("other")).unwrap();
    fs::write(
        fixture.source.join("other/over.toml"),
        "mode = 'checkout'\n",
    )
    .unwrap();
    let other_target = fixture.temp.path().join("other-target");
    assert!(
        !fixture
            .command("restore")
            .args(["--overlay", "other"])
            .arg(fixture.target.join("file"))
            .arg("--root")
            .arg(other_target)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::read(fixture.target.join("sub/file")).unwrap(),
        b"worktree\n"
    );
}

#[cfg(unix)]
#[test]
fn executable_state_is_restored_without_broadening_private_access() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.git(&["update-index", "--chmod=+x", "ov/file"]);
    fs::set_permissions(
        fixture.target.join("file"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    checked(fixture.restore(&[], &["file"]));
    assert_eq!(
        fs::metadata(fixture.target.join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn declared_nested_repositories_are_never_restore_destinations() {
    let remote = TempDir::new().unwrap();
    let repo = git2::Repository::init(remote.path()).unwrap();
    fs::write(remote.path().join("file"), "nested repository\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file")).unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    let fixture = Fixture::new();
    fs::write(
        fixture.source.join("ov/over.toml"),
        format!(
            "mode = 'checkout'\n[git.sub]\nurl = {:?}\n",
            remote.path().to_str().unwrap()
        ),
    )
    .unwrap();
    // Exercise live boundaries on an already-associated legacy root checkout.
    fs::remove_dir_all(fixture.target.join("sub")).unwrap();
    git2::Repository::clone(remote.path().to_str().unwrap(), fixture.target.join("sub")).unwrap();
    assert!(fixture.target.join("sub/.git").exists());
    fs::write(fixture.target.join("sub/file"), "nested edits\n").unwrap();
    fixture.staged_and_unstaged();
    let before = inventory(&fixture.source);
    let nested = inventory(&fixture.target.join("sub"));
    assert!(
        !fixture
            .restore(
                &["--source", "HEAD~1", "--staged", "--worktree"],
                &["file", "sub/file"]
            )
            .status
            .success()
    );
    assert_eq!(inventory(&fixture.source), before);
    assert_eq!(inventory(&fixture.target.join("sub")), nested);
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        b"worktree\n"
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn staged_restore_does_not_resolve_collisions_by_unstaging_other_paths(#[case] ancestor: bool) {
    let fixture = Fixture::new();
    let selected = if ancestor {
        fixture.git(&["rm", "-r", "ov/sub"]);
        fs::write(fixture.source.join("ov/sub"), "staged ancestor\n").unwrap();
        fixture.git(&["add", "ov/sub"]);
        "sub/file"
    } else {
        fixture.git(&["rm", "ov/new-in-head"]);
        fs::create_dir(fixture.source.join("ov/new-in-head")).unwrap();
        fs::write(
            fixture.source.join("ov/new-in-head/child"),
            "staged child\n",
        )
        .unwrap();
        fixture.git(&["add", "ov/new-in-head/child"]);
        "new-in-head"
    };
    let source = inventory(&fixture.source);
    let target = inventory(&fixture.target);
    assert!(
        !fixture
            .restore(&["--staged", "--worktree"], &[selected])
            .status
            .success()
    );
    assert_eq!(inventory(&fixture.source), source);
    assert_eq!(inventory(&fixture.target), target);
}

#[test]
fn a_no_op_does_not_rewrite_the_index_or_target() {
    let fixture = Fixture::new();
    let source = inventory(&fixture.source);
    let target = inventory(&fixture.target);
    checked(fixture.restore(&["--staged", "--worktree"], &["file"]));
    assert_eq!(inventory(&fixture.source), source);
    assert_eq!(inventory(&fixture.target), target);
}
