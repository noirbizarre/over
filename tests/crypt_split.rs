//! CLI contracts for independently versioned access methods and native secrets.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use assert_cmd::cargo::cargo_bin;

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self::empty();
        fixture.enroll("alice");
        fixture
    }

    fn empty() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let fixture = Self { temp, root };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.name", "Split test"]);
        fixture.git(&["config", "user.email", "split@example.invalid"]);
        fixture.git(&["config", "commit.gpgSign", "false"]);
        fixture
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.root)
            .env_remove("OVER_HOME")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("XDG_CACHE_HOME", self.temp.path().join("cache"));
        command
    }

    fn git(&self, arguments: &[&str]) -> Vec<u8> {
        success(
            self.command(Path::new("git"))
                .args(arguments)
                .output()
                .unwrap(),
        )
        .stdout
    }

    fn over(&self, arguments: &[&str]) -> Output {
        self.command(&cargo_bin("over"))
            .args(["--home", self.root.to_str().unwrap()])
            .args(arguments)
            .output()
            .unwrap()
    }

    fn enroll(&self, name: &str) {
        let identity = self.temp.path().join(format!("{name}.age"));
        success(self.over(&[
            "user",
            "add",
            name,
            "--identity-output",
            identity.to_str().unwrap(),
        ]));
    }

    fn secret(&self, name: &str, value: &[u8], replace: bool) -> Output {
        let mut command = self.command(&cargo_bin("over"));
        command.args(["--home", self.root.to_str().unwrap(), "secret", "add", name]);
        if replace {
            command.arg("--replace");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(value).unwrap();
        child.wait_with_output().unwrap()
    }

    fn value(&self, name: &str) -> Vec<u8> {
        success(self.over(&["secret", "get", name])).stdout
    }

    fn changed(&self) -> Vec<u8> {
        self.git(&["diff", "--no-renames", "HEAD^", "HEAD", "--name-only"])
    }
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn access_and_secret_mutations_commit_only_their_independent_files() {
    let fixture = Fixture::new();
    assert!(!fixture.root.join(".over/crypt/access.json").exists());
    assert!(fixture.root.join(".over/users/alice/default").is_file());
    assert!(!fixture.root.join(".over/crypt").exists());
    assert_eq!(
        fs::read(fixture.root.join(".over/users/alice/user.json")).unwrap(),
        b"{\"version\":2}\n"
    );
    let header = fs::read(fixture.root.join(".over/crypt.json")).unwrap();
    let alice = fs::read(fixture.root.join(".over/users/alice/default")).unwrap();
    fs::write(fixture.root.join("unrelated"), b"staged work").unwrap();
    fixture.git(&["add", "unrelated"]);
    fixture.enroll("bob");
    assert_eq!(
        fixture.changed(),
        b".over/users/bob/default\n.over/users/bob/user.json\n"
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt.json")).unwrap(),
        header
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/users/alice/default")).unwrap(),
        alice
    );
    success(fixture.secret("A", b"first\0\xff", false));
    success(fixture.secret("B", b"second", false));
    let other = fs::read(fixture.root.join(".over/secrets/B")).unwrap();
    success(fixture.secret("A", b"updated\0\xff", true));
    assert_eq!(fixture.changed(), b".over/secrets/A\n");
    assert_eq!(fixture.value("A"), b"updated\0\xff");
    assert_eq!(
        fs::read(fixture.root.join(".over/secrets/B")).unwrap(),
        other
    );
    over::crypt::format::inspect(&other).unwrap();
    assert_eq!(
        fixture.git(&["diff", "--cached", "--name-only"]),
        b"unrelated\n"
    );
    success(fixture.over(&["user", "key", "detach", "bob", "default"]));
    assert!(fixture.root.join(".over/users/bob/user.json").is_file());
    success(fixture.over(&["user", "delete", "bob"]));
    assert_eq!(fixture.changed(), b".over/users/bob/user.json\n");
}

#[test]
fn independent_secret_changes_merge_without_rewriting_other_paths() {
    let fixture = Fixture::new();
    success(fixture.secret("A", b"base A", false));
    success(fixture.secret("B", b"base B", false));
    fixture.git(&["branch", "right"]);
    fixture.git(&["checkout", "-qb", "left"]);
    success(fixture.secret("A", b"left A", true));
    fixture.git(&["checkout", "-q", "right"]);
    success(fixture.secret("B", b"right B", true));
    fixture.git(&["merge", "--no-edit", "left"]);
    assert_eq!(fixture.value("A"), b"left A");
    assert_eq!(fixture.value("B"), b"right B");
}

#[test]
fn independent_user_enrollments_merge_without_changing_the_header() {
    let fixture = Fixture::new();
    let header = fs::read(fixture.root.join(".over/crypt.json")).unwrap();
    fixture.git(&["branch", "right"]);
    fixture.git(&["checkout", "-qb", "left"]);
    fixture.enroll("bob");
    fixture.git(&["checkout", "-q", "right"]);
    fixture.enroll("carol");
    fixture.git(&["merge", "--no-edit", "left"]);
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt.json")).unwrap(),
        header
    );
    let registry =
        over::crypt::access::load(&git2::Repository::open(&fixture.root).unwrap()).unwrap();
    assert_eq!(registry.users.len(), 3);
}

#[test]
fn rotation_commits_ciphertext_and_preserves_unrelated_staging() {
    let fixture = Fixture::new();
    success(fixture.secret("TOKEN", b"value\0\xff", false));
    let before = fs::read(fixture.root.join(".over/secrets/TOKEN")).unwrap();
    fs::write(fixture.root.join("unrelated"), b"pending").unwrap();
    fixture.git(&["add", "unrelated"]);
    success(fixture.over(&["key", "rotate"]));
    let after = fs::read(fixture.root.join(".over/secrets/TOKEN")).unwrap();
    assert_ne!(before, after);
    assert_eq!(fixture.value("TOKEN"), b"value\0\xff");
    assert_eq!(
        fixture.git(&["diff", "--cached", "--name-only"]),
        b"unrelated\n"
    );
    success(fixture.over(&["lock"]));
    assert!(!fixture.over(&["secret", "get", "TOKEN"]).status.success());
    success(fixture.over(&[
        "unlock",
        "--identity",
        fixture.temp.path().join("alice.age").to_str().unwrap(),
    ]));
    assert_eq!(fixture.value("TOKEN"), b"value\0\xff");
    assert_eq!(
        fs::read(fixture.root.join(".over/secrets/TOKEN")).unwrap(),
        after
    );
}

#[test]
fn corrupting_another_entry_does_not_prevent_reading_a_valid_secret() {
    let fixture = Fixture::new();
    success(fixture.secret("A", b"valid", false));
    success(fixture.secret("B", b"other", false));
    fs::write(fixture.root.join(".over/secrets/B"), b"corrupt").unwrap();
    assert_eq!(fixture.value("A"), b"valid");
    assert!(!fixture.over(&["secret", "get", "B"]).status.success());
}

#[test]
fn access_and_secret_migrations_are_triggered_independently() {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(&fixture.root).unwrap();
    let registry = over::crypt::access::load(&repo).unwrap();
    let key = over::crypt::access::local_key(&repo).unwrap();
    fs::remove_dir_all(fixture.root.join(".over/users")).unwrap();
    fs::remove_file(fixture.root.join(".over/crypt.json")).unwrap();
    fs::create_dir_all(fixture.root.join(".over/crypt")).unwrap();
    fs::write(
        fixture.root.join(".over/crypt/access.json"),
        serde_json::to_vec_pretty(&registry).unwrap(),
    )
    .unwrap();
    let values = std::collections::BTreeMap::from([("TOKEN", b"legacy\0\xff".to_vec())]);
    fs::write(
        fixture.root.join(".over/secrets"),
        key.encrypt(&serde_json::to_vec(&(values, 1u32)).unwrap(), 1)
            .unwrap(),
    )
    .unwrap();
    fixture.git(&["add", "-A"]);
    fixture.git(&["commit", "-qm", "historical layouts"]);
    let before = fixture.git(&["rev-parse", "HEAD"]);
    assert_eq!(fixture.value("TOKEN"), b"legacy\0\xff");
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), before);
    success(fixture.secret("OTHER", b"new", false));
    assert!(fixture.root.join(".over/crypt/access.json").is_file());
    assert!(fixture.root.join(".over/secrets/TOKEN").is_file());
    let secret = fs::read(fixture.root.join(".over/secrets/TOKEN")).unwrap();
    fixture.enroll("bob");
    assert!(!fixture.root.join(".over/crypt/access.json").exists());
    assert!(fixture.root.join(".over/users/bob/default").is_file());
    assert_eq!(
        fs::read(fixture.root.join(".over/secrets/TOKEN")).unwrap(),
        secret
    );
    assert_eq!(
        over::crypt::access::load(&repo).unwrap().users["alice"],
        registry.users["alice"]
    );
}

#[test]
fn failed_initial_signing_publishes_neither_access_nor_an_active_key() {
    let fixture = Fixture::empty();
    fixture.git(&["config", "commit.gpgSign", "true"]);
    fixture.git(&["config", "gpg.format", "openpgp"]);
    fixture.git(&["config", "gpg.program", "false"]);
    let output = fixture.over(&[
        "user",
        "add",
        "alice",
        "--identity-output",
        fixture.temp.path().join("alice.age").to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(!fixture.root.join(".over/crypt.json").exists());
    assert!(!fixture.root.join(".git/over-crypt/key").exists());
    assert!(!fixture.root.join(".git/over-crypt/mutation.json").exists());
    assert!(
        git2::Repository::open(&fixture.root)
            .unwrap()
            .head()
            .is_err()
    );
    fixture.git(&["config", "commit.gpgSign", "false"]);
    fixture.enroll("bob");
}

#[cfg(unix)]
fn hook(fixture: &Fixture, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    let path = fixture.root.join(".git/hooks").join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn first_enrollment_ref_failure_rolls_back_every_public_file_and_index() {
    let fixture = Fixture::empty();
    hook(
        &fixture,
        "reference-transaction",
        "if [ \"$1\" = prepared ]; then exit 1; fi",
    );
    let output = fixture.over(&[
        "user",
        "add",
        "alice",
        "--identity-output",
        fixture.temp.path().join("alice.age").to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(!fixture.root.join(".over").exists());
    assert!(!fixture.root.join(".git/index").exists());
    assert!(!fixture.root.join(".git/over-crypt/key").exists());
    assert!(!fixture.root.join(".git/over-crypt/mutation.json").exists());
    assert!(!fixture.root.join(".git/over-crypt/mutation-key").exists());
}

#[cfg(unix)]
#[test]
fn a_git_failure_after_ref_publication_finalizes_the_key_without_a_duplicate_commit() {
    let fixture = Fixture::empty();
    hook(
        &fixture,
        "reference-transaction",
        "if [ \"$1\" = committed ]; then kill -KILL \"$PPID\"; fi",
    );
    fixture.enroll("alice");
    assert_eq!(fixture.git(&["rev-list", "--count", "HEAD"]), b"1\n");
    assert!(fixture.root.join(".git/over-crypt/key").is_file());
    assert!(!fixture.root.join(".git/over-crypt/mutation.json").exists());
}

#[cfg(unix)]
#[test]
fn rejected_aggregate_migration_restores_the_original_file_shape() {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(&fixture.root).unwrap();
    let key = over::crypt::access::local_key(&repo).unwrap();
    let values = std::collections::BTreeMap::from([("TOKEN", b"legacy".to_vec())]);
    let original = key
        .encrypt(&serde_json::to_vec(&(values, 1u32)).unwrap(), 1)
        .unwrap();
    fs::write(fixture.root.join(".over/secrets"), &original).unwrap();
    fixture.git(&["add", ".over/secrets"]);
    fixture.git(&["commit", "-qm", "historical aggregate"]);
    let index = fs::read(fixture.root.join(".git/index")).unwrap();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    hook(
        &fixture,
        "reference-transaction",
        "if [ \"$1\" = prepared ]; then exit 1; fi",
    );
    assert!(!fixture.secret("OTHER", b"new", false).status.success());
    assert_eq!(
        fs::read(fixture.root.join(".over/secrets")).unwrap(),
        original
    );
    assert_eq!(fs::read(fixture.root.join(".git/index")).unwrap(), index);
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert!(!fixture.root.join(".git/over-crypt/mutation.json").exists());
}

#[test]
fn rotation_commits_only_the_initiating_branch_and_stages_linked_transitions() {
    let fixture = Fixture::new();
    success(fixture.secret("TOKEN", b"shared value", false));
    let sibling = fixture.temp.path().join("linked");
    fixture.git(&[
        "worktree",
        "add",
        "-qb",
        "linked",
        sibling.to_str().unwrap(),
    ]);
    let old = fixture.git(&["rev-parse", "linked"]);
    success(fixture.over(&["key", "rotate"]));
    assert_eq!(fixture.git(&["rev-parse", "linked"]), old);
    let staged = success(
        fixture
            .command(Path::new("git"))
            .arg("-C")
            .arg(&sibling)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap(),
    )
    .stdout;
    assert!(String::from_utf8_lossy(&staged).contains(".over/secrets/TOKEN"));
    let output = fixture
        .command(&cargo_bin("over"))
        .args([
            "--home",
            sibling.to_str().unwrap(),
            "secret",
            "get",
            "TOKEN",
        ])
        .output()
        .unwrap();
    assert_eq!(success(output).stdout, b"shared value");
}

#[test]
fn staged_protected_edits_block_rotation_without_committing_them() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("protected"), b"original").unwrap();
    success(fixture.over(&["crypt", "add", "protected"]));
    fixture.git(&["commit", "-qm", "protected file"]);
    fs::write(fixture.root.join("protected"), b"pending edit").unwrap();
    fixture.git(&["add", "protected"]);
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let staged = fs::read(fixture.root.join(".git/index")).unwrap();
    assert!(!fixture.over(&["key", "rotate"]).status.success());
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read(fixture.root.join(".git/index")).unwrap(), staged);
    assert_eq!(
        fs::read(fixture.root.join("protected")).unwrap(),
        b"pending edit"
    );
}

#[cfg(target_os = "linux")]
#[rstest::rstest]
#[case(false)]
#[case(true)]
fn interrupted_published_enrollment_or_rotation_finalizes_the_pending_key(#[case] rotating: bool) {
    let fixture = Fixture::empty();
    if rotating {
        fixture.enroll("alice");
        success(fixture.secret("TOKEN", b"retained", false));
    }
    // Kill only this fixture's Over parent after Git has made HEAD authoritative.
    hook(
        &fixture,
        "reference-transaction",
        "if [ \"$1\" = committed ]; then set -- $(ps -o ppid= -p \"$PPID\"); kill -KILL \"$1\"; fi",
    );
    let output = if rotating {
        fixture.over(&["key", "rotate"])
    } else {
        fixture.over(&[
            "user",
            "add",
            "alice",
            "--identity-output",
            fixture.temp.path().join("alice.age").to_str().unwrap(),
        ])
    };
    assert!(!output.status.success());
    assert!(fixture.root.join(".git/over-crypt/mutation.json").is_file());
    fs::remove_file(fixture.root.join(".git/hooks/reference-transaction")).unwrap();
    success(fixture.over(&["crypt", "recover"]));
    over::crypt::access::local_key(&git2::Repository::open(&fixture.root).unwrap()).unwrap();
    assert!(!fixture.root.join(".git/over-crypt/mutation.json").exists());
    assert!(!fixture.root.join(".git/over-crypt/mutation-key").exists());
    if rotating {
        assert_eq!(fixture.value("TOKEN"), b"retained");
    } else {
        assert_eq!(fixture.git(&["rev-list", "--count", "HEAD"]), b"1\n");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_preserves_unknown_children_added_after_an_interrupted_split() {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(&fixture.root).unwrap();
    let key = over::crypt::access::local_key(&repo).unwrap();
    let map = std::collections::BTreeMap::from([("TOKEN", b"old".to_vec())]);
    let original = key
        .encrypt(&serde_json::to_vec(&(map, 1u32)).unwrap(), 1)
        .unwrap();
    fs::write(fixture.root.join(".over/secrets"), &original).unwrap();
    fixture.git(&["add", ".over/secrets"]);
    fixture.git(&["commit", "-qm", "aggregate"]);
    hook(
        &fixture,
        "reference-transaction",
        "if [ \"$1\" = prepared ]; then set -- $(ps -o ppid= -p \"$PPID\"); kill -KILL \"$1\"; exit 1; fi",
    );
    assert!(!fixture.secret("OTHER", b"new", false).status.success());
    fs::remove_file(fixture.root.join(".git/hooks/reference-transaction")).unwrap();
    fs::write(
        fixture.root.join(".over/secrets/independent"),
        b"preserve this edit",
    )
    .unwrap();
    assert!(!fixture.over(&["crypt", "recover"]).status.success());
    assert_eq!(
        fs::read(fixture.root.join(".over/secrets/independent")).unwrap(),
        b"preserve this edit"
    );
    fs::remove_file(fixture.root.join(".over/secrets/independent")).unwrap();
    success(fixture.over(&["crypt", "recover"]));
    assert_eq!(
        fs::read(fixture.root.join(".over/secrets")).unwrap(),
        original
    );
}

#[test]
fn access_templates_change_messages_without_committing_the_descriptor() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("over.toml"),
        "[crypt.commit_messages]\nuser_add = 'chore: granted {{ user }} {{ count }} key'\n",
    )
    .unwrap();
    fixture.enroll("bob");
    assert_eq!(
        fixture.git(&["log", "-1", "--format=%s"]),
        b"chore: granted bob 1 key\n"
    );
    assert!(!String::from_utf8_lossy(&fixture.changed()).contains("over.toml"));
    let head = fixture.git(&["rev-parse", "HEAD"]);
    fs::write(
        fixture.root.join("over.toml"),
        "crypt.commit_messages.user_add = '{{ unavailable }}'\n",
    )
    .unwrap();
    let output = fixture.over(&[
        "user",
        "add",
        "carol",
        "--identity-output",
        fixture.temp.path().join("carol.age").to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert!(!fixture.root.join(".over/users/carol/user.json").exists());
}

#[test]
fn independent_keys_for_one_user_merge_and_preserve_existing_record_bytes() {
    let fixture = Fixture::new();
    let marker = b"\n{\"version\":2,\"note\":\"preserve formatting\"}\n";
    fs::write(fixture.root.join(".over/users/alice/user.json"), marker).unwrap();
    let header_path = fixture.root.join(".over/crypt.json");
    let mut header: serde_json::Value =
        serde_json::from_slice(&fs::read(&header_path).unwrap()).unwrap();
    header["note"] = serde_json::json!("preserve additive fields");
    let header = serde_json::to_vec(&header).unwrap();
    fs::write(header_path, &header).unwrap();
    fixture.git(&["add", ".over/crypt.json", ".over/users"]);
    fixture.git(&["commit", "-qm", "custom metadata formatting"]);
    fixture.git(&["branch", "right"]);
    fixture.git(&["checkout", "-qb", "left"]);
    let left = age::x25519::Identity::generate().to_public().to_string();
    success(fixture.over(&[
        "user",
        "key",
        "add",
        "alice",
        "--name",
        "laptop",
        "--recipient",
        &left,
    ]));
    assert_eq!(fixture.changed(), b".over/users/alice/laptop\n");
    fixture.git(&["checkout", "-q", "right"]);
    let right = age::x25519::Identity::generate().to_public().to_string();
    success(fixture.over(&[
        "user",
        "key",
        "add",
        "alice",
        "--name",
        "recovery",
        "--recipient",
        &right,
    ]));
    fixture.git(&["merge", "--no-edit", "left"]);
    assert_eq!(
        fs::read(fixture.root.join(".over/users/alice/user.json")).unwrap(),
        marker
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt.json")).unwrap(),
        header
    );
    assert_eq!(
        over::crypt::access::load(&git2::Repository::open(&fixture.root).unwrap())
            .unwrap()
            .users["alice"]
            .len(),
        3
    );
}

#[test]
fn rotating_an_unlocked_ordinary_file_keeps_plaintext_out_of_git_objects() {
    let fixture = Fixture::new();
    let plaintext = b"ordinary plaintext rotation sentinel";
    fs::write(fixture.root.join("protected"), plaintext).unwrap();
    success(fixture.over(&["crypt", "add", "protected"]));
    fixture.git(&["commit", "-qm", "protected file"]);
    let before = fixture.git(&["show", "HEAD:protected"]);
    success(fixture.over(&["key", "rotate"]));
    assert_eq!(fs::read(fixture.root.join("protected")).unwrap(), plaintext);
    assert_ne!(fixture.git(&["show", "HEAD:protected"]), before);
    let status = fixture.git(&["status", "--porcelain"]);
    assert!(status.is_empty(), "{}", String::from_utf8_lossy(&status));
    let repo = git2::Repository::open(&fixture.root).unwrap();
    assert!(
        repo.find_blob(git2::Oid::hash_object(git2::ObjectType::Blob, plaintext).unwrap())
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn initial_enrollment_honors_real_ssh_commit_signing() {
    let fixture = Fixture::empty();
    let key = fixture.temp.path().join("signing-key");
    success(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .output()
            .unwrap(),
    );
    let allowed = fixture.temp.path().join("allowed-signers");
    fs::write(
        &allowed,
        format!(
            "split@example.invalid {}",
            fs::read_to_string(key.with_extension("pub")).unwrap()
        ),
    )
    .unwrap();
    fixture.git(&["config", "commit.gpgSign", "true"]);
    fixture.git(&["config", "gpg.format", "ssh"]);
    fixture.git(&["config", "user.signingKey", key.to_str().unwrap()]);
    fixture.git(&[
        "config",
        "gpg.ssh.allowedSignersFile",
        allowed.to_str().unwrap(),
    ]);
    fixture.enroll("alice");
    fixture.git(&["verify-commit", "HEAD"]);
    over::crypt::access::local_key(&git2::Repository::open(&fixture.root).unwrap()).unwrap();
}

fn use_nested_layout(fixture: &Fixture) {
    fs::create_dir_all(fixture.root.join(".over/crypt")).unwrap();
    fs::rename(
        fixture.root.join(".over/crypt.json"),
        fixture.root.join(".over/crypt/repository.json"),
    )
    .unwrap();
    fs::rename(
        fixture.root.join(".over/users"),
        fixture.root.join(".over/crypt/users"),
    )
    .unwrap();
    for user in fs::read_dir(fixture.root.join(".over/crypt/users")).unwrap() {
        let user = user.unwrap().path();
        fs::rename(user.join("user.json"), user.join(".user")).unwrap();
    }
    let attributes = fixture.root.join(".over/.gitattributes");
    let previous = fs::read_to_string(&attributes)
        .unwrap()
        .replace("crypt.json -filter !diff -text\n", "")
        .replace("users/** -filter !diff -text\n", "");
    fs::write(
        attributes,
        format!("{previous}# preserve this policy comment\n"),
    )
    .unwrap();
    fixture.git(&["add", "-A", ".over"]);
    fixture.git(&["commit", "-qm", "previous split layout"]);
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn nested_split_metadata_is_read_without_migration_then_moved_in_one_commit(
    #[case] explicit: bool,
) {
    let fixture = Fixture::new();
    fixture.enroll("empty");
    success(fixture.over(&["user", "key", "detach", "empty", "default"]));
    success(fixture.secret("TOKEN", b"preserve ciphertext", false));
    use_nested_layout(&fixture);
    let header = fs::read(fixture.root.join(".over/crypt/repository.json")).unwrap();
    let marker = fs::read(fixture.root.join(".over/crypt/users/alice/.user")).unwrap();
    let key = fs::read(fixture.root.join(".over/crypt/users/alice/default")).unwrap();
    let secret = fs::read(fixture.root.join(".over/secrets/TOKEN")).unwrap();
    let attributes = fs::read(fixture.root.join(".over/.gitattributes")).unwrap();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    success(fixture.over(&["user", "list"]));
    success(fixture.over(&["lock"]));
    success(fixture.over(&[
        "unlock",
        "--identity",
        fixture.temp.path().join("alice.age").to_str().unwrap(),
    ]));
    assert_eq!(fixture.value("TOKEN"), b"preserve ciphertext");
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert!(!fixture.root.join(".over/crypt.json").exists());
    over::crypt::detect::inspect(&git2::Repository::open(&fixture.root).unwrap())
        .unwrap()
        .validate_checkout()
        .unwrap();
    fs::write(fixture.root.join("unrelated"), b"pending work").unwrap();
    fixture.git(&["add", "unrelated"]);
    let count = fixture.git(&["rev-list", "--count", "HEAD"]);
    if explicit {
        success(fixture.over(&["crypt", "migrate"]));
    } else {
        fixture.enroll("bob");
    }
    let after_count = fixture.git(&["rev-list", "--count", "HEAD"]);
    assert_eq!(
        String::from_utf8(after_count)
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap(),
        String::from_utf8(count)
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap()
            + 1
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt.json")).unwrap(),
        header
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/users/alice/user.json")).unwrap(),
        marker
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/users/alice/default")).unwrap(),
        key
    );
    assert!(fixture.root.join(".over/users/empty/user.json").is_file());
    assert!(!fixture.root.join(".over/crypt/repository.json").exists());
    assert!(!fixture.root.join(".over/crypt/users/alice/.user").exists());
    assert_eq!(
        fs::read(fixture.root.join(".over/secrets/TOKEN")).unwrap(),
        secret
    );
    assert!(
        fs::read(fixture.root.join(".over/.gitattributes"))
            .unwrap()
            .starts_with(&attributes)
    );
    assert_eq!(
        fixture.git(&["diff", "--cached", "--name-only"]),
        b"unrelated\n"
    );
    let registry =
        over::crypt::access::load(&git2::Repository::open(&fixture.root).unwrap()).unwrap();
    assert!(registry.users["empty"].is_empty());
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn initial_linked_checkout_reads_all_access_records_before_users_are_materialized(
    #[case] process: bool,
) {
    let fixture = Fixture::new();
    fs::write(fixture.root.join(".env"), b"early protected checkout").unwrap();
    success(fixture.over(&["crypt", "add", ".env"]));
    fixture.git(&["commit", "-qm", "protected before bootstrap paths"]);
    success(fixture.secret("TOKEN", b"late native entry", false));
    if !process {
        fixture.git(&["config", "--unset", "filter.over.process"]);
    }
    let sibling = fixture.temp.path().join("first-checkout");
    fixture.git(&[
        "worktree",
        "add",
        "-qb",
        "first-checkout",
        sibling.to_str().unwrap(),
    ]);
    assert_eq!(
        fs::read(sibling.join(".env")).unwrap(),
        b"early protected checkout"
    );
    assert!(sibling.join(".over/users/alice/user.json").is_file());
    over::crypt::format::inspect(&fs::read(sibling.join(".over/secrets/TOKEN")).unwrap()).unwrap();
}

#[test]
fn mixed_flat_and_nested_headers_cannot_hide_access_records() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".over/crypt")).unwrap();
    fs::copy(
        fixture.root.join(".over/crypt.json"),
        fixture.root.join(".over/crypt/repository.json"),
    )
    .unwrap();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    assert!(!fixture.over(&["user", "list"]).status.success());
    let output = fixture.over(&["user", "delete", "alice"]);
    assert!(!output.status.success());
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    fixture.git(&["add", ".over/crypt/repository.json"]);
    fixture.git(&["commit", "-qm", "conflicting layouts"]);
    let detection =
        over::crypt::detect::inspect(&git2::Repository::open(&fixture.root).unwrap()).unwrap();
    assert_eq!(detection.metadata, over::crypt::detect::Metadata::Malformed);
    assert!(detection.validate_checkout().is_err());
}

#[cfg(unix)]
#[test]
fn rejected_layout_migration_restores_nested_metadata_and_attributes() {
    let fixture = Fixture::new();
    use_nested_layout(&fixture);
    let header = fs::read(fixture.root.join(".over/crypt/repository.json")).unwrap();
    let key = fs::read(fixture.root.join(".over/crypt/users/alice/default")).unwrap();
    let attributes = fs::read(fixture.root.join(".over/.gitattributes")).unwrap();
    let index = fs::read(fixture.root.join(".git/index")).unwrap();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    hook(
        &fixture,
        "reference-transaction",
        "if [ \"$1\" = prepared ]; then exit 1; fi",
    );
    assert!(!fixture.over(&["crypt", "migrate"]).status.success());
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt/repository.json")).unwrap(),
        header
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt/users/alice/default")).unwrap(),
        key
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/.gitattributes")).unwrap(),
        attributes
    );
    assert_eq!(fs::read(fixture.root.join(".git/index")).unwrap(), index);
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert!(!fixture.root.join(".over/crypt.json").exists());
    assert!(!fixture.root.join(".over/users/alice/user.json").exists());
}

#[test]
fn rotation_compares_logical_access_across_flat_and_nested_worktrees() {
    let fixture = Fixture::new();
    success(fixture.secret("TOKEN", b"same logical access", false));
    let sibling = fixture.temp.path().join("flat-sibling");
    fixture.git(&[
        "worktree",
        "add",
        "-qb",
        "flat-sibling",
        sibling.to_str().unwrap(),
    ]);
    let sibling_head = fixture.git(&["rev-parse", "flat-sibling"]);
    use_nested_layout(&fixture);
    success(fixture.over(&["key", "rotate"]));
    assert!(fixture.root.join(".over/crypt.json").is_file());
    assert!(!fixture.root.join(".over/crypt/repository.json").exists());
    assert_eq!(fixture.value("TOKEN"), b"same logical access");
    assert_eq!(fixture.git(&["rev-parse", "flat-sibling"]), sibling_head);
    let sibling_registry =
        over::crypt::access::load(&git2::Repository::open(&sibling).unwrap()).unwrap();
    assert_eq!(
        sibling_registry,
        over::crypt::access::load(&git2::Repository::open(&fixture.root).unwrap()).unwrap()
    );
}

#[test]
fn migration_upgrades_bootstrap_exclusions_under_a_broad_encryption_rule() {
    let fixture = Fixture::new();
    use_nested_layout(&fixture);
    fs::write(
        fixture.root.join(".gitattributes"),
        "** filter=over diff=over -text\n.gitattributes -filter !diff\n",
    )
    .unwrap();
    fixture.git(&["add", ".gitattributes"]);
    fixture.git(&["commit", "-qm", "broad encryption policy"]);
    success(fixture.over(&["crypt", "migrate"]));
    for path in [
        ".over/crypt.json",
        ".over/users/alice/user.json",
        ".over/users/alice/default",
    ] {
        assert_eq!(
            fixture.git(&["check-attr", "filter", "--", path]),
            format!("{path}: filter: unset\n").as_bytes()
        );
    }
    over::crypt::detect::inspect(&git2::Repository::open(&fixture.root).unwrap())
        .unwrap()
        .validate_checkout()
        .unwrap();
}

#[test]
fn flattened_bootstrap_files_cannot_be_selected_for_encryption() {
    let fixture = Fixture::new();
    let index = fs::read(fixture.root.join(".git/index")).unwrap();
    for path in [
        ".over/crypt.json",
        ".over/users/alice/user.json",
        ".over/users/alice/default",
    ] {
        assert!(!fixture.over(&["crypt", "add", path]).status.success());
        assert_eq!(fs::read(fixture.root.join(".git/index")).unwrap(), index);
    }
    fs::write(
        fixture.root.join(".git/info/attributes"),
        ".over/crypt.json filter=over diff=over\n.over/users/** filter=over diff=over\n",
    )
    .unwrap();
    let findings = success(fixture.over(&["crypt", "list"]));
    assert!(String::from_utf8_lossy(&findings.stdout).contains("bootstrap metadata"));
}

#[test]
fn clean_crlf_attributes_allow_secret_creation_and_rotation() {
    let fixture = Fixture::new();
    fixture.git(&["config", "core.autocrlf", "true"]);
    let path = fixture.root.join(".over/.gitattributes");
    let attributes = fs::read_to_string(&path).unwrap().replace('\n', "\r\n");
    fs::write(&path, attributes).unwrap();
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    success(fixture.secret("TOKEN", b"line endings are not edits", false));
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    success(fixture.over(&["key", "rotate"]));
    assert_eq!(fixture.value("TOKEN"), b"line endings are not edits");
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    let attributes = fs::read(&path).unwrap();
    assert!(attributes.windows(2).any(|bytes| bytes == b"\r\n"));
    assert!(
        !fixture
            .git(&["show", "HEAD:.over/.gitattributes"])
            .contains(&b'\r')
    );
}

#[cfg(target_os = "linux")]
#[test]
fn unlock_cannot_install_a_key_for_an_unpublished_enrollment() {
    let fixture = Fixture::empty();
    hook(
        &fixture,
        "reference-transaction",
        "if [ \"$1\" = prepared ]; then set -- $(ps -o ppid= -p \"$PPID\"); kill -KILL \"$1\"; exit 1; fi",
    );
    let identity = fixture.temp.path().join("alice.age");
    assert!(
        !fixture
            .over(&[
                "user",
                "add",
                "alice",
                "--identity-output",
                identity.to_str().unwrap()
            ])
            .status
            .success()
    );
    assert!(fixture.root.join(".git/over-crypt/mutation.json").is_file());
    assert!(!fixture.root.join(".git/over-crypt/key").exists());
    fs::remove_file(fixture.root.join(".git/hooks/reference-transaction")).unwrap();
    let unlock = fixture.over(&["unlock", "--identity", identity.to_str().unwrap()]);
    assert!(!unlock.status.success());
    assert!(String::from_utf8_lossy(&unlock.stderr).contains("recover"));
    assert!(!fixture.root.join(".git/over-crypt/key").exists());
    success(fixture.over(&["crypt", "recover"]));
    assert!(!fixture.root.join(".over/crypt.json").exists());
    assert!(!fixture.root.join(".git/over-crypt/key").exists());
    fixture.enroll("bob");
}

#[rstest::rstest]
#[case(".over/secrets", false)]
#[case(".over/secrets", true)]
#[case(".over/secrets.yaml", false)]
#[case(".over/secrets.yaml", true)]
fn aggregate_case_collisions_fail_before_changing_the_store_index_or_head(
    #[case] path: &str,
    #[case] incoming_collision: bool,
) {
    let fixture = Fixture::new();
    let repo = git2::Repository::open(&fixture.root).unwrap();
    let key = over::crypt::access::local_key(&repo).unwrap();
    let mut values = std::collections::BTreeMap::from([("TOKEN", b"original".to_vec())]);
    if !incoming_collision {
        values.insert("token", b"another original".to_vec());
    }
    let original = key
        .encrypt(&serde_json::to_vec(&(values, 1u32)).unwrap(), 1)
        .unwrap();
    fs::write(fixture.root.join(path), &original).unwrap();
    fixture.git(&["add", path]);
    fixture.git(&["commit", "-qm", "historical aggregate"]);
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let index = fs::read(fixture.root.join(".git/index")).unwrap();
    let output = if incoming_collision {
        fixture.secret("token", b"new value", false)
    } else {
        fixture.over(&["crypt", "migrate"])
    };
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("case-colliding secret names"));
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read(fixture.root.join(".git/index")).unwrap(), index);
    assert_eq!(fs::read(fixture.root.join(path)).unwrap(), original);
    assert_eq!(fixture.value("TOKEN"), b"original");
    if !incoming_collision {
        assert_eq!(fixture.value("token"), b"another original");
    }
}

#[cfg(unix)]
#[test]
fn access_mutations_preserve_unrelated_non_utf8_git_names_and_staging() {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    let fixture = Fixture::new();
    let names = [
        PathBuf::from(std::ffi::OsString::from_vec(b"notes-\xff".to_vec())),
        PathBuf::from(std::ffi::OsString::from_vec(b"archive-\xfe".to_vec())).join("readme"),
        PathBuf::from(".over").join(std::ffi::OsString::from_vec(b"cache-\xfd".to_vec())),
    ];
    let repo = git2::Repository::open(&fixture.root).unwrap();
    let mut index = repo.index().unwrap();
    let blob = repo.blob(b"unrelated original bytes").unwrap();
    for path in &names {
        #[cfg(target_os = "linux")]
        {
            fs::create_dir_all(fixture.root.join(path).parent().unwrap()).unwrap();
            fs::write(fixture.root.join(path), b"unrelated original bytes").unwrap();
        }
        // APFS cannot materialize invalid UTF-8 filenames, but Git objects and
        // index entries can contain them and must survive unrelated mutations.
        let mut entry = index.get_path(Path::new(".over/crypt.json"), 0).unwrap();
        entry.path = path.as_os_str().as_bytes().to_vec();
        entry.id = blob;
        entry.file_size = 0;
        index.add(&entry).unwrap();
    }
    index.write().unwrap();
    drop(index);
    fixture.git(&["commit", "-qm", "unrelated raw filenames"]);
    let before = repo.head().unwrap().peel_to_tree().unwrap();
    let blobs: Vec<_> = names
        .iter()
        .map(|path| before.get_path(path).unwrap().id())
        .collect();
    fs::write(fixture.root.join("pending"), b"leave staged").unwrap();
    fixture.git(&["add", "pending"]);
    fixture.enroll("bob");
    success(fixture.over(&["user", "key", "detach", "bob", "default"]));
    success(fixture.secret("TOKEN", b"independent secret", false));
    let after = repo.head().unwrap().peel_to_tree().unwrap();
    for (path, blob) in names.iter().zip(blobs) {
        assert_eq!(after.get_path(path).unwrap().id(), blob);
        assert_eq!(repo.index().unwrap().get_path(path, 0).unwrap().id, blob);
        #[cfg(target_os = "linux")]
        assert_eq!(
            fs::read(fixture.root.join(path)).unwrap(),
            b"unrelated original bytes"
        );
    }
    assert_eq!(
        fixture.git(&["diff", "--cached", "--name-only"]),
        b"pending\n"
    );
}
