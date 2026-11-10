use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const PASSPHRASE: &str = "overlay-passphrase-sentinel";
const VALUE: &[u8] = b"overlay-value-sentinel\0\n";

fn command(home: &Path, root: &Path, verb: &str) -> Command {
    let mut command = Command::cargo_bin("over").unwrap();
    command
        .arg("--home")
        .arg(home)
        .arg(verb)
        .arg("--target")
        .arg(root)
        .env("OVER_SECRET", PASSPHRASE)
        .env("NO_COLOR", "1");
    if let Some(parent) = root.parent() {
        command
            .env("XDG_STATE_HOME", parent.join("state"))
            .env("XDG_CACHE_HOME", parent.join("cache"));
    }
    command
}

fn fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("source");
    let root = temp.path().join("target");
    fs::create_dir_all(home.join("selected/config")).unwrap();
    fs::write(home.join("selected/over.yaml"), "target: '~'\n").unwrap();
    let recipient = age::scrypt::Recipient::new(PASSPHRASE.into());
    let ciphertext = age::encrypt(&recipient, VALUE).unwrap();
    fs::write(home.join("selected/config/token.age"), &ciphertext).unwrap();
    (temp, home, root, ciphertext)
}

#[test]
fn apply_status_and_unapply_share_the_ciphertext_recipe_without_modifying_the_source() {
    let (_temp, home, root, ciphertext) = fixture();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    let target = root.join("config/token");
    assert_eq!(fs::read(&target).unwrap(), VALUE);
    assert!(!target.is_symlink());
    assert!(!root.join("config/token.age").exists());
    assert_eq!(
        fs::read(home.join("selected/config/token.age")).unwrap(),
        ciphertext
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    command(&home, &root, "status")
        .args(["selected", "--verbose"])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied:"))
        .stdout(predicate::str::contains("overlay-value-sentinel").not());
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    command(&home, &root, "status")
        .arg("selected")
        .env_remove("OVER_SECRET")
        .assert()
        .failure()
        .stdout(predicate::str::contains("locked"));
    command(&home, &root, "unapply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    assert!(!target.exists());
    assert_eq!(
        fs::read(home.join("selected/config/token.age")).unwrap(),
        ciphertext
    );
}

#[test]
fn locked_previews_never_prompt_and_failed_authentication_precedes_all_writes() {
    let (_temp, home, root, ciphertext) = fixture();
    fs::write(home.join("selected/aaa"), "ordinary").unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--dry-run"])
        .env_remove("OVER_SECRET")
        .assert()
        .success();
    assert!(!root.exists());
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .env_remove("OVER_SECRET")
        .assert()
        .failure()
        .stderr(predicate::str::contains("OVER_SECRET"));
    assert!(!root.exists());
    command(&home, &root, "apply")
        .args(["selected", "--force", "--no-prompt"])
        .env("OVER_SECRET", "wrong")
        .assert()
        .failure()
        .stderr(predicate::str::contains("sentinel").not());
    assert!(!root.exists());
    assert_eq!(
        fs::read(home.join("selected/config/token.age")).unwrap(),
        ciphertext
    );
}

#[test]
fn edited_plaintext_is_preserved_by_apply_unapply_and_reverse_add() {
    let (_temp, home, root, ciphertext) = fixture();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    let target = root.join("config/token");
    fs::write(&target, "local edit sentinel").unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .failure();
    command(&home, &root, "unapply")
        .args(["selected", "--no-prompt"])
        .assert()
        .failure();
    assert_eq!(fs::read(&target).unwrap(), b"local edit sentinel");
    command(&home, &root, "add")
        .arg(&target)
        .args(["--overlay", "selected"])
        .assert()
        .failure();
    command(&home, &root, "add")
        .arg(root.join("config"))
        .args(["--overlay", "selected"])
        .assert()
        .failure();
    assert!(!home.join("selected/config/token").exists());
    assert_eq!(
        fs::read(home.join("selected/config/token.age")).unwrap(),
        ciphertext
    );
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt", "--force"])
        .assert()
        .success();
    assert_eq!(fs::read(target).unwrap(), VALUE);
}

#[rstest::rstest]
#[case("symlink-directory")]
#[case("checkout")]
fn terminal_modes_reject_encrypted_descendants_before_writing(#[case] mode: &str) {
    let (_temp, home, root, _) = fixture();
    fs::write(
        home.join("selected/over.yaml"),
        format!("target: '~'\nmode: {mode}\n"),
    )
    .unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "ordinary file-level materialization",
        ));
    assert!(!root.exists());
}

#[test]
fn decrypted_name_collisions_are_rejected_before_any_mutation() {
    let (_temp, home, root, _) = fixture();
    fs::write(home.join("selected/config/token"), "collision").unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .failure();
    assert!(!root.exists());
}

#[test]
fn repository_administration_is_never_deployed_even_when_ignore_rules_negate_it() {
    let (_temp, home, root, _) = fixture();
    fs::create_dir_all(home.join(".over/secrets")).unwrap();
    fs::write(home.join(".over/secrets/private.age"), "not overlay input").unwrap();
    fs::write(home.join("over.yaml"), "target: '~'\n").unwrap();
    fs::write(home.join(".overignore"), "!.over/\n!.over/**\n").unwrap();
    command(&home, &root, "apply")
        .args([".", "--no-prompt"])
        .assert()
        .success();
    assert!(!root.join(".over").exists());
    command(&home, &root, "apply")
        .args([".over", "--no-prompt"])
        .assert()
        .failure();
}

#[test]
fn mappings_and_permission_rules_use_the_decrypted_logical_filename() {
    let (_temp, home, root, _) = fixture();
    fs::write(home.join("selected/over.yaml"), "target: '~'\nmap:\n  config: {target: '~/mapped'}\nrules:\n  - {path: config/token, permissions: '640'}\n").unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    let target = root.join("mapped/token");
    assert_eq!(fs::read(&target).unwrap(), VALUE);
    assert!(!root.join("config").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        // A permission repair must not truncate/rewrite already authenticated content.
        let file = fs::File::open(&target).unwrap();
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        command(&home, &root, "apply")
            .args(["selected", "--no-prompt"])
            .assert()
            .success();
        assert_eq!(fs::metadata(&target).unwrap().modified().unwrap(), modified);
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

fn commit(repo: &git2::Repository, paths: &[&str]) {
    let mut index = repo.index().unwrap();
    for path in paths {
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Over tests", "over@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
}

#[test]
fn retained_decrypted_outputs_stay_excluded_and_diff_never_writes_plaintext_git_objects() {
    let (_temp, home, root, _) = fixture();
    let source_repo = git2::Repository::init(&home).unwrap();
    commit(
        &source_repo,
        &["selected/over.yaml", "selected/config/token.age"],
    );
    fs::create_dir_all(&root).unwrap();
    let target_repo = git2::Repository::init(&root).unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    assert!(
        target_repo
            .is_path_ignored(Path::new("config/token"))
            .unwrap()
    );
    fs::write(root.join("config/token"), "local edit sentinel").unwrap();
    command(&home, &root, "diff")
        .arg("selected")
        .assert()
        .success()
        .stdout(predicate::str::contains("sentinel").not())
        .stderr(predicate::str::contains("sentinel").not());
    command(&home, &root, "unapply")
        .args(["selected", "--no-prompt"])
        .assert()
        .failure();
    assert!(
        target_repo
            .is_path_ignored(Path::new("config/token"))
            .unwrap()
    );
    let sentinels = [VALUE, b"local edit sentinel".as_slice()];
    for repo in [&source_repo, &target_repo] {
        let odb = repo.odb().unwrap();
        odb.foreach(|oid| {
            let object = odb.read(*oid).unwrap();
            for sentinel in sentinels {
                assert!(
                    !object
                        .data()
                        .windows(sentinel.len())
                        .any(|bytes| bytes == sentinel)
                );
            }
            true
        })
        .unwrap();
    }
    for name in ["state", "cache"] {
        let path = root.parent().unwrap().join(name);
        if path.exists() {
            for entry in walkdir::WalkDir::new(path) {
                let entry = entry.unwrap();
                if entry.file_type().is_file() {
                    let content = fs::read(entry.path()).unwrap();
                    for sentinel in sentinels {
                        assert!(
                            !content
                                .windows(sentinel.len())
                                .any(|bytes| bytes == sentinel)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn already_tracked_plaintext_destinations_are_rejected_before_apply_writes() {
    let (_temp, home, root, _) = fixture();
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(root.join("config/token"), "tracked original").unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    commit(&repo, &["config/token"]);
    command(&home, &root, "apply")
        .args(["selected", "--force", "--no-prompt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already tracked"));
    assert_eq!(
        fs::read(root.join("config/token")).unwrap(),
        b"tracked original"
    );
}

#[rstest::rstest]
#[case("token[prod]")]
#[case("token key")]
#[case("token#key")]
#[case("token!key")]
fn encrypted_destinations_are_excluded_as_literal_git_paths(#[case] name: &str) {
    let (_temp, home, root, _) = fixture();
    fs::rename(
        home.join("selected/config/token.age"),
        home.join(format!("selected/config/{name}.age")),
    )
    .unwrap();
    fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(fs::read(root.join("config").join(name)).unwrap(), VALUE);
    assert!(
        repo.is_path_ignored(Path::new("config").join(name))
            .unwrap()
    );
}

#[test]
fn later_apply_failure_retains_exclusions_for_every_published_secret() {
    let (_temp, home, root, _) = fixture();
    fs::copy(
        home.join("selected/config/token.age"),
        home.join("selected/config/other.age"),
    )
    .unwrap();
    fs::write(home.join("selected/zzz"), "source").unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("zzz"), "foreign").unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .failure();
    for name in ["config/token", "config/other"] {
        assert_eq!(fs::read(root.join(name)).unwrap(), VALUE);
        assert!(repo.is_path_ignored(Path::new(name)).unwrap());
    }
}

#[test]
fn malformed_exclusions_prevent_plaintext_publication() {
    let (_temp, home, root, _) = fixture();
    fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    fs::create_dir_all(repo.path().join("info")).unwrap();
    let exclude = repo.path().join("info/exclude");
    let malformed = "# >>> over: exclude:selected >>>\n/stray\n";
    fs::write(&exclude, malformed).unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "malformed host Git exclusion block",
        ));
    assert!(!root.join("config/token").exists());
    assert_eq!(fs::read_to_string(exclude).unwrap(), malformed);
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
fn checkout_rejection_includes_ciphertext_missing_from_the_source_worktree(
    #[case] committed: bool,
) {
    let (_temp, home, root, _) = fixture();
    fs::write(
        home.join("selected/over.yaml"),
        "target: '~'\nmode: checkout\n",
    )
    .unwrap();
    let repo = git2::Repository::init(&home).unwrap();
    commit(
        &repo,
        if committed {
            &["selected/over.yaml", "selected/config/token.age"]
        } else {
            &["selected/over.yaml"]
        },
    );
    if !committed {
        let mut index = repo.index().unwrap();
        index
            .add_path(Path::new("selected/config/token.age"))
            .unwrap();
        index.write().unwrap();
    }
    fs::remove_file(home.join("selected/config/token.age")).unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "ordinary file-level materialization",
        ));
    assert!(!root.exists());
}

#[cfg(unix)]
#[test]
fn force_cannot_modify_other_names_through_hardlinks_or_symlinks() {
    let (_temp, home, root, _) = fixture();
    fs::create_dir_all(root.join("config")).unwrap();
    let outside = root.parent().unwrap().join("other");
    fs::write(&outside, "untouched").unwrap();
    let target = root.join("config/token");
    fs::hard_link(&outside, &target).unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--force", "--no-prompt"])
        .assert()
        .failure();
    assert_eq!(fs::read(&outside).unwrap(), b"untouched");
    fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink(&outside, &target).unwrap();
    command(&home, &root, "apply")
        .args(["selected", "--force", "--no-prompt"])
        .assert()
        .failure();
    assert_eq!(fs::read(outside).unwrap(), b"untouched");
}
