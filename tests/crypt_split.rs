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
    assert!(
        fixture
            .root
            .join(".over/crypt/users/alice/default")
            .is_file()
    );
    let header = fs::read(fixture.root.join(".over/crypt/repository.json")).unwrap();
    let alice = fs::read(fixture.root.join(".over/crypt/users/alice/default")).unwrap();
    fs::write(fixture.root.join("unrelated"), b"staged work").unwrap();
    fixture.git(&["add", "unrelated"]);
    fixture.enroll("bob");
    assert_eq!(
        fixture.changed(),
        b".over/crypt/users/bob/.user\n.over/crypt/users/bob/default\n"
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt/repository.json")).unwrap(),
        header
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt/users/alice/default")).unwrap(),
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
    assert!(fixture.root.join(".over/crypt/users/bob/.user").is_file());
    success(fixture.over(&["user", "delete", "bob"]));
    assert_eq!(fixture.changed(), b".over/crypt/users/bob/.user\n");
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
    let header = fs::read(fixture.root.join(".over/crypt/repository.json")).unwrap();
    fixture.git(&["branch", "right"]);
    fixture.git(&["checkout", "-qb", "left"]);
    fixture.enroll("bob");
    fixture.git(&["checkout", "-q", "right"]);
    fixture.enroll("carol");
    fixture.git(&["merge", "--no-edit", "left"]);
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt/repository.json")).unwrap(),
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
    fs::remove_dir_all(fixture.root.join(".over/crypt/users")).unwrap();
    fs::remove_file(fixture.root.join(".over/crypt/repository.json")).unwrap();
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
    assert!(fixture.root.join(".over/crypt/users/bob/default").is_file());
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
    assert!(!fixture.root.join(".over/crypt/repository.json").exists());
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
    assert!(!fixture.root.join(".over/crypt/users/carol/.user").exists());
}

#[test]
fn independent_keys_for_one_user_merge_and_preserve_existing_record_bytes() {
    let fixture = Fixture::new();
    let marker = b"\n{\"version\":2,\"note\":\"preserve formatting\"}\n";
    fs::write(fixture.root.join(".over/crypt/users/alice/.user"), marker).unwrap();
    let header_path = fixture.root.join(".over/crypt/repository.json");
    let mut header: serde_json::Value =
        serde_json::from_slice(&fs::read(&header_path).unwrap()).unwrap();
    header["note"] = serde_json::json!("preserve additive fields");
    let header = serde_json::to_vec(&header).unwrap();
    fs::write(header_path, &header).unwrap();
    fixture.git(&["add", ".over/crypt"]);
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
    assert_eq!(fixture.changed(), b".over/crypt/users/alice/laptop\n");
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
        fs::read(fixture.root.join(".over/crypt/users/alice/.user")).unwrap(),
        marker
    );
    assert_eq!(
        fs::read(fixture.root.join(".over/crypt/repository.json")).unwrap(),
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
