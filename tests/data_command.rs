use std::error::Error;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

fn descriptor(repo: &Path, name: &str, content: &str) -> std::io::Result<()> {
    let dir = repo.join(name);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("over.yaml"), content)
}

fn command(repo: &Path) -> Result<Command, Box<dyn Error>> {
    let mut command = Command::cargo_bin("over")?;
    // Contextual selection must not inherit the developer checkout's Git association.
    command.current_dir(repo.parent().unwrap());
    command.arg("--home").arg(repo).arg("data");
    Ok(command)
}

fn assert_json(command: &mut Command, expected: Value) -> TestResult {
    let assertion = command.assert().success().stderr("");
    let stdout = &assertion.get_output().stdout;
    assert_eq!(serde_json::from_slice::<Value>(stdout)?, expected);
    // Comparing compact encodings checks whitespace without depending on map key order.
    let parsed: Value = serde_json::from_slice(stdout)?;
    assert_eq!(stdout.len(), serde_json::to_vec(&parsed)?.len() + 1);
    assert_eq!(stdout.last(), Some(&b'\n'));
    Ok(())
}

#[rstest]
#[case("host".into())]
#[case("host-{{ machine.arch }}".into())]
fn an_omitted_name_exports_the_configured_default_context(#[case] default: String) -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "", &format!("default_overlay: '{default}'"))?;
    let name = default.replace("{{ machine.arch }}", std::env::consts::ARCH);
    descriptor(
        repo.path(),
        &name,
        "uses: [parent]\ndata: {host: workstation}",
    )?;
    descriptor(repo.path(), "parent", "uses: [base]\ndata: {parent: true}")?;
    descriptor(repo.path(), "base", "data: {base: true}")?;
    descriptor(repo.path(), "unrelated", "data: {unrelated: true}")?;
    assert_json(
        &mut command(repo.path())?,
        json!({"host": "workstation", "parent": true, "base": true}),
    )
}

#[test]
fn an_explicit_name_without_a_default_includes_only_its_dependency_closure() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(
        repo.path(),
        "selected",
        "uses: [parent]\ndata: {selected: true}",
    )?;
    descriptor(repo.path(), "parent", "uses: [base]\ndata: {parent: true}")?;
    descriptor(repo.path(), "base", "data: {base: true}")?;
    descriptor(repo.path(), "unrelated", "data: {unrelated: true}")?;
    assert_json(
        command(repo.path())?.arg("selected"),
        json!({"selected": true, "parent": true, "base": true}),
    )
}

#[test]
fn an_explicit_name_composes_both_closures_with_canonical_precedence() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "", "default_overlay: host")?;
    descriptor(
        repo.path(),
        "shared",
        "data: {shared: dependency, nested: {retained: true}, list: [old], optional: old}",
    )?;
    descriptor(
        repo.path(),
        "host",
        "uses: [shared]\ndata: {shared: host, host_only: true, dependency_wins: host, nested: {host: true}, list: [host]}",
    )?;
    descriptor(repo.path(), "first", "data: {sibling: first}")?;
    descriptor(
        repo.path(),
        "second",
        "data: {sibling: second, dependency_wins: selected_dependency}",
    )?;
    descriptor(
        repo.path(),
        "selected",
        "uses: [shared, first, second]\ndata: {nested: {selected: true}, list: [new], optional: null}",
    )?;
    assert_json(
        command(repo.path())?.arg("selected"),
        json!({
            "shared": "host", "host_only": true, "dependency_wins": "selected_dependency",
            "sibling": "second", "nested": {"retained": true, "host": true, "selected": true},
            "list": ["new"], "optional": null,
        }),
    )
}

#[test]
fn the_selected_root_wins_even_when_the_default_depends_on_it() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "", "default_overlay: host")?;
    descriptor(repo.path(), "host", "uses: [selected]\ndata: {value: host}")?;
    descriptor(repo.path(), "selected", "data: {value: selected}")?;
    assert_json(
        command(repo.path())?.arg("selected"),
        json!({"value": "selected"}),
    )
}

#[test]
fn json_serialization_preserves_types_nulls_and_escaped_strings() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(
        repo.path(),
        "selected",
        r#"data:
  enabled: true
  disabled: false
  count: 42
  negative: -7
  ratio: 1.25
  text: "line\n\"quoted\" \\ unicode: é"
  list: [one, 2, false, null, {nested: value}]
  optional: null
  replaced: {old: true}
uses: [base]
"#,
    )?;
    descriptor(repo.path(), "base", "data: {replaced: scalar}")?;
    assert_json(
        command(repo.path())?.arg("selected"),
        json!({
            "enabled": true, "disabled": false, "count": 42, "negative": -7,
            "ratio": 1.25, "text": "line\n\"quoted\" \\ unicode: é",
            "list": ["one", 2, false, null, {"nested": "value"}],
            "optional": null, "replaced": {"old": true},
        }),
    )
}

#[test]
fn an_overlay_without_data_exports_an_empty_object() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "selected", "{}")?;
    assert_json(command(repo.path())?.arg("selected"), json!({}))
}

#[test]
fn the_environment_can_select_the_repository() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "", "default_overlay: host")?;
    descriptor(repo.path(), "host", "data: {source: environment}")?;
    assert_json(
        Command::cargo_bin("over")?
            .current_dir(repo.path().parent().unwrap())
            .env("OVER_HOME", repo.path())
            .arg("data"),
        json!({"source": "environment"}),
    )
}

#[rstest]
#[case("--debug")]
#[case("--verbose")]
fn diagnostic_flags_do_not_contaminate_json_stdout(#[case] flag: &str) -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "selected", "data: {valid: true}")?;
    let assertion = command(repo.path())?
        .args(["selected", flag])
        .assert()
        .success();
    assert_eq!(
        serde_json::from_slice::<Value>(&assertion.get_output().stdout)?,
        json!({"valid": true})
    );
    if flag == "--debug" {
        assertion.stderr(contains("CLI args"));
    }
    Ok(())
}

#[rstest]
#[case("{}", None, "{}", "cannot infer an overlay")]
#[case("{}", Some("missing"), "{}", "overlay 'missing' not found")]
#[case("default_overlay: missing", None, "{}", "default_overlay")]
#[case("default_overlay: missing", Some("selected"), "{}", "default_overlay")]
#[case(
    "default_overlay: '{{ undefined.value }}'",
    Some("selected"),
    "{}",
    "default_overlay"
)]
#[case(
    "default_overlay: [host]",
    Some("selected"),
    "{}",
    "invalid repository root config"
)]
#[case("[invalid", None, "{}", "repository root config")]
#[case(
    "{}",
    Some("selected"),
    "[invalid",
    "failed to load overlay 'selected'"
)]
#[case("{}", Some("selected"), "data: []", "`data` must be a mapping")]
#[case("{}", Some("selected"), "uses: [missing]", "data dependency 'missing'")]
#[case(
    "{}",
    Some("selected"),
    "uses: [selected]",
    "cycle in data dependencies"
)]
fn selection_and_resolution_errors_fail_without_writing_stdout(
    #[case] root: &str,
    #[case] selected: Option<&str>,
    #[case] content: &str,
    #[case] diagnostic: &str,
) -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "", root)?;
    // An available overlay must not cause missing selection to aggregate or prompt.
    descriptor(repo.path(), "selected", content)?;
    let mut command = command(repo.path())?;
    if let Some(selected) = selected {
        command.arg(selected);
    }
    command
        .assert()
        .failure()
        .stdout("")
        .stderr(contains(diagnostic));
    Ok(())
}
