#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use rstest::rstest;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    home: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let home = temp.path().join("home");
        let target = home.join("checkout");
        fs::create_dir_all(source.join("ov")).unwrap();
        fs::write(source.join("ov/file"), "original").unwrap();
        fs::write(source.join("sibling"), "sibling original").unwrap();
        fs::write(
            source.join("ov/over.yaml"),
            "target: ~/checkout\nmode: checkout\n",
        )
        .unwrap();
        symlink("file", source.join("ov/link")).unwrap();
        symlink("sibling", source.join("outside-link")).unwrap();
        let repo = git2::Repository::init(&source).unwrap();
        repo.config().unwrap().set_str("user.name", "Test").unwrap();
        repo.config()
            .unwrap()
            .set_str("user.email", "test@example.com")
            .unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        commit_index(&repo);
        // A gitlink only records a commit ID. Using an existing commit avoids
        // needing a remote or an external git process to construct the fixture.
        let mut module = index.get_path(Path::new("ov/file"), 0).unwrap();
        module.path = b"ov/module".to_vec();
        module.mode = 0o160000;
        module.id = repo.head().unwrap().target().unwrap();
        index.add(&module).unwrap();
        index.write().unwrap();
        fs::create_dir(source.join("ov/module")).unwrap();
        commit_index(&repo);
        Self {
            temp,
            source,
            home,
            target,
        }
    }

    fn command(&self, command: &str) -> Command {
        let mut cmd = Command::cargo_bin("over").unwrap();
        cmd.env("XDG_STATE_HOME", self.temp.path().join("state"))
            .arg("--home")
            .arg(&self.source)
            .arg(command);
        cmd
    }

    fn apply(&self) {
        self.command("apply")
            .args(["ov", "--no-prompt", "--target"])
            .arg(&self.home)
            .assert()
            .success();
    }

    fn add(&self) {
        self.command("add")
            .args(["--overlay", "ov", "--target"])
            .arg(&self.home)
            .arg(&self.target)
            .assert()
            .success();
    }

    fn commit_command(&self) -> Command {
        let mut cmd = self.command("commit");
        cmd.args([
            "--overlay",
            "ov",
            "--no-prompt",
            "--message",
            "selected",
            "--target",
        ])
        .arg(&self.home);
        cmd
    }
}

fn commit_index(repo: &git2::Repository) {
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
    let parents: Vec<_> = parent.iter().collect();
    let signature = repo.signature().unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "fixture",
        &tree,
        &parents,
    )
    .unwrap();
}

#[rstest]
#[case("outside", true, "--message")]
#[case("source", false, "-m")]
#[case("target", false, "--message")]
#[case("nested_source", false, "-m")]
#[case("nested_target", false, "-m")]
fn path_operands_commit_only_staged_snapshots_and_preserve_other_changes(
    #[case] context: &str,
    #[case] explicit: bool,
    #[case] message_flag: &str,
) {
    let fixture = Fixture::new();
    fs::create_dir(fixture.source.join("ov/nested")).unwrap();
    fs::write(fixture.source.join("ov/nested/second"), "second original").unwrap();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("ov/nested/second")).unwrap();
    index.write().unwrap();
    commit_index(&repo);
    fixture.apply();
    fs::write(fixture.target.join("file"), "first staged").unwrap();
    fs::write(fixture.target.join("nested/second"), "second staged").unwrap();
    fixture.add();
    fs::write(fixture.target.join("nested/second"), "second unstaged").unwrap();

    let (cwd, operand) = match context {
        "source" => (fixture.source.join("ov"), "nested/second"),
        "target" => (fixture.target.clone(), "nested/second"),
        "nested_source" => (fixture.source.join("ov/nested"), "second"),
        "nested_target" => (fixture.target.join("nested"), "second"),
        _ => (fixture.temp.path().to_path_buf(), "nested/second"),
    };
    let mut command = fixture.command("commit");
    command.current_dir(cwd);
    if explicit {
        command.args(["--overlay", "ov"]);
    }
    command
        .args([message_flag, "focused", "--target"])
        .arg(&fixture.home)
        .arg(operand)
        .assert()
        .success();
    let head = repo.head().unwrap().peel_to_tree().unwrap();
    let blob = |path: &str| {
        repo.find_blob(head.get_path(Path::new(path)).unwrap().id())
            .unwrap()
            .content()
            .to_vec()
    };
    assert_eq!(blob("ov/file"), b"original");
    assert_eq!(blob("ov/nested/second"), b"second staged");
    assert_eq!(
        fs::read(fixture.target.join("nested/second")).unwrap(),
        b"second unstaged"
    );
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    assert_ne!(
        index.get_path(Path::new("ov/file"), 0).unwrap().id,
        head.get_path(Path::new("ov/file")).unwrap().id()
    );
    // Advancing the whole-unit checkpoint must still allow a second focused commit.
    fixture.commit_command().arg("file").assert().success();
    assert_eq!(
        fs::read(fixture.source.join("ov/file")).unwrap(),
        b"first staged"
    );
}

#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
fn commit_inference_uses_the_same_namespace_for_aliased_collections_and_cwd(
    #[case] target_context: bool,
    #[case] remove_association: bool,
) {
    let fixture = Fixture::new();
    fixture.apply();
    let alias = fixture.temp.path().join("source-alias");
    symlink(&fixture.source, &alias).unwrap();
    if remove_association {
        // Descriptor-based source inference must also work before a checkout is associated.
        fs::remove_file(fixture.temp.path().join("state/over/virtual_checkout.toml")).unwrap();
    }
    Command::cargo_bin("over")
        .unwrap()
        .env("XDG_STATE_HOME", fixture.temp.path().join("state"))
        .current_dir(if target_context {
            fixture.target.clone()
        } else {
            fixture.source.join("ov")
        })
        .arg("--home")
        .arg(alias)
        .args(["commit", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("nothing to commit"));
}

#[test]
fn commit_ignores_unrelated_overlays_and_untracked_repository_trees() {
    let fixture = Fixture::new();
    fixture.apply();
    fs::write(fixture.target.join("file"), "staged").unwrap();
    fixture.add();
    fs::write(fixture.source.join("over.yaml"), "default_overlay: host\n").unwrap();
    fs::create_dir(fixture.source.join("host")).unwrap();
    // Default metadata may be necessary, but its schema warnings are not this overlay's warnings.
    fs::write(
        fixture.source.join("host/over.yaml"),
        "unrelated_key: true\n",
    )
    .unwrap();
    for index in 0..100 {
        let unrelated = fixture.source.join(format!("unrelated-{index}"));
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join("over.yaml"), "[invalid YAML").unwrap();
        for root in [fixture.source.join("ov"), fixture.target.clone()] {
            let untracked = root.join(format!("untracked-{index}"));
            fs::create_dir(&untracked).unwrap();
            // An eager ignore/diagnostic walk would parse these invalid rule bytes.
            fs::write(untracked.join(".overignore"), [0xff]).unwrap();
            fs::write(untracked.join("secret.age"), "unrelated encrypted data").unwrap();
        }
    }
    fixture
        .command("commit")
        .current_dir(&fixture.target)
        .args(["-m", "focused", "file", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stderr(contains("WARN").not().and(contains("unrelated-").not()));
}

#[test]
fn a_missing_directory_operand_selects_staged_deletions_without_walking_it() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.source.join("ov/deleted")).unwrap();
    fs::write(fixture.source.join("ov/deleted/file"), "delete me").unwrap();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("ov/deleted/file")).unwrap();
    index.write().unwrap();
    commit_index(&repo);
    fixture.apply();
    fs::remove_dir_all(fixture.target.join("deleted")).unwrap();
    fixture.add();
    fixture
        .commit_command()
        .args(["deleted", "deleted/file"])
        .assert()
        .success();
    assert!(
        repo.head()
            .unwrap()
            .peel_to_tree()
            .unwrap()
            .get_path(Path::new("ov/deleted/file"))
            .is_err()
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn unselected_unsupported_changes_or_index_conflicts_do_not_block_selected_paths(
    #[case] conflict: bool,
) {
    let fixture = Fixture::new();
    fixture.apply();
    fs::write(fixture.target.join("file"), "selected").unwrap();
    fixture.add();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let mut index = repo.index().unwrap();
    let mut other = index.get_path(Path::new("ov/link"), 0).unwrap();
    other.id = repo.blob(b"unrelated symlink target").unwrap();
    if conflict {
        index.remove_path(Path::new("ov/link")).unwrap();
        for stage in 1..=3 {
            other.flags = (other.flags & !0x3000) | (stage << 12);
            index.add(&other).unwrap();
        }
    } else {
        index.add(&other).unwrap();
    }
    index.write().unwrap();
    let before = fs::read(repo.path().join("index")).unwrap();
    fixture.commit_command().arg("file").assert().success();
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), before);
    assert_eq!(
        fs::read(fixture.source.join("ov/file")).unwrap(),
        b"selected"
    );
    assert_eq!(
        fs::read_link(fixture.source.join("ov/link")).unwrap(),
        Path::new("file")
    );
}

#[test]
fn a_partial_commit_cannot_hide_unselected_source_advancement_in_its_checkpoint() {
    let fixture = Fixture::new();
    fixture.apply();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    fs::write(fixture.source.join("ov/advanced"), "source advanced").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("ov/advanced")).unwrap();
    index.write().unwrap();
    commit_index(&repo);
    fs::write(fixture.target.join("file"), "selected").unwrap();
    // External Git can stage despite over add's source-advancement guard.
    let selected = index.get_path(Path::new("ov/file"), 0).unwrap();
    index.add_frombuffer(&selected, b"selected").unwrap();
    index.write().unwrap();
    let head = repo.head().unwrap().target();
    let before = fs::read(repo.path().join("index")).unwrap();
    fixture
        .commit_command()
        .arg("file")
        .assert()
        .failure()
        .stderr(contains("sync before committing"));
    assert_eq!(repo.head().unwrap().target(), head);
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), before);
}

#[cfg(target_os = "linux")]
#[test]
fn interactive_message_input_starts_below_the_question_and_records_edited_text() {
    let fixture = Fixture::new();
    fixture.apply();
    fs::write(fixture.target.join("file"), "selected").unwrap();
    fixture.add();
    // A real PTY exercises dialoguer's redraw/cursor handling, not just theme strings.
    let output = Command::new("script")
        .args(["--quiet", "--return", "--echo", "never", "--command",
            "exec \"$OVER_TEST_BINARY\" --home \"$OVER_TEST_SOURCE\" commit --overlay ov --target \"$OVER_TEST_HOME\" file", "/dev/null"])
        .env("OVER_TEST_BINARY", assert_cmd::cargo::cargo_bin("over"))
        .env("OVER_TEST_SOURCE", &fixture.source)
        .env("OVER_TEST_HOME", &fixture.home)
        .env("XDG_STATE_HOME", fixture.temp.path().join("state"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .write_stdin(b"focusex\x7fd\n")
        .timeout(std::time::Duration::from_secs(10))
        .output().unwrap();
    let transcript = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{transcript}");
    let question = transcript.split_once("Commit message for ").unwrap().1;
    let (heading, input) = question.split_once('\n').unwrap();
    assert!(!heading.contains("focused"), "{transcript}");
    assert!(input.starts_with("› "), "{transcript}");
    let repo = git2::Repository::open(&fixture.source).unwrap();
    assert_eq!(
        repo.head().unwrap().peel_to_commit().unwrap().message(),
        Ok("focused")
    );
}

#[test]
fn intent_to_add_preserves_unstaged_content_and_does_not_publish_empty_files() {
    let fixture = Fixture::new();
    fixture.apply();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    fs::write(fixture.source.join("ov/new"), "").unwrap();
    assert!(
        std::process::Command::new("git")
            .current_dir(&fixture.source)
            .args(["add", "-N", "ov/new"])
            .status()
            .unwrap()
            .success()
    );
    fs::write(fixture.target.join("new"), "unstaged target\n").unwrap();
    let head = repo.head().unwrap().target().unwrap();
    let index = fs::read(repo.path().join("index")).unwrap();
    let state_path = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let state = fs::read(&state_path).unwrap();
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("unstaged"));
    fixture
        .commit_command()
        .assert()
        .success()
        .stdout(contains("nothing to commit"));
    assert_eq!(repo.head().unwrap().target(), Some(head));
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
    assert_eq!(fs::read(&state_path).unwrap(), state);

    // An independent staged edit may commit without consuming the reserved path.
    fs::write(fixture.target.join("file"), "selected").unwrap();
    fixture
        .command("add")
        .args(["--overlay", "ov", "--target"])
        .arg(&fixture.home)
        .arg(fixture.target.join("file"))
        .assert()
        .success();
    fixture.commit_command().assert().success();
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    assert!(tree.get_path(Path::new("ov/new")).is_err());
    assert_eq!(fs::read(fixture.source.join("ov/new")).unwrap(), b"");
    assert_eq!(
        fs::read(fixture.target.join("new")).unwrap(),
        b"unstaged target\n"
    );
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    assert!(
        git2::IndexEntryExtendedFlag::from_bits_truncate(
            index
                .get_path(Path::new("ov/new"), 0)
                .unwrap()
                .flags_extended
        )
        .is_intent_to_add()
    );
}

#[test]
fn full_staging_status_diff_commit_sync_and_unapply_share_clean_filters_and_ignored_modes() {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.filemode", false)
        .unwrap();
    repo.config()
        .unwrap()
        .set_str("filter.upper.clean", "tr a-z A-Z")
        .unwrap();
    repo.config()
        .unwrap()
        .set_str("filter.upper.smudge", "tr A-Z a-z")
        .unwrap();
    fs::write(
        fixture.source.join(".gitattributes"),
        "ov/file text eol=crlf filter=upper\n",
    )
    .unwrap();
    fs::write(fixture.source.join("ov/file"), "original\r\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(".gitattributes")).unwrap();
    let entry = index.get_path(Path::new("ov/file"), 0).unwrap();
    index.add_frombuffer(&entry, b"ORIGINAL\n").unwrap();
    index.write().unwrap();
    commit_index(&repo);
    fixture.apply();
    fs::write(fixture.target.join("file"), "original\r\n").unwrap();
    fs::set_permissions(
        fixture.source.join("ov/file"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::set_permissions(
        fixture.target.join("file"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("applied"));
    fixture
        .command("diff")
        .args(["ov", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("diff --git").not());
    fs::write(fixture.target.join("file"), "selected\r\n").unwrap();
    let inventory = || {
        walkdir::WalkDir::new(repo.path())
            .into_iter()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| (entry.path().to_path_buf(), fs::read(entry.path()).unwrap()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let before_preview = inventory();
    fixture
        .command("add")
        .args(["--overlay", "ov", "--dry-run", "--target"])
        .arg(&fixture.home)
        .arg(&fixture.target)
        .assert()
        .success();
    // Inventory the entire gitdir, not just its index: filtered previews must
    // not leave candidate blobs, synthetic trees, or locks in the source.
    assert_eq!(inventory(), before_preview);
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        b"selected\r\n"
    );
    fixture.add();
    // The CLI runs in another process; libgit2's cached index still contains
    // this fixture's original staging until explicitly refreshed.
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let indexed = index.get_path(Path::new("ov/file"), 0).unwrap();
    assert_eq!(repo.find_blob(indexed.id).unwrap().content(), b"SELECTED\n");
    assert_eq!(indexed.mode, 0o100644);
    fixture
        .command("diff")
        .args(["ov", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("diff --git").not());
    fixture.commit_command().assert().success();
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("applied"));
    let entry = repo
        .index()
        .unwrap()
        .get_path(Path::new("ov/file"), 0)
        .unwrap();
    let mut index = repo.index().unwrap();
    index.add_frombuffer(&entry, b"UPSTREAM\n").unwrap();
    index.write().unwrap();
    fs::write(fixture.source.join("ov/file"), "upstream\r\n").unwrap();
    commit_index(&repo);
    fixture
        .command("sync")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("fast-forwarded"));
    // Canonical equivalence must survive execution-time teardown revalidation too.
    fs::write(fixture.target.join("file"), "upstream\r\n").unwrap();
    fs::set_permissions(
        fixture.target.join("file"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("applied"));
    fixture
        .command("unapply")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert!(!fixture.target.join("file").exists());
    assert_eq!(
        fs::read(fixture.source.join("ov/file")).unwrap(),
        b"upstream\r\n"
    );
}

#[test]
fn committing_filtered_targets_publishes_smudged_source_bytes() {
    let fixture = Fixture::new();
    let repo = configure_prefix_filter(&fixture);
    fixture.apply();
    fs::write(fixture.target.join("file"), "selected\n").unwrap();
    fixture.add();
    fixture.commit_command().assert().success();
    assert_native_source_is_clean(&fixture);
    assert_eq!(
        fs::read(fixture.source.join("ov/file")).unwrap(),
        b"selected\n"
    );
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    assert_eq!(
        repo.find_blob(tree.get_path(Path::new("ov/file")).unwrap().id())
            .unwrap()
            .content(),
        b"clean:selected\n"
    );
}

#[test]
fn stale_checkpoints_recognize_raw_head_targets_and_sync_converges() {
    let fixture = Fixture::new();
    let repo = configure_prefix_filter(&fixture);
    fixture.apply();
    let checkpoint = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let baseline = fs::read(&checkpoint).unwrap();
    let mut index = repo.index().unwrap();
    let entry = index.get_path(Path::new("ov/file"), 0).unwrap();
    index.add_frombuffer(&entry, b"clean:upstream\n").unwrap();
    index.write().unwrap();
    fs::write(fixture.source.join("ov/file"), "upstream\n").unwrap();
    commit_index(&repo);
    fixture
        .command("sync")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("fast-forwarded"));
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        b"clean:upstream\n"
    );
    // Recreate a checkpoint-write failure after target publication succeeded.
    fs::write(&checkpoint, baseline).unwrap();
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("Fully applied"))
        .stdout(contains("⚠ conflict:").not());
    fixture
        .command("sync")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("applied"));
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        b"clean:upstream\n"
    );
    assert_native_source_is_clean(&fixture);
}

#[test]
fn raw_canonical_source_bytes_are_rejected_as_native_git_unstaged_edits() {
    let fixture = Fixture::new();
    configure_prefix_filter(&fixture);
    fixture.apply();
    fs::write(fixture.target.join("file"), "selected\n").unwrap();
    fixture.add();
    // Raw blobs are valid at the target, but re-clean differently in a real worktree.
    fs::write(fixture.source.join("ov/file"), "clean:original").unwrap();
    fixture
        .commit_command()
        .assert()
        .failure()
        .stderr(contains("unstaged edits"));
    assert_eq!(
        fs::read(fixture.source.join("ov/file")).unwrap(),
        b"clean:original"
    );
}

fn configure_prefix_filter(fixture: &Fixture) -> git2::Repository {
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let mut config = repo.config().unwrap();
    config
        .set_str("filter.prefix.clean", "sed 's/^/clean:/'")
        .unwrap();
    config
        .set_str("filter.prefix.smudge", "sed 's/^clean://'")
        .unwrap();
    config.set_bool("filter.prefix.required", true).unwrap();
    fs::write(
        fixture.source.join(".gitattributes"),
        "ov/file filter=prefix\n",
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(".gitattributes")).unwrap();
    let entry = index.get_path(Path::new("ov/file"), 0).unwrap();
    index.add_frombuffer(&entry, b"clean:original").unwrap();
    index.write().unwrap();
    commit_index(&repo);
    assert_native_source_is_clean(fixture);
    repo
}

fn assert_native_source_is_clean(fixture: &Fixture) {
    let output = std::process::Command::new("git")
        .current_dir(&fixture.source)
        .args(["diff", "--exit-code", "HEAD", "--", "ov/file"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn full_staging_applies_non_idempotent_clean_filters_exactly_once() {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    repo.config()
        .unwrap()
        .set_str("filter.prefix.clean", "sed 's/^/clean:/'")
        .unwrap();
    fs::write(
        fixture.source.join(".gitattributes"),
        "ov/file filter=prefix\n",
    )
    .unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(".gitattributes")).unwrap();
    let entry = index.get_path(Path::new("ov/file"), 0).unwrap();
    index.add_frombuffer(&entry, b"clean:original").unwrap();
    index.write().unwrap();
    commit_index(&repo);
    fixture.apply();
    // Materialization writes canonical blobs directly. Re-adding those exact
    // bytes must take the raw fast path rather than prepend a second prefix.
    fixture.add();
    index.read(true).unwrap();
    assert_eq!(
        repo.find_blob(index.get_path(Path::new("ov/file"), 0).unwrap().id)
            .unwrap()
            .content(),
        b"clean:original"
    );
    fs::write(fixture.target.join("file"), "target\n").unwrap();
    for _ in 0..2 {
        fixture.add();
        index.read(true).unwrap();
        let staged = index.get_path(Path::new("ov/file"), 0).unwrap();
        assert_eq!(
            repo.find_blob(staged.id).unwrap().content(),
            b"clean:target\n"
        );
    }
    assert_eq!(fs::read(fixture.target.join("file")).unwrap(), b"target\n");
    assert_eq!(
        fs::read(fixture.source.join("ov/file")).unwrap(),
        b"original"
    );
}

#[test]
fn clean_symlinks_and_gitlinks_do_not_enter_checkout_views_or_regular_file_commits() {
    let fixture = Fixture::new();
    fixture.apply();
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("applied"))
        .stdout(contains("module").not())
        .stdout(contains("link (unstaged)").not());
    assert!(!fixture.target.join("link").exists());
    assert!(!fixture.target.join("module").exists());
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let old = repo.head().unwrap().peel_to_tree().unwrap();
    let mut index = repo.index().unwrap();
    let sibling = index.get_path(Path::new("sibling"), 0).unwrap();
    index.add_frombuffer(&sibling, b"unrelated staged").unwrap();
    let mut outside = index.get_path(Path::new("outside-link"), 0).unwrap();
    outside.id = repo.blob(b"unrelated staged destination").unwrap();
    index.add(&outside).unwrap();
    index.write().unwrap();
    let staged_sibling = repo
        .index()
        .unwrap()
        .get_path(Path::new("sibling"), 0)
        .unwrap()
        .id;
    fs::write(fixture.target.join("file"), "selected content").unwrap();
    fixture.add();
    let index_before_commit = fs::read(repo.path().join("index")).unwrap();

    fixture.commit_command().assert().success();

    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    for path in ["ov/link", "ov/module", "sibling", "outside-link"] {
        let path = Path::new(path);
        let before = old.get_path(path).unwrap();
        let after = tree.get_path(path).unwrap();
        assert_eq!(
            (after.id(), after.filemode()),
            (before.id(), before.filemode())
        );
    }
    assert_eq!(
        repo.find_blob(tree.get_path(Path::new("ov/file")).unwrap().id())
            .unwrap()
            .content(),
        b"selected content"
    );
    assert_eq!(
        fs::read(repo.path().join("index")).unwrap(),
        index_before_commit
    );
    assert_eq!(
        repo.index()
            .unwrap()
            .get_path(Path::new("sibling"), 0)
            .unwrap()
            .id,
        staged_sibling
    );
    assert_eq!(
        fs::read_link(fixture.source.join("ov/link")).unwrap(),
        PathBuf::from("file")
    );
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("applied"))
        .stdout(contains("module").not())
        .stdout(contains("link (unstaged)").not());
}

#[test]
fn intentionally_staged_symlink_changes_are_rejected_without_committing_regular_files() {
    let fixture = Fixture::new();
    fixture.apply();
    fs::write(fixture.target.join("file"), "selected content").unwrap();
    fixture.add();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let mut index = repo.index().unwrap();
    let mut link = index.get_path(Path::new("ov/link"), 0).unwrap();
    link.id = repo.blob(b"different destination").unwrap();
    index.add(&link).unwrap();
    index.write().unwrap();
    let head = repo.head().unwrap().target().unwrap();
    let index = fs::read(repo.path().join("index")).unwrap();

    fixture
        .commit_command()
        .assert()
        .failure()
        .stderr(contains("unsupported staged checkout change"));

    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
    assert_eq!(
        fs::read(fixture.source.join("ov/file")).unwrap(),
        b"original"
    );
}

#[test]
fn ignored_unsupported_staged_changes_do_not_block_or_enter_selected_commits() {
    let fixture = Fixture::new();
    fs::write(fixture.source.join(".overignore"), "link\nmodule/\n").unwrap();
    fixture.apply();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let old = repo.head().unwrap().peel_to_tree().unwrap();
    let mut index = repo.index().unwrap();
    let mut link = index.get_path(Path::new("ov/link"), 0).unwrap();
    link.id = repo.blob(b"ignored staged destination").unwrap();
    index.add(&link).unwrap();
    index.remove_path(Path::new("ov/module")).unwrap();
    index.write().unwrap();
    fs::write(fixture.target.join("file"), "selected content").unwrap();
    fixture.add();
    let staged = fs::read(repo.path().join("index")).unwrap();
    fixture.commit_command().assert().success();
    let head = repo.head().unwrap().peel_to_tree().unwrap();
    for path in ["ov/link", "ov/module"] {
        assert_eq!(
            head.get_path(Path::new(path)).unwrap().id(),
            old.get_path(Path::new(path)).unwrap().id()
        );
    }
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), staged);
}

#[test]
fn failed_source_preparation_preserves_head_sources_index_targets_and_checkpoint() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.source.join("ov/locked")).unwrap();
    fs::write(fixture.source.join("ov/locked/file"), "locked original").unwrap();
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("ov/locked/file")).unwrap();
    index.write().unwrap();
    commit_index(&repo);
    fixture.apply();
    fs::write(fixture.target.join("file"), "selected content").unwrap();
    fs::write(fixture.target.join("locked/file"), "locked staged").unwrap();
    fixture.add();
    let directory = fixture.source.join("ov/locked");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    // Root bypasses Unix access checks; the deterministic obstruction and ref
    // lock unit tests still exercise failure recovery under such runners.
    let probe = directory.join("probe");
    if fs::File::create(&probe).is_ok() {
        fs::remove_file(probe).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let head = repo.head().unwrap().target().unwrap();
    let index = fs::read(repo.path().join("index")).unwrap();
    let checkpoint = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let checkpoint_before = fs::read(&checkpoint).unwrap();

    let result = fixture.commit_command().assert();
    // Restore traversal/write access before assertions so failures cannot make
    // the temporary fixture leak an unremovable directory.
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    result.failure();

    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
    assert_eq!(
        fs::read(fixture.source.join("ov/file")).unwrap(),
        b"original"
    );
    assert_eq!(
        fs::read(fixture.source.join("ov/locked/file")).unwrap(),
        b"locked original"
    );
    assert_eq!(
        fs::read(fixture.target.join("file")).unwrap(),
        b"selected content"
    );
    assert_eq!(
        fs::read(fixture.target.join("locked/file")).unwrap(),
        b"locked staged"
    );
    assert_eq!(fs::read(checkpoint).unwrap(), checkpoint_before);
}
