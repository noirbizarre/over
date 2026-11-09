use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    home: PathBuf,
    base: PathBuf,
    etc: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new(checkout: bool) -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let home = temp.path().join("home");
        let base = home.join("Workspace/Client");
        let etc = temp.path().join("system/client");
        let config = base.join(".config/client");
        for dir in ["main", "etc", "config"] {
            fs::create_dir_all(source.join("workspace/client").join(dir)).unwrap();
            fs::write(
                source.join("workspace/client").join(dir).join("file"),
                format!("{dir}\n"),
            )
            .unwrap();
        }
        fs::write(source.join("workspace/client/over.yaml"), format!(
            "target: ~/Workspace/Client\n{}map:\n  etc:\n    target: {:?}\n  config:\n    target: ~/Workspace/Client/.config/client\n",
            if checkout { "mode: checkout\n" } else { "" }, etc.to_string_lossy()
        )).unwrap();
        let repo = git2::Repository::init(&source).unwrap();
        repo.config().unwrap().set_str("user.name", "Test").unwrap();
        repo.config()
            .unwrap()
            .set_str("user.email", "test@example.com")
            .unwrap();
        commit_all(&repo, "initial");
        Self {
            temp,
            source,
            home,
            base,
            etc,
            config,
        }
    }

    fn command(&self, command: &str) -> Command {
        let mut cmd = Command::cargo_bin("over").unwrap();
        cmd.env("XDG_STATE_HOME", self.temp.path().join("state"))
            .args(["--home"])
            .arg(&self.source)
            .arg(command);
        cmd
    }

    fn apply(&self) {
        self.command("apply")
            .args(["workspace/client", "--no-prompt", "--root"])
            .arg(&self.home)
            .assert()
            .success();
    }

    fn add(&self, paths: &[&Path], dry_run: bool) {
        let mut cmd = self.command("add");
        cmd.args(["--overlay", "workspace/client", "--root"])
            .arg(&self.home);
        if dry_run {
            cmd.arg("--dry-run");
        }
        cmd.args(paths).assert().success();
    }

    fn commit(&self) {
        self.command("commit")
            .args([
                "workspace/client",
                "--no-prompt",
                "--message",
                "selected",
                "--root",
            ])
            .arg(&self.home)
            .assert()
            .success();
    }

    fn repo(&self) -> git2::Repository {
        git2::Repository::open(&self.source).unwrap()
    }
}

fn commit_all(repo: &git2::Repository, message: &str) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parents: Vec<_> = repo
        .head()
        .ok()
        .map(|head| head.peel_to_commit().unwrap())
        .into_iter()
        .collect();
    let parents: Vec<_> = parents.iter().collect();
    let signature = repo.signature().unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )
    .unwrap();
}

#[test]
fn mapped_symlinks_apply_and_reverse_add_without_base_prefix_copies() {
    let fixture = Fixture::new(false);
    fixture.apply();
    for target in [&fixture.etc, &fixture.config, &fixture.base.join("main")] {
        assert!(target.join("file").is_symlink());
    }
    assert!(!fixture.base.join("etc").exists());
    assert!(!fixture.base.join("config").exists());
    let new = fixture.etc.join("other.yaml");
    fs::write(&new, "key: value\n").unwrap();
    fixture.add(&[&new], true);
    assert!(!new.is_symlink());
    fixture.add(&[&new], false);
    assert!(new.is_symlink());
    assert_eq!(
        fs::read_to_string(fixture.source.join("workspace/client/etc/other.yaml")).unwrap(),
        "key: value\n"
    );
    fixture.command("lint").assert().success();
}

#[test]
fn mapped_checkout_defaults_preserve_index_and_exclude_both_routing_namespaces() {
    let fixture = Fixture::new(true);
    let before = fs::read(fixture.source.join(".git/index")).unwrap();
    fixture.apply();
    assert_eq!(before, fs::read(fixture.source.join(".git/index")).unwrap());
    for target in [&fixture.etc, &fixture.config, &fixture.base.join("main")] {
        assert!(target.join("file").is_file());
        assert!(!target.join("file").is_symlink());
    }
    assert!(!fixture.base.join("etc").exists());
    assert!(!fixture.base.join("config").exists());
    fixture
        .command("status")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("3 applied"));
}

#[test]
fn mapped_checkout_add_stages_snapshots_and_commit_preserves_unstaged_edits() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let file = fixture.config.join("file");
    fs::write(&file, "staged\n").unwrap();
    let index_before = fs::read(fixture.source.join(".git/index")).unwrap();
    fixture.add(&[&file], true);
    assert_eq!(
        index_before,
        fs::read(fixture.source.join(".git/index")).unwrap()
    );
    fixture.add(&[&file], false);
    assert!(!file.is_symlink());
    let repo = fixture.repo();
    let entry = repo
        .index()
        .unwrap()
        .get_path(Path::new("workspace/client/config/file"), 0)
        .unwrap();
    assert_eq!(repo.find_blob(entry.id).unwrap().content(), b"staged\n");
    fs::write(&file, "unstaged\n").unwrap();
    fixture
        .command("status")
        .args(["workspace/client", "--verbose", "--root"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("file (staged)"))
        .stdout(contains("file (unstaged)"));
    fixture
        .command("diff")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("staged"))
        .stdout(contains("unstaged"));
    fixture.commit();
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    let committed = tree
        .get_path(Path::new("workspace/client/config/file"))
        .unwrap();
    assert_eq!(
        repo.find_blob(committed.id()).unwrap().content(),
        b"staged\n"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "unstaged\n");
    assert_eq!(
        fs::read_to_string(fixture.source.join("workspace/client/config/file")).unwrap(),
        "staged\n"
    );
}

#[test]
fn mapped_checkout_staging_handles_deletions_and_preserves_unrelated_index_entries() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let repo = fixture.repo();
    fs::write(fixture.source.join("unrelated"), "keep staged").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("unrelated")).unwrap();
    index.write().unwrap();
    fs::remove_file(fixture.etc.join("file")).unwrap();
    fixture.add(&[&fixture.etc.join("file")], false);
    fixture.commit();
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    assert!(
        tree.get_path(Path::new("workspace/client/etc/file"))
            .is_err()
    );
    assert!(tree.get_path(Path::new("unrelated")).is_err());
    assert!(
        repo.index()
            .unwrap()
            .get_path(Path::new("unrelated"), 0)
            .is_some()
    );
    assert!(!fixture.source.join("workspace/client/etc/file").exists());
}

#[test]
fn clean_residual_unapply_preserves_a_dirty_nested_mapped_checkout() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.config.join("file"), "keep local").unwrap();
    fixture
        .command("unapply")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .failure();
    assert_eq!(
        fs::read_to_string(fixture.config.join("file")).unwrap(),
        "keep local"
    );
    assert!(!fixture.base.join("main/file").exists());
    assert!(!fixture.etc.join("file").exists());
}

#[test]
fn mapped_source_drift_syncs_only_its_own_checkout() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(
        fixture.source.join("workspace/client/etc/file"),
        "source advance\n",
    )
    .unwrap();
    commit_all(&fixture.repo(), "advance etc");
    fixture
        .command("status")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("2 applied"))
        .stdout(contains("1 behind"));
    fixture
        .command("sync")
        .args(["workspace/client", "--no-prompt", "--root"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(fixture.etc.join("file")).unwrap(),
        "source advance\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.config.join("file")).unwrap(),
        "config\n"
    );
}

#[test]
fn overlapping_mappings_fail_before_creating_targets() {
    let fixture = Fixture::new(false);
    fs::write(fixture.source.join("workspace/client/over.yaml"), "target: ~/Workspace/Client\nmap:\n  config:\n    target: ~/.config/client\n  config/editor:\n    target: ~/.config/editor\n").unwrap();
    fixture
        .command("apply")
        .args(["workspace/client", "--no-prompt", "--root"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("nested mapping"));
    assert!(!fixture.base.exists());
}

#[test]
fn mapped_whole_directory_symlinks_create_their_external_parents() {
    let fixture = Fixture::new(false);
    let descriptor = fixture.source.join("workspace/client/over.yaml");
    let mut config = fs::read_to_string(&descriptor).unwrap();
    config.push_str("mode: symlink-directory\n");
    fs::write(&descriptor, config).unwrap();
    fixture.apply();
    assert!(fixture.etc.is_symlink());
    assert!(fixture.config.is_symlink());
    assert_eq!(
        fs::read_link(&fixture.etc).unwrap(),
        fixture.source.join("workspace/client/etc")
    );
}

#[test]
fn mappings_inside_whole_directory_symlinks_are_rejected() {
    let fixture = Fixture::new(false);
    fs::create_dir_all(fixture.source.join("workspace/client/config/editor")).unwrap();
    fs::write(fixture.source.join("workspace/client/over.yaml"), "target: ~/workspace\nrules:\n  - path: config\n    mode: symlink-directory\nmap:\n  config/editor:\n    target: ~/editor\n").unwrap();
    fixture
        .command("apply")
        .args(["workspace/client", "--no-prompt", "--root"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("inside whole-directory symlink"));
    fixture
        .command("lint")
        .assert()
        .failure()
        .stdout(contains("inside whole-directory symlink"));
    assert!(!fixture.home.exists());
}

#[test]
fn absent_mapped_checkout_directories_can_be_populated_and_staged() {
    let fixture = Fixture::new(true);
    fs::remove_dir_all(fixture.source.join("workspace/client/etc")).unwrap();
    let repo = fixture.repo();
    let mut index = repo.index().unwrap();
    index
        .remove_path(Path::new("workspace/client/etc/file"))
        .unwrap();
    index.write().unwrap();
    commit_all(&repo, "remove etc");
    fixture.apply();
    fs::write(fixture.etc.join("new"), "new configuration").unwrap();
    fixture.add(&[&fixture.etc], false);
    fixture.commit();
    assert_eq!(
        fs::read_to_string(fixture.source.join("workspace/client/etc/new")).unwrap(),
        "new configuration"
    );
    assert!(!fixture.etc.join("new").is_symlink());
}

#[test]
fn mappings_partition_an_enclosing_subtree_checkout() {
    let fixture = Fixture::new(false);
    fs::create_dir_all(fixture.source.join("workspace/client/main/external")).unwrap();
    fs::write(
        fixture.source.join("workspace/client/main/external/new"),
        "mapped",
    )
    .unwrap();
    fs::write(fixture.source.join("workspace/client/over.yaml"), "target: ~/Workspace/Client\nrules:\n  - path: main\n    mode: checkout\n  - path: main/external\n    mode: checkout\nmap:\n  main/external:\n    target: ~/external\n").unwrap();
    commit_all(&fixture.repo(), "subtree mapping");
    fixture.apply();
    assert_eq!(
        fs::read_to_string(fixture.home.join("external/new")).unwrap(),
        "mapped"
    );
    assert!(!fixture.base.join("main/external").exists());
    fixture
        .command("status")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("0 modified"));
}

#[test]
fn source_collisions_with_protected_checkout_destinations_fail_before_writes() {
    let fixture = Fixture::new(true);
    fs::create_dir_all(fixture.source.join("workspace/client/.config/client")).unwrap();
    fs::write(
        fixture
            .source
            .join("workspace/client/.config/client/collision"),
        "residual",
    )
    .unwrap();
    commit_all(&fixture.repo(), "collision");
    fixture
        .command("apply")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains(
            "collides with an independently mapped destination",
        ));
    assert!(!fixture.base.exists());
    assert!(!fixture.etc.exists());
}

#[test]
fn deployed_checkout_scopes_cannot_be_silently_changed() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let descriptor = fixture.source.join("workspace/client/over.yaml");
    let config = fs::read_to_string(&descriptor)
        .unwrap()
        .replace(".config/client", ".settings/client");
    fs::write(descriptor, config).unwrap();
    fixture
        .command("apply")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("differs from its source or mapping scope"));
    assert!(fixture.config.join("file").exists());
    assert!(!fixture.base.join(".settings/client").exists());
}

#[test]
fn recursive_add_stages_multiple_units_in_one_commit_and_ignores_unselected_edits() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.base.join("main/file"), "main staged").unwrap();
    fs::write(fixture.config.join("file"), "config staged").unwrap();
    fs::write(fixture.etc.join("file"), "not selected").unwrap();
    let old_head = fixture
        .repo()
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id();
    fixture.add(&[&fixture.base], false);
    fixture.commit();
    let repo = fixture.repo();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_id(0).unwrap(), old_head);
    let tree = head.tree().unwrap();
    for (path, expected) in [
        ("main/file", "main staged"),
        ("config/file", "config staged"),
        ("etc/file", "etc\n"),
    ] {
        let entry = tree
            .get_path(&Path::new("workspace/client").join(path))
            .unwrap();
        assert_eq!(
            repo.find_blob(entry.id()).unwrap().content(),
            expected.as_bytes()
        );
    }
}

#[test]
fn directory_staging_records_tracked_deletions_and_refuses_unknown_missing_paths() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::remove_file(fixture.config.join("file")).unwrap();
    fixture.add(&[&fixture.config], false);
    assert!(
        fixture
            .repo()
            .index()
            .unwrap()
            .get_path(Path::new("workspace/client/config/file"), 0)
            .is_none()
    );
    fixture
        .command("add")
        .args(["--overlay", "workspace/client", "--root"])
        .arg(&fixture.home)
        .arg(fixture.config.join("unknown"))
        .assert()
        .failure()
        .stderr(contains("does not match any tracked"));
}

#[test]
fn staging_refuses_to_overwrite_unstaged_source_edits() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.config.join("file"), "target edit").unwrap();
    fs::write(
        fixture.source.join("workspace/client/config/file"),
        "source edit",
    )
    .unwrap();
    let before = fs::read(fixture.source.join(".git/index")).unwrap();
    fixture
        .command("add")
        .args(["--overlay", "workspace/client", "--root"])
        .arg(&fixture.home)
        .arg(fixture.config.join("file"))
        .assert()
        .failure()
        .stderr(contains("has unstaged edits"));
    assert_eq!(before, fs::read(fixture.source.join(".git/index")).unwrap());
    assert_eq!(
        fs::read_to_string(fixture.config.join("file")).unwrap(),
        "target edit"
    );
}

#[test]
fn log_infers_a_mapped_checkout_outside_the_base_target() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(
        fixture.source.join("workspace/client/etc/file"),
        "etc history",
    )
    .unwrap();
    commit_all(&fixture.repo(), "etc history");
    fs::write(
        fixture.source.join("workspace/client/main/file"),
        "main history",
    )
    .unwrap();
    commit_all(&fixture.repo(), "main history");
    fixture
        .command("log")
        .arg("--root")
        .arg(&fixture.home)
        .current_dir(&fixture.etc)
        .assert()
        .success()
        .stdout(contains("etc history"))
        .stdout(predicates::prelude::PredicateBooleanExt::not(contains(
            "main history",
        )));
    fixture
        .command("log")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("etc history"))
        .stdout(contains("main history"));
}

#[test]
fn scoped_checkout_excludes_do_not_hide_foreign_files_in_the_host_repository() {
    let fixture = Fixture::new(true);
    fs::create_dir_all(&fixture.home).unwrap();
    let host = git2::Repository::init(&fixture.home).unwrap();
    fixture.apply();
    fs::write(fixture.base.join("foreign"), "not managed").unwrap();
    assert!(
        host.status_file(Path::new("Workspace/Client/foreign"))
            .unwrap()
            .is_wt_new()
    );
    assert!(
        host.status_file(Path::new("Workspace/Client/main/file"))
            .unwrap()
            .is_ignored()
    );
}

#[test]
fn mapped_sidecars_are_independent_of_their_enclosing_checkout() {
    let fixture = Fixture::new(true);
    let external = fixture.temp.path().join("external");
    fs::write(&external, "linked").unwrap();
    fs::write(
        fixture
            .source
            .join("workspace/client/config/link.link.toml"),
        format!("target = {:?}\n", external.to_string_lossy()),
    )
    .unwrap();
    commit_all(&fixture.repo(), "sidecar");
    fixture.apply();
    assert!(fixture.config.join("link").is_symlink());
    fixture
        .command("status")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("0 modified"));
}

#[test]
fn git_add_and_status_reverse_route_an_external_repository() {
    let fixture = Fixture::new(false);
    fixture.apply();
    let host = git2::Repository::init(&fixture.etc).unwrap();
    host.config()
        .unwrap()
        .set_str("over.overlay", "workspace/client")
        .unwrap();
    fs::write(fixture.etc.join("new"), "git integration").unwrap();
    fixture
        .command("git")
        .args(["add", "new"])
        .current_dir(&fixture.etc)
        .assert()
        .success();
    assert!(fixture.etc.join("new").is_symlink());
    assert_eq!(
        fs::read_to_string(fixture.source.join("workspace/client/etc/new")).unwrap(),
        "git integration"
    );
    fixture
        .command("git")
        .arg("status")
        .current_dir(&fixture.etc)
        .assert()
        .success()
        .stdout(contains("Managed files"));
}

#[test]
fn staging_never_follows_source_symlinks_into_unrelated_files() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let external = fixture.temp.path().join("external");
    fs::write(&external, "preserve").unwrap();
    let source = fixture.source.join("workspace/client/config/new");
    symlink::symlink_file(&external, &source).unwrap();
    fs::write(fixture.config.join("new"), "target edit").unwrap();
    fixture
        .command("add")
        .args(["--overlay", "workspace/client", "--root"])
        .arg(&fixture.home)
        .arg(fixture.config.join("new"))
        .assert()
        .failure()
        .stderr(contains("is a symlink"));
    assert_eq!(fs::read_to_string(external).unwrap(), "preserve");
}

#[test]
fn sync_advances_source_only_changes_without_losing_staged_snapshots_or_local_edits() {
    let fixture = Fixture::new(true);
    let second = fixture.source.join("workspace/client/main/second");
    fs::write(&second, "original").unwrap();
    commit_all(&fixture.repo(), "second file");
    fixture.apply();
    let first_target = fixture.base.join("main/file");
    fs::write(&first_target, "staged snapshot").unwrap();
    fixture.add(&[&first_target], false);
    fs::write(&first_target, "later unstaged edit").unwrap();
    fs::write(&second, "source advance").unwrap();
    let repo = fixture.repo();
    let path = Path::new("workspace/client/main/second");
    let mut index = repo.index().unwrap();
    index.add_path(path).unwrap();
    index.write().unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let mut builder = git2::build::TreeUpdateBuilder::new();
    builder.upsert(
        path,
        repo.blob(b"source advance").unwrap(),
        git2::FileMode::Blob,
    );
    let tree = repo
        .find_tree(
            builder
                .create_updated(&repo, &parent.tree().unwrap())
                .unwrap(),
        )
        .unwrap();
    let signature = repo.signature().unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "advance second only",
        &tree,
        &[&parent],
    )
    .unwrap();
    fixture
        .command("sync")
        .args(["workspace/client", "--no-prompt", "--root"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(&first_target).unwrap(),
        "later unstaged edit"
    );
    assert_eq!(
        fs::read_to_string(fixture.base.join("main/second")).unwrap(),
        "source advance"
    );
    fixture.commit();
    let head = repo.head().unwrap().peel_to_tree().unwrap();
    let committed = head
        .get_path(Path::new("workspace/client/main/file"))
        .unwrap();
    assert_eq!(
        repo.find_blob(committed.id()).unwrap().content(),
        b"staged snapshot"
    );
    assert_eq!(
        fs::read_to_string(first_target).unwrap(),
        "later unstaged edit"
    );
}

#[test]
fn sync_recovers_the_checkpoint_after_a_plain_git_commit_of_staged_target_content() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.etc.join("file"), "staged externally").unwrap();
    fixture.add(&[&fixture.etc.join("file")], false);
    let repo = fixture.repo();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let signature = repo.signature().unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "plain git commit",
        &tree,
        &[&parent],
    )
    .unwrap();
    fixture
        .command("sync")
        .args(["workspace/client", "--no-prompt", "--root"])
        .arg(&fixture.home)
        .assert()
        .success();
    fixture
        .command("status")
        .args(["workspace/client", "--root"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("3 applied"));
    assert_eq!(
        fs::read_to_string(fixture.etc.join("file")).unwrap(),
        "staged externally"
    );
}

#[test]
fn source_deletions_conflict_with_local_edits_instead_of_erasing_them() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.etc.join("file"), "preserve local").unwrap();
    fs::remove_file(fixture.source.join("workspace/client/etc/file")).unwrap();
    let repo = fixture.repo();
    let mut index = repo.index().unwrap();
    index
        .remove_path(Path::new("workspace/client/etc/file"))
        .unwrap();
    index.write().unwrap();
    commit_all(&repo, "source deletion");
    fixture
        .command("sync")
        .args(["workspace/client", "--no-prompt", "--root"])
        .arg(&fixture.home)
        .assert()
        .failure();
    assert_eq!(
        fs::read_to_string(fixture.etc.join("file")).unwrap(),
        "preserve local"
    );
}

#[test]
fn ancestor_aliases_preserve_mapped_checkout_staging_and_history_inference() {
    let fixture = Fixture::new(true);
    fs::create_dir_all(fixture.etc.parent().unwrap()).unwrap();
    let alias = fixture.temp.path().join("system-alias");
    symlink::symlink_dir(fixture.etc.parent().unwrap(), &alias).unwrap();
    let descriptor = fixture.source.join("workspace/client/over.yaml");
    let config = fs::read_to_string(&descriptor).unwrap().replace(
        &format!("{:?}", fixture.etc.to_string_lossy()),
        &format!("{:?}", alias.join("client").to_string_lossy()),
    );
    fs::write(descriptor, config).unwrap();
    fixture.apply();
    let new = fixture.etc.join("new");
    fs::write(&new, "alias snapshot").unwrap();
    fixture.add(&[&new], false);
    assert!(!new.is_symlink());
    let repo = fixture.repo();
    let staged = repo
        .index()
        .unwrap()
        .get_path(Path::new("workspace/client/etc/new"), 0)
        .unwrap();
    assert_eq!(
        repo.find_blob(staged.id).unwrap().content(),
        b"alias snapshot"
    );
    fixture
        .command("log")
        .arg("--root")
        .arg(&fixture.home)
        .current_dir(&fixture.etc)
        .assert()
        .success()
        .stdout(contains("History for workspace/client"));
}
