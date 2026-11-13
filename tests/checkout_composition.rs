use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let home = temp.path().join("home");
        fs::create_dir_all(source.join("hosts/laptop")).unwrap();
        fs::write(
            source.join("over.yaml"),
            "mode: checkout\ndefault_overlay: hosts/laptop\noverlays: [apps/*, hosts/*]\n",
        )
        .unwrap();
        fs::write(
            source.join("hosts/laptop/over.yaml"),
            "uses: [apps/git, apps/ssh]\n",
        )
        .unwrap();
        for app in ["git", "ssh"] {
            for directory in [".env.d", ".alias.d", ".config/systemd/user", ".local/bin"] {
                let path = source.join("apps").join(app).join(directory).join(app);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, format!("{app}\n")).unwrap();
            }
            fs::write(
                source.join("apps").join(app).join(format!(".{app}rc")),
                format!("{app}\n"),
            )
            .unwrap();
        }
        let repo = git2::Repository::init(&source).unwrap();
        repo.config().unwrap().set_str("user.name", "Test").unwrap();
        repo.config()
            .unwrap()
            .set_str("user.email", "test@example.com")
            .unwrap();
        commit_all(&repo);
        Self { temp, source, home }
    }

    fn command(&self, command: &str) -> Command {
        let mut cmd = Command::cargo_bin("over").unwrap();
        cmd.env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .args(["--home"])
            .arg(&self.source)
            .arg(command);
        cmd
    }

    fn apply(&self) {
        self.command("apply")
            .args(["hosts/laptop", "--no-prompt", "--target"])
            .arg(&self.home)
            .assert()
            .success();
    }

    fn add(&self, app: &str, path: &Path) {
        self.command("add")
            .args(["--overlay", app, "--target"])
            .arg(&self.home)
            .arg(path)
            .assert()
            .success();
    }

    fn repo(&self) -> git2::Repository {
        git2::Repository::open(&self.source).unwrap()
    }
}

fn commit_all(repo: &git2::Repository) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
    let signature = repo.signature().unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "source",
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap();
}

#[test]
fn flat_checkout_overlays_share_fragments_and_root_files_without_owning_foreign_content() {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.home).unwrap();
    fs::write(fixture.home.join("foreign"), "keep").unwrap();
    fixture.apply();
    fixture.apply();
    for app in ["git", "ssh"] {
        for directory in [
            ".env.d",
            ".alias.d",
            ".config/systemd/user",
            ".local/bin",
            "",
        ] {
            let path = fixture.home.join(directory).join(if directory.is_empty() {
                format!(".{app}rc")
            } else {
                app.into()
            });
            assert!(path.is_file() && !path.is_symlink(), "{}", path.display());
            assert_eq!(fs::read_to_string(path).unwrap(), format!("{app}\n"));
        }
    }
    assert!(!fixture.home.join(".git").exists());
    assert_eq!(fs::read(fixture.home.join("foreign")).unwrap(), b"keep");
    let repo = fixture.repo();
    assert!(
        repo.index()
            .unwrap()
            .get_path(Path::new("apps/git/foreign"), 0)
            .is_none()
    );
}

#[test]
fn reverse_add_and_commit_isolate_siblings_sharing_a_target() {
    let fixture = Fixture::new();
    fixture.apply();
    fs::write(fixture.home.join(".env.d/git"), "staged git").unwrap();
    fs::write(fixture.home.join(".env.d/ssh"), "staged ssh").unwrap();
    fixture.add("apps/git", &fixture.home.join(".env.d"));
    let repo = fixture.repo();
    let index = repo.index().unwrap();
    let ssh = index.get_path(Path::new("apps/ssh/.env.d/ssh"), 0).unwrap();
    assert_eq!(repo.find_blob(ssh.id).unwrap().content(), b"ssh\n");
    fixture.add("apps/ssh", &fixture.home.join(".env.d/ssh"));
    fs::write(fixture.home.join(".env.d/git"), "later git").unwrap();
    fixture
        .command("commit")
        .args([
            "--overlay",
            "apps/git",
            "--no-prompt",
            "-m",
            "git only",
            "--target",
        ])
        .arg(&fixture.home)
        .assert()
        .success();
    let head = repo.head().unwrap().peel_to_tree().unwrap();
    let git = head.get_path(Path::new("apps/git/.env.d/git")).unwrap();
    let ssh = head.get_path(Path::new("apps/ssh/.env.d/ssh")).unwrap();
    assert_eq!(repo.find_blob(git.id()).unwrap().content(), b"staged git");
    assert_eq!(repo.find_blob(ssh.id()).unwrap().content(), b"ssh\n");
    assert_eq!(
        fs::read(fixture.home.join(".env.d/git")).unwrap(),
        b"later git"
    );
}

#[test]
fn explicitly_acquired_files_remain_regular_and_cannot_be_stolen_by_a_sibling() {
    let fixture = Fixture::new();
    fixture.apply();
    let fresh = fixture.home.join(".env.d/fresh");
    fs::write(&fresh, "new").unwrap();
    fixture
        .command("add")
        .arg("--target")
        .arg(&fixture.home)
        .arg(&fresh)
        .assert()
        .failure();
    fixture.add("apps/git", &fresh);
    assert!(!fresh.is_symlink());
    fixture
        .command("add")
        .args(["--overlay", "apps/ssh", "--target"])
        .arg(&fixture.home)
        .arg(&fresh)
        .assert()
        .failure()
        .stderr(contains("conflicting target ownership"));
    assert_eq!(fs::read(&fresh).unwrap(), b"new");
    assert!(
        fixture
            .repo()
            .index()
            .unwrap()
            .get_path(Path::new("apps/ssh/.env.d/fresh"), 0)
            .is_none()
    );
}

#[rstest::rstest]
#[case("collision")]
#[case("git\n")]
fn duplicate_projected_files_fail_before_any_destination_is_created(#[case] content: &str) {
    let fixture = Fixture::new();
    fs::write(fixture.source.join("apps/ssh/.env.d/git"), content).unwrap();
    fixture
        .command("apply")
        .args(["hosts/laptop", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("conflicting target ownership"));
    assert!(!fixture.home.exists());
}

#[test]
fn composition_lint_works_without_git_or_deployment_state() {
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.source.join(".git")).unwrap();
    fixture
        .command("lint")
        .args(["hosts/laptop", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert!(!fixture.home.exists());
    assert!(!fixture.temp.path().join("state").exists());
}

#[test]
fn source_sync_and_unapply_preserve_sibling_fragments_and_associations() {
    let fixture = Fixture::new();
    fixture.apply();
    fs::write(fixture.home.join(".env.d/ssh"), "ssh target edit").unwrap();
    fs::write(
        fixture.source.join("apps/git/.env.d/git"),
        "git source edit",
    )
    .unwrap();
    fs::write(
        fixture.source.join("apps/git/.env.d/added"),
        "source addition",
    )
    .unwrap();
    fs::remove_file(fixture.source.join("apps/git/.alias.d/git")).unwrap();
    commit_all(&fixture.repo());
    fixture
        .command("sync")
        .args(["apps/git", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert_eq!(
        fs::read(fixture.home.join(".env.d/git")).unwrap(),
        b"git source edit"
    );
    assert_eq!(
        fs::read(fixture.home.join(".env.d/added")).unwrap(),
        b"source addition"
    );
    assert!(!fixture.home.join(".alias.d/git").exists());
    fixture
        .command("unapply")
        .args(["apps/git", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert!(!fixture.home.join(".env.d/git").exists());
    assert!(!fixture.home.join(".env.d/added").exists());
    assert_eq!(
        fs::read(fixture.home.join(".env.d/ssh")).unwrap(),
        b"ssh target edit"
    );
    fixture.add("apps/ssh", &fixture.home.join(".env.d/ssh"));
}

#[test]
fn resetting_an_acquisition_does_not_release_its_ownership() {
    let fixture = Fixture::new();
    fixture.apply();
    let fresh = fixture.home.join(".env.d/fresh");
    fs::write(&fresh, "reserved").unwrap();
    fixture.add("apps/git", &fresh);
    let repo = fixture.repo();
    let mut index = repo.index().unwrap();
    index
        .remove_path(Path::new("apps/git/.env.d/fresh"))
        .unwrap();
    index.write().unwrap();
    fixture
        .command("add")
        .args(["--overlay", "apps/ssh", "--target"])
        .arg(&fixture.home)
        .arg(&fresh)
        .assert()
        .failure()
        .stderr(contains("conflicting target ownership"));
    fixture
        .command("add")
        .arg("--target")
        .arg(&fixture.home)
        .arg(&fresh)
        .assert()
        .success();
}

#[cfg(unix)]
#[test]
fn aliases_inside_shared_containers_cannot_hide_file_conflicts() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.home.join(".env.d")).unwrap();
    std::os::unix::fs::symlink(fixture.home.join(".env.d"), fixture.home.join(".alias.d")).unwrap();
    fs::write(
        fixture.source.join("apps/ssh/.alias.d/git"),
        "aliased conflict",
    )
    .unwrap();
    fixture
        .command("apply")
        .args(["hosts/laptop", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("conflicting target ownership"));
    assert!(!fixture.home.join(".env.d/git").exists());
}

#[test]
fn simultaneous_acquisitions_of_a_shared_leaf_have_only_one_owner() {
    let fixture = Fixture::new();
    fixture.apply();
    let fresh = fixture.home.join(".env.d/fresh");
    fs::write(&fresh, "one owner").unwrap();
    let mut commands = Vec::new();
    for overlay in ["apps/git", "apps/ssh"] {
        let mut command = fixture.command("add");
        command
            .args(["--overlay", overlay, "--target"])
            .arg(&fixture.home)
            .arg(&fresh);
        commands.push(command);
    }
    let results = std::thread::scope(|scope| {
        let workers: Vec<_> = commands
            .iter_mut()
            .map(|command| scope.spawn(move || command.output().unwrap()))
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1
    );
    let index = fixture.repo().index().unwrap();
    assert_eq!(
        ["apps/git/.env.d/fresh", "apps/ssh/.env.d/fresh"]
            .iter()
            .filter(|path| index.get_path(Path::new(path), 0).is_some())
            .count(),
        1
    );
    assert!(!fresh.is_symlink());
}

#[test]
fn an_interrupted_initialization_keeps_ownership_and_completes_on_reapply() {
    let fixture = Fixture::new();
    fixture.apply();
    let path = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let mut document: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for (_, record) in document["state"]["checkouts"]
        .as_table_mut()
        .unwrap()
        .iter_mut()
    {
        if record["overlay"].as_str() == Some("apps/git") {
            record["initializing"] = true.into();
        }
    }
    fs::write(&path, toml::to_string(&document).unwrap()).unwrap();
    fs::remove_file(fixture.home.join(".gitrc")).unwrap();
    fixture
        .command("apply")
        .args(["apps/git", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert_eq!(fs::read(fixture.home.join(".gitrc")).unwrap(), b"git\n");
    fixture.add("apps/git", &fixture.home.join(".gitrc"));
    fixture.add("apps/ssh", &fixture.home.join(".sshrc"));
    let document: toml::Value = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert!(
        document["state"]["checkouts"]
            .as_table()
            .unwrap()
            .values()
            .all(|record| record["initializing"].as_bool() == Some(false))
    );
}

#[test]
fn legacy_exclusive_associations_are_not_silently_narrowed_by_fresh_composition() {
    let fixture = Fixture::new();
    fixture.apply();
    let path = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let mut document: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    document["version"] = 2.into();
    let records = document["state"]["checkouts"].as_table_mut().unwrap();
    let mut record = records
        .values()
        .find(|record| record["overlay"].as_str() == Some("apps/git"))
        .unwrap()
        .clone();
    record["scope"].as_table_mut().unwrap().remove("projection");
    record.as_table_mut().unwrap().remove("initializing");
    records.clear();
    records.insert(
        fixture
            .home
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        record,
    );
    fs::write(&path, toml::to_string(&document).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    fixture
        .command("lint")
        .args(["hosts/laptop", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stdout(contains("conflicting target ownership"));
    assert_eq!(fs::read(path).unwrap(), before);
    assert_eq!(fs::read(fixture.home.join(".env.d/ssh")).unwrap(), b"ssh\n");
}

#[cfg(unix)]
#[test]
fn force_never_converts_a_deployed_file_symlink_to_a_projection() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.home.join(".env.d")).unwrap();
    let source = fixture.source.join("apps/git/.env.d/git");
    let target = fixture.home.join(".env.d/git");
    std::os::unix::fs::symlink(&source, &target).unwrap();
    fixture
        .command("apply")
        .args(["apps/git", "--force", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("unapply the old layout first"));
    assert_eq!(fs::read_link(target).unwrap(), source);
}

#[test]
fn projected_leaves_cannot_enter_another_overlays_atomic_git_repository() {
    let fixture = Fixture::new();
    fs::write(
        fixture.source.join("apps/git/over.yaml"),
        "target: ~/.local/bin\ngit: https://example.invalid/tool.git\n",
    )
    .unwrap();
    fixture
        .command("apply")
        .args(["hosts/laptop", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("conflicting target ownership"));
    assert!(!fixture.home.exists());
}

#[test]
fn a_projected_file_cannot_replace_a_siblings_required_directory() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.source.join("apps/ssh/.env.d/git")).unwrap();
    fs::write(fixture.source.join("apps/ssh/.env.d/git/child"), "child").unwrap();
    fixture
        .command("apply")
        .args(["hosts/laptop", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("conflicting target ownership"));
    assert!(!fixture.home.exists());
}
