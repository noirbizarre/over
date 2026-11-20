//! Read-only consumption must work on writable sources too: permissions must not hide writes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use rstest::rstest;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn new(checkout: bool) -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::create_dir_all(source.join("app/nested")).unwrap();
        fs::write(source.join("over.yaml"), "host_overlay: app\n").unwrap();
        fs::write(source.join("app/over.yaml"), format!("mode: {}\ndata: {{hostname: container}}\nrules:\n  - {{path: '**/*.jinja', template: true, strip_extension: true}}\n", if checkout { "checkout" } else { "symlink" })).unwrap();
        fs::write(
            source.join("app/config.jinja"),
            "host={{ data.hostname }}\n",
        )
        .unwrap();
        fs::write(source.join("app/nested/plain"), "committed\n").unwrap();
        let repo = git2::Repository::init(&source).unwrap();
        for (key, value) in [
            ("user.name", "Read-only tests"),
            ("user.email", "readonly@example.invalid"),
            ("maintenance.auto", "false"),
            ("gc.auto", "0"),
        ] {
            repo.config().unwrap().set_str(key, value).unwrap();
        }
        let fixture = Self {
            temp,
            source,
            target,
        };
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "fixture"]);
        // An unavailable source remote proves consumption does not clone/pull or rewrite remotes.
        fixture.git(&["remote", "add", "origin", "/unavailable-source"]);
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("over").unwrap();
        command
            .current_dir(&self.source)
            .env_remove("OVER_HOME")
            .env_remove("OVER_HOOK_ACTIVE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("XDG_CACHE_HOME", self.temp.path().join("cache"))
            .env("TEST_TARGET", &self.target)
            .arg("--home")
            .arg(&self.source);
        command
    }

    fn apply(&self) -> Command {
        let mut command = self.command();
        command
            .args(["apply", "app", "--read-only", "--no-prompt", "--target"])
            .arg(&self.target);
        command
    }

    fn git(&self, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(&self.source)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn protect(&self) {
        self.command()
            .args(["user", "add", "owner", "--identity-output"])
            .arg(self.temp.path().join("identity.age"))
            .assert()
            .success();
        self.command()
            .args(["crypt", "add", "app/config.jinja", "app/nested/plain"])
            .assert()
            .success();
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", "protect"]);
        // Prove even an absent lock sentinel is not recreated by reads.
        fs::remove_file(self.source.join(".git/over-crypt/operation.lock")).unwrap();
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (String, Vec<u8>, u32)> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .map(|entry| {
            let metadata = entry.path().symlink_metadata().unwrap();
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode()
            };
            #[cfg(not(unix))]
            let mode = u32::from(metadata.permissions().readonly());
            let (kind, bytes) = if metadata.is_symlink() {
                (
                    "link",
                    fs::read_link(entry.path())
                        .unwrap()
                        .to_string_lossy()
                        .as_bytes()
                        .to_vec(),
                )
            } else if metadata.is_dir() {
                ("directory", Vec::new())
            } else {
                ("file", fs::read(entry.path()).unwrap())
            };
            (
                entry.path().strip_prefix(root).unwrap().to_path_buf(),
                (kind.to_owned(), bytes, mode),
            )
        })
        .collect()
}

#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
#[case(true, true)]
fn read_only_apply_publishes_without_source_writes_or_operational_state(
    #[case] checkout: bool,
    #[case] protected: bool,
) {
    let fixture = Fixture::new(checkout);
    if protected {
        fixture.protect();
    }
    let before = snapshot(&fixture.source);
    for _ in 0..2 {
        fixture.apply().assert().success();
        assert_eq!(snapshot(&fixture.source), before);
        assert!(!fixture.temp.path().join("state").exists());
        assert!(!fixture.temp.path().join("cache").exists());
        assert_eq!(
            fs::read(fixture.target.join("config")).unwrap(),
            b"host=container"
        );
        assert!(!fixture.target.join("config").is_symlink());
        assert_eq!(
            fs::read(fixture.target.join("nested/plain")).unwrap(),
            b"committed\n"
        );
        assert_eq!(fixture.target.join("nested/plain").is_symlink(), !checkout);
    }
}

#[test]
fn read_only_virtual_outputs_use_head_without_dirty_or_untracked_source_content() {
    let fixture = Fixture::new(true);
    fs::write(fixture.source.join("app/nested/plain"), "dirty\n").unwrap();
    fs::write(fixture.source.join("app/untracked"), "untracked\n").unwrap();
    let before = snapshot(&fixture.source);
    fixture.apply().assert().success();
    assert_eq!(
        fs::read(fixture.target.join("nested/plain")).unwrap(),
        b"committed\n"
    );
    assert!(!fixture.target.join("untracked").exists());
    assert_eq!(snapshot(&fixture.source), before);
}

#[test]
fn locked_checkouts_fail_before_publication_without_unlocking_or_creating_locks() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.command().arg("lock").assert().success();
    let before = snapshot(&fixture.source);
    let state = snapshot(&fixture.temp.path().join("state"));
    fixture
        .apply()
        .assert()
        .failure()
        .stderr(contains("unlock/repair it on the host"));
    assert_eq!(snapshot(&fixture.source), before);
    assert!(!fixture.target.exists());
    assert_eq!(snapshot(&fixture.temp.path().join("state")), state);
}

#[test]
fn read_only_links_refuse_unhydrated_ciphertext_even_when_a_key_is_installed() {
    let fixture = Fixture::new(false);
    fixture.protect();
    let repository = git2::Repository::open(&fixture.source).unwrap();
    let tree = repository.head().unwrap().peel_to_tree().unwrap();
    let blob = repository
        .find_blob(tree.get_path(Path::new("app/nested/plain")).unwrap().id())
        .unwrap();
    fs::write(fixture.source.join("app/nested/plain"), blob.content()).unwrap();
    let before = snapshot(&fixture.source);
    fixture
        .apply()
        .assert()
        .failure()
        .stderr(contains("unlock/repair its working files on the host"));
    assert_eq!(snapshot(&fixture.source), before);
    assert!(!fixture.target.exists());
}

#[cfg(unix)]
#[rstest]
#[case("missing")]
#[case("stale")]
#[case("permissions")]
#[case("pending")]
fn unavailable_unlock_state_fails_without_repair_or_target_publication(#[case] failure: &str) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(true);
    fixture.protect();
    let key = fixture.source.join(".git/over-crypt/key");
    match failure {
        "missing" => fs::remove_file(key).unwrap(),
        "stale" => {
            let other = Fixture::new(true);
            other.protect();
            fs::write(
                key,
                fs::read(other.source.join(".git/over-crypt/key")).unwrap(),
            )
            .unwrap();
        }
        "permissions" => fs::set_permissions(key, fs::Permissions::from_mode(0o644)).unwrap(),
        "pending" => fs::write(
            fixture.source.join(".git/over-crypt/transaction.json"),
            "{}",
        )
        .unwrap(),
        _ => unreachable!(),
    }
    let before = snapshot(&fixture.source);
    fixture
        .apply()
        .assert()
        .failure()
        .stderr(contains("unlock/repair it on the host"));
    assert_eq!(snapshot(&fixture.source), before);
    assert!(!fixture.target.exists());
}

#[rstest]
#[case(false)]
#[case(true)]
fn incomplete_or_missing_checkouts_report_how_to_mount_the_complete_source(#[case] missing: bool) {
    let fixture = Fixture::new(false);
    if missing {
        fs::remove_dir_all(&fixture.source).unwrap();
    } else {
        fs::remove_dir_all(fixture.source.join(".git")).unwrap();
    }
    fixture
        .command()
        .current_dir(fixture.temp.path())
        .args(["apply", "app", "--read-only", "--no-prompt"])
        .assert()
        .failure()
        .stderr(contains("mount the complete Git checkout"));
    assert!(!fixture.target.exists());
    assert!(!fixture.temp.path().join("state").exists());
}

#[test]
fn ephemeral_apply_refuses_to_change_an_existing_managed_checkout_even_with_force() {
    let fixture = Fixture::new(true);
    fixture
        .command()
        .args(["apply", "app", "--no-prompt", "--target"])
        .arg(&fixture.target)
        .assert()
        .success();
    let source = snapshot(&fixture.source);
    let state = snapshot(&fixture.temp.path().join("state"));
    fixture
        .apply()
        .arg("--force")
        .assert()
        .failure()
        .stderr(contains("overlaps a recorded checkout"));
    assert_eq!(snapshot(&fixture.source), source);
    assert_eq!(snapshot(&fixture.temp.path().join("state")), state);
}

#[test]
fn read_only_apply_never_uses_force_to_publish_into_source_storage() {
    let fixture = Fixture::new(true);
    let before = snapshot(&fixture.source);
    fixture
        .command()
        .args([
            "apply",
            "app",
            "--read-only",
            "--no-prompt",
            "--force",
            "--target",
        ])
        .arg(fixture.source.join("new-target"))
        .assert()
        .failure()
        .stderr(contains("protected source storage"));
    assert_eq!(snapshot(&fixture.source), before);
}

#[cfg(unix)]
#[test]
fn read_only_apply_runs_hooks_with_native_secret_snapshots_without_creating_a_lock() {
    let fixture = Fixture::new(true);
    fs::write(fixture.source.join("over.yaml"), "host_overlay: app\nhooks:\n  post-apply:\n    - command: 'printf %s \"$TOKEN\" > \"$TEST_TARGET/hook-secret\"'\n      env: {TOKEN: \"{{ secret('TOKEN') }}\"}\n").unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "hook"]);
    fixture.protect();
    fixture
        .command()
        .args(["secret", "add", "TOKEN"])
        .write_stdin("snapshot-secret")
        .assert()
        .success();
    fs::remove_file(fixture.source.join(".git/over-crypt/operation.lock")).unwrap();
    let before = snapshot(&fixture.source);
    fixture.apply().assert().success();
    assert_eq!(
        fs::read(fixture.target.join("hook-secret")).unwrap(),
        b"snapshot-secret"
    );
    assert_eq!(snapshot(&fixture.source), before);
}

#[test]
fn dry_run_read_only_apply_does_not_publish_targets_or_state() {
    let fixture = Fixture::new(true);
    let before = snapshot(&fixture.source);
    fixture.apply().arg("--dry-run").assert().success();
    assert_eq!(snapshot(&fixture.source), before);
    assert!(!fixture.target.exists());
    assert!(!fixture.temp.path().join("state").exists());
}

#[test]
fn read_only_apply_requires_explicit_overwrite_and_never_prunes_unrecorded_outputs() {
    let fixture = Fixture::new(true);
    fixture.apply().assert().success();
    fs::write(fixture.target.join("config"), "local edit").unwrap();
    let before = snapshot(&fixture.source);
    fixture
        .apply()
        .assert()
        .failure()
        .stderr(contains("differs"));
    assert_eq!(
        fs::read(fixture.target.join("config")).unwrap(),
        b"local edit"
    );
    fixture.apply().arg("--force").assert().success();
    assert_eq!(
        fs::read(fixture.target.join("config")).unwrap(),
        b"host=container"
    );
    assert_eq!(snapshot(&fixture.source), before);
    fs::remove_file(fixture.source.join("app/nested/plain")).unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "remove source leaf"]);
    fixture.apply().assert().success();
    assert_eq!(
        fs::read(fixture.target.join("nested/plain")).unwrap(),
        b"committed\n"
    );
}

#[cfg(unix)]
#[test]
fn read_only_apply_preserves_per_source_file_and_directory_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(true);
    fs::write(fixture.source.join("app/over.yaml"), "mode: checkout\nrules:\n  - {path: nested, permissions: '0700'}\n  - {path: nested/plain, permissions: '0600'}\n").unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "permissions"]);
    fixture.apply().assert().success();
    assert_eq!(
        fs::metadata(fixture.target.join("nested"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(fixture.target.join("nested/plain"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn read_only_apply_rejects_source_aliases_before_any_target_writes(
    #[case] hardlink: bool,
    #[values(false, true)] checkout: bool,
) {
    let fixture = Fixture::new(checkout);
    fs::create_dir_all(&fixture.target).unwrap();
    if hardlink {
        fs::create_dir(fixture.target.join("nested")).unwrap();
        fs::hard_link(
            fixture.source.join("app/nested/plain"),
            fixture.target.join("nested/plain"),
        )
        .unwrap();
    } else {
        std::os::unix::fs::symlink(
            fixture.source.join("app/nested"),
            fixture.target.join("nested"),
        )
        .unwrap();
    }
    let before = snapshot(&fixture.source);
    fixture.apply().arg("--force").assert().failure();
    assert!(!fixture.target.join("config").exists());
    assert_eq!(snapshot(&fixture.source), before);
}

#[cfg(unix)]
#[test]
fn read_only_apply_runs_declared_git_dependencies_and_install_hooks_normally() {
    let fixture = Fixture::new(false);
    let dependency = fixture.temp.path().join("dependency");
    let repo = git2::Repository::init(&dependency).unwrap();
    let tree = repo
        .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
        .unwrap();
    let signature = git2::Signature::now("Dependency", "dependency@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "dependency",
        &tree,
        &[],
    )
    .unwrap();
    fs::write(fixture.source.join("app/over.yaml"), format!("git:\n  dependency: '{}'\nhooks:\n  install: ['test -d \"$TEST_TARGET/dependency/.git\" && printf installed > \"$TEST_TARGET/hook\"']\n", dependency.display())).unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-qm", "dependency and hook"]);
    let before = snapshot(&fixture.source);
    fixture.apply().arg("--install").assert().success();
    assert!(fixture.target.join("dependency/.git").is_dir());
    assert_eq!(fs::read(fixture.target.join("hook")).unwrap(), b"installed");
    assert_eq!(snapshot(&fixture.source), before);
}

#[test]
fn read_only_apply_reconciles_partial_blocks_without_source_or_state_writes() {
    let fixture = Fixture::new(false);
    fs::write(
        fixture.source.join("app/settings.partial.yaml"),
        "target: '{{ env.TEST_TARGET }}/settings'\ncontent: managed\nmarker: settings\n",
    )
    .unwrap();
    fs::create_dir_all(&fixture.target).unwrap();
    fs::write(fixture.target.join("settings"), "foreign\n").unwrap();
    let before = snapshot(&fixture.source);
    for _ in 0..2 {
        fixture.apply().assert().success();
    }
    let content = fs::read_to_string(fixture.target.join("settings")).unwrap();
    assert!(content.starts_with("foreign\n"));
    assert!(content.contains("managed"));
    assert_eq!(snapshot(&fixture.source), before);
    assert!(!fixture.temp.path().join("state").exists());
}

#[rstest]
#[case(false)]
#[case(true)]
fn read_only_apply_preserves_target_git_protection_without_touching_source_git(
    #[case] protected: bool,
) {
    let fixture = Fixture::new(true);
    if protected {
        fixture.protect();
    }
    fs::create_dir_all(&fixture.target).unwrap();
    let target = git2::Repository::init(&fixture.target).unwrap();
    let source = snapshot(&fixture.source);
    fixture.apply().assert().success();
    assert!(target.is_path_ignored(Path::new("config")).unwrap());
    assert!(target.is_path_ignored(Path::new("nested/plain")).unwrap());
    assert!(!target.is_path_ignored(Path::new("unrelated")).unwrap());
    assert_eq!(snapshot(&fixture.source), source);
    assert!(!fixture.temp.path().join("state").exists());
}

#[test]
fn read_only_apply_refuses_plaintext_publication_into_a_tracked_target_file() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fs::create_dir_all(fixture.target.join("nested")).unwrap();
    fs::write(fixture.target.join("nested/plain"), "tracked user content").unwrap();
    let target = git2::Repository::init(&fixture.target).unwrap();
    let mut index = target.index().unwrap();
    index.add_path(Path::new("nested/plain")).unwrap();
    index.write().unwrap();
    let source = snapshot(&fixture.source);
    let before = snapshot(&fixture.target);
    fixture.apply().arg("--force").assert().failure();
    assert_eq!(snapshot(&fixture.target), before);
    assert_eq!(snapshot(&fixture.source), source);
}

#[test]
fn read_only_apply_refuses_target_exclusions_stored_inside_the_source_repository() {
    let fixture = Fixture::new(false);
    fs::create_dir_all(&fixture.target).unwrap();
    // A separate worktree path can conceal administration located inside the immutable source.
    let mut options = git2::RepositoryInitOptions::new();
    options.workdir_path(&fixture.target);
    git2::Repository::init_opts(fixture.source.join("target-gitdir"), &options).unwrap();
    let source = snapshot(&fixture.source);
    fixture
        .apply()
        .assert()
        .failure()
        .stderr(contains("shares source Git administration"));
    assert_eq!(snapshot(&fixture.source), source);
    assert!(!fixture.target.join("config").exists());
}
