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
    cmd.args(["add", target_file.to_str().unwrap(), "dev", "--dry-run"]);
    cmd.assert().success();
    Ok(())
}
