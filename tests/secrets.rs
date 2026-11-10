use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const VALUE: &[u8] = b"secret-value-sentinel\0\n";
const PASSPHRASE: &str = "secret-passphrase-sentinel";

fn command(home: &Path) -> Command {
    let mut command = Command::cargo_bin("over").unwrap();
    command
        .args(["--home"])
        .arg(home)
        .arg("secret")
        .env("OVER_SECRET", PASSPHRASE)
        .env("NO_COLOR", "1");
    command
}

#[test]
fn named_secrets_round_trip_replace_and_remove_without_disclosing_values() {
    let home = TempDir::new().unwrap();
    command(home.path())
        .args(["list"])
        .env_remove("OVER_SECRET")
        .assert()
        .success()
        .stdout("");
    assert!(!home.path().join(".over").exists());
    command(home.path())
        .args(["add", "token"])
        .write_stdin(VALUE)
        .assert()
        .success()
        .stdout("");
    let path = home.path().join(".over/secrets/token.age");
    let original = fs::read(&path).unwrap();
    assert!(original.starts_with(b"age-encryption.org/v1\n"));
    assert!(!original.windows(VALUE.len()).any(|bytes| bytes == VALUE));
    command(home.path())
        .args(["list"])
        .env_remove("OVER_SECRET")
        .assert()
        .success()
        .stdout("token\n");
    command(home.path())
        .args(["get", "token"])
        .assert()
        .success()
        .stdout(VALUE);
    command(home.path())
        .args(["add", "token"])
        .write_stdin("replacement")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--replace"));
    assert_eq!(fs::read(&path).unwrap(), original);
    command(home.path())
        .args(["add", "token", "--replace"])
        .write_stdin("replacement")
        .assert()
        .success();
    command(home.path())
        .args(["get", "token"])
        .assert()
        .success()
        .stdout("replacement");
    command(home.path())
        .args(["remove", "token"])
        .env_remove("OVER_SECRET")
        .assert()
        .success();
    assert!(!path.exists());
}

#[test]
fn repositories_are_isolated_and_wrong_credentials_release_no_plaintext() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    command(first.path())
        .args(["add", "token"])
        .write_stdin(VALUE)
        .assert()
        .success();
    command(second.path())
        .args(["list"])
        .assert()
        .success()
        .stdout("");
    command(second.path())
        .args(["get", "token"])
        .assert()
        .failure()
        .stdout("");
    let output = command(first.path())
        .args(["get", "token"])
        .env("OVER_SECRET", "wrong")
        .assert()
        .failure()
        .stdout("")
        .get_output()
        .clone();
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(!diagnostics.contains("sentinel"));
    command(first.path())
        .args(["get", "token"])
        .env_remove("OVER_SECRET")
        .assert()
        .failure()
        .stdout("")
        .stderr(predicate::str::contains("OVER_SECRET"));
    command(first.path())
        .args(["get", "token"])
        .env("OVER_SECRET", "")
        .assert()
        .failure()
        .stdout("")
        .stderr(predicate::str::contains("OVER_SECRET"));
}

#[rstest::rstest]
#[case("../escape")]
#[case("/absolute")]
#[case("a/b")]
#[case("a\\b")]
#[case("a.age")]
#[case("CON")]
#[case("LPT1")]
#[case("")]
fn invalid_names_are_rejected_before_input_or_storage(#[case] name: &str) {
    let home = TempDir::new().unwrap();
    command(home.path())
        .args(["add", name])
        .write_stdin(VALUE)
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid secret name"));
    assert!(!home.path().join(".over").exists());
}

#[test]
fn failed_replacement_keeps_the_original_ciphertext_and_debug_output_is_redacted() {
    let home = TempDir::new().unwrap();
    command(home.path())
        .args(["add", "token"])
        .write_stdin(VALUE)
        .assert()
        .success();
    let path = home.path().join(".over/secrets/token.age");
    let original = fs::read(&path).unwrap();
    command(home.path())
        .args(["--debug", "add", "token", "--replace"])
        .env_remove("OVER_SECRET")
        .write_stdin(VALUE)
        .assert()
        .failure()
        .stdout("")
        .stderr(predicate::str::contains("sentinel").not());
    assert_eq!(fs::read(&path).unwrap(), original);
    let mut damaged = original;
    damaged.pop();
    fs::write(path, damaged).unwrap();
    command(home.path())
        .args(["get", "token"])
        .assert()
        .failure()
        .stdout("")
        .stderr(predicate::str::contains("sentinel").not());
}

#[cfg(unix)]
#[test]
fn symlinked_stores_and_secret_entries_cannot_escape_the_repository() {
    let home = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), home.path().join(".over")).unwrap();
    command(home.path())
        .args(["add", "token"])
        .write_stdin(VALUE)
        .assert()
        .failure();
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    fs::remove_file(home.path().join(".over")).unwrap();
    fs::create_dir_all(home.path().join(".over/secrets")).unwrap();
    let other = outside.path().join("secret");
    fs::write(&other, "untouched").unwrap();
    std::os::unix::fs::symlink(&other, home.path().join(".over/secrets/token.age")).unwrap();
    for operation in ["get", "remove"] {
        command(home.path())
            .args([operation, "token"])
            .assert()
            .failure()
            .stdout("");
    }
    assert_eq!(fs::read(other).unwrap(), b"untouched");
}
