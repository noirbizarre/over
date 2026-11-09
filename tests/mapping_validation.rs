use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    source: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("ov/etc")).unwrap();
        fs::write(source.join("ov/file"), "base\n").unwrap();
        fs::write(source.join("ov/etc/file"), "mapped\n").unwrap();
        fs::write(
            source.join("ov/over.toml"),
            "target='~/base'\nmode='checkout'\n[map.etc]\ntarget='../../external-a'\n",
        )
        .unwrap();
        let repo = git2::Repository::init(&source).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        Self { temp, source }
    }

    fn command(&self, subcommand: &str) -> Command {
        let mut command = Command::cargo_bin("over").unwrap();
        // Subprocess-local cwd and state avoid changing the test runner's environment.
        command
            .current_dir(self.temp.path())
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .arg("--home")
            .arg(&self.source)
            .arg(subcommand);
        command
    }

    fn apply(&self) {
        self.command("apply")
            .args(["ov", "--no-prompt", "--root", "home"])
            .assert()
            .success();
    }
}

#[test]
fn relative_roots_stage_checkout_files_without_ingesting_source_edits() {
    let fixture = Fixture::new();
    fixture.apply();
    let target = fixture.temp.path().join("home/base/file");
    let source = fixture.source.join("ov/file");
    fs::write(&target, "selected target edit\n").unwrap();
    fixture
        .command("add")
        .args([
            "--overlay",
            "ov",
            "--root",
            "home",
            "--force",
            "home/base/file",
        ])
        .assert()
        .success();

    assert!(!target.is_symlink());
    assert_eq!(fs::read(&target).unwrap(), b"selected target edit\n");
    assert_eq!(fs::read(&source).unwrap(), b"base\n");
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let staged = repo
        .index()
        .unwrap()
        .get_path(Path::new("ov/file"), 0)
        .unwrap();
    assert_eq!(
        repo.find_blob(staged.id).unwrap().content(),
        b"selected target edit\n"
    );
    assert!(!fixture.temp.path().join("external-a/file").is_symlink());

    fs::write(&source, "independent source edit\n").unwrap();
    fs::write(&target, "later target edit\n").unwrap();
    let index = fs::read(fixture.source.join(".git/index")).unwrap();
    // Even --force must reach staging's dirty-source guard instead of move ingestion.
    fixture
        .command("add")
        .args([
            "--overlay",
            "ov",
            "--root",
            "home",
            "--force",
            "home/base/file",
        ])
        .assert()
        .failure()
        .stderr(contains("has unstaged edits"));
    assert!(!target.is_symlink());
    assert_eq!(fs::read(&target).unwrap(), b"later target edit\n");
    assert_eq!(fs::read(&source).unwrap(), b"independent source edit\n");
    assert_eq!(fs::read(fixture.source.join(".git/index")).unwrap(), index);
}

#[test]
fn moving_a_live_external_mapping_is_rejected_before_any_writes() {
    let fixture = Fixture::new();
    fixture.apply();
    let old_target = fixture.temp.path().join("external-a/file");
    fs::write(&old_target, "keep deployed edit\n").unwrap();
    fs::write(fixture.source.join("ov/etc/file"), "keep source edit\n").unwrap();
    let config = fixture.source.join("ov/over.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("external-a", "external-b"),
    )
    .unwrap();
    let state_path = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let state = fs::read(&state_path).unwrap();
    let index = fs::read(fixture.source.join(".git/index")).unwrap();
    fixture
        .command("apply")
        .args(["ov", "--no-prompt", "--root", "home"])
        .assert()
        .failure()
        .stderr(contains("unapply with the old configuration"));

    assert!(!fixture.temp.path().join("external-b").exists());
    assert_eq!(fs::read(&old_target).unwrap(), b"keep deployed edit\n");
    assert_eq!(
        fs::read(fixture.source.join("ov/etc/file")).unwrap(),
        b"keep source edit\n"
    );
    assert_eq!(fs::read(&state_path).unwrap(), state);
    assert_eq!(fs::read(fixture.source.join(".git/index")).unwrap(), index);
}
