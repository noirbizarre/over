use std::fs;
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
}

impl Fixture {
    fn new(config: &str) -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        write(
            &source,
            "ov/over.toml",
            // Git commands use the native home directory on Windows; an absolute
            // fixture target keeps them isolated without relying on HOME overrides.
            &format!(
                "target = {:?}\n{config}",
                home.join("target").to_string_lossy()
            ),
        );
        Self { temp, source, home }
    }

    fn command(&self, command: &str) -> Command {
        let mut cmd = Command::cargo_bin("over").unwrap();
        cmd.env("XDG_STATE_HOME", self.temp.path().join("state"))
            .arg("--home")
            .arg(&self.source)
            .arg(command);
        cmd
    }

    fn apply(&self, name: &str) {
        self.command("apply")
            .args([name, "--no-prompt", "--target"])
            .arg(&self.home)
            .assert()
            .success();
    }

    fn add(&self, path: &Path) -> Command {
        let mut cmd = self.command("add");
        cmd.args(["--overlay", "ov", "--target"])
            .arg(&self.home)
            .arg(path);
        cmd
    }
}

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn commit_all(repo: &git2::Repository) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
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

#[test]
fn root_and_nested_ignores_filter_before_parsing_without_leaking_to_siblings() {
    let fixture = Fixture::new("");
    write(
        &fixture.source,
        ".overignore",
        "*.pem\n*.broken.link.toml\n*.broken.partial.yaml\n",
    );
    write(&fixture.source, "ov/.overignore", "!keep.pem\n/local\n");
    write(&fixture.source, "ov/deep/.overignore", "keep.pem\n");
    write(&fixture.source, "other/over.toml", "target = '~/other'\n");
    for overlay in ["ov", "other"] {
        for file in [
            "keep.pem",
            "drop.pem",
            "local",
            "file",
            "bad.broken.link.toml",
            "bad.broken.partial.yaml",
        ] {
            write(
                &fixture.source,
                &format!("{overlay}/{file}"),
                "not valid config [",
            );
        }
    }
    write(&fixture.source, "ov/deep/keep.pem", "secret");
    // An ignored higher-priority sidecar must not mask the selected YAML declaration.
    write(
        &fixture.source,
        "ov/.overignore",
        "!keep.pem\n/local\nlink.link.toml\n",
    );
    write(&fixture.source, "ov/link.link.toml", "not valid [");
    let referent = fixture.temp.path().join("nonexistent/selected");
    write(
        &fixture.source,
        "ov/link.link.yaml",
        &format!("target: {}\n", serde_json::to_string(&referent).unwrap()),
    );

    fixture.apply("ov");
    fixture.apply("other");
    let target = fixture.home.join("target");
    assert!(target.join("keep.pem").is_symlink());
    assert!(!target.join("drop.pem").exists());
    assert!(!target.join("local").exists());
    assert!(!target.join("deep/keep.pem").exists());
    assert!(target.join("file").is_symlink());
    assert_eq!(fs::read_link(target.join("link")).unwrap(), referent);
    for metadata in [
        "over.toml",
        ".overignore",
        "deep/.overignore",
        "bad.broken.link.toml",
        "bad.broken.partial.yaml",
    ] {
        assert!(
            target.join(metadata).symlink_metadata().is_err(),
            "{metadata}"
        );
    }
    assert!(!fixture.home.join("other/keep.pem").exists());
    assert!(fixture.home.join("other/local").is_symlink());
    fixture.command("lint").assert().success();
}

#[test]
fn newly_ignored_symlinks_remain_unmanaged_and_host_exclusions_are_removed() {
    let fixture = Fixture::new("");
    git2::Repository::init(&fixture.home).unwrap();
    write(&fixture.source, "ov/secret", "preserve");
    fixture.apply("ov");
    write(&fixture.source, ".overignore", "secret\n");
    fixture.apply("ov");
    let target = fixture.home.join("target/secret");
    assert!(target.is_symlink());
    let exclude = fs::read_to_string(fixture.home.join(".git/info/exclude")).unwrap();
    assert!(!exclude.contains("/target/secret"));
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("secret").not());
    fixture
        .command("unapply")
        .args(["ov", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert!(target.is_symlink());
    assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
}

#[rstest]
#[case("")]
#[case("mode = 'checkout'\n")]
fn independently_versioned_overlays_never_deploy_git_administration(#[case] config: &str) {
    let fixture = Fixture::new(config);
    write(&fixture.source, "ov/file", "managed content\n");
    let repo = git2::Repository::init(fixture.source.join("ov")).unwrap();
    commit_all(&repo);
    // Git administration is not configuration input, even when it contains
    // apparent Over declarations or broken ignore syntax.
    write(&fixture.source, "ov/.git/.overignore", "[\n");
    write(&fixture.source, "ov/.git/bad.link.toml", "invalid [");
    write(&fixture.source, "ov/.git/bad.partial.toml", "invalid [");
    fixture.apply("ov");
    let target = fixture.home.join("target");
    assert_eq!(
        fs::read_to_string(target.join("file")).unwrap(),
        "managed content\n"
    );
    assert!(target.join(".git").symlink_metadata().is_err());
    assert!(repo.path().join("HEAD").is_file());
}

#[test]
fn git_worktree_pointers_are_neither_deployed_nor_ingested() {
    let fixture = Fixture::new("");
    write(
        &fixture.source,
        "ov/nested/.git",
        "gitdir: /external/admin\n",
    );
    write(&fixture.source, "ov/nested/file", "managed content");
    fixture.apply("ov");
    assert!(
        fixture
            .home
            .join("target/nested/.git")
            .symlink_metadata()
            .is_err()
    );
    write(
        &fixture.home,
        "target/import/.git",
        "gitdir: /other/admin\n",
    );
    write(&fixture.home, "target/import/file", "import content");
    fixture
        .add(&fixture.home.join("target/import/.git"))
        .assert()
        .failure()
        .stderr(contains("ignored").or(contains("control metadata")));
    fixture
        .add(&fixture.home.join("target/import"))
        .assert()
        .success();
    assert!(fixture.source.join("ov/import/file").is_file());
    assert!(!fixture.source.join("ov/import/.git").exists());
    assert_eq!(
        fs::read_to_string(fixture.home.join("target/import/.git")).unwrap(),
        "gitdir: /other/admin\n"
    );
}

#[test]
fn directory_links_cannot_expose_nested_git_administration() {
    let fixture = Fixture::new("[[rules]]\npath = 'app'\nmode = 'symlink-directory'\n");
    write(
        &fixture.source,
        "ov/app/.git/config",
        "live repository configuration",
    );
    fixture
        .command("apply")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("directory symlink").and(contains(".git")));
    assert!(fixture.home.join("target/app").symlink_metadata().is_err());
}

#[test]
fn reverse_add_skips_ignored_children_and_rejects_an_explicit_ignored_selection() {
    let fixture = Fixture::new("");
    write(&fixture.source, ".overignore", "*.pem\n");
    write(&fixture.home, "target/app/keep", "selected");
    write(&fixture.home, "target/app/secret.pem", "preserve");
    fixture
        .add(&fixture.home.join("target/app/secret.pem"))
        .assert()
        .failure()
        .stderr(contains("ignored"));
    fixture
        .add(&fixture.home.join("target/app"))
        .assert()
        .success();
    assert!(fixture.home.join("target/app/keep").is_symlink());
    assert!(!fixture.home.join("target/app/secret.pem").is_symlink());
    assert!(!fixture.source.join("ov/app/secret.pem").exists());
}

#[test]
fn atomic_directory_links_reject_ignored_descendants_before_apply_or_reverse_add() {
    let fixture = Fixture::new("[[rules]]\npath = 'app'\nmode = 'symlink-directory'\n");
    write(&fixture.source, ".overignore", "*.pem\n");
    write(&fixture.source, "ov/app/secret.pem", "source secret");
    write(&fixture.home, "target/app/secret.pem", "target secret");
    fixture
        .command("apply")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("directory symlink").and(contains("secret.pem")));
    fixture
        .add(&fixture.home.join("target/app"))
        .assert()
        .failure()
        .stderr(contains("directory symlink"));
    assert_eq!(
        fs::read_to_string(fixture.home.join("target/app/secret.pem")).unwrap(),
        "target secret"
    );
    assert_eq!(
        fs::read_to_string(fixture.source.join("ov/app/secret.pem")).unwrap(),
        "source secret"
    );
}

#[rstest]
#[case("mode = 'checkout'\n", "app")]
#[case("[[rules]]\npath = 'app'\nmode = 'checkout'\n", "app")]
#[case(
    "mode = 'checkout'\n[[rules]]\npath = 'app'\ntarget = '~/target/routed'\n",
    "routed"
)]
fn checkout_workflows_preserve_newly_ignored_files_and_staged_changes(
    #[case] config: &str,
    #[case] destination: &str,
) {
    let fixture = Fixture::new(config);
    write(&fixture.source, "ov/app/file", "original");
    write(&fixture.source, "ov/app/secret", "original secret");
    let repo = git2::Repository::init(&fixture.source).unwrap();
    repo.config().unwrap().set_str("user.name", "Test").unwrap();
    repo.config()
        .unwrap()
        .set_str("user.email", "test@example.com")
        .unwrap();
    commit_all(&repo);
    fixture.apply("ov");
    let target = fixture.home.join("target").join(destination);
    write(&target, "secret", "dirty ignored target");
    write(&target, "extra.secret", "ignored addition");
    write(&fixture.source, ".overignore", "secret\n*.secret\n");
    let mut index = repo.index().unwrap();
    let secret = index.get_path(Path::new("ov/app/secret"), 0).unwrap();
    index
        .add_frombuffer(&secret, b"ignored staged content")
        .unwrap();
    index.write().unwrap();
    let ignored_oid = repo
        .index()
        .unwrap()
        .get_path(Path::new("ov/app/secret"), 0)
        .unwrap()
        .id;

    fixture.apply("ov");
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("secret").not())
        .stdout(contains("applied"));
    fixture
        .command("diff")
        .args(["ov", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("secret").not());
    write(&target, "file", "selected content");
    fixture.add(&fixture.home.join("target")).assert().success();
    fixture
        .command("commit")
        .args(["ov", "--no-prompt", "--message", "selected", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    let head = repo.head().unwrap().peel_to_tree().unwrap();
    assert_eq!(
        repo.find_blob(head.get_path(Path::new("ov/app/secret")).unwrap().id())
            .unwrap()
            .content(),
        b"original secret"
    );
    assert_eq!(
        repo.find_blob(head.get_path(Path::new("ov/app/file")).unwrap().id())
            .unwrap()
            .content(),
        b"selected content"
    );
    assert_eq!(
        repo.index()
            .unwrap()
            .get_path(Path::new("ov/app/secret"), 0)
            .unwrap()
            .id,
        ignored_oid
    );
    assert!(head.get_path(Path::new("ov/app/extra.secret")).is_err());
    fixture
        .command("sync")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    fixture
        .command("unapply")
        .args(["ov", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(target.join("secret")).unwrap(),
        "dirty ignored target"
    );
    assert_eq!(
        fs::read_to_string(target.join("extra.secret")).unwrap(),
        "ignored addition"
    );
    assert!(!target.join("file").exists());
}

#[test]
fn checkout_initial_projection_never_materializes_ignored_tracked_files_or_metadata() {
    let fixture = Fixture::new("mode = 'checkout'\n");
    write(
        &fixture.source,
        ".overignore",
        "*.pem\n!over.toml\n!.overignore\n",
    );
    write(&fixture.source, "ov/deep/.overignore", "!keep.pem\n");
    write(&fixture.source, "ov/deep/keep.pem", "selected");
    write(&fixture.source, "ov/deep/drop.pem", "secret");
    let repo = git2::Repository::init(&fixture.source).unwrap();
    commit_all(&repo);
    fixture.apply("ov");
    let target = fixture.home.join("target");
    assert_eq!(
        fs::read_to_string(target.join("deep/keep.pem")).unwrap(),
        "selected"
    );
    for path in ["over.toml", "deep/.overignore", "deep/drop.pem"] {
        assert!(!target.join(path).exists(), "{path}");
    }
}

#[test]
fn changing_to_an_atomic_symlink_cannot_discard_ignored_target_only_content() {
    let fixture = Fixture::new("[[rules]]\npath = 'app'\nmode = 'checkout'\n");
    write(&fixture.source, "ov/app/file", "selected");
    let repo = git2::Repository::init(&fixture.source).unwrap();
    commit_all(&repo);
    fixture.apply("ov");
    write(&fixture.home, "target/app/secret.pem", "preserve");
    write(&fixture.source, ".overignore", "*.pem\n");
    write(
        &fixture.source,
        "ov/over.toml",
        "target = '~/target'\n[[rules]]\npath = 'app'\nmode = 'symlink-directory'\n",
    );
    fixture
        .command("apply")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stderr(contains("secret.pem"));
    assert_eq!(
        fs::read_to_string(fixture.home.join("target/app/secret.pem")).unwrap(),
        "preserve"
    );
    assert!(!fixture.home.join("target/app").is_symlink());
}

#[test]
fn migrating_a_legacy_symlink_directory_to_checkout_preserves_newly_ignored_links() {
    let fixture = Fixture::new("");
    write(&fixture.source, "ov/app/file", "selected");
    write(&fixture.source, "ov/app/secret.pem", "preserve");
    let repo = git2::Repository::init(&fixture.source).unwrap();
    commit_all(&repo);
    fixture.apply("ov");
    write(&fixture.source, ".overignore", "*.pem\n");
    write(
        &fixture.source,
        "ov/over.toml",
        "target = '~/target'\n[[rules]]\npath = 'app'\nmode = 'checkout'\n",
    );
    let mut command = fixture.command("apply");
    command
        .args(["ov", "--verbose", "--no-prompt", "--target"])
        .arg(&fixture.home);
    let output = command.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("ignored") || text.contains("unmanaged"),
        "{text}"
    );
    assert!(fixture.home.join("target/app/secret.pem").is_symlink());
    assert_eq!(
        fs::read_to_string(fixture.home.join("target/app/secret.pem")).unwrap(),
        "preserve"
    );
}

#[test]
fn removing_an_ignore_rule_resumes_conflict_checks_for_preserved_content() {
    let fixture = Fixture::new("mode = 'checkout'\n");
    write(&fixture.source, ".overignore", "secret\n");
    write(&fixture.source, "ov/secret", "source secret");
    let repo = git2::Repository::init(&fixture.source).unwrap();
    commit_all(&repo);
    fixture.apply("ov");
    write(&fixture.home, "target/secret", "preserved target content");
    write(&fixture.source, ".overignore", "");
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .code(1)
        .stdout(contains("secret"));
    fixture
        .command("sync")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .failure()
        .stdout(contains("blocked"));
    assert_eq!(
        fs::read_to_string(fixture.home.join("target/secret")).unwrap(),
        "preserved target content"
    );
}

#[test]
fn git_add_and_status_respect_ignores_without_hiding_unmanaged_host_files() {
    let fixture = Fixture::new("");
    write(&fixture.source, ".overignore", "*.pem\n");
    write(&fixture.home, "target/app/keep", "selected");
    write(&fixture.home, "target/app/secret.pem", "preserve");
    let target = fixture.home.join("target");
    let repo = git2::Repository::init(&target).unwrap();
    repo.config()
        .unwrap()
        .set_str("over.overlay", "ov")
        .unwrap();
    fixture
        .command("git")
        .args(["add", "app"])
        .env("HOME", &fixture.home)
        .current_dir(&target)
        .assert()
        .success();
    assert!(target.join("app/keep").is_symlink());
    assert!(!target.join("app/secret.pem").is_symlink());
    assert!(repo.status_should_ignore(Path::new("app/keep")).unwrap());
    assert!(
        !repo
            .status_should_ignore(Path::new("app/secret.pem"))
            .unwrap()
    );
    // Both the old installed-link scan and raw unapplied-source scan must filter.
    write(&fixture.source, ".overignore", "*.pem\nkeep\n");
    write(&fixture.source, "ov/app/other.pem", "ignored source");
    fixture
        .command("git")
        .arg("status")
        .env("HOME", &fixture.home)
        .current_dir(&target)
        .assert()
        .success()
        .stdout(contains("No overlay-managed files found"));
}

#[rstest]
#[case("")]
#[case("mode = 'checkout'\n")]
fn a_mapped_start_cannot_bypass_an_ignored_parent_directory(#[case] materialization: &str) {
    let fixture = Fixture::new(&format!(
        "{materialization}[[rules]]\npath = 'parent/app'\ntarget = '~/target/routed'\n"
    ));
    write(&fixture.source, ".overignore", "parent/\n");
    write(&fixture.source, "ov/parent/app/.overignore", "!file\n");
    write(&fixture.source, "ov/parent/app/file", "ignored");
    let repo = git2::Repository::init(&fixture.source).unwrap();
    commit_all(&repo);
    fixture.apply("ov");
    assert!(!fixture.home.join("target/routed").exists());
    assert!(!fixture.home.join("target/parent").exists());
}

#[test]
fn descriptor_less_dependencies_inherit_root_rules_and_keep_their_own_negations() {
    let fixture = Fixture::new("uses = ['base']\n");
    write(
        &fixture.source,
        "over.toml",
        "overlays = [{ path = 'base' }, { path = 'ov' }]\n",
    );
    write(&fixture.source, ".overignore", "*.pem\n");
    write(&fixture.source, "base/base-kept", "selected dependency");
    write(&fixture.source, "base/keep.pem", "ignored dependency");
    write(&fixture.source, "ov/.overignore", "!keep.pem\n");
    write(&fixture.source, "ov/keep.pem", "selected overlay");
    fixture.apply("ov");
    assert!(fixture.home.join("base-kept").is_symlink());
    assert!(!fixture.home.join("keep.pem").exists());
    assert!(fixture.home.join("target/keep.pem").is_symlink());
}

#[test]
fn source_only_commits_to_ignored_paths_do_not_make_a_checkout_behind() {
    let fixture = Fixture::new("mode = 'checkout'\n");
    write(&fixture.source, "ov/file", "selected");
    write(&fixture.source, "ov/secret", "original");
    let repo = git2::Repository::init(&fixture.source).unwrap();
    commit_all(&repo);
    fixture.apply("ov");
    write(&fixture.source, ".overignore", "secret\n");
    write(&fixture.source, "ov/secret", "new source secret");
    commit_all(&repo);
    fixture
        .command("status")
        .args(["ov", "--verbose", "--target"])
        .arg(&fixture.home)
        .assert()
        .success()
        .stdout(contains("applied"))
        .stdout(contains("0 behind"));
    fixture
        .command("sync")
        .args(["ov", "--no-prompt", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(fixture.home.join("target/secret")).unwrap(),
        "original"
    );
}

#[test]
fn unapply_clears_old_host_exclusions_when_every_file_is_newly_ignored() {
    let fixture = Fixture::new("");
    let repo = git2::Repository::init(&fixture.home).unwrap();
    write(&fixture.source, "ov/secret", "preserve");
    fixture.apply("ov");
    write(&fixture.source, ".overignore", "secret\n");
    // Unapply must work directly after a rule edit, without an intervening apply.
    fixture
        .command("unapply")
        .args(["ov", "--target"])
        .arg(&fixture.home)
        .assert()
        .success();
    assert!(fixture.home.join("target/secret").is_symlink());
    assert!(
        !repo
            .status_should_ignore(Path::new("target/secret"))
            .unwrap()
    );
}

#[rstest]
#[case("", "apply")]
#[case("", "unapply")]
#[case("mode = 'checkout'\n", "apply")]
#[case("mode = 'checkout'\n", "unapply")]
fn newly_ignored_files_at_the_host_repository_root_lose_their_exclusions(
    #[case] config: &str,
    #[case] operation: &str,
) {
    let fixture = Fixture::new(config);
    write(&fixture.source, ".overignore", ".git/\n");
    write(&fixture.source, "ov/secret", "preserve");
    let source = git2::Repository::init(&fixture.source).unwrap();
    commit_all(&source);
    let target = fixture.home.join("target");
    fs::create_dir_all(&target).unwrap();
    let host = git2::Repository::init(&target).unwrap();
    fixture.apply("ov");
    assert!(host.status_should_ignore(Path::new("secret")).unwrap());
    write(&fixture.source, ".overignore", ".git/\nsecret\n");
    let mut command = fixture.command(operation);
    command.args(["ov", "--target"]).arg(&fixture.home);
    if operation == "apply" {
        command.arg("--no-prompt");
    }
    command.assert().success();
    assert_eq!(
        fs::read_to_string(target.join("secret")).unwrap(),
        "preserve"
    );
    assert!(!host.status_should_ignore(Path::new("secret")).unwrap());
}

#[rstest]
#[case(false)]
#[case(true)]
fn git_add_excludes_newly_staged_checkout_files_from_the_host_repository(
    #[case] with_ignore: bool,
) {
    let fixture = Fixture::new("exclude = ['.git']\nmode = 'checkout'\n");
    write(&fixture.source, "ov/file", "original");
    if with_ignore {
        write(&fixture.source, ".overignore", "*.pem\n");
    }
    let source = git2::Repository::init(&fixture.source).unwrap();
    commit_all(&source);
    let target = fixture.home.join("target");
    fs::create_dir_all(&target).unwrap();
    let host = git2::Repository::init(&target).unwrap();
    host.config()
        .unwrap()
        .set_str("over.overlay", "ov")
        .unwrap();
    fixture.apply("ov");
    write(&target, "new", "new checkout content");
    write(&target, "secret.pem", "unmanaged host content");
    fixture
        .command("git")
        .args(["add", "new"])
        .env("HOME", &fixture.home)
        .current_dir(&target)
        .assert()
        .success();
    // The subprocess updated disk; libgit2 may retain this repository's old index.
    let mut index = source.index().unwrap();
    index.read(true).unwrap();
    assert!(index.get_path(Path::new("ov/new"), 0).is_some());
    assert!(
        source
            .head()
            .unwrap()
            .peel_to_tree()
            .unwrap()
            .get_path(Path::new("ov/new"))
            .is_err()
    );
    assert!(!target.join("new").is_symlink());
    assert!(host.status_should_ignore(Path::new("new")).unwrap());
    assert!(!host.status_should_ignore(Path::new("secret.pem")).unwrap());
}
