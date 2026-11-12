#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
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
        cmd.args(["ov", "--no-prompt", "--message", "selected", "--target"])
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
