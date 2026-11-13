use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;

fn run(root: &Path, program: &Path, args: &[&str]) -> Output {
    let output = Command::new(program)
        .current_dir(root)
        .env_remove("OVER_HOME")
        .env("XDG_STATE_HOME", root.join(".git/test-state"))
        .env("XDG_CACHE_HOME", root.join(".git/test-cache"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn add_secret(root: &Path, name: &str, value: &[u8]) {
    use std::io::Write;
    let mut child = Command::new(cargo_bin("over"))
        .current_dir(root)
        .env_remove("OVER_HOME")
        .args(["--home", root.to_str().unwrap(), "secret", "add", name])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(value).unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn revocation_rotates_current_files_and_secrets_for_remaining_users_only() {
    let (root, identities) = repository();
    over(
        root.path(),
        &[
            "user",
            "add",
            "recovery",
            "--identity-output",
            identities.path().join("recovery.age").to_str().unwrap(),
        ],
    );
    fs::write(root.path().join("protected"), b"rotation sentinel").unwrap();
    add_secret(root.path(), "TOKEN", b"rotation secret sentinel");
    over(root.path(), &["crypt", "add", "protected"]);
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "protected"]);
    let old = git(root.path(), &["show", ":protected"]).stdout;
    over(root.path(), &["user", "revoke", "alice"]);
    let new = git(root.path(), &["show", ":protected"]).stdout;
    assert_ne!(old, new);
    over::crypt::format::inspect(&new).unwrap();
    over(root.path(), &["lock"]);
    let rejected = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args([
            "unlock",
            "--identity",
            identities.path().join("alice.age").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    over(
        root.path(),
        &[
            "unlock",
            "--identity",
            identities.path().join("recovery.age").to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(root.path().join("protected")).unwrap(),
        b"rotation sentinel"
    );
    assert_eq!(
        over(
            root.path(),
            &[
                "--home",
                root.path().to_str().unwrap(),
                "secret",
                "get",
                "TOKEN"
            ]
        )
        .stdout,
        b"rotation secret sentinel"
    );
}

#[test]
fn native_secrets_use_the_same_unlock_and_remain_ciphertext_in_the_worktree() {
    use std::io::Write;
    let (root, identities) = repository();
    let mut child = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args([
            "--home",
            root.path().to_str().unwrap(),
            "secret",
            "add",
            "TOKEN",
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"secret sentinel\0\xff")
        .unwrap();
    assert!(child.wait().unwrap().success());
    let args = [
        "--home",
        root.path().to_str().unwrap(),
        "secret",
        "get",
        "TOKEN",
    ];
    assert_eq!(over(root.path(), &args).stdout, b"secret sentinel\0\xff");
    over::crypt::format::inspect(&fs::read(root.path().join(".over/secrets.yaml")).unwrap())
        .unwrap();
    git(root.path(), &["add", ".over"]);
    over(root.path(), &["lock"]);
    let rejected = Command::new(cargo_bin("over"))
        .env_remove("OVER_HOME")
        .args(args)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    over(
        root.path(),
        &[
            "unlock",
            "--identity",
            identities.path().join("alice.age").to_str().unwrap(),
        ],
    );
    assert_eq!(over(root.path(), &args).stdout, b"secret sentinel\0\xff");
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn a_virtual_crypt_commit_includes_canonical_policy_and_ciphertext(#[case] direct_policy: bool) {
    let (root, _) = repository();
    let deployed = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("ov")).unwrap();
    fs::write(
        root.path().join("ov/over.toml"),
        "target = '~/checkout'\nmode = 'checkout'\n",
    )
    .unwrap();
    fs::write(root.path().join("ov/token"), "virtual sentinel").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "initial"]);
    over(
        root.path(),
        &[
            "--home",
            root.path().to_str().unwrap(),
            "apply",
            "ov",
            "--no-prompt",
            "--target",
            deployed.path().to_str().unwrap(),
        ],
    );
    fs::write(
        deployed.path().join("checkout/token"),
        b"virtual edited sentinel",
    )
    .unwrap();
    if direct_policy {
        let result = Command::new(cargo_bin("over"))
            .current_dir(deployed.path().join("checkout"))
            .env_remove("OVER_HOME")
            .env("XDG_STATE_HOME", root.path().join(".git/test-state"))
            .args([
                "--home",
                root.path().to_str().unwrap(),
                "crypt",
                "add",
                "token",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    } else {
        over(
            root.path(),
            &[
                "--home",
                root.path().to_str().unwrap(),
                "add",
                "--crypt",
                "--overlay",
                "ov",
                "--target",
                deployed.path().to_str().unwrap(),
                deployed.path().join("checkout/token").to_str().unwrap(),
            ],
        );
    }
    over(
        root.path(),
        &[
            "--home",
            root.path().to_str().unwrap(),
            "commit",
            "--overlay",
            "ov",
            "--no-prompt",
            "--message",
            "crypt transition",
            "--target",
            deployed.path().to_str().unwrap(),
        ],
    );
    over::crypt::format::inspect(&git(root.path(), &["show", "HEAD:ov/token"]).stdout).unwrap();
    assert!(
        String::from_utf8_lossy(&git(root.path(), &["show", "HEAD:.gitattributes"]).stdout)
            .contains("filter=over")
    );
    assert_eq!(
        fs::read(deployed.path().join("checkout/token")).unwrap(),
        b"virtual edited sentinel"
    );
    let plaintext_oid =
        git2::Oid::hash_object(git2::ObjectType::Blob, b"virtual edited sentinel").unwrap();
    assert!(
        !git2::Repository::open(root.path())
            .unwrap()
            .odb()
            .unwrap()
            .exists(plaintext_oid)
    );
}

#[test]
fn composed_checkouts_keep_ciphertext_in_git_and_isolate_plaintext_destinations_through_lock_and_sync()
 {
    let (root, identities) = repository();
    let deployed = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("over.toml"),
        "mode='checkout'\noverlays=['apps/*', 'hosts/*']\n",
    )
    .unwrap();
    fs::create_dir_all(root.path().join("hosts/laptop")).unwrap();
    fs::write(
        root.path().join("hosts/laptop/over.toml"),
        "uses=['apps/git', 'apps/ssh']\n",
    )
    .unwrap();
    for app in ["git", "ssh"] {
        let directory = root.path().join("apps").join(app).join(".env.d");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(app), format!("initial {app}")).unwrap();
        over(
            root.path(),
            &["crypt", "add", &format!("apps/{app}/.env.d/{app}")],
        );
    }
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "protected composition"]);
    let home = root.path().to_str().unwrap();
    let target = deployed.path().to_str().unwrap();
    over(
        root.path(),
        &[
            "--home",
            home,
            "apply",
            "hosts/laptop",
            "--no-prompt",
            "--target",
            target,
        ],
    );
    let git_target = deployed.path().join(".env.d/git");
    let ssh_target = deployed.path().join(".env.d/ssh");
    fs::write(&git_target, b"composed git target sentinel").unwrap();
    fs::write(&ssh_target, b"composed ssh target sentinel").unwrap();
    over(
        root.path(),
        &[
            "--home",
            home,
            "add",
            "--overlay",
            "apps/git",
            "--target",
            target,
            git_target.to_str().unwrap(),
        ],
    );
    over(
        root.path(),
        &[
            "--home",
            home,
            "commit",
            "--overlay",
            "apps/git",
            "--no-prompt",
            "-m",
            "git target",
            "--target",
            target,
        ],
    );
    let fresh = deployed.path().join(".env.d/fresh");
    fs::write(&fresh, b"composed acquired protected sentinel").unwrap();
    // Policy routing must not infer acquisition from another application's neighboring files.
    let unowned = Command::new(cargo_bin("over"))
        .current_dir(deployed.path())
        .env_remove("OVER_HOME")
        .env("XDG_STATE_HOME", root.path().join(".git/test-state"))
        .args(["--home", home, "crypt", "add", fresh.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!unowned.status.success());
    assert!(String::from_utf8_lossy(&unowned.stderr).contains("--overlay"));
    over(
        root.path(),
        &[
            "--home",
            home,
            "add",
            "--crypt",
            "--overlay",
            "apps/git",
            "--target",
            target,
            fresh.to_str().unwrap(),
        ],
    );
    over(
        root.path(),
        &[
            "--home",
            home,
            "commit",
            "--overlay",
            "apps/git",
            "--no-prompt",
            "-m",
            "acquired target",
            "--target",
            target,
        ],
    );
    for path in [
        "apps/git/.env.d/git",
        "apps/git/.env.d/fresh",
        "apps/ssh/.env.d/ssh",
    ] {
        over::crypt::format::inspect(&git(root.path(), &["show", &format!("HEAD:{path}")]).stdout)
            .unwrap();
        over::crypt::format::inspect(&git(root.path(), &["show", &format!(":{path}")]).stdout)
            .unwrap();
    }
    over(root.path(), &["lock"]);
    assert_eq!(
        fs::read(&git_target).unwrap(),
        b"composed git target sentinel"
    );
    assert_eq!(
        fs::read(&ssh_target).unwrap(),
        b"composed ssh target sentinel"
    );
    assert_eq!(
        fs::read(&fresh).unwrap(),
        b"composed acquired protected sentinel"
    );
    assert!(!fresh.is_symlink());
    over(
        root.path(),
        &[
            "unlock",
            "--identity",
            identities.path().join("alice.age").to_str().unwrap(),
        ],
    );
    fs::write(
        root.path().join("apps/git/.env.d/git"),
        b"composed source advancement sentinel",
    )
    .unwrap();
    git(root.path(), &["add", "apps/git/.env.d/git"]);
    git(root.path(), &["commit", "-qm", "source advance"]);
    over(
        root.path(),
        &[
            "--home",
            home,
            "sync",
            "apps/git",
            "--no-prompt",
            "--target",
            target,
        ],
    );
    assert_eq!(
        fs::read(&git_target).unwrap(),
        b"composed source advancement sentinel"
    );
    assert_eq!(
        fs::read(&ssh_target).unwrap(),
        b"composed ssh target sentinel"
    );
    let repo = git2::Repository::open(root.path()).unwrap();
    for plaintext in [
        b"composed git target sentinel".as_slice(),
        b"composed acquired protected sentinel",
        b"composed source advancement sentinel",
    ] {
        let oid = git2::Oid::hash_object(git2::ObjectType::Blob, plaintext).unwrap();
        assert!(
            repo.find_blob(oid).is_err(),
            "plaintext reached the source object database"
        );
    }
}

#[test]
fn legacy_migration_preserves_age_backups_and_native_binary_values() {
    let (root, _) = repository();
    let legacy = age::scrypt::Recipient::new("migration credential".into());
    fs::write(
        root.path().join("token.age"),
        age::encrypt(&legacy, b"legacy overlay value").unwrap(),
    )
    .unwrap();
    fs::create_dir(root.path().join(".over/secrets")).unwrap();
    fs::write(
        root.path().join(".over/secrets/TOKEN.age"),
        age::encrypt(&legacy, b"legacy secret\0\xff").unwrap(),
    )
    .unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "legacy"]);
    let migrated = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .env("OVER_SECRET", "migration credential")
        .args(["crypt", "migrate"])
        .output()
        .unwrap();
    assert!(
        migrated.status.success(),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    assert_eq!(
        fs::read(root.path().join("token")).unwrap(),
        b"legacy overlay value"
    );
    assert!(!root.path().join("token.age").exists());
    assert!(
        root.path()
            .join(".git/over-crypt/legacy-backup/token.age")
            .is_file()
    );
    over::crypt::format::inspect(&git(root.path(), &["show", ":token"]).stdout).unwrap();
    assert_eq!(
        over(
            root.path(),
            &[
                "--home",
                root.path().to_str().unwrap(),
                "secret",
                "get",
                "TOKEN"
            ]
        )
        .stdout,
        b"legacy secret\0\xff"
    );
}

#[test]
fn explicit_policy_removal_stages_plaintext_without_changing_working_content() {
    let (root, _) = repository();
    fs::write(root.path().join("token"), b"deliberate plaintext").unwrap();
    over(root.path(), &["crypt", "add", "token"]);
    over(root.path(), &["crypt", "remove", "token", "--yes"]);
    assert_eq!(
        git(root.path(), &["show", ":token"]).stdout,
        b"deliberate plaintext"
    );
    assert_eq!(
        fs::read(root.path().join("token")).unwrap(),
        b"deliberate plaintext"
    );
}

#[test]
fn nested_attribute_overrides_fail_before_plaintext_objects_are_published() {
    let (root, _) = repository();
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(root.path().join("nested/.gitattributes"), "token -filter\n").unwrap();
    let plaintext = b"override sentinel never stored";
    fs::write(root.path().join("nested/token"), plaintext).unwrap();
    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, plaintext).unwrap();
    let rejected = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args(["crypt", "add", "nested/token"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(
        !git2::Repository::open(root.path())
            .unwrap()
            .odb()
            .unwrap()
            .exists(oid)
    );
    assert!(!root.path().join(".gitattributes").exists());
}

#[test]
fn persistent_unlock_is_shared_by_linked_worktrees_and_dirty_lock_refuses() {
    let (root, identities) = repository();
    let sibling = tempfile::tempdir().unwrap();
    let linked = sibling.path().join("linked");
    fs::write(root.path().join("token"), b"linked sentinel").unwrap();
    over(root.path(), &["crypt", "add", "token"]);
    git(root.path(), &["commit", "-qm", "protected"]);
    git(
        root.path(),
        &["worktree", "add", "-qb", "linked", linked.to_str().unwrap()],
    );
    assert_eq!(fs::read(linked.join("token")).unwrap(), b"linked sentinel");
    fs::write(linked.join("token"), b"dirty sentinel").unwrap();
    let refused = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .arg("lock")
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(fs::read(linked.join("token")).unwrap(), b"dirty sentinel");
    assert!(root.path().join(".git/over-crypt/key").exists());
    fs::write(linked.join("token"), b"linked sentinel").unwrap();
    over(root.path(), &["lock"]);
    over::crypt::format::inspect(&fs::read(linked.join("token")).unwrap()).unwrap();
    over(
        linked.as_path(),
        &[
            "unlock",
            "--identity",
            identities.path().join("alice.age").to_str().unwrap(),
        ],
    );
    assert_eq!(fs::read(linked.join("token")).unwrap(), b"linked sentinel");
    assert_eq!(
        fs::read(root.path().join("token")).unwrap(),
        b"linked sentinel"
    );
    let old = git(root.path(), &["show", ":token"]).stdout;
    over(root.path(), &["key", "rotate"]);
    let rotated = git(root.path(), &["show", ":token"]).stdout;
    assert_ne!(old, rotated);
    assert_eq!(rotated, git(&linked, &["show", ":token"]).stdout);
    over(root.path(), &["lock"]);
    over::crypt::format::inspect(&fs::read(linked.join("token")).unwrap()).unwrap();
}

#[test]
fn a_manual_clone_can_checkout_ciphertext_then_unlock_once() {
    let (root, identities) = repository();
    fs::write(root.path().join("token"), b"clone sentinel").unwrap();
    over(root.path(), &["crypt", "add", "token"]);
    git(root.path(), &["commit", "-qm", "protected"]);
    let clone = tempfile::tempdir().unwrap();
    git(
        clone.path(),
        &["clone", "-q", root.path().to_str().unwrap(), "."],
    );
    over::crypt::format::inspect(&fs::read(clone.path().join("token")).unwrap()).unwrap();
    over(
        clone.path(),
        &[
            "unlock",
            "--identity",
            identities.path().join("alice.age").to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(clone.path().join("token")).unwrap(),
        b"clone sentinel"
    );
    // A fresh invocation has no shared process memory and still reads the persisted key.
    over(clone.path(), &["crypt", "list"]);
    git(clone.path(), &["add", "token"]);
    assert!(
        git(clone.path(), &["status", "--porcelain"])
            .stdout
            .is_empty()
    );
}

#[test]
fn concurrent_native_updates_preserve_every_secret() {
    use std::io::Write;
    let (root, _) = repository();
    let mut children = Vec::new();
    for name in ["ONE", "TWO", "THREE", "FOUR"] {
        let mut child = Command::new(cargo_bin("over"))
            .env_remove("OVER_HOME")
            .args([
                "--home",
                root.path().to_str().unwrap(),
                "secret",
                "add",
                name,
            ])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(name.as_bytes())
            .unwrap();
        children.push(child);
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        over(
            root.path(),
            &["--home", root.path().to_str().unwrap(), "secret", "list"]
        )
        .stdout,
        b"FOUR\nONE\nTHREE\nTWO\n"
    );
}

#[test]
fn a_long_lived_filter_does_not_retain_key_access_after_explicit_lock() {
    use std::io::{Read, Write};
    fn send(output: &mut impl Write, bytes: &[u8]) {
        write!(output, "{:04x}", bytes.len() + 4).unwrap();
        output.write_all(bytes).unwrap();
    }
    fn section(input: &mut impl Read) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let mut length = [0; 4];
            input.read_exact(&mut length).unwrap();
            let length = usize::from_str_radix(std::str::from_utf8(&length).unwrap(), 16).unwrap();
            if length == 0 {
                return bytes;
            }
            let mut packet = vec![0; length - 4];
            input.read_exact(&mut packet).unwrap();
            bytes.extend(packet);
        }
    }
    let (root, _) = repository();
    let mut child = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args(["git", "filter-process"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    send(&mut input, b"git-filter-client\n");
    send(&mut input, b"version=2\n");
    input.write_all(b"0000").unwrap();
    input.flush().unwrap();
    assert_eq!(section(&mut output), b"git-filter-server\nversion=2\n");
    send(&mut input, b"capability=clean\n");
    send(&mut input, b"capability=smudge\n");
    input.write_all(b"0000").unwrap();
    input.flush().unwrap();
    assert_eq!(
        section(&mut output),
        b"capability=clean\ncapability=smudge\n"
    );
    for locked in [false, true] {
        if locked {
            over(root.path(), &["lock"]);
        }
        send(&mut input, b"command=clean\n");
        send(&mut input, b"pathname=token\n");
        input.write_all(b"0000").unwrap();
        send(&mut input, b"process sentinel");
        input.write_all(b"0000").unwrap();
        input.flush().unwrap();
        if locked {
            assert_eq!(section(&mut output), b"status=error\n");
        } else {
            assert_eq!(section(&mut output), b"status=success\n");
            over::crypt::format::inspect(&section(&mut output)).unwrap();
            assert!(section(&mut output).is_empty());
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn noninteractive_init_retains_ciphertext_until_explicit_identity_unlock() {
    let (root, identities) = repository();
    fs::write(root.path().join("over.toml"), "target = '~'\n").unwrap();
    fs::write(root.path().join("token"), b"init sentinel").unwrap();
    over(root.path(), &["crypt", "add", "token"]);
    git(root.path(), &["add", "over.toml"]);
    git(root.path(), &["commit", "-qm", "protected"]);
    let clone = tempfile::tempdir().unwrap();
    over(
        root.path(),
        &[
            "--home",
            clone.path().to_str().unwrap(),
            "init",
            root.path().to_str().unwrap(),
        ],
    );
    over::crypt::format::inspect(&fs::read(clone.path().join("token")).unwrap()).unwrap();
    assert!(!clone.path().join(".git/over-crypt/key").exists());
    over(
        root.path(),
        &[
            "--home",
            clone.path().to_str().unwrap(),
            "init",
            "--unlock",
            "--identity",
            identities.path().join("alice.age").to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(clone.path().join("token")).unwrap(),
        b"init sentinel"
    );
}

#[test]
fn storage_diagnostics_find_index_leaks_without_mutation_or_value_disclosure() {
    let (root, _) = repository();
    fs::write(root.path().join("token"), b"working sentinel").unwrap();
    over(root.path(), &["crypt", "add", "token"]);
    git(root.path(), &["commit", "-qm", "protected"]);
    let repo = git2::Repository::open(root.path()).unwrap();
    let mut index = repo.index().unwrap();
    let mut entry = index.get_path(Path::new("token"), 0).unwrap();
    entry.id = repo.blob(b"leaked sentinel").unwrap();
    index.add(&entry).unwrap();
    index.write().unwrap();
    let before = fs::read(root.path().join(".git/index")).unwrap();
    let report = over(root.path(), &["crypt", "list"]);
    let report = String::from_utf8(report.stdout).unwrap();
    assert!(report.contains("ERROR token index"));
    assert!(report.contains("token HEAD: ciphertext (authenticated)"));
    assert!(report.contains("working-tree: plaintext is expected"));
    assert!(!report.contains("leaked sentinel"));
    assert!(!report.contains("working sentinel"));
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), before);
    assert_eq!(
        fs::read(root.path().join("token")).unwrap(),
        b"working sentinel"
    );
    fs::write(root.path().join(".git/info/attributes"), "token -filter\n").unwrap();
    assert!(
        String::from_utf8_lossy(&over(root.path(), &["crypt", "list"]).stdout)
            .contains("effective attributes disable declared protection")
    );
}

#[test]
fn detaching_one_key_keeps_the_other_independently_usable_without_rotation() {
    let (root, identities) = repository();
    over(
        root.path(),
        &[
            "user",
            "key",
            "add",
            "alice",
            "--name",
            "backup",
            "--identity-output",
            identities.path().join("backup.age").to_str().unwrap(),
        ],
    );
    fs::write(root.path().join("token"), b"detached sentinel").unwrap();
    over(root.path(), &["crypt", "add", "token"]);
    let before = git(root.path(), &["show", ":token"]).stdout;
    over(root.path(), &["user", "key", "detach", "alice", "default"]);
    assert_eq!(before, git(root.path(), &["show", ":token"]).stdout);
    over(root.path(), &["lock"]);
    over(
        root.path(),
        &[
            "unlock",
            "--identity",
            identities.path().join("backup.age").to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(root.path().join("token")).unwrap(),
        b"detached sentinel"
    );
    let rejected = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args(["user", "key", "detach", "alice", "backup"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
}

#[test]
fn generating_a_private_identity_cannot_publish_it_inside_a_working_tree() {
    let (root, _) = repository();
    let before = fs::read(root.path().join(".over/crypt/access.json")).unwrap();
    let output = root.path().join(".over/private.age");
    let rejected = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args([
            "user",
            "key",
            "add",
            "alice",
            "--name",
            "unsafe",
            "--identity-output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(!output.exists());
    assert_eq!(
        fs::read(root.path().join(".over/crypt/access.json")).unwrap(),
        before
    );
}

#[test]
fn one_damaged_envelope_does_not_block_an_independent_trusted_key() {
    let (root, identities) = repository();
    over(
        root.path(),
        &[
            "user",
            "add",
            "aardvark",
            "--identity-output",
            identities.path().join("alternate.age").to_str().unwrap(),
        ],
    );
    let path = root.path().join(".over/crypt/access.json");
    let mut registry: over::crypt::access::Registry =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    registry
        .users
        .get_mut("aardvark")
        .unwrap()
        .get_mut("default")
        .unwrap()
        .envelope = b"corrupt envelope".to_vec();
    fs::write(path, serde_json::to_vec(&registry).unwrap()).unwrap();
    over(root.path(), &["lock"]);
    over(
        root.path(),
        &[
            "unlock",
            "--identity",
            identities.path().join("alice.age").to_str().unwrap(),
        ],
    );
    assert!(root.path().join(".git/over-crypt/key").exists());
}

fn over(root: &Path, args: &[&str]) -> Output {
    run(root, &cargo_bin("over"), args)
}
fn git(root: &Path, args: &[&str]) -> Output {
    run(root, Path::new("git"), args)
}

fn repository() -> (tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let identities = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["config", "user.name", "Crypt test"]);
    git(
        root.path(),
        &["config", "user.email", "crypt@example.invalid"],
    );
    over(
        root.path(),
        &[
            "user",
            "add",
            "alice",
            "--identity-output",
            identities.path().join("alice.age").to_str().unwrap(),
        ],
    );
    (root, identities)
}

#[test]
fn git_filters_are_stable_and_lock_never_allows_plaintext_staging() {
    let (root, identities) = repository();
    fs::write(root.path().join(".env"), b"TOKEN=sentinel\0\xff\n").unwrap();
    over(root.path(), &["crypt", "add", ".env"]);
    let first = git(root.path(), &["show", ":.env"]).stdout;
    over::crypt::format::inspect(&first).unwrap();
    git(root.path(), &["add", ".env"]);
    assert_eq!(first, git(root.path(), &["show", ":.env"]).stdout);
    git(root.path(), &["add", ".over", ".gitattributes"]);
    git(root.path(), &["commit", "-qm", "protected"]);
    assert!(
        git(root.path(), &["status", "--porcelain"])
            .stdout
            .is_empty()
    );
    over(root.path(), &["lock"]);
    assert_eq!(fs::read(root.path().join(".env")).unwrap(), first);
    fs::write(root.path().join(".env"), b"new plaintext sentinel").unwrap();
    let rejected = Command::new("git")
        .current_dir(root.path())
        .args(["add", ".env"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert_eq!(first, git(root.path(), &["show", ":.env"]).stdout);
    fs::write(root.path().join(".env"), &first).unwrap();
    over(
        root.path(),
        &[
            "unlock",
            "--identity",
            identities.path().join("alice.age").to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(root.path().join(".env")).unwrap(),
        b"TOKEN=sentinel\0\xff\n"
    );
}

#[test]
fn protecting_an_existing_plaintext_commit_replaces_its_index_blob() {
    let (root, _) = repository();
    fs::write(root.path().join("existing"), "known exposure").unwrap();
    git(root.path(), &["add", "existing"]);
    git(root.path(), &["commit", "-qm", "legacy plaintext"]);
    over(root.path(), &["crypt", "add", "existing"]);
    over::crypt::format::inspect(&git(root.path(), &["show", ":existing"]).stdout).unwrap();
    let report = over(root.path(), &["crypt", "list"]);
    assert!(String::from_utf8_lossy(&report.stdout).contains("ERROR existing HEAD"));
    assert_eq!(
        fs::read_to_string(root.path().join("existing")).unwrap(),
        "known exposure"
    );
}
