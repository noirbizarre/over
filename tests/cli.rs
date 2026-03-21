use std::{error::Error, fs};

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

fn setup_overlay_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let overlay_dir = tmp.path().join("dev");
    fs::create_dir_all(&overlay_dir).unwrap();
    // minimal overlay marker file expected by pattern()
    fs::write(overlay_dir.join("over.toml"), b"[overlay]\nname='dev'\n").unwrap();
    tmp
}

#[test]
fn runs() -> TestResult {
    let mut cmd = Command::cargo_bin("over")?;
    cmd.assert().success();
    Ok(())
}

#[test]
fn list_overlays() -> TestResult {
    let repo = setup_overlay_repo();
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.arg("list");
    cmd.assert().success().stdout(contains("dev"));
    Ok(())
}

#[test]
fn show_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args(["show", "dev"]);
    cmd.assert().success().stdout(contains("dev"));
    Ok(())
}

#[test]
fn apply_overlay_dry_run() -> TestResult {
    let repo = setup_overlay_repo();
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args(["apply", "dev", "--dry-run"]);
    cmd.assert().success();
    Ok(())
}

#[test]
fn add_file_to_overlay_dry_run() -> TestResult {
    let repo = setup_overlay_repo();
    // create a dummy target file that would be added
    let target_file = repo.path().join("sample.txt");
    fs::write(&target_file, b"hello")?;
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args([
        "add",
        target_file.to_str().unwrap(),
        "-o",
        "dev",
        "--root",
        repo.path().to_str().unwrap(),
        "--dry-run",
    ]);
    cmd.assert().success();
    Ok(())
}

#[test]
fn add_multiple_files_dry_run() -> TestResult {
    let repo = setup_overlay_repo();
    let file_a = repo.path().join("a.txt");
    let file_b = repo.path().join("b.txt");
    fs::write(&file_a, b"aaa")?;
    fs::write(&file_b, b"bbb")?;
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args([
        "add",
        file_a.to_str().unwrap(),
        file_b.to_str().unwrap(),
        "-o",
        "dev",
        "--root",
        repo.path().to_str().unwrap(),
        "--dry-run",
    ]);
    cmd.assert().success();
    Ok(())
}

#[test]
fn add_directory_dry_run() -> TestResult {
    let repo = setup_overlay_repo();
    let dir = repo.path().join("mydir");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("file.txt"), b"content")?;
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args([
        "add",
        dir.to_str().unwrap(),
        "-o",
        "dev",
        "--root",
        repo.path().to_str().unwrap(),
        "--dry-run",
    ]);
    cmd.assert().success();
    Ok(())
}

#[test]
fn add_nonexistent_file_fails() -> TestResult {
    let repo = setup_overlay_repo();
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args([
        "add",
        "/tmp/nonexistent_file_xyz.txt",
        "-o",
        "dev",
        "--root",
        repo.path().to_str().unwrap(),
    ]);
    cmd.assert().failure();
    Ok(())
}

// ── lint integration tests ──────────────────────────────────────────────

#[test]
fn lint_clean_overlay() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("clean");
    fs::create_dir_all(&ov)?;
    fs::write(ov.join("over.toml"), b"target = \"~\"")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("lint")
        .assert()
        .success()
        .stdout(contains("No issues found"));
    Ok(())
}

#[test]
fn lint_with_warnings_exits_zero() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("redundant");
    fs::create_dir_all(&ov)?;
    fs::write(ov.join("over.toml"), b"target = \"~\"\nexclude = []")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("lint")
        .assert()
        .success()
        .stdout(contains("warning"))
        .stdout(contains("1 warning"));
    Ok(())
}

#[test]
fn lint_with_errors_exits_nonzero() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("broken");
    fs::create_dir_all(&ov)?;
    fs::write(ov.join("over.toml"), b"this is not valid toml [[[")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("lint")
        .assert()
        .failure()
        .stdout(contains("error"))
        .stdout(contains("1 error"));
    Ok(())
}

#[test]
fn lint_uses_nonexistent_overlay() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("badref");
    fs::create_dir_all(&ov)?;
    fs::write(
        ov.join("over.toml"),
        b"target = \"~\"\nuses = [\"nonexistent\"]",
    )?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("lint")
        .assert()
        .failure()
        .stdout(contains("error"))
        .stdout(contains("non-existent overlay"));
    Ok(())
}

#[test]
fn lint_cycle_detection() -> TestResult {
    let tmp = TempDir::new()?;
    let a = tmp.path().join("a");
    fs::create_dir_all(&a)?;
    fs::write(a.join("over.toml"), b"target = \"~\"\nuses = [\"b\"]")?;
    let b = tmp.path().join("b");
    fs::create_dir_all(&b)?;
    fs::write(b.join("over.toml"), b"target = \"~\"\nuses = [\"a\"]")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("lint")
        .assert()
        .failure()
        .stdout(contains("cycle detected"));
    Ok(())
}

// ── show integration tests ──────────────────────────────────────────────

#[test]
fn show_overlay_with_target() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("myoverlay");
    fs::create_dir_all(&ov)?;
    fs::write(
        ov.join("over.toml"),
        b"target = \"/home/user\"\ndescription = \"My test overlay\"",
    )?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["show", "myoverlay"])
        .assert()
        .success()
        .stdout(contains("myoverlay"))
        .stdout(contains("/home/user"))
        .stdout(contains("My test overlay"));
    Ok(())
}

#[test]
fn show_nonexistent_overlay_fails() -> TestResult {
    let tmp = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["show", "does_not_exist"])
        .assert()
        .failure();
    Ok(())
}
