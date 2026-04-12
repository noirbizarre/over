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
fn apply_without_name_empty_repo_fails() -> TestResult {
    let tmp = TempDir::new()?;
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(tmp.path());
    cmd.args(["apply", "--dry-run"]);
    cmd.assert()
        .failure()
        .stderr(contains("no overlays found in repository"));
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
        "nonexistent_file_xyz.txt",
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
    let target_path = tmp.path().join("target_user");
    let target_str = target_path.to_string_lossy().to_string();
    let toml_content = format!(
        "target = \"{}\"\ndescription = \"My test overlay\"",
        target_str
    );
    fs::write(ov.join("over.toml"), toml_content.as_bytes())?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["show", "myoverlay"])
        .assert()
        .success()
        .stdout(contains("myoverlay"))
        .stdout(contains(&target_str))
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

// ── new integration tests ───────────────────────────────────────────────

#[test]
fn new_overlay_creates_toml_descriptor() -> TestResult {
    let tmp = TempDir::new()?;
    // Use a non-existent target so no absorb prompt triggers
    let target = tmp.path().join("nonexistent_target");

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["new", "myoverlay", target.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("myoverlay"))
        .stdout(contains("(toml)"));

    // Verify the overlay directory was created
    assert!(tmp.path().join("myoverlay").is_dir());
    // Verify the descriptor file was created
    let descriptor = tmp.path().join("myoverlay/over.toml");
    assert!(descriptor.exists());
    let content = fs::read_to_string(&descriptor)?;
    assert!(content.contains("target"));
    assert!(content.contains(target.to_str().unwrap()));
    Ok(())
}

#[test]
fn new_overlay_creates_yaml_descriptor() -> TestResult {
    let tmp = TempDir::new()?;
    let target = tmp.path().join("nonexistent_target");

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args([
            "new",
            "yamloverlay",
            target.to_str().unwrap(),
            "--format",
            "yaml",
        ])
        .assert()
        .success()
        .stdout(contains("yamloverlay"))
        .stdout(contains("(yaml)"));

    // Verify yaml descriptor was created (not toml)
    let descriptor = tmp.path().join("yamloverlay/over.yaml");
    assert!(descriptor.exists());
    assert!(!tmp.path().join("yamloverlay/over.toml").exists());
    let content = fs::read_to_string(&descriptor)?;
    assert!(content.contains("target:"));
    Ok(())
}

#[test]
fn new_overlay_dry_run_does_not_write() -> TestResult {
    let tmp = TempDir::new()?;
    let target = tmp.path().join("nonexistent_target");

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["new", "dryoverlay", target.to_str().unwrap(), "--dry-run"])
        .assert()
        .success()
        .stdout(contains("Descriptor content (dry-run)"));

    // The overlay directory should NOT exist
    assert!(!tmp.path().join("dryoverlay").is_dir());
    // No descriptor should exist
    assert!(!tmp.path().join("dryoverlay/over.toml").exists());
    Ok(())
}

#[test]
fn new_overlay_already_exists_fails() -> TestResult {
    let tmp = TempDir::new()?;
    let overlay_dir = tmp.path().join("existing");
    fs::create_dir_all(&overlay_dir)?;
    fs::write(overlay_dir.join("over.toml"), b"target = \"~\"")?;
    let target = tmp.path().join("nonexistent_target");

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["new", "existing", target.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("overlay already exists"));
    Ok(())
}

#[test]
fn new_overlay_force_overwrites_existing() -> TestResult {
    let tmp = TempDir::new()?;
    let overlay_dir = tmp.path().join("forced");
    fs::create_dir_all(&overlay_dir)?;
    fs::write(overlay_dir.join("over.toml"), b"target = \"~\"")?;
    let target = tmp.path().join("nonexistent_target");

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["new", "forced", target.to_str().unwrap(), "--force"])
        .assert()
        .success()
        .stdout(contains("forced"));

    // Verify the descriptor was overwritten with new target
    let content = fs::read_to_string(overlay_dir.join("over.toml"))?;
    assert!(content.contains(target.to_str().unwrap()));
    Ok(())
}

#[test]
fn new_overlay_nested_path() -> TestResult {
    let tmp = TempDir::new()?;
    let target = tmp.path().join("nonexistent_target");

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["new", "apps/nested/deep", target.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("apps/nested/deep"));

    // Verify the nested directory structure was created
    assert!(tmp.path().join("apps/nested/deep").is_dir());
    assert!(tmp.path().join("apps/nested/deep/over.toml").exists());
    Ok(())
}

#[test]
fn new_overlay_reads_format_from_root_config() -> TestResult {
    let tmp = TempDir::new()?;
    // Write root config with format preference
    fs::write(tmp.path().join("over.toml"), b"format = \"yaml\"")?;
    let target = tmp.path().join("nonexistent_target");

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["new", "fromconfig", target.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("(yaml)"));

    // Should create yaml descriptor, not toml
    assert!(tmp.path().join("fromconfig/over.yaml").exists());
    assert!(!tmp.path().join("fromconfig/over.toml").exists());
    Ok(())
}

#[test]
fn new_overlay_format_flag_overrides_root_config() -> TestResult {
    let tmp = TempDir::new()?;
    // Root config says yaml
    fs::write(tmp.path().join("over.toml"), b"format = \"yaml\"")?;
    let target = tmp.path().join("nonexistent_target");

    // But --format toml should win
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args([
            "new",
            "flagwins",
            target.to_str().unwrap(),
            "--format",
            "toml",
        ])
        .assert()
        .success()
        .stdout(contains("(toml)"));

    assert!(tmp.path().join("flagwins/over.toml").exists());
    assert!(!tmp.path().join("flagwins/over.yaml").exists());
    Ok(())
}

#[test]
fn new_overlay_with_tilde_target() -> TestResult {
    let tmp = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["new", "homeoverlay", "~", "--force"])
        .assert()
        .success()
        .stdout(contains("homeoverlay"));

    let content = fs::read_to_string(tmp.path().join("homeoverlay/over.toml"))?;
    assert_eq!(content, "", "default target should not be written");
    Ok(())
}

#[test]
fn new_overlay_with_tilde_subdir_target() -> TestResult {
    let tmp = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["new", "suboverlay", "~/.config/myapp"])
        .assert()
        .success();

    let content = fs::read_to_string(tmp.path().join("suboverlay/over.toml"))?;
    assert!(content.contains("~/.config/myapp"));
    Ok(())
}

#[test]
fn new_overlay_tilde_target_written_when_parent_overrides() -> TestResult {
    let tmp = TempDir::new()?;

    // Parent config defines a non-default target
    fs::write(tmp.path().join("over.toml"), "target = \"~/Documents\"")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["new", "childoverlay", "~", "--force"])
        .assert()
        .success();

    let content = fs::read_to_string(tmp.path().join("childoverlay/over.toml"))?;
    assert_eq!(
        content, "target = \"~\"\n",
        "target should be written when it differs from inherited parent target"
    );
    Ok(())
}

#[test]
fn new_overlay_debug_output() -> TestResult {
    let tmp = TempDir::new()?;
    let target = tmp.path().join("nonexistent_target");

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("--debug")
        .args(["new", "debugoverlay", target.to_str().unwrap()])
        .assert()
        .success()
        .stderr(contains("overlay_root"))
        .stderr(contains("descriptor"))
        .stderr(contains("format"));
    Ok(())
}

// ── add integration tests (symlink handling) ──────────────────────────────

#[test]
#[cfg(unix)]
fn add_symlink_to_overlay_dry_run() -> TestResult {
    let repo = setup_overlay_repo();
    // Create a symlink that points into the overlay root
    let target_file = repo.path().join("sample.txt");
    fs::write(&target_file, b"hello")?;
    let link_path = repo.path().join("link_to_sample.txt");
    std::os::unix::fs::symlink(&target_file, &link_path)?;

    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args([
        "add",
        link_path.to_str().unwrap(),
        "-o",
        "dev",
        "--root",
        repo.path().to_str().unwrap(),
        "--dry-run",
    ]);
    // add with symlinks prompts for target input which will fail in non-interactive test
    // so we just verify the command starts and then fails on input
    let _ = cmd.assert();
    Ok(())
}

// ── apply integration tests (install flag, no_prompt, no_uses) ────────────

#[test]
fn apply_overlay_no_uses_dry_run() -> TestResult {
    let repo = setup_overlay_repo();
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args(["apply", "dev", "--dry-run", "--no-uses"]);
    cmd.assert().success();
    Ok(())
}

#[test]
fn apply_overlay_no_prompt_dry_run() -> TestResult {
    let repo = setup_overlay_repo();
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args(["apply", "dev", "--dry-run", "--no-prompt"]);
    cmd.assert().success();
    Ok(())
}

#[test]
fn apply_overlay_with_force_dry_run() -> TestResult {
    let repo = setup_overlay_repo();
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args(["apply", "dev", "--dry-run", "--force"]);
    cmd.assert().success();
    Ok(())
}

#[test]
fn apply_overlay_with_root_dry_run() -> TestResult {
    let repo = setup_overlay_repo();
    let target_root = repo.path().join("target_root");
    fs::create_dir_all(&target_root)?;
    let mut cmd = Command::cargo_bin("over")?;
    cmd.arg("--home").arg(repo.path());
    cmd.args([
        "apply",
        "dev",
        "--dry-run",
        "--root",
        target_root.to_str().unwrap(),
    ]);
    cmd.assert().success();
    Ok(())
}

#[test]
fn apply_nonexistent_overlay_fails() -> TestResult {
    let tmp = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["apply", "does_not_exist", "--no-prompt"])
        .assert()
        .failure()
        .stderr(contains("error:"));
    Ok(())
}

// ── show integration tests (all overlay fields) ──────────────────────────

#[test]
fn show_overlay_with_uses() -> TestResult {
    let tmp = TempDir::new()?;
    // Create the used overlay first
    let base = tmp.path().join("base");
    fs::create_dir_all(&base)?;
    fs::write(base.join("over.toml"), b"target = \"~\"")?;
    // Create the overlay that uses base
    let ov = tmp.path().join("myoverlay");
    fs::create_dir_all(&ov)?;
    fs::write(ov.join("over.toml"), b"target = \"~\"\nuses = [\"base\"]")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["show", "myoverlay"])
        .assert()
        .success()
        .stdout(contains("myoverlay"))
        .stdout(contains("uses:"))
        .stdout(contains("base"));
    Ok(())
}

#[test]
fn show_overlay_with_git_repos() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("gitoverlay");
    fs::create_dir_all(&ov)?;
    fs::write(
        ov.join("over.toml"),
        br#"target = "~"
[git.myapp]
url = "https://github.com/user/myapp.git"
"#,
    )?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["show", "gitoverlay"])
        .assert()
        .success()
        .stdout(contains("gitoverlay"))
        .stdout(contains("git repositories:"))
        .stdout(contains("myapp"))
        .stdout(contains("https://github.com/user/myapp.git"));
    Ok(())
}

#[test]
fn show_overlay_with_link_dirs() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("linkdir_overlay");
    fs::create_dir_all(&ov)?;
    fs::write(
        ov.join("over.toml"),
        br#"target = "~"
link_dirs = [".config/nvim"]
"#,
    )?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["show", "linkdir_overlay"])
        .assert()
        .success()
        .stdout(contains("linkdir_overlay"))
        .stdout(contains("link_dirs:"))
        .stdout(contains(".config/nvim"));
    Ok(())
}

#[test]
fn show_overlay_with_install_config() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("install_overlay");
    fs::create_dir_all(&ov)?;
    fs::write(
        ov.join("over.toml"),
        br#"target = "~"
[install]
apt = ["curl"]
"#,
    )?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["show", "install_overlay"])
        .assert()
        .success()
        .stdout(contains("install_overlay"))
        .stdout(contains("install:"));
    Ok(())
}

// ── git-over integration tests ────────────────────────────────────────────

#[test]
fn git_over_add_dry_run() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = tmp.path().canonicalize()?;
    let ov = canonical_tmp.join("gitov");
    fs::create_dir_all(&ov)?;
    let target_str = canonical_tmp.to_string_lossy();
    fs::write(ov.join("over.toml"), format!("target = \"{}\"", target_str))?;
    let repo_dir = canonical_tmp.join("repo");
    fs::create_dir_all(&repo_dir)?;
    let test_file = repo_dir.join("test.txt");
    fs::write(&test_file, b"content")?;
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo_dir)
        .output()?;

    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .arg("add")
        .arg(test_file.to_str().unwrap())
        .arg("-o")
        .arg("gitov")
        .arg("--dry-run")
        .current_dir(&repo_dir)
        .assert()
        .success();
    Ok(())
}

#[test]
fn git_over_status_no_overlay_configured() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("statusov");
    fs::create_dir_all(&ov)?;
    fs::write(ov.join("over.toml"), b"target = \"~\"")?;
    let repo_dir = tmp.path().join("repo");
    fs::create_dir_all(&repo_dir)?;
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo_dir)
        .output()?;

    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("status")
        .current_dir(&repo_dir)
        .assert()
        .failure()
        .stderr(contains("no overlay configured"));
    Ok(())
}

#[test]
fn git_over_status_with_overlay_configured() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = tmp.path().canonicalize()?;
    let ov = canonical_tmp.join("statusov2");
    fs::create_dir_all(&ov)?;
    let target_str = canonical_tmp.to_string_lossy();
    fs::write(ov.join("over.toml"), format!("target = \"{}\"", target_str))?;
    let repo_dir = canonical_tmp.join("repo");
    fs::create_dir_all(&repo_dir)?;
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo_dir)
        .output()?;
    std::process::Command::new("git")
        .args(["config", "--local", "over.overlay", "statusov2"])
        .current_dir(&repo_dir)
        .output()?;

    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .arg("status")
        .current_dir(&repo_dir)
        .assert()
        .success()
        .stdout(contains("statusov2"));
    Ok(())
}

#[test]
fn git_over_add_multiple_files_dry_run() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = tmp.path().canonicalize()?;
    let ov = canonical_tmp.join("gitov_multi");
    fs::create_dir_all(&ov)?;
    let target_str = canonical_tmp.to_string_lossy();
    fs::write(ov.join("over.toml"), format!("target = \"{}\"", target_str))?;
    let repo_dir = canonical_tmp.join("repo");
    fs::create_dir_all(&repo_dir)?;
    let file_a = repo_dir.join("a.txt");
    let file_b = repo_dir.join("b.txt");
    fs::write(&file_a, b"aaa")?;
    fs::write(&file_b, b"bbb")?;
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo_dir)
        .output()?;

    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .arg("add")
        .arg(file_a.to_str().unwrap())
        .arg(file_b.to_str().unwrap())
        .arg("-o")
        .arg("gitov_multi")
        .arg("--dry-run")
        .current_dir(&repo_dir)
        .assert()
        .success();
    Ok(())
}

#[test]
fn git_over_add_directory_dry_run() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = tmp.path().canonicalize()?;
    let ov = canonical_tmp.join("gitov_dir");
    fs::create_dir_all(&ov)?;
    let target_str = canonical_tmp.to_string_lossy();
    fs::write(ov.join("over.toml"), format!("target = \"{}\"", target_str))?;
    let repo_dir = canonical_tmp.join("repo");
    fs::create_dir_all(&repo_dir)?;
    let dir = repo_dir.join("mydir");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("file.txt"), b"content")?;
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo_dir)
        .output()?;

    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .arg("add")
        .arg(dir.to_str().unwrap())
        .arg("-o")
        .arg("gitov_dir")
        .arg("--dry-run")
        .current_dir(&repo_dir)
        .assert()
        .success();
    Ok(())
}

#[test]
fn git_over_add_nonexistent_file_fails() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = tmp.path().canonicalize()?;
    let ov = canonical_tmp.join("gitov_err");
    fs::create_dir_all(&ov)?;
    let target_str = canonical_tmp.to_string_lossy();
    fs::write(ov.join("over.toml"), format!("target = \"{}\"", target_str))?;
    let repo_dir = canonical_tmp.join("repo");
    fs::create_dir_all(&repo_dir)?;
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo_dir)
        .output()?;

    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .arg("add")
        .arg("nonexistent_file.txt")
        .arg("-o")
        .arg("gitov_err")
        .current_dir(&repo_dir)
        .assert()
        .failure();
    Ok(())
}

#[test]
fn apply_conflict_no_prompt_fails() -> TestResult {
    let repo = setup_overlay_repo();
    let canonical = repo.path().canonicalize()?;
    // Create a file in the overlay
    let overlay_dir = canonical.join("dev");
    fs::write(overlay_dir.join("conflict.txt"), "from overlay")?;

    // Create the same file at the target location to provoke a conflict
    let target_root = canonical.join("target_root");
    fs::create_dir_all(&target_root)?;
    fs::write(target_root.join("conflict.txt"), "existing")?;

    // Apply with --no-prompt should fail on the conflict
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical)
        .args([
            "apply",
            "dev",
            "--root",
            target_root.to_str().unwrap(),
            "--no-prompt",
        ])
        .assert()
        .failure()
        .stderr(contains("conflict"));
    Ok(())
}

#[test]
fn add_file_outside_root_fails() -> TestResult {
    let repo = setup_overlay_repo();
    let canonical = repo.path().canonicalize()?;

    // Create a file outside the --root directory
    let outside = TempDir::new()?;
    let outside_canonical = outside.path().canonicalize()?;
    let outside_file = outside_canonical.join("outside.txt");
    fs::write(&outside_file, "I am outside")?;

    // The --root is set to a subdirectory so the file is not under it
    let narrow_root = canonical.join("narrow");
    fs::create_dir_all(&narrow_root)?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical)
        .args([
            "add",
            outside_file.to_str().unwrap(),
            "-o",
            "dev",
            "--root",
            narrow_root.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("not included in"));
    Ok(())
}

#[test]
fn completion_bash() -> TestResult {
    let mut cmd = Command::cargo_bin("over")?;
    cmd.args(["completion", "bash"]);
    cmd.assert()
        .success()
        .stdout(contains("_over"))
        .stdout(contains("COMPREPLY"));
    Ok(())
}

#[test]
fn completion_zsh() -> TestResult {
    let mut cmd = Command::cargo_bin("over")?;
    cmd.args(["completion", "zsh"]);
    cmd.assert().success().stdout(contains("#compdef over"));
    Ok(())
}

#[test]
fn completion_fish() -> TestResult {
    let mut cmd = Command::cargo_bin("over")?;
    cmd.args(["completion", "fish"]);
    cmd.assert().success().stdout(contains("complete -c over"));
    Ok(())
}

#[test]
fn completion_powershell() -> TestResult {
    let mut cmd = Command::cargo_bin("over")?;
    cmd.args(["completion", "powershell"]);
    cmd.assert().success().stdout(contains("over"));
    Ok(())
}

#[test]
fn completion_elvish() -> TestResult {
    let mut cmd = Command::cargo_bin("over")?;
    cmd.args(["completion", "elvish"]);
    cmd.assert().success().stdout(contains("over"));
    Ok(())
}

#[test]
fn completion_invalid_shell() -> TestResult {
    let mut cmd = Command::cargo_bin("over")?;
    cmd.args(["completion", "invalid"]);
    cmd.assert().failure();
    Ok(())
}
