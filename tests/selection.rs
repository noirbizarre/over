use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use over::exec::{Context, Ctx};
use over::overlays::Repository;
use over::selection;
use predicates::str::contains;
use rstest::rstest;

struct Fixture {
    temp: TempDir,
    ctx: Ctx,
}

#[cfg(unix)]
#[rstest]
#[case("outside", "TWO\r", false, true)]
#[case("home", "two\r", false, true)]
#[case("outside", "\u{1b}", false, false)]
#[case("outside", "", true, false)]
fn terminal_selection_filters_missing_or_ambiguous_context_without_implicit_consent(
    #[case] cwd: &str,
    #[case] answer: &str,
    #[case] no_prompt: bool,
    #[case] applied: bool,
) {
    let fixture = Fixture::new();
    for name in ["one", "two"] {
        fixture.overlay(name, "target = '~'\nmode = 'symlink'");
        fixture
            .temp
            .child(format!("repo/{name}/{name}"))
            .write_str(name)
            .unwrap();
    }
    let steps = if no_prompt {
        Vec::new()
    } else {
        vec![("Choose an overlay", answer)]
    };
    let mut command = std::process::Command::new("python3");
    command
        .current_dir(fixture.path(cwd))
        .args(["-c", include_str!("fixtures/age-pty.py")])
        .arg(serde_json::to_string(&steps).unwrap())
        .arg(assert_cmd::cargo::cargo_bin!("over"))
        .arg("--home")
        .arg(fixture.path("repo"))
        .args(["apply", "--target"])
        .arg(fixture.path("home"))
        .env("XDG_STATE_HOME", fixture.path("state"))
        .env("GIT_CONFIG_GLOBAL", fixture.path("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("OVER_TEST_PTY_SPLIT_STDOUT", "1")
        .env("NO_COLOR", "1")
        .env("TERM", "xterm");
    if no_prompt {
        command.arg("--no-prompt");
    }
    let output = command.output().unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert_ne!(output.status.code(), Some(124), "picker timed out: {text}");
    assert_ne!(output.status.code(), Some(125), "missing prompt: {text}");
    assert_eq!(output.status.success(), applied, "{text}");
    assert!(!fixture.path("home/one").exists());
    assert_eq!(fixture.path("home/two").exists(), applied);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Choose an overlay"));
    if applied {
        assert_eq!(fs::read_to_string(fixture.path("home/two")).unwrap(), "two");
    }
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        temp.child("repo").create_dir_all().unwrap();
        temp.child("home").create_dir_all().unwrap();
        temp.child("outside").create_dir_all().unwrap();
        let ctx = Context::builder()
            .repository(Repository::new(temp.path().join("repo")))
            .root(temp.path().join("home"))
            .build();
        Self { temp, ctx }
    }

    fn overlay(&self, name: &str, descriptor: &str) {
        self.temp
            .child(format!("repo/{name}/over.toml"))
            .write_str(descriptor)
            .unwrap();
    }

    fn path(&self, path: &str) -> PathBuf {
        self.temp.path().join(path)
    }

    fn infer(&self, paths: &[&str]) -> anyhow::Result<Option<String>> {
        let paths = paths.iter().map(|path| self.path(path)).collect::<Vec<_>>();
        Ok(selection::infer(&self.ctx, &paths)?.map(|overlay| overlay.name))
    }

    fn command(&self, command: &str, cwd: &Path) -> Command {
        let mut cli = Command::cargo_bin("over").unwrap();
        cli.current_dir(cwd)
            .arg("--home")
            .arg(&self.ctx.repository.root)
            .arg(command);
        cli
    }
}

#[cfg(unix)]
#[rstest]
#[case("apply", false)]
#[case("apply", true)]
#[case("unapply", false)]
#[case("sync", false)]
fn choosing_an_overlay_cannot_resolve_ambiguous_source_mappings_at_cwd(
    #[case] command: &str,
    #[case] exclusions: bool,
) {
    let fixture = Fixture::new();
    for name in ["one", "two"] {
        fixture.overlay(name, "mode = 'symlink'\ntarget = '~/base'\n[[rules]]\npath = 'first'\ntarget = '../mapped'\n[[rules]]\npath = 'second'\ntarget = '../mapped/nested'");
        fixture
            .temp
            .child(format!("repo/{name}/first/file"))
            .write_str("first")
            .unwrap();
        fixture
            .temp
            .child(format!("repo/{name}/second/file"))
            .write_str("second")
            .unwrap();
    }
    fixture
        .temp
        .child("repo/mapped/nested")
        .create_dir_all()
        .unwrap();
    let mut process = std::process::Command::new("python3");
    process
        .current_dir(fixture.path("repo/mapped/nested"))
        .args(["-c", include_str!("fixtures/age-pty.py")])
        .arg(serde_json::to_string(&[("Choose an overlay", "one\r")]).unwrap())
        .arg(assert_cmd::cargo::cargo_bin!("over"))
        .arg("--home")
        .arg(fixture.path("repo"))
        .arg(command)
        .arg("--target")
        .arg(fixture.path("home"))
        .env("XDG_STATE_HOME", fixture.path("state"))
        .env("GIT_CONFIG_GLOBAL", fixture.path("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("NO_COLOR", "1")
        .env("TERM", "xterm");
    if exclusions {
        process.args(["--skip", "absent"]);
    }
    let output = process.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{text}");
    assert_ne!(output.status.code(), Some(124), "picker timed out: {text}");
    assert_ne!(output.status.code(), Some(125), "missing prompt: {text}");
    assert!(text.contains("ambiguous source path"), "{text}");
    assert!(!fixture.path("repo/mapped/file").exists());
    assert!(!fixture.path("repo/mapped/nested/file").exists());
}

#[cfg(unix)]
#[test]
fn absorption_typeahead_retains_checked_entries_hidden_by_a_later_filter() {
    let fixture = Fixture::new();
    for name in ["one", "two", "untouched"] {
        fixture
            .temp
            .child(format!("home/{name}"))
            .write_str(name)
            .unwrap();
    }
    // Check one, replace the query, then check two while one is hidden.
    let steps = [("select entries to absorb", "one \u{7f}\u{7f}\u{7f}two \r")];
    let output = std::process::Command::new("python3")
        .current_dir(fixture.path("outside"))
        .args(["-c", include_str!("fixtures/age-pty.py")])
        .arg(serde_json::to_string(&steps).unwrap())
        .arg(assert_cmd::cargo::cargo_bin!("over"))
        .arg("--home")
        .arg(fixture.path("repo"))
        .args(["new", "picked"])
        .arg(fixture.path("home"))
        .env("OVER_TEST_PTY_RAW_INPUT", "1")
        .env("XDG_STATE_HOME", fixture.path("state"))
        .env("NO_COLOR", "1")
        .env("TERM", "xterm")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    for name in ["one", "two"] {
        assert_eq!(
            fs::read_to_string(fixture.path(&format!("repo/picked/{name}"))).unwrap(),
            name
        );
    }
    assert!(!fixture.path("repo/picked/untouched").exists());
    assert_eq!(
        fs::read_to_string(fixture.path("home/untouched")).unwrap(),
        "untouched"
    );
}

#[rstest]
#[case("home")]
#[case("home/nested/missing")]
#[case("repo/one")]
#[case("repo/one/nested/missing")]
fn a_unique_target_or_source_context_selects_its_overlay(#[case] path: &str) {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    assert_eq!(fixture.infer(&[path]).unwrap().as_deref(), Some("one"));
}

#[test]
fn source_context_outranks_another_overlays_target_without_resolving_it() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~/one'");
    fixture.overlay(
        "two",
        &format!(
            "target = '{}'\nmode = 'checkout'",
            fixture.path("repo/one").display()
        ),
    );
    assert_eq!(
        fixture.infer(&["repo/one"]).unwrap().as_deref(),
        Some("one")
    );
    fixture.overlay("two", "target = '{{ env.OVER_UNRELATED_TARGET }}'");
    assert_eq!(
        fixture.infer(&["repo/one"]).unwrap().as_deref(),
        Some("one")
    );
}

#[rstest]
#[case("home/file")]
#[case("home/new-file")]
#[case("home")]
fn managed_files_and_immediate_siblings_outrank_shared_targets(#[case] path: &str) {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture.overlay("two", "target = '~'");
    fixture
        .temp
        .child("repo/one/file")
        .write_str("managed")
        .unwrap();
    // Desired ownership is sufficient before any links have been deployed.
    assert_eq!(fixture.infer(&[path]).unwrap().as_deref(), Some("one"));
}

#[rstest]
#[case("home", "target = '~'")]
#[case("home/sub/file", "target = '~/sub'")]
fn same_tier_candidates_are_ambiguous_even_with_a_configured_default(
    #[case] path: &str,
    #[case] second: &str,
) {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture.overlay("two", second);
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("host_overlay = 'one'")
        .unwrap();
    let error = selection::select(&fixture.ctx, None, &[fixture.path(path)], true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("ambiguous overlay"), "{error}");
    assert!(error.contains("one, two"), "{error}");
}

#[test]
fn an_unrelated_single_overlay_is_not_an_implicit_default() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    assert_eq!(fixture.infer(&["outside"]).unwrap(), None);
    let error = selection::select(&fixture.ctx, None, &[fixture.path("outside")], false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot infer an overlay"));
    assert!(error.contains("--overlay NAME"));
}

#[test]
fn contextual_selection_precedes_default_without_importing_the_defaults_local_data() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~/one'\n[data]\nowner = 'one'");
    fixture.overlay(
        "two",
        "target = '~/two'\n[data]\nowner = 'two'\ndefault_data = true\n[host]\nmachine_override = true",
    );
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("host_overlay = 'two'")
        .unwrap();
    let overlay = selection::select(&fixture.ctx, None, &[fixture.path("home/one")], true).unwrap();
    assert_eq!(overlay.name, "one");
    let ctx = fixture.ctx.with_overlay(overlay).resolve_data().unwrap();
    let data = serde_json::to_value(&ctx.data).unwrap();
    assert_eq!(data["owner"], serde_json::json!("one"));
    assert!(data.get("default_data").is_none());
    assert_eq!(
        serde_json::to_value(&ctx.host).unwrap()["machine_override"],
        serde_json::json!(true)
    );
    assert_eq!(
        selection::select(&fixture.ctx, None, &[fixture.path("outside")], true)
            .unwrap()
            .name,
        "two"
    );
}

#[test]
fn operands_from_different_owners_fail_before_ingestion() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~/one'");
    fixture.overlay("two", "target = '~/two'");
    fixture
        .temp
        .child("home/one/file")
        .write_str("one")
        .unwrap();
    fixture
        .temp
        .child("home/two/file")
        .write_str("two")
        .unwrap();
    fixture
        .command("add", &fixture.path("outside"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .arg(fixture.path("home/one/file"))
        .arg(fixture.path("home/two/file"))
        .assert()
        .failure()
        .stderr(contains("ambiguous overlay across selected paths"));
    assert_eq!(
        fs::read_to_string(fixture.path("home/one/file")).unwrap(),
        "one"
    );
    assert!(!fixture.path("repo/one/file").exists());
}

#[test]
fn mappings_infer_external_destinations_and_exclude_the_old_base_subtree() {
    let fixture = Fixture::new();
    fixture.overlay(
        "one",
        "target = '~'\n[[rules]]\npath = 'config'\ntarget = '../mapped'",
    );
    fixture
        .temp
        .child("repo/one/config/file")
        .write_str("content")
        .unwrap();
    assert_eq!(
        fixture.infer(&["repo/mapped/new"]).unwrap().as_deref(),
        Some("one")
    );
    assert_eq!(fixture.infer(&["home/config/new"]).unwrap(), None);
}

#[test]
fn target_inference_retains_the_rule_and_corresponding_source_directory() {
    let fixture = Fixture::new();
    fixture.overlay(
        "one",
        "target = '~/base'\n[data]\nowner = 'one'\n[[rules]]\npath = 'config'\ntarget = '../mapped'",
    );
    let path = fixture.path("repo/mapped/editor/nested");
    let selected = selection::infer_selection(&fixture.ctx, std::slice::from_ref(&path))
        .unwrap()
        .unwrap();
    assert_eq!(selected.overlay.name, "one");
    assert_eq!(selected.paths[0].0, path);
    assert_eq!(
        selected.paths[0].1.source,
        Path::new("config/editor/nested")
    );
    let origin = selected.paths[0].1.rule.as_ref().unwrap();
    assert_eq!(origin.descriptor, fixture.path("repo/one/over.toml"));
    assert_eq!(origin.index, 0);
    fixture
        .temp
        .child("repo/mapped/editor/nested")
        .create_dir_all()
        .unwrap();
    fixture
        .command("data", &path)
        .assert()
        .success()
        .stdout(contains("\"owner\":\"one\""));
}

#[test]
fn overlapping_target_rules_in_one_overlay_do_not_guess_a_source() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~/base'\n[[rules]]\npath = 'first'\ntarget = '../mapped'\n[[rules]]\npath = 'second'\ntarget = '../mapped/nested'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("host_overlay = 'one'")
        .unwrap();
    let error = selection::select(
        &fixture.ctx,
        None,
        &[fixture.path("repo/mapped/nested/new")],
        true,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("ambiguous source path"),
        "{error:#}"
    );
    for source in [
        Path::new("first").join("nested").join("new"),
        Path::new("second").join("new"),
    ] {
        assert!(
            error
                .to_string()
                .contains(source.to_string_lossy().as_ref()),
            "{error:#}"
        );
    }
}

#[test]
fn rule_targets_shared_by_two_overlays_remain_ambiguous() {
    let fixture = Fixture::new();
    for name in ["one", "two"] {
        fixture.overlay(
            name,
            "target = '~/base'\n[[rules]]\npath = 'config'\ntarget = '../mapped'",
        );
    }
    let error = fixture.infer(&["repo/mapped/nested"]).unwrap_err();
    assert!(error.to_string().contains("ambiguous overlay"), "{error:#}");
}

#[test]
fn dependency_consumers_do_not_claim_their_dependencies_files() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture.overlay("two", "target = '~'\nuses = ['one']");
    fixture
        .temp
        .child("repo/one/file")
        .write_str("content")
        .unwrap();
    assert_eq!(
        fixture.infer(&["home/file"]).unwrap().as_deref(),
        Some("one")
    );
}

#[test]
fn an_explicit_git_association_outranks_ownership_and_never_changes_config() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'\n[data]\nowner = 'one'");
    fixture.overlay("two", "target = '~'\n[data]\nowner = 'two'");
    fixture
        .temp
        .child("repo/one/project/file")
        .write_str("managed")
        .unwrap();
    let repo = git2::Repository::init(fixture.path("home/project")).unwrap();
    repo.config()
        .unwrap()
        .set_str("over.overlay", "two")
        .unwrap();
    let before = fs::read(repo.path().join("config")).unwrap();
    assert_eq!(
        fixture
            .infer(&["home/project/nested/missing"])
            .unwrap()
            .as_deref(),
        Some("two")
    );
    assert_eq!(fs::read(repo.path().join("config")).unwrap(), before);
    fixture
        .temp
        .child("home/project/nested")
        .create_dir_all()
        .unwrap();
    fixture
        .command("data", &fixture.path("home/project/nested"))
        .assert()
        .success()
        .stdout(contains("\"owner\":\"two\""));
    assert_eq!(fs::read(repo.path().join("config")).unwrap(), before);
}

#[test]
fn stale_git_associations_fail_but_explicit_arguments_bypass_them() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    let repo = git2::Repository::init(fixture.path("outside/project")).unwrap();
    repo.config()
        .unwrap()
        .set_str("over.overlay", "missing")
        .unwrap();
    assert!(
        fixture
            .infer(&["outside/project"])
            .unwrap_err()
            .to_string()
            .contains("over.overlay")
    );
    assert_eq!(
        selection::select(
            &fixture.ctx,
            Some("one"),
            &[fixture.path("outside/project")],
            false
        )
        .unwrap()
        .name,
        "one"
    );
}

#[test]
fn matching_repository_names_and_remotes_are_not_associations() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    let repo = git2::Repository::init(fixture.path("outside/one")).unwrap();
    repo.remote("origin", "https://example.com/one.git")
        .unwrap();
    assert_eq!(fixture.infer(&["outside/one"]).unwrap(), None);
}

#[test]
fn a_bare_worktree_workspace_can_supply_an_explicit_association() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    let repo = git2::Repository::init_bare(fixture.path("outside/workspace/.git")).unwrap();
    repo.config()
        .unwrap()
        .set_str("over.overlay", "one")
        .unwrap();
    assert_eq!(
        fixture.infer(&["outside/workspace"]).unwrap().as_deref(),
        Some("one")
    );
}

#[rstest]
fn add_does_not_use_the_host_when_its_operands_have_no_context(
    #[values(false, true)] enabled: bool,
) {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str(&format!(
            "host_overlay = 'one'\napply.default_to_host = {enabled}"
        ))
        .unwrap();
    fixture
        .temp
        .child("outside/new")
        .write_str("preserved")
        .unwrap();
    fixture
        .command("add", &fixture.path("outside"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .arg("new")
        .assert()
        .failure()
        .stderr(contains("cannot infer an overlay"));
    assert_eq!(
        fs::read_to_string(fixture.path("outside/new")).unwrap(),
        "preserved"
    );
}

#[test]
fn broken_source_context_reports_its_configuration_instead_of_falling_back() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture.overlay(
        "broken",
        "target = '{{ env.OVER_SELECTION_MISSING_TARGET }}'",
    );
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("host_overlay = 'one'")
        .unwrap();
    let error = selection::select(&fixture.ctx, None, &[fixture.path("repo/broken")], true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot resolve overlay 'broken'"), "{error}");
}

#[rstest]
#[case(true)]
#[case(false)]
fn broken_contextual_configuration_never_activates_an_unrelated_default(#[case] source: bool) {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~/one'");
    fixture.overlay("two", "target = '~/two'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("host_overlay = 'two'\napply.default_to_host = true")
        .unwrap();
    fixture.temp.child("home/one").create_dir_all().unwrap();
    if source {
        fixture.overlay("one", "[invalid");
    } else {
        fixture
            .temp
            .child("repo/one/broken.partial.toml")
            .write_str("[invalid")
            .unwrap();
    }
    let path = if source { "repo/one" } else { "home/one" };
    let error = selection::select(&fixture.ctx, None, &[fixture.path(path)], true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot resolve overlay 'one'"), "{error}");
    fixture
        .command("apply", &fixture.path(path))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .args(["--dry-run", "--no-prompt"])
        .assert()
        .failure()
        .stderr(contains("cannot resolve overlay 'one'"));
    assert!(!fixture.path("home/two").exists());
}

#[cfg(unix)]
#[test]
fn unrelated_dangling_deployment_ancestors_do_not_block_a_healthy_context() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~/one'");
    fixture.overlay("two", "target = '~/two'");
    fixture
        .temp
        .child("repo/two/nested/file")
        .write_str("content")
        .unwrap();
    fixture.temp.child("home/two").create_dir_all().unwrap();
    std::os::unix::fs::symlink(fixture.path("missing"), fixture.path("home/two/nested")).unwrap();
    assert_eq!(
        fixture.infer(&["home/one"]).unwrap().as_deref(),
        Some("one")
    );
}

#[test]
fn reporting_commands_infer_in_context_and_all_bypasses_ambiguity() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~/one'");
    // The broken second target makes unintended fan-out observable.
    fixture.overlay("two", "target = '{{ env.OVER_SELECTION_MISSING_TARGET }}'");
    fixture.temp.child("home/one").create_dir_all().unwrap();
    fixture
        .command("status", &fixture.path("home/one"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .assert()
        .success();
    fixture
        .command("status", &fixture.path("outside"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .assert()
        .failure()
        .stdout(contains("Check failed"));
    fixture.overlay("two", "target = '~/one'");
    fixture
        .command("status", &fixture.path("home/one"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .assert()
        .failure()
        .stderr(contains("ambiguous overlay"));
    fixture
        .command("status", &fixture.path("home/one"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .arg("--all")
        .assert()
        .success();
}

#[test]
fn the_environment_target_reaches_contextual_selection_before_the_default() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~/one'");
    fixture.overlay("two", "target = '~/two'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("host_overlay = 'two'")
        .unwrap();
    fixture.temp.child("home/one").create_dir_all().unwrap();
    let output = fixture
        .command("status", &fixture.path("home/one"))
        .env("OVER_TARGET", &fixture.ctx.root)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("one"), "{output}");
    assert!(!output.contains("two"), "{output}");
}

#[test]
fn data_prefers_active_source_context_before_the_configured_host() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'\n[data]\nowner = 'one'");
    fixture.overlay("two", "target = '~'\n[data]\nowner = 'two'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("host_overlay = 'two'")
        .unwrap();
    fixture
        .command("data", &fixture.path("repo/one"))
        .assert()
        .success()
        .stdout(contains("\"owner\":\"one\""));
}

#[rstest]
fn explicit_and_active_apply_targets_precede_the_host(
    #[values(false, true)] enabled: bool,
    #[values(None, Some("two"), Some("host"))] explicit: Option<&str>,
) {
    let fixture = Fixture::new();
    for name in ["one", "two", "host"] {
        fixture.overlay(name, &format!("target = '~/{name}'"));
        fixture
            .temp
            .child(format!("repo/{name}/file"))
            .write_str(name)
            .unwrap();
    }
    fixture
        .temp
        .child("repo/over.toml")
        .write_str(&format!(
            "host_overlay = 'host'\napply.default_to_host = {enabled}"
        ))
        .unwrap();
    let mut command = fixture.command("apply", &fixture.path("repo/one"));
    if let Some(name) = explicit {
        command.arg(name);
    }
    command
        .args(["--no-prompt", "--target"])
        .arg(&fixture.ctx.root)
        .assert()
        .success();
    let selected = explicit.unwrap_or("one");
    for name in ["one", "two", "host"] {
        assert_eq!(
            fixture.path(&format!("home/{name}/file")).exists(),
            name == selected
        );
    }
}

#[rstest]
fn explicit_composition_never_gains_the_configured_host(#[values(false, true)] enabled: bool) {
    let fixture = Fixture::new();
    for name in ["one", "two", "host"] {
        fixture.overlay(name, &format!("target = '~/{name}'"));
        fixture
            .temp
            .child(format!("repo/{name}/file"))
            .write_str(name)
            .unwrap();
    }
    fixture
        .temp
        .child("repo/over.toml")
        .write_str(&format!(
            "host_overlay = 'host'\napply.default_to_host = {enabled}"
        ))
        .unwrap();
    fixture
        .command("apply", &fixture.path("repo/host"))
        .args(["one", "two", "--no-prompt", "--target"])
        .arg(&fixture.ctx.root)
        .assert()
        .success();
    assert!(fixture.path("home/one/file").exists());
    assert!(fixture.path("home/two/file").exists());
    assert!(!fixture.path("home/host/file").exists());
}

#[test]
fn host_context_alone_runs_neither_deployment_nor_hooks() {
    let fixture = Fixture::new();
    fixture.overlay(
        "host",
        &format!(
            "target = '~'\n[hooks]\npost-apply = ['touch {}']",
            fixture.path("hook-ran").display()
        ),
    );
    fixture
        .temp
        .child("repo/host/file")
        .write_str("preserved")
        .unwrap();
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("host_overlay = 'host'")
        .unwrap();
    fixture
        .command("apply", &fixture.path("outside"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .assert()
        .failure()
        .stderr(contains("apply.default_to_host"));
    assert!(!fixture.path("hook-ran").exists());
    assert_eq!(fs::read_dir(&fixture.ctx.root).unwrap().count(), 0);
}

#[rstest]
fn apply_refuses_ambiguous_context_even_with_host_deployment_enabled(
    #[values(false, true)] enabled: bool,
) {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture.overlay("two", "target = '~'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str(&format!(
            "host_overlay = 'one'\napply.default_to_host = {enabled}"
        ))
        .unwrap();
    fixture
        .command("apply", &fixture.path("home"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .assert()
        .failure()
        .stderr(contains("ambiguous"));
    assert_eq!(fs::read_dir(&fixture.ctx.root).unwrap().count(), 0);
}

#[rstest]
fn sync_uses_the_host_only_with_its_own_opt_in(
    #[values(false, true)] enabled: bool,
    #[values("outside", "repo/one", "home")] cwd: &str,
    #[values(None, Some("two"), Some("host"))] explicit: Option<&str>,
) {
    let fixture = Fixture::new();
    for name in ["one", "two", "host"] {
        fixture.overlay(name, "target = '~'");
    }
    fixture
        .temp
        .child("repo/over.toml")
        .write_str(&format!(
            "host_overlay = 'host'\napply.default_to_host = true\nsync.default_to_host = {enabled}"
        ))
        .unwrap();
    let mut command = fixture.command("sync", &fixture.path(cwd));
    if let Some(name) = explicit {
        command.arg(name);
    }
    let assertion = command
        .args(["--verbose", "--target"])
        .arg(&fixture.ctx.root)
        .env("XDG_STATE_HOME", fixture.path("state"))
        .assert();
    if explicit.is_none() && cwd == "home" {
        assertion.failure().stderr(contains("ambiguous"));
    } else if explicit.is_none() && cwd == "outside" && !enabled {
        assertion.failure().stderr(contains("sync.default_to_host"));
    } else {
        let selected = explicit.unwrap_or(if cwd == "repo/one" { "one" } else { "host" });
        assertion.success().stdout(contains(format!(
            "Overlay {selected} has no virtual checkout"
        )));
    }
}

#[test]
fn sync_host_opt_in_without_a_host_requires_a_target() {
    let fixture = Fixture::new();
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("sync.default_to_host = true")
        .unwrap();
    fixture
        .command("sync", &fixture.path("outside"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .env("XDG_STATE_HOME", fixture.path("state"))
        .assert()
        .failure()
        .stderr(contains("over sync NAME"))
        .stderr(contains("host_overlay"));
}

#[rstest]
#[case("unapply", &[])]
#[case("commit", &[])]
#[case("restore", &["missing"])]
#[case("sync", &[])]
fn mutating_commands_require_context_regardless_of_apply_opt_in(
    #[case] command: &str,
    #[case] args: &[&str],
    #[values(false, true)] enabled: bool,
) {
    let fixture = Fixture::new();
    fixture.overlay("host", "target = '~'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str(&format!(
            "host_overlay = 'host'\napply.default_to_host = {enabled}"
        ))
        .unwrap();
    fixture
        .command(command, &fixture.path("outside"))
        .args(args)
        .arg("--target")
        .arg(&fixture.ctx.root)
        .env("XDG_STATE_HOME", fixture.path("state"))
        .assert()
        .failure()
        .stderr(contains("cannot infer an overlay"));
    assert_eq!(fs::read_dir(&fixture.ctx.root).unwrap().count(), 0);
}

#[rstest]
#[case("lint")]
#[case("doctor")]
fn global_diagnostics_include_unrelated_overlays_despite_active_and_host_context(
    #[case] command: &str,
) {
    let fixture = Fixture::new();
    fixture.overlay("host", "target = '~'");
    fixture.overlay("broken", "uses = ['missing']");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("host_overlay = 'host'")
        .unwrap();
    fixture
        .command(command, &fixture.path("repo/host"))
        .env("XDG_STATE_HOME", fixture.path("state"))
        .assert()
        .failure()
        .stdout(contains("broken"));
}

#[test]
fn enabling_host_application_without_a_host_is_actionable_and_has_no_effects() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("apply.default_to_host = true")
        .unwrap();
    fixture
        .command("apply", &fixture.path("outside"))
        .arg("--target")
        .arg(&fixture.ctx.root)
        .assert()
        .failure()
        .stderr(contains("over apply NAME"))
        .stderr(contains("host_overlay"));
    assert_eq!(fs::read_dir(&fixture.ctx.root).unwrap().count(), 0);
}

#[test]
fn add_infers_a_managed_sibling_without_a_selection_prompt() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture.overlay("two", "target = '~'");
    fixture
        .temp
        .child("repo/one/existing")
        .write_str("managed")
        .unwrap();
    fixture.temp.child("home/new").write_str("new").unwrap();
    fixture
        .command("add", &fixture.path("home"))
        .args(["--force", "--target"])
        .arg(&fixture.ctx.root)
        .arg("new")
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(fixture.path("repo/one/new")).unwrap(),
        "new"
    );
    assert!(!fixture.path("repo/two/new").exists());
}

#[test]
fn immediate_siblings_do_not_include_scaffolds_or_recursive_neighbors() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture.overlay("two", "target = '~'");
    fixture
        .temp
        .child("repo/one/nested/file")
        .write_str("content")
        .unwrap();
    assert!(
        fixture
            .infer(&["home/new"])
            .unwrap_err()
            .to_string()
            .contains("ambiguous overlay")
    );
    fixture
        .temp
        .child("repo/two/nested/other")
        .write_str("content")
        .unwrap();
    assert!(
        fixture
            .infer(&["home/nested/new"])
            .unwrap_err()
            .to_string()
            .contains("one, two")
    );
}

#[test]
fn partial_files_outside_the_base_target_supply_ownership_and_sibling_evidence() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture.overlay("two", "target = '~'");
    fixture
        .temp
        .child("repo/one/block.partial.toml")
        .write_str(&format!(
            "target = '{}'\ncontent = 'block'\n",
            fixture.path("outside/config").display()
        ))
        .unwrap();
    assert_eq!(
        fixture.infer(&["outside/config"]).unwrap().as_deref(),
        Some("one")
    );
    assert_eq!(
        fixture.infer(&["outside/new"]).unwrap().as_deref(),
        Some("one")
    );
}

#[test]
fn linked_worktrees_read_their_effective_association_from_nested_directories() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture.overlay("two", "target = '~'");
    let repo = git2::Repository::init(fixture.path("outside/main")).unwrap();
    let tree = repo
        .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
        .unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    repo.config()
        .unwrap()
        .set_str("over.overlay", "one")
        .unwrap();
    repo.config()
        .unwrap()
        .set_bool("extensions.worktreeConfig", true)
        .unwrap();
    repo.worktree("linked", &fixture.path("outside/linked"), None)
        .unwrap();
    let worktree = git2::Repository::open(fixture.path("outside/linked")).unwrap();
    fs::write(
        worktree.path().join("config.worktree"),
        "[over]\noverlay = two\n",
    )
    .unwrap();
    assert_eq!(
        fixture
            .infer(&["outside/linked/nested/missing"])
            .unwrap()
            .as_deref(),
        Some("two")
    );
    assert_eq!(
        fixture.infer(&["outside/main"]).unwrap().as_deref(),
        Some("one")
    );
}

#[cfg(unix)]
#[test]
fn directory_aliases_and_directory_links_keep_deployment_ownership() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    fixture.overlay(
        "one",
        "target = '~'\nrules = [{path = 'directory', mode = 'symlink-directory'}]",
    );
    fixture.overlay("two", "target = '~'");
    fixture
        .temp
        .child("repo/one/directory/file")
        .write_str("content")
        .unwrap();
    symlink(fixture.path("home"), fixture.path("alias")).unwrap();
    symlink(
        fixture.path("repo/one/directory"),
        fixture.path("home/directory"),
    )
    .unwrap();
    assert_eq!(
        fixture.infer(&["home/directory/file"]).unwrap().as_deref(),
        Some("one")
    );
    assert_eq!(
        fixture.infer(&["alias/directory/file"]).unwrap().as_deref(),
        Some("one")
    );
}
