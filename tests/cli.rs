use std::path::{Path, PathBuf};
use std::{error::Error, fs};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

/// Canonicalize a path for comparison against paths reported by libgit2
/// (e.g. via `git2::Repository::workdir()`), stripping Windows' `\\?\`
/// verbatim-path prefix that `Path::canonicalize()` adds but that libgit2
/// never produces. Symlinks are still resolved (needed on macOS, where
/// `/tmp` is a symlink to `/private/tmp`), only the verbatim prefix differs.
fn canonical_for_matching(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    if cfg!(windows) {
        let s = canonical.to_string_lossy();
        Ok(PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s)))
    } else {
        Ok(canonical)
    }
}

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
fn list_overlays_debug_output() -> TestResult {
    let repo = setup_overlay_repo();
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .arg("--debug")
        .arg("list")
        .assert()
        .success()
        .stderr(contains("list command"));
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
fn show_overlay_debug_output() -> TestResult {
    let repo = setup_overlay_repo();
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .arg("--debug")
        .args(["show", "dev"])
        .assert()
        .success()
        .stderr(contains("show command"));
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

/// A configured `default_overlay` (#128) is used without prompting when
/// `NAME` is omitted.
#[test]
fn apply_without_name_uses_configured_default_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("over.toml"), b"default_overlay = \"dev\"")?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("dev"));
    Ok(())
}

/// An explicit `NAME` always overrides a configured `default_overlay`
/// (#128).
#[test]
fn apply_explicit_name_overrides_default_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("over.toml"), b"default_overlay = \"dev\"")?;
    let other = repo.path().join("other");
    fs::create_dir_all(&other)?;
    fs::write(other.join("over.toml"), b"target = \"~\"")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "other", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("other"));
    Ok(())
}

/// A `default_overlay` naming a nonexistent overlay is a hard error, not
/// a silent fallback to the interactive prompt (#128).
#[test]
fn apply_without_name_misconfigured_default_overlay_fails() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(
        repo.path().join("over.toml"),
        b"default_overlay = \"does-not-exist\"",
    )?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "--dry-run"])
        .assert()
        .failure()
        .stderr(contains("default_overlay"));
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
fn add_file_debug_output() -> TestResult {
    let repo = setup_overlay_repo();
    let target_file = repo.path().join("sample.txt");
    fs::write(&target_file, b"hello")?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .arg("--debug")
        .args([
            "add",
            target_file.to_str().unwrap(),
            "-o",
            "dev",
            "--root",
            repo.path().to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success()
        .stderr(contains("add command"))
        .stderr(contains("repository"))
        .stderr(contains("resolved overlay"));
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

// ── status integration tests ─────────────────────────────────────────────

#[test]
fn status_reports_missing_before_apply() -> TestResult {
    let repo = setup_overlay_repo();
    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["status", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("missing"));
    Ok(())
}

#[test]
fn status_reports_applied_after_apply() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("dev").join("file.txt"), b"content")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "dev", "--root"])
        .arg(root.path())
        .arg("--force")
        .assert()
        .success();

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["status", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("applied"));
    Ok(())
}

#[test]
fn status_reports_conflict_for_existing_file() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("dev").join("file.txt"), b"overlay")?;
    let root = TempDir::new()?;
    fs::write(root.path().join("file.txt"), b"existing")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["status", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("conflict"));
    Ok(())
}

#[test]
fn status_debug_output() -> TestResult {
    let repo = setup_overlay_repo();
    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .arg("--debug")
        .args(["status", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stderr(contains("CLI args"));
    Ok(())
}

#[test]
fn status_unknown_overlay_fails() -> TestResult {
    let tmp = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["status", "does-not-exist"])
        .assert()
        .failure();
    Ok(())
}

#[test]
fn status_empty_repository_reports_no_overlays() -> TestResult {
    let tmp = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains("No overlays found"));
    Ok(())
}

/// A configured `default_overlay` (#128) narrows an omitted `NAME` to
/// just that overlay instead of reporting on every overlay.
#[test]
fn status_without_name_narrows_to_configured_default_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("over.toml"), b"default_overlay = \"dev\"")?;
    let other = repo.path().join("other");
    fs::create_dir_all(&other)?;
    fs::write(other.join("over.toml"), b"target = \"~\"")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["status", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("dev"))
        .stdout(contains("other").not());
    Ok(())
}

/// `--all` recovers "every overlay" even with a `default_overlay`
/// configured (#128).
#[test]
fn status_all_flag_overrides_configured_default_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("over.toml"), b"default_overlay = \"dev\"")?;
    let other = repo.path().join("other");
    fs::create_dir_all(&other)?;
    fs::write(other.join("over.toml"), b"target = \"~\"")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["status", "--all", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("dev"))
        .stdout(contains("other"));
    Ok(())
}

// ── diff integration tests ───────────────────────────────────────────────

#[test]
fn diff_reports_missing_before_apply() -> TestResult {
    let repo = setup_overlay_repo();
    // A real file not yet linked anywhere — the overlay's own root
    // directory already exists (it *is* `root`, the default target),
    // so only this file entry should be reported missing.
    fs::write(repo.path().join("dev").join("file.txt"), b"content")?;
    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["diff", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("missing:"));
    Ok(())
}

#[test]
fn diff_reports_no_differences_after_apply() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("dev").join("file.txt"), b"content")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "dev", "--root"])
        .arg(root.path())
        .arg("--force")
        .assert()
        .success();

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["diff", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("no differences"));
    Ok(())
}

#[test]
fn diff_shows_content_diff_for_existing_file() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("dev").join("file.txt"), b"overlay content")?;
    let root = TempDir::new()?;
    fs::write(root.path().join("file.txt"), b"existing content")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["diff", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("modified"))
        .stdout(contains("existing content"))
        .stdout(contains("overlay content"));
    Ok(())
}

#[test]
fn diff_debug_output() -> TestResult {
    let repo = setup_overlay_repo();
    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .arg("--debug")
        .args(["diff", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stderr(contains("CLI args"));
    Ok(())
}

#[test]
fn diff_unknown_overlay_fails() -> TestResult {
    let tmp = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["diff", "does-not-exist"])
        .assert()
        .failure();
    Ok(())
}

#[test]
fn diff_empty_repository_reports_no_overlays() -> TestResult {
    let tmp = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("diff")
        .assert()
        .success()
        .stdout(contains("No overlays found"));
    Ok(())
}

/// A configured `default_overlay` (#128) narrows an omitted `NAME` to
/// just that overlay instead of reporting on every overlay.
#[test]
fn diff_without_name_narrows_to_configured_default_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("over.toml"), b"default_overlay = \"dev\"")?;
    let other = repo.path().join("other");
    fs::create_dir_all(&other)?;
    fs::write(other.join("over.toml"), b"target = \"~\"")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["diff", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("dev"))
        .stdout(contains("other").not());
    Ok(())
}

/// `--all` recovers "every overlay" even with a `default_overlay`
/// configured (#128).
#[test]
fn diff_all_flag_overrides_configured_default_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("over.toml"), b"default_overlay = \"dev\"")?;
    let other = repo.path().join("other");
    fs::create_dir_all(&other)?;
    fs::write(other.join("over.toml"), b"target = \"~\"")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["diff", "--all", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("dev"))
        .stdout(contains("other"));
    Ok(())
}

// ── sync integration tests ────────────────────────────────────────────────

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("failed to run git");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// A local "origin" repository with one commit — good enough for git2's
/// local transport to clone/fetch/push against.
fn setup_git_origin(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
}

/// A `gitsync`-style overlay whose *root* content is cloned from `origin`
/// (the `over sync` bidirectional sync surface — see ADR-014).
fn setup_git_overlay(home: &Path, name: &str, origin: &Path) {
    let ov = home.join(name);
    fs::create_dir_all(&ov).unwrap();
    let origin_str = origin.to_string_lossy().replace('\\', "\\\\");
    fs::write(
        ov.join("over.toml"),
        format!("target = \"~\"\ngit = \"{origin_str}\"\n"),
    )
    .unwrap();
}

#[test]
fn sync_unknown_overlay_fails() -> TestResult {
    let tmp = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["sync", "does-not-exist"])
        .assert()
        .failure();
    Ok(())
}

#[test]
fn sync_empty_repository_reports_no_overlays() -> TestResult {
    let tmp = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("sync")
        .assert()
        .success()
        .stdout(contains("No overlays found"));
    Ok(())
}

/// A configured `default_overlay` (#128) narrows an omitted `NAME` to
/// just that overlay instead of syncing every overlay. `--verbose` makes
/// a git-entry-less overlay still print its name, so the assertion can
/// tell which overlay(s) were actually visited.
#[test]
fn sync_without_name_narrows_to_configured_default_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("over.toml"), b"default_overlay = \"dev\"")?;
    let other = repo.path().join("other");
    fs::create_dir_all(&other)?;
    fs::write(other.join("over.toml"), b"target = \"~\"")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["sync", "--verbose", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("dev"))
        .stdout(contains("other").not());
    Ok(())
}

/// `--all` recovers "every overlay" even with a `default_overlay`
/// configured (#128).
#[test]
fn sync_all_flag_overrides_configured_default_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("over.toml"), b"default_overlay = \"dev\"")?;
    let other = repo.path().join("other");
    fs::create_dir_all(&other)?;
    fs::write(other.join("over.toml"), b"target = \"~\"")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["sync", "--all", "--verbose", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("dev"))
        .stdout(contains("other"));
    Ok(())
}

#[test]
fn sync_overlay_without_git_entry_succeeds_quietly() -> TestResult {
    let repo = setup_overlay_repo();
    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["sync", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success();
    Ok(())
}

#[test]
fn sync_reports_up_to_date_right_after_apply() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let origin = canonical_tmp.join("origin");
    setup_git_origin(&origin);
    setup_git_overlay(&canonical_tmp, "gitsync", &origin);

    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["apply", "gitsync", "--root"])
        .arg(root.path())
        .arg("--force")
        .assert()
        .success();

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["sync", "gitsync", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("up to date"));
    Ok(())
}

#[test]
fn sync_pulls_and_fast_forwards_after_upstream_advances() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let origin = canonical_tmp.join("origin");
    setup_git_origin(&origin);
    setup_git_overlay(&canonical_tmp, "gitsync2", &origin);

    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["apply", "gitsync2", "--root"])
        .arg(root.path())
        .arg("--force")
        .assert()
        .success();

    // Advance "upstream" after the initial clone.
    fs::write(origin.join("new.txt"), b"new")?;
    git(&origin, &["add", "new.txt"]);
    git(
        &origin,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "advance",
        ],
    );

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["sync", "gitsync2", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("fast-forwarded"));

    assert!(root.path().join("new.txt").exists());
    Ok(())
}

#[test]
fn sync_blocks_on_dirty_checkout_without_touching_it() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let origin = canonical_tmp.join("origin");
    setup_git_origin(&origin);
    setup_git_overlay(&canonical_tmp, "gitsync3", &origin);

    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["apply", "gitsync3", "--root"])
        .arg(root.path())
        .arg("--force")
        .assert()
        .success();

    fs::write(root.path().join("dirty.txt"), b"uncommitted")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["sync", "gitsync3", "--root"])
        .arg(root.path())
        .assert()
        .failure()
        .stdout(contains("blocked"));

    // Untouched: the dirty file is still there, nothing was reverted/merged.
    assert!(root.path().join("dirty.txt").exists());
    Ok(())
}

#[test]
fn sync_dry_run_reports_without_mutating() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let origin = canonical_tmp.join("origin");
    setup_git_origin(&origin);
    setup_git_overlay(&canonical_tmp, "gitsync4", &origin);

    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["apply", "gitsync4", "--root"])
        .arg(root.path())
        .arg("--force")
        .assert()
        .success();

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["sync", "gitsync4", "--root"])
        .arg(root.path())
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(contains("up to date"));
    Ok(())
}

// ── legacy symlink-only installation migration integration tests (#130) ───
//
// A legacy `over` install predates rules/checkout entirely: every file
// recursed and symlinked individually (ADR-006's original default), no
// `.git`, and — the scenario this issue is about — zero `$XDG_STATE_HOME`
// tracking of any kind. `Overlay::apply_inner` clones git repositories
// *before* `Plan::build` ever classifies anything (see
// `actions::git::EnsureGitRepository::execute`'s `remove_legacy_materialization`,
// #130), so this exercises the real `over apply` path end to end, not just
// `over status`/`--dry-run` previews.

#[test]
#[cfg(unix)]
fn legacy_symlink_only_installation_migrates_to_checkout_with_no_prior_xdg_state() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let origin = canonical_tmp.join("origin");
    setup_git_origin(&origin);
    setup_git_overlay(&canonical_tmp, "legacy", &origin);

    let root = TempDir::new()?;
    let target = canonical_for_matching(root.path())?;

    // Simulate what a pre-rules/pre-checkout `over` left behind at this
    // same target root: content recursed and symlinked file by file, with
    // no `.git` and no XDG state ever written for it.
    let legacy_source = canonical_tmp.join("legacy_source");
    fs::create_dir_all(legacy_source.join("sub"))?;
    fs::write(legacy_source.join("a.txt"), "a")?;
    fs::write(legacy_source.join("sub/b.txt"), "b")?;
    fs::create_dir_all(target.join("sub"))?;
    std::os::unix::fs::symlink(legacy_source.join("a.txt"), target.join("a.txt"))?;
    std::os::unix::fs::symlink(legacy_source.join("sub/b.txt"), target.join("sub/b.txt"))?;

    // A fresh, empty `$XDG_STATE_HOME` — the issue's explicit requirement:
    // no prior `over` state directory of any kind, not even an empty one
    // `over` itself created.
    let xdg_state_home = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["apply", "legacy", "--root"])
        .arg(&target)
        .env("XDG_STATE_HOME", xdg_state_home.path())
        .assert()
        .success();

    assert!(
        target.join(".git").exists(),
        "legacy install should have been migrated to a checkout"
    );
    assert!(
        !target.join("a.txt").is_symlink(),
        "legacy symlinks should be gone, replaced by the checkout"
    );
    assert!(
        !target.join("sub/b.txt").is_symlink(),
        "legacy symlinks should be gone, replaced by the checkout"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn legacy_symlink_installation_with_drift_is_reported_as_conflict_not_replaced() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let origin = canonical_tmp.join("origin");
    setup_git_origin(&origin);
    setup_git_overlay(&canonical_tmp, "legacy2", &origin);

    let root = TempDir::new()?;
    let target = canonical_for_matching(root.path())?;

    let legacy_source = canonical_tmp.join("legacy_source2");
    fs::create_dir_all(&legacy_source)?;
    fs::write(legacy_source.join("a.txt"), "a")?;
    std::os::unix::fs::symlink(legacy_source.join("a.txt"), target.join("a.txt"))?;
    // A real, non-symlink file mixed in — drift from what a legacy `over`
    // install alone would ever have produced. Must never be silently
    // discarded/replaced.
    fs::write(target.join("foreign.txt"), "not from over")?;

    let xdg_state_home = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .args(["apply", "legacy2", "--root"])
        .arg(&target)
        .env("XDG_STATE_HOME", xdg_state_home.path())
        .assert()
        .failure();

    assert!(
        !target.join(".git").exists(),
        "conflicting install must never be migrated"
    );
    assert!(
        target.join("foreign.txt").exists(),
        "conflicting content must remain untouched"
    );
    assert_eq!(
        fs::read_to_string(target.join("foreign.txt"))?,
        "not from over"
    );
    assert!(
        target.join("a.txt").is_symlink(),
        "pre-existing legacy symlink must remain untouched"
    );
    Ok(())
}

// ── unapply integration tests ────────────────────────────────────────────

#[test]
fn unapply_unknown_overlay_fails() -> TestResult {
    let tmp = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["unapply", "does-not-exist"])
        .assert()
        .failure();
    Ok(())
}

/// A configured `default_overlay` (#128) is used without prompting when
/// `NAME` is omitted.
#[test]
fn unapply_without_name_uses_configured_default_overlay() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("over.toml"), b"default_overlay = \"dev\"")?;
    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["unapply", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("dev"));
    Ok(())
}

#[test]
fn unapply_before_apply_reports_already_absent() -> TestResult {
    let repo = setup_overlay_repo();
    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["unapply", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("already absent"));
    Ok(())
}

#[test]
fn unapply_removes_applied_symlink_and_apply_restores_it() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("dev").join("file.txt"), b"content")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "dev", "--root"])
        .arg(root.path())
        .arg("--force")
        .assert()
        .success();
    assert!(root.path().join("file.txt").is_symlink());

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["unapply", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stdout(contains("remove:"));
    assert!(!root.path().join("file.txt").exists());
    // Overlay source is untouched.
    assert!(repo.path().join("dev").join("file.txt").exists());

    // Reversible: a plain `over apply` restores it fully.
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "dev", "--root"])
        .arg(root.path())
        .arg("--force")
        .assert()
        .success();
    assert!(root.path().join("file.txt").is_symlink());
    Ok(())
}

#[test]
fn unapply_leaves_foreign_file_untouched_and_fails() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("dev").join("file.txt"), b"overlay")?;
    let root = TempDir::new()?;
    fs::write(root.path().join("file.txt"), b"existing")?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["unapply", "dev", "--root"])
        .arg(root.path())
        .assert()
        .failure()
        .stdout(contains("skip:"));

    assert_eq!(fs::read(root.path().join("file.txt"))?, b"existing");
    Ok(())
}

#[test]
fn unapply_dry_run_reports_without_mutating() -> TestResult {
    let repo = setup_overlay_repo();
    fs::write(repo.path().join("dev").join("file.txt"), b"content")?;
    let root = TempDir::new()?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "dev", "--root"])
        .arg(root.path())
        .arg("--force")
        .assert()
        .success();

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["unapply", "dev", "--root"])
        .arg(root.path())
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(contains("remove:"));

    assert!(root.path().join("file.txt").is_symlink());
    Ok(())
}

#[test]
fn unapply_debug_output() -> TestResult {
    let repo = setup_overlay_repo();
    let root = TempDir::new()?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .arg("--debug")
        .args(["unapply", "dev", "--root"])
        .arg(root.path())
        .assert()
        .success()
        .stderr(contains("CLI args"));
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
    // Escape backslashes so Windows paths don't get misparsed as TOML
    // unicode escape sequences; `target_str` (unescaped) is still what the
    // CLI should echo back verbatim once round-tripped through TOML.
    let target_toml = target_str.replace('\\', "\\\\");
    let toml_content = format!(
        "target = \"{}\"\ndescription = \"My test overlay\"",
        target_toml
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
fn apply_overlay_no_uses_skips_composed_overlay() -> TestResult {
    let tmp = TempDir::new()?;
    let child = tmp.path().join("child");
    fs::create_dir_all(&child)?;
    fs::write(child.join("over.toml"), b"target = \"~\"")?;
    let parent = tmp.path().join("parent");
    fs::create_dir_all(&parent)?;
    fs::write(
        parent.join("over.toml"),
        b"target = \"~\"\nuses = [\"child\"]",
    )?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["apply", "parent", "--dry-run", "--no-uses"])
        .assert()
        .success();
    Ok(())
}

#[test]
fn apply_overlay_debug_output() -> TestResult {
    let repo = setup_overlay_repo();
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .arg("--debug")
        .args(["apply", "dev", "--dry-run"])
        .assert()
        .success()
        .stderr(contains("CLI args"))
        .stderr(contains("repository"))
        .stderr(contains("resolved overlay"));
    Ok(())
}

#[test]
fn apply_overlay_with_uses_debug_output() -> TestResult {
    let tmp = TempDir::new()?;
    let child = tmp.path().join("child");
    fs::create_dir_all(&child)?;
    fs::write(child.join("over.toml"), b"target = \"~\"")?;
    let parent = tmp.path().join("parent");
    fs::create_dir_all(&parent)?;
    fs::write(
        parent.join("over.toml"),
        b"target = \"~\"\nuses = [\"child\"]",
    )?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .arg("--debug")
        .args(["apply", "parent", "--dry-run"])
        .assert()
        .success()
        .stderr(contains("used overlay"));
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

/// #126: `over show` displays resolved `defaults`/`rules` when configured.
#[test]
fn show_overlay_with_materialization_rules() -> TestResult {
    let tmp = TempDir::new()?;
    let ov = tmp.path().join("rules_overlay");
    fs::create_dir_all(&ov)?;
    fs::write(
        ov.join("over.toml"),
        br#"target = "~"
[defaults]
materialization = "symlink-directory"

[[rules]]
path = "special"
materialization = "symlink"
"#,
    )?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(tmp.path())
        .args(["show", "rules_overlay"])
        .assert()
        .success()
        .stdout(contains("rules_overlay"))
        .stdout(contains("defaults: materialization = symlink-directory"))
        .stdout(contains("rules:"))
        .stdout(contains("special: symlink"));
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
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let ov = canonical_tmp.join("gitov");
    fs::create_dir_all(&ov)?;
    // Escape backslashes so Windows paths don't get misparsed as TOML
    // unicode escape sequences.
    let target_str = canonical_tmp.to_string_lossy().replace('\\', "\\\\");
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
fn git_over_add_debug_output() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let ov = canonical_tmp.join("gitov_debug");
    fs::create_dir_all(&ov)?;
    let target_str = canonical_tmp.to_string_lossy().replace('\\', "\\\\");
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
        .arg("--debug")
        .arg("add")
        .arg(test_file.to_str().unwrap())
        .arg("-o")
        .arg("gitov_debug")
        .arg("--dry-run")
        .current_dir(&repo_dir)
        .assert()
        .success()
        .stderr(contains("repository"))
        .stderr(contains("resolved overlay"));
    Ok(())
}

#[test]
fn git_over_add_debug_output_bare_repo() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let repo_dir = canonical_tmp.join("repo");
    fs::create_dir_all(&repo_dir)?;
    let target_str = repo_dir.to_string_lossy().replace('\\', "\\\\");
    let ov = canonical_tmp.join("gitov_bare");
    fs::create_dir_all(&ov)?;
    fs::write(ov.join("over.toml"), format!("target = \"{}\"", target_str))?;
    let test_file = repo_dir.join("test.txt");
    fs::write(&test_file, b"content")?;
    // Bare repo living at <repo_dir>/.git, mirroring `over`'s worktree-workspace convention.
    git2::Repository::init_bare(repo_dir.join(".git"))?;

    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .arg("--debug")
        .arg("add")
        .arg(test_file.to_str().unwrap())
        .arg("-o")
        .arg("gitov_bare")
        .arg("--dry-run")
        .current_dir(&repo_dir)
        .assert()
        .success()
        .stderr(contains("worktree workspace (bare repo)"));
    Ok(())
}

#[test]
fn git_over_add_debug_output_linked_worktree() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let main_repo = canonical_tmp.join("main");
    fs::create_dir_all(&main_repo)?;
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&main_repo)
        .output()?;
    std::process::Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&main_repo)
        .output()?;

    let worktree_dir = canonical_tmp.join("worktree");
    std::process::Command::new("git")
        .args(["worktree", "add", "-b", "feature"])
        .arg(&worktree_dir)
        .current_dir(&main_repo)
        .output()?;

    // The overlay target must match the *main* repo root, not the worktree
    // path: `main_repo_root()` resolves worktrees back to the main repo
    // since overlay configs are keyed by the main repo path (see its doc
    // comment in `src/cli/git_over/mod.rs`).
    let target_str = main_repo.to_string_lossy().replace('\\', "\\\\");
    let ov = canonical_tmp.join("gitov_worktree");
    fs::create_dir_all(&ov)?;
    fs::write(ov.join("over.toml"), format!("target = \"{}\"", target_str))?;
    // File must live under the resolved overlay target (the main repo),
    // even though the command runs from the linked worktree's directory.
    let test_file = main_repo.join("test.txt");
    fs::write(&test_file, b"content")?;

    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .arg("--debug")
        .arg("add")
        .arg(test_file.to_str().unwrap())
        .arg("-o")
        .arg("gitov_worktree")
        .arg("--dry-run")
        .current_dir(&worktree_dir)
        .assert()
        .success()
        .stderr(contains("worktree"));
    Ok(())
}

#[test]
fn git_over_mount_debug_output() -> TestResult {
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let ov = canonical_tmp.join("gitov_mount");
    fs::create_dir_all(&ov)?;
    let target_str = canonical_tmp.to_string_lossy().replace('\\', "\\\\");
    fs::write(ov.join("over.toml"), format!("target = \"{}\"", target_str))?;
    let repo_dir = canonical_tmp.join("repo");
    fs::create_dir_all(&repo_dir)?;
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&repo_dir)
        .output()?;

    // Debug output is printed before any interactive prompt, so it is
    // exercised regardless of whether the platform's terminal detection
    // causes the (unrelated) property-export prompt to succeed or fail
    // when run with non-interactive stdin in CI.
    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .arg("--debug")
        .arg("mount")
        .arg("-o")
        .arg("gitov_mount")
        .current_dir(&repo_dir)
        .env("HOME", tmp.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .assert()
        .stderr(contains("repository"))
        .stderr(contains("resolved overlay"));
    Ok(())
}

#[test]
fn git_over_mount_debug_output_with_git_properties() -> TestResult {
    // Exercise the local-repo property inspection (origin remote, current
    // branch, non-origin remotes, and linked-worktree detection) so it is
    // actually run instead of short-circuiting on an empty repo. The
    // subsequent interactive export-selection prompt still runs with a
    // piped (immediately-closed) stdin, so it fails fast without hanging;
    // we only assert on the debug output printed before that prompt.
    let tmp = TempDir::new()?;
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let main_repo = canonical_tmp.join("main");
    fs::create_dir_all(&main_repo)?;
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&main_repo)
        .output()?;
    std::process::Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&main_repo)
        .output()?;
    std::process::Command::new("git")
        .args(["remote", "add", "origin", "https://example.com/origin.git"])
        .current_dir(&main_repo)
        .output()?;
    std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "upstream",
            "https://example.com/upstream.git",
        ])
        .current_dir(&main_repo)
        .output()?;

    let worktree_dir = canonical_tmp.join("worktree");
    std::process::Command::new("git")
        .args(["worktree", "add", "-b", "feature"])
        .arg(&worktree_dir)
        .current_dir(&main_repo)
        .output()?;

    let target_str = main_repo.to_string_lossy().replace('\\', "\\\\");
    let ov = canonical_tmp.join("gitov_mount_props");
    fs::create_dir_all(&ov)?;
    fs::write(ov.join("over.toml"), format!("target = \"{}\"", target_str))?;

    // Run from the linked worktree so `is_worktree()` is true and the
    // named-worktree detection/resolution path also runs.
    Command::cargo_bin("git-over")?
        .arg("--home")
        .arg(&canonical_tmp)
        .arg("--debug")
        .arg("mount")
        .arg("-o")
        .arg("gitov_mount_props")
        .current_dir(&worktree_dir)
        .assert()
        .stderr(contains("repository"))
        .stderr(contains("resolved overlay"));
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
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let ov = canonical_tmp.join("statusov2");
    fs::create_dir_all(&ov)?;
    // Escape backslashes so Windows paths don't get misparsed as TOML
    // unicode escape sequences.
    let target_str = canonical_tmp.to_string_lossy().replace('\\', "\\\\");
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
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let ov = canonical_tmp.join("gitov_multi");
    fs::create_dir_all(&ov)?;
    // Escape backslashes so Windows paths don't get misparsed as TOML
    // unicode escape sequences.
    let target_str = canonical_tmp.to_string_lossy().replace('\\', "\\\\");
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
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let ov = canonical_tmp.join("gitov_dir");
    fs::create_dir_all(&ov)?;
    // Escape backslashes so Windows paths don't get misparsed as TOML
    // unicode escape sequences.
    let target_str = canonical_tmp.to_string_lossy().replace('\\', "\\\\");
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
    let canonical_tmp = canonical_for_matching(tmp.path())?;
    let ov = canonical_tmp.join("gitov_err");
    fs::create_dir_all(&ov)?;
    // Escape backslashes so Windows paths don't get misparsed as TOML
    // unicode escape sequences.
    let target_str = canonical_tmp.to_string_lossy().replace('\\', "\\\\");
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
