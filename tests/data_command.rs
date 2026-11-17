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
    command.env_remove("OVER_TARGET");
    command.arg("--home").arg(repo).arg("data");
    Ok(command)
}

fn scope(data: Value) -> Value {
    json!({"data": data, "overlays": {}, "host": {}})
}

#[rstest]
#[case(true)]
#[case(false)]
fn exported_targets_and_contextual_selection_honor_the_environment_root(
    #[case] named: bool,
) -> TestResult {
    let temp = TempDir::new()?;
    let repo = temp.path().join("source");
    let target = temp.path().join("deployment");
    fs::create_dir_all(target.join("workspace"))?;
    descriptor(&repo, "", "host_overlay: fallback")?;
    descriptor(
        &repo,
        "fallback",
        "target: ~/fallback\ndata: {selected: wrong}",
    )?;
    descriptor(
        &repo,
        "selected",
        "target: ~/workspace\nuses: [dependency]\ndata: {literal: '~/unchanged'}",
    )?;
    descriptor(
        &repo,
        "dependency",
        "target: ~/agent\ndata: {value: dependency}",
    )?;
    let mut dependency = node(&repo, "dependency", json!({"value": "dependency"}));
    dependency["target"] = json!(target.join("agent"));
    let mut cmd = command(&repo)?;
    cmd.env("OVER_TARGET", &target)
        .current_dir(target.join("workspace"));
    if named {
        cmd.arg("selected");
    }
    assert_json(
        &mut cmd,
        json!({"data": {"literal": "~/unchanged"}, "host": {}, "overlays": {"dependency": dependency}}),
    )?;
    assert!(!target.join("agent").exists());
    Ok(())
}

fn node(repo: &Path, name: &str, data: Value) -> Value {
    // Canonical overlay identifiers use '/', but filesystem paths use native separators.
    let root: std::path::PathBuf = repo.join(name).components().collect();
    json!({"name": name, "root": root, "target": dirs::home_dir().unwrap(), "data": data, "host": {}})
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

#[test]
fn exported_data_preserves_literal_home_paths() -> TestResult {
    let repo = TempDir::new()?;
    let path = dirs::home_dir()
        .unwrap()
        .join("literal/value")
        .display()
        .to_string();
    descriptor(
        repo.path(),
        "selected",
        &format!("data: {}", json!({"path": path})),
    )?;
    assert_json(
        command(repo.path())?.arg("selected"),
        scope(json!({"path": path})),
    )
}

#[rstest]
#[case("host".into())]
#[case("host-{{ machine.arch }}".into())]
fn an_omitted_name_exports_the_configured_default_context(#[case] default: String) -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "", &format!("host_overlay: '{default}'"))?;
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
        json!({"data": {"host": "workstation"}, "host": {}, "overlays": {
            "parent": node(repo.path(), "parent", json!({"parent": true})),
            "base": node(repo.path(), "base", json!({"base": true})),
        }}),
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
        json!({"data": {"selected": true}, "host": {}, "overlays": {
            "parent": node(repo.path(), "parent", json!({"parent": true})),
            "base": node(repo.path(), "base", json!({"base": true})),
        }}),
    )
}

#[test]
fn an_explicit_name_exposes_dependencies_without_merging_their_data() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "", "host_overlay: host")?;
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
        json!({"data": {"nested": {"selected": true}, "list": ["new"], "optional": null}, "host": {}, "overlays": {
            "shared": node(repo.path(), "shared", json!({"shared": "dependency", "nested": {"retained": true}, "list": ["old"], "optional": "old"})),
            "first": node(repo.path(), "first", json!({"sibling": "first"})),
            "second": node(repo.path(), "second", json!({"sibling": "second", "dependency_wins": "selected_dependency"})),
        }}),
    )
}

#[test]
fn the_selected_root_wins_even_when_the_default_depends_on_it() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "", "host_overlay: host")?;
    descriptor(repo.path(), "host", "uses: [selected]\ndata: {value: host}")?;
    descriptor(repo.path(), "selected", "data: {value: selected}")?;
    assert_json(
        command(repo.path())?.arg("selected"),
        scope(json!({"value": "selected"})),
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
        json!({"data": {
            "enabled": true, "disabled": false, "count": 42, "negative": -7,
            "ratio": 1.25, "text": "line\n\"quoted\" \\ unicode: é",
            "list": ["one", 2, false, null, {"nested": "value"}],
            "optional": null, "replaced": {"old": true},
        }, "host": {}, "overlays": {"base": node(repo.path(), "base", json!({"replaced": "scalar"}))}}),
    )
}

#[test]
fn an_overlay_without_data_exports_an_empty_object() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "selected", "{}")?;
    assert_json(command(repo.path())?.arg("selected"), scope(json!({})))
}

#[test]
fn host_overrides_do_not_require_resolving_the_hosts_unrelated_dependencies() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(
        repo.path(),
        "",
        "host_overlay: machine\nhost: {ancestor: forbidden}",
    )?;
    descriptor(
        repo.path(),
        "machine",
        "uses: [missing]\ndata: {private: unrelated}\nhost: {ssh: {agent: host}, packages: [host]}",
    )?;
    descriptor(
        repo.path(),
        "apps/ssh",
        "data: {packages: [openssh]}\nhost: {ssh: {agent: default, socket: retained}, packages: [default]}",
    )?;
    assert_json(
        command(repo.path())?.arg("apps/ssh"),
        json!({
            "data": {"packages": ["openssh"]}, "overlays": {},
            "host": {"ssh": {"agent": "host", "socket": "retained"}, "packages": ["host"]},
        }),
    )
}

#[rstest]
#[case(false)]
#[case(true)]
fn omitted_selection_prefers_source_context_with_or_without_a_host(
    #[case] configured: bool,
) -> TestResult {
    let repo = TempDir::new()?;
    descriptor(
        repo.path(),
        "",
        if configured {
            "host_overlay: machine"
        } else {
            "{}"
        },
    )?;
    descriptor(repo.path(), "machine", "data: {selected: host}")?;
    descriptor(repo.path(), "local", "data: {selected: contextual}")?;
    assert_json(
        command(repo.path())?.current_dir(repo.path().join("local")),
        scope(json!({
            "selected": "contextual",
        })),
    )
}

#[test]
fn dependency_targets_are_resolved_before_their_consumers() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(
        repo.path(),
        "apps/ssh-agent",
        "target: ~/agent\ndata: {packages: [openssh]}",
    )?;
    descriptor(
        repo.path(),
        "selected",
        "uses: ['./apps/ssh-agent/']\ntarget: '{{ overlays.apps[\"ssh-agent\"].target }}/consumer'",
    )?;
    let mut dependency = node(
        repo.path(),
        "apps/ssh-agent",
        json!({"packages": ["openssh"]}),
    );
    dependency["target"] = json!(dirs::home_dir().unwrap().join("agent"));
    assert_json(
        command(repo.path())?.arg("selected"),
        json!({
            "data": {}, "host": {}, "overlays": {"apps": {"ssh-agent": dependency}},
        }),
    )
}

#[rstest]
#[case(None)]
#[case(Some("1"))]
#[case(Some("0"))]
fn redirected_data_stays_compact_and_uncolored_even_with_forced_colors(
    #[case] no_color: Option<&str>,
) -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "selected", "data: {nested: {value: true}}")?;
    let mut command = command(repo.path())?;
    command.arg("selected").env("CLICOLOR_FORCE", "1");
    if let Some(no_color) = no_color {
        command.env("NO_COLOR", no_color);
    } else {
        command.env_remove("NO_COLOR");
    }
    assert_json(&mut command, scope(json!({"nested": {"value": true}})))
}

#[test]
fn the_environment_can_select_the_repository() -> TestResult {
    let repo = TempDir::new()?;
    descriptor(repo.path(), "", "host_overlay: host")?;
    descriptor(repo.path(), "host", "data: {source: environment}")?;
    assert_json(
        Command::cargo_bin("over")?
            .current_dir(repo.path().parent().unwrap())
            .env("OVER_HOME", repo.path())
            .arg("data"),
        scope(json!({"source": "environment"})),
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
        scope(json!({"valid": true}))
    );
    if flag == "--debug" {
        assertion.stderr(contains("CLI args"));
    }
    Ok(())
}

#[rstest]
#[case("{}", None, "{}", "cannot infer an overlay")]
#[case("{}", Some("missing"), "{}", "overlay 'missing' not found")]
#[case("host_overlay: missing", None, "{}", "host_overlay")]
#[case("host_overlay: missing", Some("selected"), "{}", "host_overlay")]
#[case(
    "host_overlay: '{{ undefined.value }}'",
    Some("selected"),
    "{}",
    "host_overlay"
)]
#[case(
    "host_overlay: [host]",
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
