use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;

fn git(root: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn over(root: &Path, args: &[&str]) -> Output {
    Command::new(cargo_bin("over"))
        .current_dir(root)
        .env_remove("OVER_HOME")
        .env("XDG_STATE_HOME", root.join(".git/test-state"))
        .args(args)
        .output()
        .unwrap()
}

fn repository() -> (tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let identities = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["config", "user.name", "Ingestion test"]);
    git(
        root.path(),
        &["config", "user.email", "ingestion@example.invalid"],
    );
    let output = over(
        root.path(),
        &[
            "user",
            "add",
            "alice",
            "--identity-output",
            identities.path().join("alice.age").to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (root, identities)
}

#[rstest::rstest]
#[case("secret")]
#[case("over.toml")]
fn refused_crypt_ingestion_preserves_policy_and_index(#[case] name: &str) {
    let (root, _identities) = repository();
    let target = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("ov")).unwrap();
    fs::write(root.path().join("ov/.overignore"), "secret\n").unwrap();
    fs::write(target.path().join(name), "private sentinel").unwrap();
    git(root.path(), &["add", "."]);
    let before_index = fs::read(root.path().join(".git/index")).unwrap();
    let before_policy = fs::read(root.path().join(".gitattributes")).ok();
    let output = over(
        root.path(),
        &[
            "--home",
            root.path().to_str().unwrap(),
            "add",
            "--crypt",
            "--overlay",
            "ov",
            "--target",
            target.path().to_str().unwrap(),
            target.path().join(name).to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert_eq!(
        fs::read(root.path().join(".git/index")).unwrap(),
        before_index
    );
    assert_eq!(
        fs::read(root.path().join(".gitattributes")).ok(),
        before_policy
    );
    assert_eq!(
        fs::read(target.path().join(name)).unwrap(),
        b"private sentinel"
    );
}

#[rstest::rstest]
#[case("staged")]
#[case("unstaged")]
#[case("behind")]
fn crypt_ingestion_refuses_checkout_source_drift_before_publication(#[case] drift: &str) {
    let (root, _identities) = repository();
    let target = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("ov")).unwrap();
    fs::write(
        root.path().join("ov/over.toml"),
        "target = '~/checkout'\nmode = 'checkout'\n",
    )
    .unwrap();
    fs::write(root.path().join("ov/token"), "original").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "initial"]);
    assert!(
        over(
            root.path(),
            &[
                "--home",
                root.path().to_str().unwrap(),
                "apply",
                "ov",
                "--no-prompt",
                "--target",
                target.path().to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    fs::write(root.path().join("ov/token"), "source drift").unwrap();
    if drift != "unstaged" {
        git(root.path(), &["add", "ov/token"]);
    }
    if drift == "behind" {
        git(root.path(), &["commit", "-qm", "advance source"]);
    }
    fs::write(root.path().join("unrelated"), "preserve me").unwrap();
    git(root.path(), &["add", "unrelated"]);
    fs::write(target.path().join("checkout/token"), "target edit").unwrap();
    let before_index = fs::read(root.path().join(".git/index")).unwrap();
    let before_policy = fs::read(root.path().join(".gitattributes")).ok();
    let output = over(
        root.path(),
        &[
            "--home",
            root.path().to_str().unwrap(),
            "add",
            "--crypt",
            "--overlay",
            "ov",
            "--target",
            target.path().to_str().unwrap(),
            target.path().join("checkout/token").to_str().unwrap(),
        ],
    );
    assert!(!output.status.success(), "source drift must be refused");
    assert_eq!(
        fs::read(root.path().join(".git/index")).unwrap(),
        before_index
    );
    assert_eq!(
        fs::read(root.path().join(".gitattributes")).ok(),
        before_policy
    );
}

#[rstest::rstest]
#[case(".over/crypt/access.json")]
#[case(".over/.gitattributes")]
fn bootstrap_divergence_is_refused_without_replacing_the_index(#[case] metadata: &str) {
    let (root, _identities) = repository();
    fs::write(root.path().join("token"), "private sentinel").unwrap();
    git(root.path(), &["add", ".over"]);
    let mut changed = fs::read(root.path().join(metadata)).unwrap();
    changed.push(b'\n');
    fs::write(root.path().join(metadata), changed).unwrap();
    let before = fs::read(root.path().join(".git/index")).unwrap();
    let policy = fs::read(root.path().join(".gitattributes")).ok();
    let output = over(root.path(), &["crypt", "add", "token"]);
    assert!(!output.status.success());
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), before);
    assert_eq!(fs::read(root.path().join(".gitattributes")).ok(), policy);
}

#[test]
fn a_late_removal_override_does_not_publish_earlier_plaintext_objects() {
    let (root, _identities) = repository();
    fs::create_dir(root.path().join("nested")).unwrap();
    let sentinel = b"plaintext that must never enter the canonical ODB";
    fs::write(root.path().join("a"), sentinel).unwrap();
    fs::write(root.path().join("nested/z"), "another private value").unwrap();
    assert!(
        over(root.path(), &["crypt", "add", "a", "nested/z"])
            .status
            .success()
    );
    fs::write(root.path().join("nested/.gitattributes"), "z filter=over\n").unwrap();
    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, sentinel).unwrap();
    let repo = git2::Repository::open(root.path()).unwrap();
    assert!(!repo.odb().unwrap().exists(oid));
    let before = fs::read(root.path().join(".git/index")).unwrap();
    let output = over(root.path(), &["crypt", "remove", "a", "nested/z"]);
    assert!(!output.status.success());
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), before);
    assert!(!repo.odb().unwrap().exists(oid));
}

#[rstest::rstest]
#[case("new input")]
#[case("existing source")]
fn an_independent_ordinary_source_is_refused_before_policy_publication(#[case] input: &str) {
    let (root, _identities) = repository();
    let target = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("ov")).unwrap();
    fs::write(root.path().join("ov/token"), "existing source").unwrap();
    fs::write(target.path().join("token"), input).unwrap();
    git(root.path(), &["add", "."]);
    let before = fs::read(root.path().join(".git/index")).unwrap();
    let policy = fs::read(root.path().join(".gitattributes")).ok();
    let output = over(
        root.path(),
        &[
            "--home",
            root.path().to_str().unwrap(),
            "add",
            "--crypt",
            "--overlay",
            "ov",
            "--target",
            target.path().to_str().unwrap(),
            target.path().join("token").to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), before);
    assert_eq!(fs::read(root.path().join(".gitattributes")).ok(), policy);
    assert_eq!(
        fs::read(root.path().join("ov/token")).unwrap(),
        b"existing source"
    );
    assert_eq!(
        fs::read(target.path().join("token")).unwrap(),
        input.as_bytes()
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case(false)]
#[case(true)]
fn full_stage_rejects_untrusted_ciphertext_and_concurrent_index_changes(#[case] concurrent: bool) {
    let (root, _identities) = repository();
    let target = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("ov")).unwrap();
    fs::write(
        root.path().join("ov/over.toml"),
        "target = '~/checkout'\nmode = 'checkout'\n",
    )
    .unwrap();
    fs::write(root.path().join("ov/token"), "original").unwrap();
    if !concurrent {
        assert!(
            over(root.path(), &["crypt", "add", "ov/token"])
                .status
                .success()
        );
    }
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "initial"]);
    assert!(
        over(
            root.path(),
            &[
                "--home",
                root.path().to_str().unwrap(),
                "apply",
                "ov",
                "--no-prompt",
                "--target",
                target.path().to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    let sentinel = b"target bytes that must not become a plaintext object";
    fs::write(target.path().join("checkout/token"), sentinel).unwrap();
    let mut expected = fs::read(root.path().join(".git/index")).unwrap();
    if concurrent {
        let repo = git2::Repository::open(root.path()).unwrap();
        let oid = repo.blob(b"concurrent staged value").unwrap();
        git(
            root.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{oid},unrelated"),
            ],
        );
        expected = fs::read(root.path().join(".git/index")).unwrap();
        git(
            root.path(),
            &["update-index", "--force-remove", "unrelated"],
        );
        // Explicitly address the real index: clean filters normally inherit a private scratch index.
        let filter = format!(
            "GIT_INDEX_FILE='{}' git --git-dir='{}' update-index --add --cacheinfo '100644,{oid},unrelated'; cat",
            root.path().join(".git/index").display(),
            root.path().join(".git").display()
        );
        git(root.path(), &["config", "filter.race.clean", &filter]);
        fs::write(root.path().join("ov/.gitattributes"), "token filter=race\n").unwrap();
    } else {
        git(root.path(), &["config", "filter.over.process", ""]);
        git(root.path(), &["config", "filter.over.clean", "cat"]);
    }
    let output = over(
        root.path(),
        &[
            "--home",
            root.path().to_str().unwrap(),
            "add",
            "--overlay",
            "ov",
            "--target",
            target.path().to_str().unwrap(),
            target.path().join("checkout/token").to_str().unwrap(),
        ],
    );
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), expected);
    if !concurrent {
        let repo = git2::Repository::open(root.path()).unwrap();
        let oid = git2::Oid::hash_object(git2::ObjectType::Blob, sentinel).unwrap();
        assert!(!repo.odb().unwrap().exists(oid));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_failed_cross_device_move_rolls_back_policy_index_and_inputs() {
    use std::os::unix::fs::MetadataExt;

    let (root, _identities) = repository();
    let target = tempfile::tempdir_in("/dev/shm").unwrap();
    if fs::metadata(root.path()).unwrap().dev() == fs::metadata(target.path()).unwrap().dev() {
        return;
    }
    fs::create_dir(root.path().join("ov")).unwrap();
    fs::write(target.path().join("token"), "private input").unwrap();
    git(root.path(), &["add", "."]);
    let before = fs::read(root.path().join(".git/index")).unwrap();
    let policy = fs::read(root.path().join(".gitattributes")).ok();
    let output = over(
        root.path(),
        &[
            "--home",
            root.path().to_str().unwrap(),
            "add",
            "--crypt",
            "--overlay",
            "ov",
            "--target",
            target.path().to_str().unwrap(),
            target.path().join("token").to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), before);
    assert_eq!(fs::read(root.path().join(".gitattributes")).ok(), policy);
    assert_eq!(
        fs::read(target.path().join("token")).unwrap(),
        b"private input"
    );
    assert!(!root.path().join("ov/token").exists());
    assert!(
        !root
            .path()
            .join(".git/over-crypt/transaction.json")
            .exists()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_late_ingestion_failure_restores_original_input_inodes_and_access_metadata() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let (root, _identities) = repository();
    let target = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("ov/z")).unwrap();
    fs::create_dir(target.path().join("z")).unwrap();
    let first = target.path().join("a");
    fs::write(&first, b"first input").unwrap();
    fs::set_permissions(&first, fs::Permissions::from_mode(0o640)).unwrap();
    xattr::set(&first, "user.over-ingestion", b"original metadata").unwrap();
    let original = fs::metadata(&first).unwrap();
    fs::write(target.path().join("z/token"), b"second input").unwrap();
    git(root.path(), &["add", "."]);
    let index = fs::read(root.path().join(".git/index")).unwrap();
    fs::set_permissions(root.path().join("ov/z"), fs::Permissions::from_mode(0o555)).unwrap();
    let probe = root.path().join("ov/z/privilege-probe");
    if fs::write(&probe, b"probe").is_ok() {
        fs::remove_file(probe).unwrap();
        fs::set_permissions(root.path().join("ov/z"), fs::Permissions::from_mode(0o755)).unwrap();
        return; // Privileged runners cannot exercise this filesystem failure boundary.
    }
    let output = over(
        root.path(),
        &[
            "--home",
            root.path().to_str().unwrap(),
            "add",
            "--crypt",
            "--overlay",
            "ov",
            "--target",
            target.path().to_str().unwrap(),
            first.to_str().unwrap(),
            target.path().join("z/token").to_str().unwrap(),
        ],
    );
    fs::set_permissions(root.path().join("ov/z"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), index);
    assert!(!root.path().join(".gitattributes").exists());
    assert_eq!(fs::read(&first).unwrap(), b"first input");
    assert_eq!(
        fs::metadata(&first).unwrap().ino(),
        original.ino(),
        "rollback must move back the original input inode rather than recreate only its bytes"
    );
    assert_eq!(
        fs::metadata(&first).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        xattr::get(&first, "user.over-ingestion").unwrap().unwrap(),
        b"original metadata"
    );
    assert!(!root.path().join("ov/a").exists());
}
