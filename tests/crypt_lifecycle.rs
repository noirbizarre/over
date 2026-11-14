use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;
use rstest::rstest;

struct Fixture {
    root: tempfile::TempDir,
    identity: tempfile::TempDir,
}

impl Fixture {
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command.current_dir(self.root.path());
        for name in [
            "OVER_HOME",
            "OVER_TARGET",
            "OVER_SECRET",
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
        ] {
            command.env_remove(name);
        }
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                self.identity.path().join("missing-global"),
            )
            .env("XDG_STATE_HOME", self.identity.path().join("state"))
            .env("XDG_CACHE_HOME", self.identity.path().join("cache"));
        command
    }

    fn over(&self, arguments: &[&str]) -> Output {
        self.command(cargo_bin("over"))
            .args(arguments)
            .output()
            .unwrap()
    }

    fn success(&self, arguments: &[&str]) {
        let output = self.over(arguments);
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git(&self, arguments: &[&str]) {
        let output = self.command("git").args(arguments).output().unwrap();
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn path(&self, path: &str) -> PathBuf {
        self.root.path().join(path)
    }
    fn key(&self) -> PathBuf {
        self.path(".git/over-crypt/key")
    }
    fn unlock(&self) -> Output {
        self.over(&[
            "unlock",
            "--identity",
            self.identity.path().join("key.age").to_str().unwrap(),
        ])
    }

    fn new() -> Self {
        let fixture = Self {
            root: tempfile::tempdir().unwrap(),
            identity: tempfile::tempdir().unwrap(),
        };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.name", "Lifecycle tests"]);
        fixture.git(&["config", "user.email", "lifecycle@example.invalid"]);
        fixture.success(&[
            "user",
            "add",
            "owner",
            "--identity-output",
            fixture.identity.path().join("key.age").to_str().unwrap(),
        ]);
        fs::create_dir(fixture.path("protected")).unwrap();
        fs::write(fixture.path("protected/token"), b"lifecycle sentinel\n").unwrap();
        fixture.success(&["crypt", "add", "protected/**"]);
        fixture.git(&["commit", "-qm", "protect files"]);
        fixture
    }
}

#[test]
fn crypto_only_lifecycle_does_not_require_a_checkout_state_directory_beneath_a_file() {
    let fixture = Fixture::new();
    let state = fixture.identity.path().join("obstructed-state");
    fs::create_dir(&state).unwrap();
    fs::write(state.join("over"), b"an unrelated file").unwrap();
    let locked = fixture
        .command(cargo_bin("over"))
        .env("XDG_STATE_HOME", &state)
        .arg("lock")
        .output()
        .unwrap();
    assert!(
        locked.status.success(),
        "{}",
        String::from_utf8_lossy(&locked.stderr)
    );
    assert!(!fixture.key().exists());
    let unlocked = fixture
        .command(cargo_bin("over"))
        .env("XDG_STATE_HOME", &state)
        .args(["unlock", "--identity"])
        .arg(fixture.identity.path().join("key.age"))
        .output()
        .unwrap();
    assert!(
        unlocked.status.success(),
        "{}",
        String::from_utf8_lossy(&unlocked.stderr)
    );
    assert!(fixture.key().exists());
    assert_eq!(fs::read(state.join("over")).unwrap(), b"an unrelated file");
}

#[test]
fn filter_configuration_failure_does_not_install_a_previously_locked_key() {
    let fixture = Fixture::new();
    fixture.success(&["lock"]);
    let before = fs::read(fixture.path("protected/token")).unwrap();
    fs::write(fixture.path(".git/config.lock"), b"another Git process").unwrap();
    let output = fixture.unlock();
    assert!(!output.status.success());
    assert!(
        !fixture.key().exists(),
        "failed unlock must preserve the locked access state"
    );
    assert_eq!(fs::read(fixture.path("protected/token")).unwrap(), before);
}

#[test]
fn a_dirty_locked_worktree_refuses_unlock_without_installing_a_key() {
    let fixture = Fixture::new();
    fixture.success(&["lock"]);
    fs::write(fixture.path("protected/token"), b"independent edit").unwrap();
    let output = fixture.unlock();
    assert!(!output.status.success());
    assert!(
        !fixture.key().exists(),
        "failed preflight must not grant persistent key access"
    );
    assert_eq!(
        fs::read(fixture.path("protected/token")).unwrap(),
        b"independent edit"
    );
}

#[rstest]
#[case("untracked")]
#[case("attributes")]
#[case("missing")]
fn unsafe_protected_states_refuse_lock_before_removing_access(#[case] state: &str) {
    let fixture = Fixture::new();
    let before = fs::read(fixture.key()).unwrap();
    match state {
        "untracked" => fs::write(fixture.path("protected/new"), b"untracked sentinel").unwrap(),
        "attributes" => fs::write(
            fixture.path(".git/info/attributes"),
            "protected/token -filter\n",
        )
        .unwrap(),
        "missing" => fs::remove_file(fixture.path("protected/token")).unwrap(),
        _ => unreachable!(),
    }
    let output = fixture.over(&["lock"]);
    assert!(
        !output.status.success(),
        "unsafe protected {state} must not be silently skipped"
    );
    assert_eq!(fs::read(fixture.key()).unwrap(), before);
    if state != "missing" {
        assert_eq!(
            fs::read(fixture.path("protected/token")).unwrap(),
            b"lifecycle sentinel\n"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_worktree_parent_is_rejected_without_mutating_its_external_target() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let external = tempfile::tempdir().unwrap();
    fs::rename(fixture.path("protected"), external.path().join("moved")).unwrap();
    symlink(external.path().join("moved"), fixture.path("protected")).unwrap();
    let output = fixture.over(&["lock"]);
    assert!(!output.status.success());
    assert!(fixture.key().exists());
    assert_eq!(
        fs::read(external.path().join("moved/token")).unwrap(),
        b"lifecycle sentinel\n"
    );
}

#[cfg(unix)]
#[test]
fn lock_and_unlock_preserve_executable_mode_and_native_file_metadata() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let file = fixture.path("protected/token");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o750)).unwrap();
    #[cfg(target_os = "linux")]
    xattr::set(&file, "user.over-lifecycle", b"metadata sentinel").unwrap();
    fixture.git(&["add", "protected/token"]);
    fixture.git(&["commit", "-qm", "executable"]);
    fixture.success(&["lock"]);
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o750
    );
    assert!(fixture.unlock().status.success());
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o750
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        xattr::get(&file, "user.over-lifecycle").unwrap().unwrap(),
        b"metadata sentinel"
    );
}

#[cfg(unix)]
#[rstest]
#[case("key-mode")]
#[case("directory-mode")]
#[case("key-link")]
#[case("key-symlink")]
fn unsafe_private_key_storage_is_rejected_before_any_refresh(#[case] state: &str) {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = Fixture::new();
    let key = fixture.key();
    match state {
        "key-mode" => fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap(),
        "directory-mode" => {
            fs::set_permissions(key.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap()
        }
        "key-link" => fs::hard_link(&key, fixture.identity.path().join("linked-key")).unwrap(),
        "key-symlink" => {
            let moved = fixture.identity.path().join("moved-key");
            fs::rename(&key, &moved).unwrap();
            symlink(moved, &key).unwrap();
        }
        _ => unreachable!(),
    }
    assert!(!fixture.over(&["lock"]).status.success());
    assert_eq!(
        fs::read(fixture.path("protected/token")).unwrap(),
        b"lifecycle sentinel\n"
    );
}

#[test]
fn concurrent_unlocks_serialize_and_leave_one_valid_persistent_key() {
    let fixture = Fixture::new();
    fixture.success(&["lock"]);
    let identity = fixture.identity.path().join("key.age");
    let mut children = Vec::new();
    for _ in 0..3 {
        children.push(
            fixture
                .command(cargo_bin("over"))
                .args(["unlock", "--identity", identity.to_str().unwrap()])
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert!(fixture.key().exists());
    assert_eq!(
        fs::read(fixture.path("protected/token")).unwrap(),
        b"lifecycle sentinel\n"
    );
}

#[rstest]
#[case("dirty")]
#[case("inaccessible")]
fn failed_linked_worktree_unlock_preserves_the_locked_primary(#[case] failure: &str) {
    let fixture = Fixture::new();
    let linked = fixture.identity.path().join("linked");
    fixture.git(&["worktree", "add", "-qb", "linked", linked.to_str().unwrap()]);
    fixture.success(&["lock"]);
    let before = fs::read(fixture.path("protected/token")).unwrap();
    if failure == "dirty" {
        fs::write(linked.join("protected/token"), b"linked edit").unwrap();
    } else {
        fs::rename(&linked, fixture.identity.path().join("moved-linked")).unwrap();
    }
    assert!(!fixture.unlock().status.success());
    assert!(!fixture.key().exists());
    assert_eq!(fs::read(fixture.path("protected/token")).unwrap(), before);
    if failure == "dirty" {
        assert_eq!(
            fs::read(linked.join("protected/token")).unwrap(),
            b"linked edit"
        );
    }
}

#[test]
fn an_accessible_administratively_locked_worktree_is_still_refreshed() {
    let fixture = Fixture::new();
    let linked = fixture.identity.path().join("linked");
    fixture.git(&["worktree", "add", "-qb", "linked", linked.to_str().unwrap()]);
    fixture.git(&["worktree", "lock", linked.to_str().unwrap()]);
    fixture.success(&["lock"]);
    over::crypt::format::inspect(&fs::read(linked.join("protected/token")).unwrap()).unwrap();
    assert!(fixture.unlock().status.success());
    assert_eq!(
        fs::read(linked.join("protected/token")).unwrap(),
        b"lifecycle sentinel\n"
    );
}

#[cfg(unix)]
#[test]
fn protected_executable_commits_and_restore_preserve_native_access_policy() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fs::create_dir(fixture.path("ov")).unwrap();
    fs::write(
        fixture.path("ov/over.toml"),
        "target = '~'\nmode = 'checkout'\n",
    )
    .unwrap();
    fs::write(fixture.path("ov/script"), b"original executable\n").unwrap();
    fs::set_permissions(fixture.path("ov/script"), fs::Permissions::from_mode(0o750)).unwrap();
    #[cfg(target_os = "linux")]
    xattr::set(
        fixture.path("ov/script"),
        "user.over-source",
        b"source metadata",
    )
    .unwrap();
    fixture.success(&["crypt", "add", "ov/script"]);
    fixture.git(&["add", "ov/over.toml"]);
    fixture.git(&["commit", "-qm", "protected executable"]);
    let target = fixture.identity.path().join("deployment");
    let root = fixture.root.path().to_str().unwrap();
    fixture.success(&[
        "--home",
        root,
        "apply",
        "ov",
        "--target",
        target.to_str().unwrap(),
        "--no-prompt",
    ]);
    let script = target.join("script");
    fs::write(&script, b"edited executable\n").unwrap();
    fixture.success(&[
        "--home",
        root,
        "add",
        "--overlay",
        "ov",
        "--target",
        target.to_str().unwrap(),
        script.to_str().unwrap(),
    ]);
    fixture.success(&[
        "--home",
        root,
        "commit",
        "--overlay",
        "ov",
        "--target",
        target.to_str().unwrap(),
        "--no-prompt",
        "--message",
        "edit executable",
    ]);
    assert_eq!(
        fs::metadata(fixture.path("ov/script"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        xattr::get(fixture.path("ov/script"), "user.over-source")
            .unwrap()
            .unwrap(),
        b"source metadata"
    );
    fixture.git(&["diff", "--exit-code", "--", "ov/script"]);
    fs::set_permissions(&script, fs::Permissions::from_mode(0o750)).unwrap();
    #[cfg(target_os = "linux")]
    xattr::set(&script, "user.over-target", b"target metadata").unwrap();
    fs::write(&script, b"discard this edit\n").unwrap();
    fixture.success(&[
        "--home",
        root,
        "restore",
        "--overlay",
        "ov",
        "--target",
        target.to_str().unwrap(),
        script.to_str().unwrap(),
    ]);
    assert_eq!(fs::read(&script).unwrap(), b"edited executable\n");
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o750
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        xattr::get(&script, "user.over-target").unwrap().unwrap(),
        b"target metadata"
    );
    fs::write(&script, b"cannot replace this\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o440)).unwrap();
    let refused = fixture.over(&[
        "--home",
        root,
        "restore",
        "--overlay",
        "ov",
        "--target",
        target.to_str().unwrap(),
        script.to_str().unwrap(),
    ]);
    assert!(!refused.status.success());
    assert_eq!(fs::read(&script).unwrap(), b"cannot replace this\n");
    // Restore writability so temporary-directory cleanup also works for ordinary users.
    fs::set_permissions(&script, fs::Permissions::from_mode(0o750)).unwrap();
}

#[test]
fn effective_worktree_filter_overrides_are_diagnosed_and_prevent_unsafe_lock() {
    let fixture = Fixture::new();
    fixture.git(&["config", "extensions.worktreeConfig", "true"]);
    fs::write(
        fixture.path(".git/config.worktree"),
        "[filter \"over\"]\n required = false\n process = cat\n",
    )
    .unwrap();
    let findings = fixture.over(&["crypt", "list"]);
    assert!(findings.status.success());
    assert!(
        String::from_utf8_lossy(&findings.stdout)
            .contains("required local Over filters missing/incorrect")
    );
    assert!(!fixture.over(&["lock"]).status.success());
    assert!(fixture.key().exists());
    assert_eq!(
        fs::read(fixture.path("protected/token")).unwrap(),
        b"lifecycle sentinel\n"
    );
}
