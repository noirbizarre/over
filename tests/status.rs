use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::TempDir;

fn command(source: &Path, root: &Path, verb: &str) -> Command {
    let mut command = Command::cargo_bin("over").unwrap();
    command
        .env("NO_COLOR", "1")
        .arg("--home")
        .arg(source)
        .arg(verb)
        .arg("--root")
        .arg(root);
    command
}

#[test]
fn unapplied_status_resolves_inheritance_composition_mappings_and_rule_provenance() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    fs::create_dir_all(source.join("selected/config")).unwrap();
    fs::create_dir_all(source.join("base")).unwrap();
    fs::write(source.join("over.yaml"), "target: '~/{{ data.destination }}'\nmode: symlink\ndata: {destination: resolved}\nrules:\n  - {path: selected/config, mode: symlink-directory}\n").unwrap();
    fs::write(
        source.join("selected/over.yaml"),
        "uses: [base]\nmap:\n  config: {target: '~/mapped'}\n",
    )
    .unwrap();
    fs::write(source.join("selected/config/file"), "managed").unwrap();
    fs::write(source.join("base/over.yaml"), "target: '~/base'\n").unwrap();
    fs::write(source.join("base/file"), "dependency").unwrap();
    let status = command(&source, &root, "status")
        .args(["selected", "--verbose"])
        .assert()
        .code(1)
        .stdout(contains("Configuration:"))
        .stdout(contains(root.join("resolved").to_string_lossy().as_ref()))
        .stdout(contains(root.join("mapped").to_string_lossy().as_ref()))
        .stdout(contains("composition: base -> selected"))
        .stdout(contains("Dependency base:"))
        .stdout(contains("mode: symlink-directory"))
        .stdout(contains("over.yaml rule 1"))
        .stdout(contains("overlay: base"))
        .stdout(contains("\x1b[").not())
        .get_output()
        .clone();
    let show = command(&source, &root, "show")
        .args(["selected", "--verbose"])
        .output()
        .unwrap();
    assert_eq!(status.stdout, show.stdout);
    assert_eq!(status.stderr, show.stderr);
    assert_eq!(status.status.code(), show.status.code());
    assert!(!root.exists());
}

#[test]
fn all_overlays_reports_valid_health_and_configuration_failures_before_its_summary() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    fs::create_dir_all(&root).unwrap();
    for name in ["applied", "missing", "invalid"] {
        fs::create_dir_all(source.join(name)).unwrap();
    }
    fs::write(source.join("applied/over.yaml"), "target: '~'").unwrap();
    fs::write(source.join("missing/over.yaml"), "target: '~/missing'").unwrap();
    fs::write(source.join("invalid/over.yaml"), "hooks: [invalid]").unwrap();
    command(&source, &root, "status")
        .arg("--all")
        .assert()
        .code(1)
        .stdout(contains("Overlay applied"))
        .stdout(contains("Overlay missing"))
        .stdout(contains("failed to load overlay"))
        .stdout(contains(
            "Overall: 3 overlays; 1 fully applied, 1 need attention, 1 could not be checked",
        ));
}

#[test]
fn healthy_entries_are_summarized_until_verbose_is_requested() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    fs::create_dir_all(source.join("selected")).unwrap();
    fs::write(source.join("selected/over.yaml"), "target: '~'").unwrap();
    fs::write(source.join("selected/file"), "managed").unwrap();
    command(&source, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    command(&source, &root, "status")
        .arg("selected")
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .assert()
        .success()
        .stdout(contains("Fully applied"))
        .stdout(contains("source:").count(1))
        .stdout(contains("\x1b[").not());
    command(&source, &root, "status")
        .args(["selected", "--verbose"])
        .assert()
        .success()
        .stdout(contains("overlay: selected"))
        .stdout(contains("\x1b[").not());
}

#[rstest::rstest]
#[case("bash")]
#[case("zsh")]
#[case("fish")]
#[case("powershell")]
#[case("elvish")]
fn generated_completions_include_canonical_status_and_its_compatibility_alias(#[case] shell: &str) {
    Command::cargo_bin("over")
        .unwrap()
        .args(["completion", shell])
        .assert()
        .success()
        .stdout(contains("status"))
        .stdout(contains("show"))
        .stdout(contains("no-uses"));
}

#[rstest::rstest]
#[case("status")]
#[case("show")]
fn both_command_names_explain_the_same_inspection_options(#[case] verb: &str) {
    Command::cargo_bin("over")
        .unwrap()
        .args([verb, "--help"])
        .assert()
        .success()
        .stdout(contains("resolved overlay configuration"))
        .stdout(contains("--all"))
        .stdout(contains("--no-uses"))
        .stdout(contains("--root"));
}
