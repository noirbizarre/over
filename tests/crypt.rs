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

fn replace_secret(root: &Path, value: &[u8]) -> Output {
    use std::io::Write;
    let mut child = Command::new(cargo_bin("over"))
        .current_dir(root)
        .env_remove("OVER_HOME")
        .args([
            "--home",
            root.to_str().unwrap(),
            "secret",
            "add",
            "TOKEN",
            "--replace",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(value).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn native_mutations_commit_only_the_store_and_preserve_unrelated_staging() {
    let (root, _) = repository();
    fs::write(root.path().join("unrelated"), b"staged work").unwrap();
    git(root.path(), &["add", "unrelated"]);
    add_secret(root.path(), "TOKEN", b"first\0\xff");
    assert_eq!(
        git(root.path(), &["log", "-1", "--format=%s"]).stdout,
        b"chore(secrets): added TOKEN\n"
    );
    let paths = git(root.path(), &["ls-tree", "-r", "--name-only", "HEAD"]).stdout;
    assert!(!String::from_utf8_lossy(&paths).contains("unrelated"));
    assert_eq!(
        git(root.path(), &["diff", "--cached", "--name-only"]).stdout,
        b"unrelated\n"
    );
    let stored = git(root.path(), &["show", "HEAD:.over/secrets"]).stdout;
    over::crypt::format::inspect(&stored).unwrap();
    assert_eq!(stored, fs::read(root.path().join(".over/secrets")).unwrap());
    let head = git(root.path(), &["rev-parse", "HEAD"]).stdout;
    assert!(replace_secret(root.path(), b"first\0\xff").status.success());
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]).stdout, head);
    assert!(replace_secret(root.path(), b"second").status.success());
    assert_eq!(
        git(root.path(), &["log", "-1", "--format=%s"]).stdout,
        b"chore(secrets): replaced TOKEN\n"
    );
    over(
        root.path(),
        &[
            "--home",
            root.path().to_str().unwrap(),
            "secret",
            "remove",
            "TOKEN",
        ],
    );
    assert_eq!(
        git(root.path(), &["log", "-1", "--format=%s"]).stdout,
        b"chore(secrets): removed TOKEN\n"
    );
    assert_eq!(
        git(root.path(), &["diff", "--cached", "--name-only"]).stdout,
        b"unrelated\n"
    );
}

#[test]
fn custom_messages_are_used_without_committing_the_descriptor() {
    let (root, _) = repository();
    fs::write(root.path().join("over.toml"), "[secrets.commit_messages]\nadd = 'chore: stored a credential'\nreplace = '{{ missing }}'\n").unwrap();
    add_secret(root.path(), "TOKEN", b"first");
    assert_eq!(
        git(root.path(), &["log", "-1", "--format=%s"]).stdout,
        b"chore: stored a credential\n"
    );
    assert!(
        !String::from_utf8_lossy(
            &git(root.path(), &["ls-tree", "-r", "--name-only", "HEAD"]).stdout
        )
        .contains("over.toml")
    );
    let stored = fs::read(root.path().join(".over/secrets")).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]).stdout;
    assert!(!replace_secret(root.path(), b"second").status.success());
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]).stdout, head);
    assert_eq!(fs::read(root.path().join(".over/secrets")).unwrap(), stored);
}

#[cfg(unix)]
#[test]
fn a_rejected_secret_commit_leaves_the_store_index_and_head_unchanged() {
    use std::os::unix::fs::PermissionsExt;
    let (root, _) = repository();
    add_secret(root.path(), "TOKEN", b"first");
    let stored = fs::read(root.path().join(".over/secrets")).unwrap();
    let index = fs::read(root.path().join(".git/index")).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]).stdout;
    let hook = root.path().join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!replace_secret(root.path(), b"second").status.success());
    assert_eq!(fs::read(root.path().join(".over/secrets")).unwrap(), stored);
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), index);
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]).stdout, head);
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
    over::crypt::format::inspect(&fs::read(root.path().join(".over/secrets")).unwrap()).unwrap();
    over::crypt::format::inspect(&git(root.path(), &["show", "HEAD:.over/secrets"]).stdout)
        .unwrap();
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
fn repeated_narrow_additions_keep_policy_minimal_and_index_blobs_encrypted() {
    let (root, _) = repository();
    for name in ["first.env", "second.env"] {
        fs::write(root.path().join(name), name).unwrap();
    }
    for (selection, protected) in [
        ("first.env", vec!["first.env"]),
        ("second.env", vec!["first.env", "second.env"]),
        ("first.env", vec!["first.env", "second.env"]),
    ] {
        over(root.path(), &["crypt", "add", selection]);
        let policy = fs::read_to_string(root.path().join(".gitattributes")).unwrap();
        assert!(
            policy
                .lines()
                .all(|line| line.ends_with("filter=over diff=over -text"))
        );
        assert!(!policy.contains(".gitattributes"));
        assert!(!policy.contains(".over/crypt"));
        for name in protected {
            over::crypt::format::inspect(&git(root.path(), &["show", &format!(":{name}")]).stdout)
                .unwrap();
            assert_eq!(fs::read(root.path().join(name)).unwrap(), name.as_bytes());
        }
    }
}

#[test]
fn policy_updates_preserve_existing_comments_rules_and_legacy_guards() {
    let (root, _) = repository();
    let original = "# User policy\n*.md diff=markdown\n.gitattributes -filter !diff\n.over/crypt/** -filter !diff -text\n# A legacy duplicate stays in its original position\n.gitattributes -filter !diff\n.over/crypt/** -filter !diff -text\n";
    fs::write(root.path().join(".gitattributes"), original).unwrap();
    fs::write(root.path().join("token"), "secret").unwrap();
    over(root.path(), &["crypt", "add", "token"]);
    assert_eq!(
        fs::read_to_string(root.path().join(".gitattributes")).unwrap(),
        format!("{original}\"/token\" filter=over diff=over -text\n")
    );
    over::crypt::format::inspect(&git(root.path(), &["show", ":token"]).stdout).unwrap();
}

#[rstest::rstest]
#[case("* filter=over diff=over -text\n", None, ".gitattributes")]
#[case(
    "* filter=over diff=over -text\n",
    Some(".gitattributes -filter\n"),
    ".gitattributes"
)]
#[case("nested/** filter=over -text\n", None, "nested/.gitattributes")]
#[case("", Some(".over/crypt/** filter=over\n"), ".over/crypt/access.json")]
fn unsafe_bootstrap_policy_fails_before_policy_index_or_plaintext_publication(
    #[case] policy: &str,
    #[case] local: Option<&str>,
    #[case] offending: &str,
) {
    let (root, _) = repository();
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(
        root.path().join("nested/.gitattributes"),
        "# nested policy\n",
    )
    .unwrap();
    // Establish an index before introducing unsafe policy, without storing the sentinel.
    git(root.path(), &["add", ".over"]);
    fs::write(root.path().join(".gitattributes"), policy).unwrap();
    if let Some(local) = local {
        fs::write(root.path().join(".git/info/attributes"), local).unwrap();
    }
    let plaintext = b"bootstrap rejection sentinel never stored";
    fs::write(root.path().join("token"), plaintext).unwrap();
    let before = fs::read(root.path().join(".git/index")).unwrap();
    let rejected = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args(["crypt", "add", "token"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    // Filesystem-discovered Windows paths use native separators in diagnostics.
    let diagnostic = String::from_utf8_lossy(&rejected.stderr).replace('\\', "/");
    assert!(diagnostic.contains(offending), "{diagnostic}");
    assert!(diagnostic.contains("-filter override"), "{diagnostic}");
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), before);
    assert_eq!(
        fs::read_to_string(root.path().join(".gitattributes")).unwrap(),
        policy
    );
    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, plaintext).unwrap();
    assert!(
        !git2::Repository::open(root.path())
            .unwrap()
            .odb()
            .unwrap()
            .exists(oid)
    );
}

#[test]
fn indexed_bootstrap_policy_is_checked_when_its_working_file_is_missing() {
    let (root, _) = repository();
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(
        root.path().join("nested/.gitattributes"),
        "# indexed policy\n",
    )
    .unwrap();
    git(root.path(), &["add", "nested/.gitattributes"]);
    fs::remove_file(root.path().join("nested/.gitattributes")).unwrap();
    let policy = "nested/** filter=over -text\n";
    fs::write(root.path().join(".gitattributes"), policy).unwrap();
    fs::write(root.path().join("token"), "secret").unwrap();
    let before = fs::read(root.path().join(".git/index")).unwrap();
    let rejected = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args(["crypt", "add", "token"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    let diagnostic = String::from_utf8_lossy(&rejected.stderr).replace('\\', "/");
    assert!(diagnostic.contains("nested/.gitattributes"), "{diagnostic}");
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), before);
    assert_eq!(
        fs::read_to_string(root.path().join(".gitattributes")).unwrap(),
        policy
    );
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
fn unstaged_bootstrap_exemptions_cannot_conceal_unsafe_indexed_policy(#[case] tracked: bool) {
    let (root, _) = repository();
    fs::create_dir(root.path().join("nested")).unwrap();
    let nested = root.path().join("nested/.gitattributes");
    fs::write(&nested, "# policy without an exemption\n").unwrap();
    git(root.path(), &["add", ".over"]);
    if tracked {
        git(root.path(), &["add", "nested/.gitattributes"]);
    }
    fs::write(&nested, ".gitattributes -filter !diff\n").unwrap();
    let policy = "nested/** filter=over diff=over -text\n";
    fs::write(root.path().join(".gitattributes"), policy).unwrap();
    let plaintext = b"unstaged exemption sentinel never stored";
    fs::write(root.path().join("nested/token"), plaintext).unwrap();
    let before = fs::read(root.path().join(".git/index")).unwrap();
    let rejected = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args(["crypt", "add", "nested/token"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    let diagnostic = String::from_utf8_lossy(&rejected.stderr).replace('\\', "/");
    assert!(diagnostic.contains("nested/.gitattributes"), "{diagnostic}");
    assert!(diagnostic.contains("-filter override"), "{diagnostic}");
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), before);
    assert_eq!(
        fs::read_to_string(root.path().join(".gitattributes")).unwrap(),
        policy
    );
    assert_eq!(
        fs::read_to_string(nested).unwrap(),
        ".gitattributes -filter !diff\n"
    );
    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, plaintext).unwrap();
    assert!(
        !git2::Repository::open(root.path())
            .unwrap()
            .odb()
            .unwrap()
            .exists(oid)
    );
}

#[rstest::rstest]
#[case("nested")]
#[case("*")]
#[case("**")]
#[case(".*")]
#[case(".over/**")]
#[case(".over/crypt/**")]
fn broad_selections_cannot_encrypt_bootstrap_files(#[case] selection: &str) {
    let (root, _) = repository();
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(root.path().join("nested/.gitattributes"), "# policy\n").unwrap();
    fs::write(root.path().join("nested/token"), "secret").unwrap();
    let rejected = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args(["crypt", "add", selection])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("select narrower paths"));
    assert!(!root.path().join(".gitattributes").exists());
}

#[test]
fn nested_administrative_policy_preserves_bootstrap_metadata_under_broad_root_rules() {
    let (root, _) = repository();
    let original = "* filter=over diff=over -text\n/.gitattributes -filter !diff\n";
    fs::write(root.path().join(".gitattributes"), original).unwrap();
    fs::write(root.path().join("token"), "secret").unwrap();
    for _ in 0..2 {
        over(root.path(), &["crypt", "add", "token"]);
        over::crypt::format::inspect(&git(root.path(), &["show", ":token"]).stdout).unwrap();
        for path in [".over/.gitattributes", ".over/crypt/access.json"] {
            assert_eq!(
                git(
                    root.path(),
                    &["check-attr", "-z", "filter", "diff", "--", path]
                )
                .stdout,
                format!("{path}\0filter\0unset\0{path}\0diff\0unspecified\0").as_bytes()
            );
            assert_eq!(
                git(root.path(), &["show", &format!(":{path}")]).stdout,
                fs::read(root.path().join(path)).unwrap()
            );
        }
        assert!(
            !fs::read_to_string(root.path().join(".gitattributes"))
                .unwrap()
                .contains(".over/crypt/**")
        );
    }
    git(root.path(), &["commit", "-qm", "safe broad policy"]);
    let repo = git2::Repository::open(root.path()).unwrap();
    over::crypt::detect::inspect(&repo)
        .unwrap()
        .validate_checkout()
        .unwrap();
}

#[test]
fn an_old_aggregate_is_read_without_migration_and_renamed_by_the_next_mutation() {
    let (root, _) = repository();
    add_secret(root.path(), "TOKEN", b"original\0\xff");
    git(root.path(), &["mv", ".over/secrets", ".over/secrets.yaml"]);
    // Reconstruct a repository created before the extensionless rule existed.
    let attributes = root.path().join(".over/.gitattributes");
    let policy = fs::read_to_string(&attributes)
        .unwrap()
        .replace("secrets filter=over diff=over -text\n", "");
    fs::write(&attributes, policy).unwrap();
    git(root.path(), &["add", ".over/.gitattributes"]);
    git(root.path(), &["commit", "-qm", "old layout"]);
    let old = fs::read(root.path().join(".over/secrets.yaml")).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]).stdout;
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
        b"original\0\xff"
    );
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]).stdout, head);
    assert!(root.path().join(".over/secrets.yaml").is_file());
    assert!(
        replace_secret(root.path(), b"original\0\xff")
            .status
            .success()
    );
    assert!(!root.path().join(".over/secrets.yaml").exists());
    assert_eq!(fs::read(root.path().join(".over/secrets")).unwrap(), old);
    assert_eq!(
        git(
            root.path(),
            &["diff", "--no-renames", "HEAD^", "HEAD", "--name-only"]
        )
        .stdout,
        b".over/.gitattributes\n.over/secrets\n.over/secrets.yaml\n"
    );
    assert!(
        git(root.path(), &["status", "--porcelain"])
            .stdout
            .is_empty()
    );
}

fn legacy_secret_repository() -> (tempfile::TempDir, tempfile::TempDir) {
    let (root, identities) = repository();
    let legacy = age::scrypt::Recipient::new("migration credential".into());
    fs::create_dir(root.path().join(".over/secrets")).unwrap();
    fs::write(
        root.path().join(".over/secrets/TOKEN.age"),
        age::encrypt(&legacy, b"original\0\xff").unwrap(),
    )
    .unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "legacy secrets"]);
    (root, identities)
}

fn mutate_legacy_secret(root: &Path) -> Output {
    use std::io::Write;
    let mut child = Command::new(cargo_bin("over"))
        .current_dir(root)
        .env_remove("OVER_HOME")
        .env("OVER_SECRET", "migration credential")
        .args([
            "--home",
            root.to_str().unwrap(),
            "secret",
            "add",
            "TOKEN",
            "--replace",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"updated\0\xff")
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn a_secret_mutation_migrates_the_legacy_directory_in_the_same_commit() {
    let (root, _) = legacy_secret_repository();
    let original = fs::read(root.path().join(".over/secrets/TOKEN.age")).unwrap();
    let result = mutate_legacy_secret(root.path());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(root.path().join(".over/secrets").is_file());
    assert_eq!(
        git(root.path(), &["log", "-1", "--format=%s"]).stdout,
        b"chore(secrets): replaced TOKEN\n"
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
        b"updated\0\xff"
    );
    let backups: Vec<_> = fs::read_dir(root.path().join(".git/over-crypt"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("legacy-secrets-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].join("TOKEN.age")).unwrap(), original);
    assert!(
        git(root.path(), &["status", "--porcelain"])
            .stdout
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn rejected_ref_publication_restores_the_legacy_directory_and_index() {
    use std::os::unix::fs::PermissionsExt;
    let (root, _) = legacy_secret_repository();
    let original = fs::read(root.path().join(".over/secrets/TOKEN.age")).unwrap();
    let index = fs::read(root.path().join(".git/index")).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]).stdout;
    let hook = root.path().join(".git/hooks/reference-transaction");
    fs::write(
        &hook,
        "#!/bin/sh\nif [ \"$1\" = prepared ]; then exit 1; fi\n",
    )
    .unwrap();
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    let result = mutate_legacy_secret(root.path());
    assert!(!result.status.success());
    assert_eq!(
        fs::read(root.path().join(".over/secrets/TOKEN.age")).unwrap(),
        original
    );
    assert_eq!(fs::read(root.path().join(".git/index")).unwrap(), index);
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]).stdout, head);
    assert!(
        !root
            .path()
            .join(".git/over-crypt/transaction.json")
            .exists()
    );
}

#[test]
fn competing_aggregate_paths_are_rejected_without_mutating_either() {
    let (root, _) = repository();
    add_secret(root.path(), "TOKEN", b"original");
    let bytes = fs::read(root.path().join(".over/secrets")).unwrap();
    fs::write(root.path().join(".over/secrets.yaml"), &bytes).unwrap();
    assert!(!replace_secret(root.path(), b"updated").status.success());
    assert_eq!(fs::read(root.path().join(".over/secrets")).unwrap(), bytes);
    assert_eq!(
        fs::read(root.path().join(".over/secrets.yaml")).unwrap(),
        bytes
    );
}

#[test]
fn explicit_policy_removal_stages_plaintext_without_changing_working_content() {
    let (root, _) = repository();
    fs::write(root.path().join("token"), b"deliberate plaintext").unwrap();
    over(root.path(), &["crypt", "add", "token"]);
    over(root.path(), &["crypt", "remove", "token", "--yes"]);
    let policy = fs::read_to_string(root.path().join(".gitattributes")).unwrap();
    assert_eq!(
        policy,
        "\"/token\" filter=over diff=over -text\n\"/token\" -filter !diff -text\n"
    );
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
    let (root, identities) = repository();
    let repo = git2::Repository::open(root.path()).unwrap();
    let ciphertext = over::crypt::access::local_key(&repo)
        .unwrap()
        .encrypt(b"process sentinel", 0)
        .unwrap();
    let mut child = Command::new(cargo_bin("over"))
        .current_dir(root.path())
        .env_remove("OVER_HOME")
        .args(["crypt", "filter-process"])
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
    for (transition, locked) in [false, true, false].into_iter().enumerate() {
        if locked {
            over(root.path(), &["lock"]);
        } else if transition > 0 {
            over(
                root.path(),
                &[
                    "unlock",
                    "--identity",
                    identities.path().join("alice.age").to_str().unwrap(),
                ],
            );
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
        // Failed clean requests must not desynchronize subsequent ciphertext requests.
        for command in [b"command=smudge\n".as_slice(), b"command=clean\n"] {
            send(&mut input, command);
            send(&mut input, b"pathname=other-token\n");
            input.write_all(b"0000").unwrap();
            send(&mut input, &ciphertext);
            input.write_all(b"0000").unwrap();
            input.flush().unwrap();
            assert_eq!(section(&mut output), b"status=success\n");
            let expected = if locked || command == b"command=clean\n" {
                ciphertext.as_slice()
            } else {
                b"process sentinel"
            };
            assert_eq!(section(&mut output), expected);
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
