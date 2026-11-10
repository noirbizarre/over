use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use git2::{Repository, Signature};
use predicates::str::contains;
use rstest::rstest;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let home = temp.path().join("home");
        fs::create_dir_all(source.join("ov")).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(
            source.join("ov/over.toml"),
            format!("target='~/target'\n{config}"),
        )
        .unwrap();
        Self { temp, source, home }
    }

    fn command(&self, command: &str) -> Command {
        let mut cmd = Command::cargo_bin("over").unwrap();
        cmd.env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .arg("--home")
            .arg(&self.source)
            .arg(command)
            .arg("ov")
            .arg("--root")
            .arg(&self.home);
        cmd
    }

    fn target(&self) -> PathBuf {
        self.home.join("target")
    }

    fn initialize_source(&self) -> Repository {
        let repo = Repository::init(&self.source).unwrap();
        commit_all(&repo);
        repo
    }

    fn real_checkout(&self) -> Repository {
        let origin = self.temp.path().join("origin");
        let repo = Repository::init(&origin).unwrap();
        fs::write(origin.join("file"), "base\n").unwrap();
        commit_all(&repo);
        fs::write(
            self.source.join("ov/over.toml"),
            format!("target='~/target'\ngit='{}'\n", origin.display()),
        )
        .unwrap();
        self.command("apply").arg("--no-prompt").assert().success();
        Repository::open(self.target()).unwrap()
    }
}

fn commit_all(repo: &Repository) {
    let signature = Signature::now("Test", "test@example.com").unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "fixture",
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap();
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        if !dir.exists() {
            return;
        }
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, result);
            } else if entry.file_type().unwrap().is_symlink() {
                result.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read_link(&path)
                        .unwrap()
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec(),
                );
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[rstest]
#[case(false, false, "soft")]
#[case(true, false, "soft")]
#[case(true, true, "soft")]
#[case(false, false, "hard")]
#[case(true, false, "hard")]
fn sidecar_conflicts_obey_the_same_noninteractive_policy(
    #[case] force: bool,
    #[case] dry_run: bool,
    #[case] link_type: &str,
) {
    let fixture = Fixture::new("");
    let external = fixture.temp.path().join("external");
    fs::write(&external, "source").unwrap();
    fs::write(
        fixture.source.join("ov/link.link.toml"),
        format!("target='{}'\ntype='{link_type}'", external.display()),
    )
    .unwrap();
    fs::create_dir_all(fixture.target().join("link")).unwrap();
    fs::write(fixture.target().join("link/private"), "user data").unwrap();
    let before = snapshot(&fixture.home);
    let mut cmd = fixture.command("apply");
    cmd.arg("--no-prompt");
    if force {
        cmd.arg("--force");
    }
    if dry_run {
        cmd.arg("--dry-run");
    }
    if force || dry_run {
        cmd.assert().success();
    } else {
        cmd.assert().failure().stderr(contains("conflict"));
    }
    if !force || dry_run {
        assert_eq!(snapshot(&fixture.home), before);
    } else {
        assert_eq!(fs::read(fixture.target().join("link")).unwrap(), b"source");
    }
    assert_eq!(fs::read(external).unwrap(), b"source");
}

#[test]
fn hard_link_ownership_is_idempotent_and_unapply_preserves_the_source_inode() {
    let fixture = Fixture::new("");
    let external = fixture.temp.path().join("external");
    fs::write(&external, "source").unwrap();
    fs::write(
        fixture.source.join("ov/link.link.toml"),
        format!("target='{}'\ntype='hard'", external.display()),
    )
    .unwrap();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert!(same_file::is_same_file(&external, fixture.target().join("link")).unwrap());
    fixture
        .command("unapply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert!(!fixture.target().join("link").exists());
    assert_eq!(fs::read(&external).unwrap(), b"source");
}

#[cfg(unix)]
#[rstest]
fn a_scaffold_conflict_never_writes_through_a_foreign_directory_link(
    #[values(false, true)] force: bool,
) {
    let fixture = Fixture::new("");
    fs::create_dir_all(fixture.source.join("ov/sub")).unwrap();
    fs::write(fixture.source.join("ov/sub/file"), "overlay").unwrap();
    let foreign = fixture.temp.path().join("foreign");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("file"), "private").unwrap();
    fs::create_dir_all(fixture.target()).unwrap();
    std::os::unix::fs::symlink(&foreign, fixture.target().join("sub")).unwrap();
    let mut cmd = fixture.command("apply");
    cmd.arg("--no-prompt");
    if force {
        cmd.arg("--force").assert().success();
    } else {
        cmd.assert().failure();
    }
    assert_eq!(fs::read(foreign.join("file")).unwrap(), b"private");
    if force {
        assert!(!fixture.target().join("sub").is_symlink());
    } else {
        assert!(fixture.target().join("sub").is_symlink());
    }
}

#[rstest]
#[case("ignored")]
#[case("staged")]
#[case("detached")]
#[case("branch")]
#[case("stash")]
fn real_checkout_loss_requires_the_dedicated_opt_in(#[case] risk: &str) {
    let fixture = Fixture::new("");
    let mut repo = fixture.real_checkout();
    match risk {
        "ignored" => {
            fs::write(repo.path().join("info/exclude"), "private\n").unwrap();
            fs::write(fixture.target().join("private"), "ignored user data").unwrap();
        }
        "staged" => {
            fs::write(fixture.target().join("file"), "staged user data").unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("file")).unwrap();
            index.write().unwrap();
        }
        "detached" => {
            let oid = repo.head().unwrap().target().unwrap();
            repo.set_head_detached(oid).unwrap();
        }
        "branch" => {
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("local-only", &head, false).unwrap();
        }
        "stash" => {
            fs::write(fixture.target().join("file"), "stashed user data").unwrap();
            repo.stash_save(
                &Signature::now("Test", "test@example.com").unwrap(),
                "private stash",
                None,
            )
            .unwrap();
        }
        _ => unreachable!(),
    }
    let before = snapshot(&fixture.home);
    fixture
        .command("unapply")
        .arg("--no-prompt")
        .assert()
        .failure()
        .stderr(contains("left untouched"));
    assert_eq!(snapshot(&fixture.home), before);
    fixture
        .command("unapply")
        .args(["--discard-checkout", "--no-prompt", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("Git history"));
    assert_eq!(snapshot(&fixture.home), before);
    fixture
        .command("unapply")
        .args(["--discard-checkout", "--no-prompt"])
        .assert()
        .success();
    assert!(!fixture.target().exists());
    assert!(fixture.temp.path().join("origin/.git").exists());
}

#[test]
fn discard_checkout_never_overrides_a_foreign_repository_identity() {
    let fixture = Fixture::new("");
    let repo = fixture.real_checkout();
    repo.remote_set_url("origin", "https://example.com/foreign.git")
        .unwrap();
    let before = snapshot(&fixture.home);
    fixture
        .command("unapply")
        .args(["--discard-checkout", "--no-prompt"])
        .assert()
        .failure()
        .stdout(contains("identity"));
    assert_eq!(snapshot(&fixture.home), before);
}

#[test]
fn apply_requires_checkout_discard_separately_from_force_during_migration() {
    let fixture = Fixture::new("");
    fixture.real_checkout();
    let original = fixture.target();
    let replacement = fixture.temp.path().join("replacement");
    fs::create_dir_all(&replacement).unwrap();
    fs::write(fixture.source.join("ov/over.toml"), "target='~'\n").unwrap();
    fs::write(
        fixture.source.join("ov/target.link.toml"),
        format!("target='{}'\n", replacement.display()),
    )
    .unwrap();
    let before = snapshot(&fixture.home);
    fixture
        .command("apply")
        .args(["--force", "--no-prompt"])
        .assert()
        .failure()
        .stderr(contains("--discard-checkout"));
    assert_eq!(snapshot(&fixture.home), before);
    fixture
        .command("apply")
        .args(["--discard-checkout", "--no-prompt", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("Git history"));
    assert_eq!(snapshot(&fixture.home), before);
    fixture
        .command("apply")
        .args(["--discard-checkout", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(fs::read_link(original).unwrap(), replacement);
}

#[cfg(unix)]
#[test]
fn known_noninteractive_conflicts_prevent_installer_side_effects() {
    let fixture = Fixture::new("");
    let marker = fixture.temp.path().join("installer-ran");
    fs::write(
        fixture.source.join("ov/over.toml"),
        format!(
            "target='~/target'\n[install]\npre=[\"touch '{}'\"]\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::write(fixture.source.join("ov/file"), "overlay").unwrap();
    fs::create_dir_all(fixture.target()).unwrap();
    fs::write(fixture.target().join("file"), "private").unwrap();
    fixture
        .command("apply")
        .args(["--install", "--no-prompt"])
        .assert()
        .failure()
        .stderr(contains("prevent installation"));
    assert!(!marker.exists());
    assert_eq!(fs::read(fixture.target().join("file")).unwrap(), b"private");
}

#[test]
fn changing_a_dirty_virtual_checkout_to_recursive_links_cannot_be_forced() {
    let fixture = Fixture::new("mode='checkout'\n");
    fs::write(fixture.source.join("ov/file"), "base").unwrap();
    let source = fixture.initialize_source();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    fs::write(fixture.target().join("file"), "private edit").unwrap();
    fs::write(fixture.source.join("ov/over.toml"), "target='~/target'\n").unwrap();
    let index = fs::read(source.path().join("index")).unwrap();
    let before = snapshot(&fixture.home);
    fixture
        .command("apply")
        .args(["--force", "--discard-checkout", "--no-prompt"])
        .assert()
        .failure()
        .stderr(contains("old layout"));
    assert_eq!(snapshot(&fixture.home), before);
    assert_eq!(fs::read(source.path().join("index")).unwrap(), index);
}

#[cfg(unix)]
#[test]
fn virtual_refresh_preserves_private_access_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("mode='checkout'\n");
    fs::write(fixture.source.join("ov/file"), "base").unwrap();
    let source = fixture.initialize_source();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    let target = fixture.target().join("file");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(fixture.source.join("ov/file"), "advanced").unwrap();
    commit_all(&source);
    fixture
        .command("sync")
        .arg("--no-prompt")
        .assert()
        .success();
    assert_eq!(fs::read(&target).unwrap(), b"advanced");
    assert_eq!(
        fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn virtual_sync_preview_preserves_filtered_objects_indexes_targets_and_state() {
    let fixture = Fixture::new("mode='checkout'\nignore=['ignored']\n");
    fs::write(fixture.source.join("ov/file"), "base").unwrap();
    fs::write(fixture.source.join("ov/ignored"), "ignored").unwrap();
    let source = fixture.initialize_source();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    fs::write(fixture.source.join("ov/file"), "advanced").unwrap();
    commit_all(&source);
    let before = snapshot(fixture.temp.path());
    fixture
        .command("sync")
        .args(["--dry-run", "--no-prompt"])
        .assert()
        .success()
        .stdout(contains("would replace"));
    assert_eq!(snapshot(fixture.temp.path()), before);
}

#[test]
fn abort_preview_preserves_the_conflicted_merge_and_all_user_edits() {
    let fixture = Fixture::new("");
    let repo = fixture.real_checkout();
    fs::write(fixture.target().join("file"), "local\n").unwrap();
    commit_all(&repo);
    let origin = Repository::open(fixture.temp.path().join("origin")).unwrap();
    fs::write(origin.workdir().unwrap().join("file"), "upstream\n").unwrap();
    commit_all(&origin);
    fixture
        .command("sync")
        .arg("--pull-only")
        .assert()
        .failure();
    assert_eq!(repo.state(), git2::RepositoryState::Merge);
    fs::write(fixture.target().join("unrelated"), "keep this too").unwrap();
    let before = snapshot(fixture.temp.path());
    fixture
        .command("sync")
        .args(["--abort", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("would abort"));
    assert_eq!(snapshot(fixture.temp.path()), before);
    assert_eq!(repo.state(), git2::RepositoryState::Merge);
}

#[test]
fn abort_refuses_an_unrelated_cherry_pick_operation() {
    let fixture = Fixture::new("");
    let repo = fixture.real_checkout();
    fs::write(
        repo.path().join("CHERRY_PICK_HEAD"),
        format!("{}\n", repo.head().unwrap().target().unwrap()),
    )
    .unwrap();
    let before = snapshot(&fixture.home);
    fixture
        .command("sync")
        .arg("--abort")
        .assert()
        .failure()
        .stdout(contains("only an in-progress merge"));
    assert_eq!(snapshot(&fixture.home), before);
}

#[cfg(unix)]
#[test]
fn virtual_unapply_cleans_the_original_association_through_an_ancestor_alias() {
    let mut fixture = Fixture::new("mode='checkout'\n");
    fs::write(fixture.source.join("ov/file"), "base").unwrap();
    let source = fixture.initialize_source();
    let physical_home = fixture.home.clone();
    let alias = fixture.temp.path().join("home-alias");
    std::os::unix::fs::symlink(&physical_home, &alias).unwrap();
    fixture.home = alias;
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    let state = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let key = physical_home
        .join("target")
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    assert!(fs::read_to_string(&state).unwrap().contains(&key));
    let index = fs::read(source.path().join("index")).unwrap();
    fixture
        .command("unapply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert!(!fixture.target().exists());
    assert!(!fs::read_to_string(state).unwrap().contains(&key));
    assert_eq!(fs::read(source.path().join("index")).unwrap(), index);
}

#[cfg(unix)]
#[test]
fn checkout_discard_cannot_remove_a_chmod_only_virtual_checkout_edit() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("mode='checkout'\n");
    fs::write(fixture.source.join("ov/file"), "base").unwrap();
    fixture.initialize_source();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    let file = fixture.target().join("file");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
    fixture
        .command("unapply")
        .args(["--discard-checkout", "--no-prompt"])
        .assert()
        .failure();
    assert_eq!(fs::read(&file).unwrap(), b"base");
    assert_eq!(
        fs::metadata(file).unwrap().permissions().mode() & 0o777,
        0o700
    );
}
