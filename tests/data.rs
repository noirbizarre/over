use std::error::Error;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;
use rstest::rstest;
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

fn descriptor(repo: &Path, name: &str, content: &str) -> std::io::Result<()> {
    let dir = repo.join(name);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("over.yaml"), content)
}

#[rstest]
#[case(false)]
#[case(true)]
fn explicit_apply_renders_all_path_surfaces_with_host_data_without_applying_host_files(
    #[case] no_uses: bool,
) -> TestResult {
    let repo = TempDir::new()?;
    let target = TempDir::new()?;
    descriptor(repo.path(), "", "default_overlay: host")?;
    descriptor(
        repo.path(),
        "host",
        &format!(
            "uses: [shared]\ntarget: ~/host-files\ndata:\n  destination: host\n  path: '{}'\n",
            target.path().display().to_string().replace('\'', "''")
        ),
    )?;
    descriptor(
        repo.path(),
        "shared",
        "target: ~/dependency-files\ndata: {mapped: mapped-files}",
    )?;
    descriptor(
        repo.path(),
        "selected",
        "uses: [shared]\ntarget: '~/{{ data.destination }}'\ndata: {destination: selected-files}\nmap:\n  config:\n    target: '~/{{ data.mapped }}'",
    )?;
    fs::write(repo.path().join("host/host-only"), "host")?;
    fs::write(repo.path().join("shared/dependency-only"), "dependency")?;
    fs::write(repo.path().join("selected/selected-only"), "selected")?;
    fs::create_dir_all(repo.path().join("selected/config"))?;
    fs::write(repo.path().join("selected/config/settings"), "mapped")?;
    fs::write(target.path().join("external"), "external")?;
    fs::write(
        repo.path().join("selected/external.link.yaml"),
        "target: '{{ data.path }}/external'",
    )?;
    fs::write(
        repo.path().join("selected/block.partial.yaml"),
        "target: '{{ data.path }}/managed'\ncontent: literal content",
    )?;

    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["lint", "--target"])
        .arg(target.path())
        .assert()
        .success();
    let mut command = Command::cargo_bin("over")?;
    command
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "selected", "--no-prompt", "--target"])
        .arg(target.path());
    if no_uses {
        command.arg("--no-uses");
    }
    command.assert().success();

    assert_eq!(
        fs::read_to_string(target.path().join("selected-files/selected-only"))?,
        "selected"
    );
    assert_eq!(
        fs::read_to_string(target.path().join("mapped-files/settings"))?,
        "mapped"
    );
    assert_eq!(
        fs::read_to_string(target.path().join("selected-files/external"))?,
        "external"
    );
    assert!(fs::read_to_string(target.path().join("managed"))?.contains("literal content"));
    assert!(!target.path().join("host-files").exists());
    assert_eq!(
        target
            .path()
            .join("dependency-files/dependency-only")
            .exists(),
        !no_uses
    );

    for verb in ["status", "diff", "sync"] {
        Command::cargo_bin("over")?
            .arg("--home")
            .arg(repo.path())
            .args([verb, "selected", "--no-uses", "--target"])
            .arg(target.path())
            .assert()
            .success();
    }
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args(["unapply", "selected", "--target"])
        .arg(target.path())
        .assert()
        .success();
    assert!(!target.path().join("selected-files/selected-only").exists());
    Ok(())
}

#[test]
fn all_targets_keep_host_data_without_leaking_other_targets_metadata() -> TestResult {
    let repo = TempDir::new()?;
    let target = TempDir::new()?;
    descriptor(repo.path(), "", "default_overlay: host")?;
    descriptor(
        repo.path(),
        "host",
        "target: ~/host\ndata: {host: workstation}",
    )?;
    descriptor(
        repo.path(),
        "alpha",
        "target: '~/alpha-{{ data.host }}'\ndata: {only_alpha: dirty}",
    )?;
    descriptor(
        repo.path(),
        "beta",
        "target: '~/beta-{{ data.host }}-{{ data.only_alpha | default(\"clean\") }}'",
    )?;
    for name in ["host", "alpha", "beta"] {
        fs::write(repo.path().join(name).join("file"), name)?;
    }
    Command::cargo_bin("over")?
        .current_dir(repo.path())
        .arg("--home")
        .arg(repo.path())
        .args(["status", "--all", "--target"])
        .arg(target.path())
        .assert()
        .code(1)
        .stdout(contains("alpha-workstation"))
        .stdout(contains("beta-workstation-clean"));
    // Omitting NAME still selects the default operational target.
    // The real checkout may have a Git overlay association unrelated to this fixture.
    Command::cargo_bin("over")?
        .current_dir(repo.path())
        .arg("--home")
        .arg(repo.path())
        .args(["apply", "--no-prompt", "--target"])
        .arg(target.path())
        .assert()
        .success();
    assert!(target.path().join("host/file").exists());
    assert!(!target.path().join("alpha-workstation").exists());
    Ok(())
}

#[rstest]
#[case("apply", "selected")]
#[case("status", "selected")]
#[case("diff", "selected")]
#[case("sync", "selected")]
#[case("unapply", "selected")]
#[case("commit", "selected")]
#[case("status", "--all")]
fn broken_host_configuration_fails_data_resolution_with_explicit_targets(
    #[case] verb: &str,
    #[case] selection: &str,
) -> TestResult {
    let repo = TempDir::new()?;
    let target = TempDir::new()?;
    descriptor(repo.path(), "", "default_overlay: nonexistent")?;
    descriptor(repo.path(), "selected", "data: {}")?;
    Command::cargo_bin("over")?
        .arg("--home")
        .arg(repo.path())
        .args([verb, selection, "--target"])
        .arg(target.path())
        .assert()
        .failure()
        .stderr(contains("default_overlay"));
    Ok(())
}
