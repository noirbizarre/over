use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn git(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            // Detached maintenance can create/remove lockfiles during preservation snapshots.
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
        ])
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit(path: &Path) {
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "fixture change"]);
}

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    writer: PathBuf,
    remote: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let writer = temp.path().join("writer");
        let remote = temp.path().join("remote.git");
        let source = temp.path().join("source");
        let home = temp.path().join("target");
        fs::create_dir_all(writer.join("ov")).unwrap();
        fs::create_dir_all(&home).unwrap();
        git(&writer, &["init", "-b", "upstream"]);
        fs::write(
            writer.join("ov/over.toml"),
            "target = '~/checkout'\nmode = 'checkout'\n",
        )
        .unwrap();
        fs::write(writer.join("ov/file"), "original").unwrap();
        commit(&writer);
        git(
            temp.path(),
            &[
                "clone",
                "--bare",
                writer.to_str().unwrap(),
                remote.to_str().unwrap(),
            ],
        );
        git(
            &writer,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&writer, &["push", "-u", "origin", "upstream"]);
        git(
            temp.path(),
            &[
                "clone",
                "-o",
                "tracking",
                remote.to_str().unwrap(),
                source.to_str().unwrap(),
            ],
        );
        git(&source, &["branch", "-m", "local"]);
        git(&source, &["config", "user.name", "Test"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        Self {
            temp,
            source,
            writer,
            remote,
            home,
        }
    }

    fn command(&self, command: &str) -> Command {
        let mut cmd = Command::cargo_bin("over").unwrap();
        cmd.current_dir(self.temp.path())
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .arg("--home")
            .arg(&self.source)
            .arg(command)
            .arg("--target")
            .arg(&self.home);
        cmd
    }

    fn apply(&self) {
        self.command("apply")
            .args(["ov", "--no-prompt"])
            .assert()
            .success();
    }

    fn advance(&self, content: &str) {
        fs::write(self.writer.join("ov/file"), content).unwrap();
        commit(&self.writer);
        git(&self.writer, &["push"]);
    }

    fn remote_head(&self) -> String {
        git(&self.remote, &["rev-parse", "refs/heads/upstream"])
    }
}

fn snapshot(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(path)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry.path().strip_prefix(path).unwrap().to_path_buf(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn default_sync_is_offline_and_preserves_local_virtual_reconciliation() {
    let f = Fixture::new();
    f.apply();
    fs::write(f.source.join("ov/file"), "local update").unwrap();
    commit(&f.source);
    git(
        &f.source,
        &["remote", "set-url", "tracking", "/nonexistent/over-remote"],
    );
    f.command("sync")
        .args(["ov", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "local update"
    );
    assert!(!f.source.join(".git/FETCH_HEAD").exists());
}

#[rstest::rstest]
fn pull_rebuilds_targets_after_fast_forward_or_replaying_local_commits(
    #[values(false, true)] divergent: bool,
) {
    let f = Fixture::new();
    f.apply();
    if divergent {
        fs::write(f.source.join("local-only"), "retained").unwrap();
        commit(&f.source);
    }
    f.advance("upstream update");
    f.command("sync")
        .args(["ov", "--pull", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "upstream update"
    );
    if divergent {
        assert_eq!(
            fs::read_to_string(f.source.join("local-only")).unwrap(),
            "retained"
        );
        assert_eq!(git(&f.source, &["rev-parse", "HEAD^"]), f.remote_head());
        assert_eq!(git(&f.source, &["rev-list", "--merges", "HEAD"]), "");
    }
}

#[rstest::rstest]
#[case("unstaged")]
#[case("staged")]
#[case("untracked")]
fn dirty_source_blocks_before_fetch_without_changing_user_data(#[case] kind: &str) {
    let f = Fixture::new();
    f.apply();
    f.advance("upstream update");
    git(&f.source, &["config", "rebase.autoStash", "true"]);
    fs::write(
        f.source.join(if kind == "untracked" {
            "untracked"
        } else {
            "ov/file"
        }),
        "user data",
    )
    .unwrap();
    if kind == "staged" {
        git(&f.source, &["add", "ov/file"]);
    }
    let before = snapshot(&f.source);
    f.command("sync")
        .args(["ov", "--pull"])
        .assert()
        .failure()
        .stderr(contains("dirty"));
    assert_eq!(snapshot(&f.source), before);
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "original"
    );
}

#[rstest::rstest]
fn rebase_conflicts_preserve_native_state_and_block_targets_until_git_resolution(
    #[values(false, true)] continue_rebase: bool,
) {
    let f = Fixture::new();
    f.apply();
    fs::write(f.source.join("ov/file"), "local conflict").unwrap();
    commit(&f.source);
    f.advance("remote conflict");
    let remote = f.remote_head();
    f.command("sync")
        .args(["ov", "--pull", "--push"])
        .assert()
        .failure()
        .stderr(contains("git rebase --continue"));
    assert!(f.source.join(".git/rebase-merge").exists());
    assert!(
        fs::read_to_string(f.source.join("ov/file"))
            .unwrap()
            .contains("<<<<<<<")
    );
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "original"
    );
    assert_eq!(f.remote_head(), remote);
    // A conflicted descriptor must not obscure the early native rebase diagnostic.
    fs::write(f.source.join("over.toml"), "<<<<<<< broken descriptor").unwrap();
    f.command("sync")
        .arg("ov")
        .assert()
        .failure()
        .stderr(contains("rebase in progress"));
    fs::remove_file(f.source.join("over.toml")).unwrap();
    if continue_rebase {
        fs::write(f.source.join("ov/file"), "resolved").unwrap();
        git(&f.source, &["add", "ov/file"]);
        git(
            &f.source,
            &["-c", "core.editor=true", "rebase", "--continue"],
        );
    } else {
        git(&f.source, &["rebase", "--abort"]);
        // The user elects to abandon the conflicting local commit using Git.
        git(&f.source, &["reset", "--hard", "tracking/upstream"]);
    }
    f.command("sync")
        .args(["ov", "--pull", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        if continue_rebase {
            "resolved"
        } else {
            "remote conflict"
        }
    );
}

#[test]
fn pulled_configuration_and_overlay_discovery_are_used_for_selection() {
    let f = Fixture::new();
    fs::create_dir_all(f.writer.join("new")).unwrap();
    fs::write(
        f.writer.join("new/over.toml"),
        "target = '~/new-target'\nmode = 'checkout'\n",
    )
    .unwrap();
    fs::write(f.writer.join("new/file"), "before").unwrap();
    fs::write(f.writer.join("over.toml"), "default_overlay = 'ov'\n").unwrap();
    commit(&f.writer);
    git(&f.writer, &["push"]);
    git(&f.source, &["fetch", "tracking"]);
    git(&f.source, &["merge", "--ff-only", "tracking/upstream"]);
    f.apply();
    f.command("apply")
        .args(["new", "--no-prompt"])
        .assert()
        .success();
    fs::write(f.writer.join("new/file"), "updated selection").unwrap();
    fs::write(
        f.writer.join("over.toml"),
        "default_overlay = 'new'\n[sync]\npush = true\n",
    )
    .unwrap();
    git(&f.writer, &["rm", "-r", "ov"]);
    commit(&f.writer);
    git(&f.writer, &["push"]);
    // Newly fetched push policy must not enable a remote mutation in this invocation.
    git(
        &f.source,
        &[
            "config",
            "remote.tracking.pushurl",
            "/nonexistent/over-remote",
        ],
    );
    f.command("sync")
        .args(["--pull", "--verbose"])
        .assert()
        .success()
        .stdout(contains("new"));
    assert_eq!(
        fs::read_to_string(f.home.join("new-target/file")).unwrap(),
        "updated selection"
    );
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "original"
    );
    f.command("sync")
        .args(["ov", "--pull", "--no-push"])
        .assert()
        .failure();
}

#[test]
fn push_publishes_commits_after_reconciliation_without_staging_source_edits() {
    let f = Fixture::new();
    f.apply();
    fs::write(f.source.join("ov/file"), "committed update").unwrap();
    commit(&f.source);
    let head = git(&f.source, &["rev-parse", "HEAD"]);
    fs::write(f.source.join("untracked"), "leave alone").unwrap();
    f.command("sync")
        .args(["ov", "--push", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(f.remote_head(), head);
    assert_eq!(git(&f.source, &["status", "--porcelain"]), "?? untracked");
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "committed update"
    );
    assert!(!f.source.join(".git/FETCH_HEAD").exists());
}

#[rstest::rstest]
fn local_failures_prevent_push(#[values(false, true)] operational_error: bool) {
    let f = Fixture::new();
    f.apply();
    let remote = f.remote_head();
    fs::write(f.source.join("local-only"), "commit to publish").unwrap();
    commit(&f.source);
    fs::write(f.home.join("checkout/file"), "uncommitted target edit").unwrap();
    f.command("sync")
        .args([
            if operational_error { "missing" } else { "ov" },
            "--push",
            "--no-prompt",
        ])
        .assert()
        .failure();
    assert_eq!(f.remote_head(), remote);
}

#[test]
fn push_rejection_reports_failure_without_rolling_back_local_reconciliation() {
    let f = Fixture::new();
    f.apply();
    fs::write(f.source.join("ov/file"), "local commit").unwrap();
    commit(&f.source);
    f.advance("remote commit");
    let remote = f.remote_head();
    f.command("sync")
        .args(["ov", "--push", "--no-prompt"])
        .assert()
        .failure()
        .stderr(contains("local reconciliation has completed"));
    assert_eq!(f.remote_head(), remote);
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "local commit"
    );
}

#[test]
fn dry_run_with_remote_flags_is_offline_and_preserves_git_targets_and_state() {
    let f = Fixture::new();
    f.apply();
    git(
        &f.source,
        &["remote", "set-url", "tracking", "/nonexistent/over-remote"],
    );
    let before = snapshot(f.temp.path());
    f.command("sync")
        .args(["ov", "--pull", "--push", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("Would fetch"));
    assert_eq!(snapshot(f.temp.path()), before);
}

#[rstest::rstest]
fn requested_remote_operations_require_an_attached_branch_and_upstream(
    #[values(false, true)] detached: bool,
) {
    let f = Fixture::new();
    if detached {
        git(&f.source, &["checkout", "--detach"]);
    } else {
        git(&f.source, &["branch", "--unset-upstream"]);
    }
    f.command("sync")
        .args(["ov", "--push"])
        .assert()
        .failure()
        .stderr(contains(if detached {
            "detached HEAD"
        } else {
            "upstream"
        }));
}

#[test]
fn an_empty_local_phase_still_pushes_existing_source_commits() {
    let f = Fixture::new();
    git(&f.source, &["rm", "-r", "ov"]);
    commit(&f.source);
    f.command("sync").arg("--push").assert().success();
    assert_eq!(f.remote_head(), git(&f.source, &["rev-parse", "HEAD"]));
}

#[test]
fn configured_pull_and_push_surround_all_local_reconciliation() {
    let f = Fixture::new();
    fs::create_dir_all(f.source.join("second")).unwrap();
    fs::write(
        f.source.join("second/over.toml"),
        "target = '~/second'\nmode = 'checkout'\n",
    )
    .unwrap();
    fs::write(f.source.join("second/file"), "second").unwrap();
    fs::write(
        f.source.join("over.toml"),
        "[sync]\npull = true\npush = true\n",
    )
    .unwrap();
    commit(&f.source);
    f.apply();
    f.command("apply")
        .args(["second", "--no-prompt"])
        .assert()
        .success();
    f.advance("remote update");
    let trace = f.temp.path().join("git-trace");
    f.command("sync")
        .env("GIT_TRACE", &trace)
        .args(["--all", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "remote update"
    );
    assert_eq!(f.remote_head(), git(&f.source, &["rev-parse", "HEAD"]));
    let trace = fs::read_to_string(trace).unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.contains("built-in: git fetch "))
            .count(),
        1
    );
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.contains("built-in: git push "))
            .count(),
        1
    );
}

#[test]
fn negative_flags_disable_configured_remote_phases_without_changing_local_sync() {
    let f = Fixture::new();
    fs::write(
        f.source.join("over.toml"),
        "[sync]\npull = true\npush = true\n",
    )
    .unwrap();
    commit(&f.source);
    f.apply();
    git(
        &f.source,
        &["remote", "set-url", "tracking", "/nonexistent/over-remote"],
    );
    f.command("sync")
        .args(["ov", "--no-pull", "--no-push"])
        .assert()
        .success();
    assert!(!f.source.join(".git/FETCH_HEAD").exists());
}

#[test]
fn fetch_respects_custom_tracking_refspecs_and_push_does_not_mirror_other_refs() {
    let f = Fixture::new();
    f.apply();
    git(
        &f.source,
        &[
            "config",
            "remote.tracking.fetch",
            "+refs/heads/upstream:refs/remotes/custom/source",
        ],
    );
    git(&f.source, &["config", "remote.tracking.mirror", "true"]);
    git(&f.source, &["branch", "private"]);
    fs::write(f.source.join("local-only"), "keep").unwrap();
    commit(&f.source);
    git(&f.source, &["branch", "other-local"]);
    let other = git(&f.source, &["rev-parse", "other-local"]);
    git(&f.source, &["config", "rebase.updateRefs", "true"]);
    f.advance("remote update");
    f.command("sync")
        .args(["ov", "--pull", "--push", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(
        git(&f.source, &["rev-parse", "refs/remotes/custom/source"]),
        f.remote_head()
    );
    assert_eq!(git(&f.source, &["rev-parse", "other-local"]), other);
    assert_eq!(
        git(
            &f.remote,
            &["for-each-ref", "--format=%(refname)", "refs/heads"]
        ),
        "refs/heads/upstream"
    );
}

#[test]
fn a_later_overlay_failure_prevents_push_after_an_earlier_target_was_refreshed() {
    let f = Fixture::new();
    fs::create_dir_all(f.source.join("z")).unwrap();
    fs::write(
        f.source.join("z/over.toml"),
        "target = '~/second'\nmode = 'checkout'\n",
    )
    .unwrap();
    fs::write(f.source.join("z/file"), "second").unwrap();
    commit(&f.source);
    f.apply();
    f.command("apply")
        .args(["z", "--no-prompt"])
        .assert()
        .success();
    let remote = f.remote_head();
    fs::write(f.home.join("second/file"), "uncommitted").unwrap();
    fs::write(f.source.join("ov/file"), "first updated").unwrap();
    commit(&f.source);
    f.command("sync")
        .args(["--all", "--push", "--no-prompt"])
        .assert()
        .failure();
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "first updated"
    );
    assert_eq!(f.remote_head(), remote);
}

#[test]
fn source_sync_uses_the_worktree_containing_a_nested_collection() {
    let f = Fixture::new();
    // After pull this nested collection has no virtual checkout requiring prior apply.
    fs::write(f.writer.join("ov/over.toml"), "target = '~/checkout'\n").unwrap();
    fs::write(f.writer.join("outside-collection"), "updated").unwrap();
    commit(&f.writer);
    git(&f.writer, &["push"]);
    Command::cargo_bin("over")
        .unwrap()
        .current_dir(f.temp.path())
        .env("XDG_STATE_HOME", f.temp.path().join("state"))
        .arg("--home")
        .arg(f.source.join("ov"))
        .args(["sync", "--all", "--pull", "--target"])
        .arg(&f.home)
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(f.source.join("outside-collection")).unwrap(),
        "updated"
    );
}

#[test]
fn source_sync_rebases_the_selected_linked_worktree_without_changing_the_main_worktree() {
    let f = Fixture::new();
    let linked = f.temp.path().join("linked");
    git(
        &f.source,
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            linked.to_str().unwrap(),
            "tracking/upstream",
        ],
    );
    let main_head = git(&f.source, &["rev-parse", "HEAD"]);
    f.advance("linked update");
    // No target is provisioned: test the source phase without selecting virtual content.
    fs::create_dir_all(linked.join("empty")).unwrap();
    Command::cargo_bin("over")
        .unwrap()
        .current_dir(f.temp.path())
        .env("XDG_STATE_HOME", f.temp.path().join("state"))
        .arg("--home")
        .arg(&linked)
        .args(["sync", "empty", "--pull", "--target"])
        .arg(&f.home)
        .assert()
        .success();
    assert_eq!(git(&f.source, &["rev-parse", "HEAD"]), main_head);
    assert_eq!(
        fs::read_to_string(linked.join("ov/file")).unwrap(),
        "linked update"
    );
    assert_eq!(
        fs::read_to_string(f.source.join("ov/file")).unwrap(),
        "original"
    );
}

#[test]
fn source_remote_phases_never_synchronize_declared_root_or_nested_repositories() {
    let f = Fixture::new();
    let remote = f.remote.to_string_lossy();
    fs::write(
        f.source.join("ov/over.toml"),
        format!("target = '~/checkout'\n[git]\n'.' = '{remote}'\nnested = '{remote}'\n"),
    )
    .unwrap();
    commit(&f.source);
    // Provision sequentially so root/nested clone ordering cannot affect this sync test.
    let target = f.home.join("checkout");
    git(
        &f.home,
        &[
            "clone",
            f.remote.to_str().unwrap(),
            target.to_str().unwrap(),
        ],
    );
    git(&target, &["clone", f.remote.to_str().unwrap(), "nested"]);
    let before = snapshot(&f.home);
    f.advance("upstream source update");
    f.command("sync")
        .args(["ov", "--pull", "--push", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(snapshot(&f.home), before);
    assert_eq!(
        fs::read_to_string(f.source.join("ov/file")).unwrap(),
        "upstream source update"
    );
}

#[rstest::rstest]
fn pull_preserves_ignored_files_that_obstruct_upstream_paths(
    #[values(false, true)] directory_obstruction: bool,
) {
    let f = Fixture::new();
    f.apply();
    fs::write(f.source.join(".git/info/exclude"), "secret\n").unwrap();
    let secret = f.source.join("secret");
    if directory_obstruction {
        fs::create_dir(&secret).unwrap();
        fs::write(secret.join("private"), "private data").unwrap();
    } else {
        fs::write(&secret, "private data").unwrap();
    }
    fs::write(f.writer.join("secret"), "upstream tracked file").unwrap();
    commit(&f.writer);
    git(&f.writer, &["push"]);
    let index = fs::read(f.source.join(".git/index")).unwrap();
    let head = git(&f.source, &["rev-parse", "HEAD"]);
    let targets = snapshot(&f.home);
    f.command("sync")
        .args(["ov", "--pull"])
        .assert()
        .failure()
        .stderr(contains("obstructed"));
    let private = if directory_obstruction {
        secret.join("private")
    } else {
        secret
    };
    assert_eq!(fs::read_to_string(private).unwrap(), "private data");
    assert_eq!(fs::read(f.source.join(".git/index")).unwrap(), index);
    assert_eq!(git(&f.source, &["rev-parse", "HEAD"]), head);
    assert_eq!(snapshot(&f.home), targets);
}

#[test]
fn unrelated_ignored_files_do_not_block_pull() {
    let f = Fixture::new();
    f.apply();
    fs::write(f.source.join(".git/info/exclude"), "secret\n").unwrap();
    fs::write(f.source.join("secret"), "private data").unwrap();
    f.advance("update");
    f.command("sync").args(["ov", "--pull"]).assert().success();
    assert_eq!(
        fs::read_to_string(f.source.join("secret")).unwrap(),
        "private data"
    );
}

#[test]
fn pull_preserves_ignored_files_obstructing_an_intermediate_replayed_commit() {
    let f = Fixture::new();
    f.apply();
    fs::write(f.source.join("secret"), "historic tracked content").unwrap();
    commit(&f.source);
    git(&f.source, &["rm", "secret"]);
    commit(&f.source);
    fs::write(f.source.join(".git/info/exclude"), "secret\n").unwrap();
    fs::write(f.source.join("secret"), "private data").unwrap();
    f.advance("remote update");
    f.command("sync")
        .args(["ov", "--pull"])
        .assert()
        .failure()
        .stderr(contains("obstructed"));
    assert_eq!(
        fs::read_to_string(f.source.join("secret")).unwrap(),
        "private data"
    );
    assert!(!f.source.join(".git/rebase-merge").exists());
}

#[test]
fn malformed_overlay_discovery_blocks_push() {
    let f = Fixture::new();
    f.apply();
    fs::create_dir_all(f.source.join("broken")).unwrap();
    fs::write(f.source.join("broken/over.toml"), "invalid = [").unwrap();
    commit(&f.source);
    let remote = f.remote_head();
    f.command("sync")
        .args(["--all", "--push", "--no-prompt"])
        .assert()
        .failure()
        .stderr(contains("failed to load overlay"));
    assert_eq!(f.remote_head(), remote);
}

#[test]
fn legacy_abort_never_runs_configured_remote_phases() {
    let f = Fixture::new();
    fs::write(f.source.join("over.toml"), "[sync]\npull=true\npush=true\n").unwrap();
    commit(&f.source);
    let before = snapshot(f.temp.path());
    f.command("sync")
        .arg("--abort")
        .assert()
        .failure()
        .stderr(contains("no abortable sync operation"));
    assert_eq!(snapshot(f.temp.path()), before);
}

#[test]
fn all_overlay_discovery_includes_overlays_introduced_by_the_pull() {
    let f = Fixture::new();
    f.apply();
    fs::create_dir_all(f.writer.join("introduced")).unwrap();
    fs::write(
        f.writer.join("introduced/over.toml"),
        "target='~/introduced'\n",
    )
    .unwrap();
    commit(&f.writer);
    git(&f.writer, &["push"]);
    f.command("sync")
        .args(["--all", "--pull", "--verbose"])
        .assert()
        .success()
        .stdout(contains("Overlay introduced"));
}

#[test]
fn pulled_metadata_rebuilds_context_before_resolving_the_target() {
    let f = Fixture::new();
    fs::write(
        f.writer.join("over.toml"),
        "default_overlay='ov'\n[data]\ncheckout_name='checkout'\n",
    )
    .unwrap();
    fs::write(
        f.writer.join("ov/over.toml"),
        "mode='checkout'\ntarget='~/{{ data.checkout_name }}'\n",
    )
    .unwrap();
    commit(&f.writer);
    git(&f.writer, &["push"]);
    git(&f.source, &["fetch", "tracking"]);
    git(&f.source, &["merge", "--ff-only", "tracking/upstream"]);
    f.apply();
    fs::write(
        f.writer.join("over.toml"),
        "default_overlay='ov'\n[data]\ncheckout_name='updated-target'\n",
    )
    .unwrap();
    f.advance("updated source");
    f.command("sync")
        .args(["ov", "--pull", "--no-prompt"])
        .assert()
        .failure()
        .stdout(contains("updated-target"));
    // The new target requires apply; stale pre-pull context must not refresh the old target.
    assert_eq!(
        fs::read_to_string(f.home.join("checkout/file")).unwrap(),
        "original"
    );
}
