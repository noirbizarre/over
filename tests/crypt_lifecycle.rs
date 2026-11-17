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
fn encrypted_diffs_distinguish_staged_working_and_historical_content_without_caching() {
    let fixture = Fixture::new();
    fs::write(fixture.path("protected/token"), b"staged sentinel\n").unwrap();
    fixture.git(&["add", "protected/token"]);
    fs::write(fixture.path("protected/token"), b"working sentinel\n").unwrap();
    for (args, expected, excluded) in [
        (
            vec!["diff", "--cached"],
            "+staged sentinel",
            "+working sentinel",
        ),
        (vec!["diff"], "+working sentinel", "-lifecycle sentinel"),
    ] {
        let output = fixture.command("git").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let patch = String::from_utf8(output.stdout).unwrap();
        assert!(patch.contains(expected), "{patch}");
        assert!(!patch.contains(excluded), "{patch}");
    }
    fixture.git(&["add", "protected/token"]);
    fixture.git(&["commit", "-qm", "change protected content"]);
    let compare = || {
        fixture
            .command("git")
            .args(["diff", "HEAD~", "HEAD"])
            .output()
            .unwrap()
    };
    assert!(
        String::from_utf8(compare().stdout)
            .unwrap()
            .contains("+working sentinel")
    );
    fixture.success(&["lock"]);
    let output = compare();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output
            .stdout
            .windows(b"working sentinel".len())
            .any(|bytes| bytes == b"working sentinel")
    );
    assert!(!fixture.path(".git/refs/notes/textconv").exists());
    assert!(fixture.unlock().status.success());
    assert!(compare().status.success());
}

#[test]
fn textconv_authenticates_both_domains_before_writing_any_plaintext() {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(fixture.root.path()).unwrap();
    let key = over::crypt::access::local_key(&repo).unwrap();
    for domain in [0, 1] {
        let mut ciphertext = key.encrypt(b"exact\0secret\xff\n", domain).unwrap();
        let path = fixture.path("-quoted ' input");
        fs::write(&path, &ciphertext).unwrap();
        let output = fixture
            .command(cargo_bin("over"))
            // Helpers must use Git's context even when Over home points elsewhere.
            .args([
                "--home",
                fixture.identity.path().to_str().unwrap(),
                "crypt",
                "textconv",
                "--",
                path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"exact\0secret\xff\n");
        *ciphertext.last_mut().unwrap() ^= 1;
        fs::write(&path, ciphertext).unwrap();
        let output = fixture.over(&["crypt", "textconv", "--", path.to_str().unwrap()]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn locked_textconv_preserves_both_domains_and_rejects_an_unusable_installed_key() {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(fixture.root.path()).unwrap();
    let key = over::crypt::access::local_key(&repo).unwrap();
    fixture.success(&["lock"]);
    for domain in [0, 1] {
        let ciphertext = key.encrypt(b"opaque\0sentinel\xff\n", domain).unwrap();
        let path = fixture.path("textconv-input");
        fs::write(&path, &ciphertext).unwrap();
        let output = fixture.over(&["crypt", "textconv", "--", path.to_str().unwrap()]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, ciphertext);
    }
    assert!(fixture.unlock().status.success());
    fs::write(fixture.key(), b"invalid installed key").unwrap();
    let output = fixture.over(&[
        "crypt",
        "textconv",
        "--",
        fixture.path("textconv-input").to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn unlock_materializes_the_index_without_touching_untracked_files_or_unrelated_edits() {
    let fixture = Fixture::new();
    fs::write(fixture.path("unrelated"), b"committed\n").unwrap();
    fixture.git(&["add", "unrelated"]);
    fixture.git(&["commit", "-qm", "unrelated baseline"]);
    fs::write(fixture.path("protected/token"), b"staged protected\n").unwrap();
    fixture.git(&["add", "protected/token"]);
    fixture.success(&["lock"]);
    fs::write(fixture.path("unrelated"), b"staged unrelated\n").unwrap();
    fixture.git(&["add", "unrelated"]);
    fs::write(fixture.path("unrelated"), b"unstaged unrelated\n").unwrap();
    fs::write(
        fixture.path("protected/untracked"),
        b"untracked protected plaintext\n",
    )
    .unwrap();
    fs::write(fixture.path("untracked"), b"unrelated untracked\n").unwrap();
    let index = fs::read(fixture.path(".git/index")).unwrap();
    let head = fs::read(fixture.path(".git/HEAD")).unwrap();
    let output = fixture.unlock();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(fixture.path(".git/index")).unwrap(), index);
    assert_eq!(fs::read(fixture.path(".git/HEAD")).unwrap(), head);
    assert_eq!(
        fs::read(fixture.path("protected/token")).unwrap(),
        b"staged protected\n"
    );
    assert_eq!(
        fs::read(fixture.path("unrelated")).unwrap(),
        b"unstaged unrelated\n"
    );
    assert_eq!(
        fs::read(fixture.path("protected/untracked")).unwrap(),
        b"untracked protected plaintext\n"
    );
    assert_eq!(
        fs::read(fixture.path("untracked")).unwrap(),
        b"unrelated untracked\n"
    );
    fixture.success(&["unlock"]);
    assert_eq!(fs::read(fixture.path(".git/index")).unwrap(), index);
    // Lock still refuses to imply protection while untracked plaintext remains exposed.
    assert!(!fixture.over(&["lock"]).status.success());
    assert!(fixture.key().exists());
}

#[rstest]
#[case(false)]
#[case(true)]
fn unlock_resumes_an_authenticated_materialization_journal_through_both_cli_paths(
    #[case] explicit: bool,
) {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(fixture.root.path()).unwrap();
    let key = over::crypt::access::local_key(&repo).unwrap();
    let entry = repo
        .index()
        .unwrap()
        .get_path(std::path::Path::new("protected/token"), 0)
        .unwrap();
    let path = fixture.path("protected/token");
    let ciphertext = repo.find_blob(entry.id).unwrap().content().to_vec();
    let head = repo.head().unwrap().target().unwrap().to_string();
    // Model a stop after plaintext publication but before the durable journal is removed.
    let snapshot = serde_json::json!({
        "version": 1,
        "operation": { "unlock": { "repair": false } },
        "administrative": fs::canonicalize(repo.commondir()).unwrap(),
        "committed": null,
        "base_head": head,
        "files": std::collections::BTreeMap::from([(path.clone(), Some(ciphertext))]),
        "expected": std::collections::BTreeMap::from([(path, Some(b"lifecycle sentinel\n".to_vec()))]),
    });
    fs::write(
        fixture.path(".git/over-crypt/transaction.json"),
        key.encrypt(&serde_json::to_vec(&snapshot).unwrap(), 1)
            .unwrap(),
    )
    .unwrap();
    let index = fs::read(fixture.path(".git/index")).unwrap();
    let output = if explicit {
        fixture.unlock()
    } else {
        fixture.over(&["unlock"])
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(fixture.path("protected/token")).unwrap(),
        b"lifecycle sentinel\n"
    );
    assert_eq!(fs::read(fixture.path(".git/index")).unwrap(), index);
    assert_eq!(repo.head().unwrap().target().unwrap().to_string(), head);
    assert!(!fixture.path(".git/over-crypt/transaction.json").exists());
}

#[rstest]
#[case(false)]
#[case(true)]
fn locked_checkout_status_and_add_preserve_ciphertext_but_never_stage_plaintext(
    #[case] process: bool,
) {
    let fixture = Fixture::new();
    fixture.success(&["lock"]);
    if !process {
        fixture.git(&["config", "--unset", "filter.over.process"]);
    }
    let repo = git2::Repository::open(fixture.root.path()).unwrap();
    let entry = repo
        .index()
        .unwrap()
        .get_path(std::path::Path::new("protected/token"), 0)
        .unwrap();
    let ciphertext = repo.find_blob(entry.id).unwrap().content().to_vec();
    fs::remove_file(fixture.path("protected/token")).unwrap();
    fixture.git(&["checkout", "--", "protected/token"]);
    assert_eq!(
        fs::read(fixture.path("protected/token")).unwrap(),
        ciphertext
    );
    fixture.git(&["status", "--porcelain"]);
    fixture.git(&["diff"]);
    fixture.git(&["show", "--textconv", "HEAD:protected/token"]);
    fixture.git(&["add", "protected/token"]);
    assert_eq!(
        repo.index()
            .unwrap()
            .get_path(std::path::Path::new("protected/token"), 0)
            .unwrap()
            .id,
        entry.id
    );
    fs::write(
        fixture.path("protected/token"),
        b"must never enter the object database",
    )
    .unwrap();
    let output = fixture
        .command("git")
        .args(["add", "protected/token"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        repo.index()
            .unwrap()
            .get_path(std::path::Path::new("protected/token"), 0)
            .unwrap()
            .id,
        entry.id
    );
}

#[test]
fn existing_git_helper_configuration_works_until_unlock_refreshes_its_namespace() {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(fixture.root.path()).unwrap();
    let mut config = repo.config().unwrap();
    let names = [
        "filter.over.clean",
        "filter.over.smudge",
        "filter.over.process",
        "diff.over.textconv",
    ];
    for name in names {
        let command = config.get_string(name).unwrap();
        assert!(command.contains(" crypt "), "{command}");
        config
            .set_str(name, &command.replace(" crypt ", " git "))
            .unwrap();
    }
    fs::write(fixture.path("protected/token"), b"compatible edit\n").unwrap();
    fixture.git(&["add", "protected/token"]);
    let output = fixture
        .command("git")
        .args(["diff", "--cached"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("+compatible edit")
    );
    fixture.success(&["unlock"]);
    let config = repo.config().unwrap();
    for name in names {
        assert!(config.get_string(name).unwrap().contains(" crypt "));
    }
    let output = fixture
        .command("git")
        .args(["check-attr", "filter", "diff", "--", "protected/token"])
        .output()
        .unwrap();
    assert_eq!(
        output.stdout,
        b"protected/token: filter: over\nprotected/token: diff: over\n"
    );
}

#[test]
fn helper_help_groups_encryption_plumbing_under_crypt_instead_of_git() {
    let fixture = Fixture::new();
    let crypt = String::from_utf8(fixture.over(&["crypt", "--help"]).stdout).unwrap();
    let git = String::from_utf8(fixture.over(&["git", "--help"]).stdout).unwrap();
    for helper in ["filter", "filter-process", "textconv"] {
        assert!(crypt.contains(helper), "{crypt}");
        assert!(!git.contains(helper), "{git}");
    }
}

#[test]
fn unlock_repairs_diff_configuration_without_rewriting_tracked_policy() {
    let fixture = Fixture::new();
    let before = fs::read(fixture.path(".gitattributes")).unwrap();
    fixture.git(&["config", "--unset", "diff.over.textconv"]);
    fixture.git(&["config", "diff.over.cachetextconv", "true"]);
    assert!(fixture.unlock().status.success());
    let output = fixture
        .command("git")
        .args(["config", "--get", "diff.over.cachetextconv"])
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"false\n");
    assert_eq!(fs::read(fixture.path(".gitattributes")).unwrap(), before);
}

#[test]
fn removed_protection_clears_the_encrypted_diff_driver() {
    let fixture = Fixture::new();
    fixture.success(&["crypt", "remove", "protected/token", "--yes"]);
    let output = fixture
        .command("git")
        .args(["check-attr", "diff", "--", "protected/token"])
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"protected/token: diff: unspecified\n");
}

#[test]
fn linked_worktree_diffs_resolve_the_common_key_from_a_subdirectory() {
    let fixture = Fixture::new();
    let linked = fixture.identity.path().join("linked");
    fixture.git(&["worktree", "add", "-qb", "linked", linked.to_str().unwrap()]);
    fs::write(linked.join("protected/token"), b"linked edit\n").unwrap();
    let output = fixture
        .command("git")
        .current_dir(linked.join("protected"))
        .args(["diff", "--", "token"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("+linked edit")
    );
}

#[test]
fn worktree_cache_overrides_refuse_unlock_without_publishing_common_config() {
    let fixture = Fixture::new();
    fixture.git(&["config", "extensions.worktreeConfig", "true"]);
    fixture.git(&["config", "--worktree", "diff.over.cachetextconv", "true"]);
    let before = fs::read(fixture.path(".git/config")).unwrap();
    assert!(!fixture.unlock().status.success());
    assert_eq!(fs::read(fixture.path(".git/config")).unwrap(), before);
}

#[test]
fn unavailable_historical_generations_fail_instead_of_displaying_ciphertext() {
    let fixture = Fixture::new();
    let ciphertext = fixture
        .command("git")
        .args(["show", "HEAD:protected/token"])
        .output()
        .unwrap()
        .stdout;
    let path = fixture.identity.path().join("old ciphertext");
    fs::write(&path, ciphertext).unwrap();
    fixture.success(&["key", "rotate"]);
    let output = fixture.over(&["crypt", "textconv", path.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
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
        "--yes",
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
        "--yes",
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
