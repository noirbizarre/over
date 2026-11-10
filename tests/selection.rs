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
        .write_str("default_overlay = 'one'")
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
fn contextual_selection_precedes_default_without_changing_default_metadata() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~/one'\n[data]\nowner = 'one'");
    fixture.overlay(
        "two",
        "target = '~/two'\n[data]\nowner = 'two'\ndefault_data = true",
    );
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("default_overlay = 'two'")
        .unwrap();
    let overlay = selection::select(&fixture.ctx, None, &[fixture.path("home/one")], true).unwrap();
    assert_eq!(overlay.name, "one");
    let ctx = fixture.ctx.with_overlay(overlay).resolve_data().unwrap();
    let data = serde_json::to_value(&ctx.data).unwrap();
    assert_eq!(data["owner"], serde_json::json!("one"));
    assert_eq!(data["default_data"], serde_json::json!(true));
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
    fixture.overlay("one", "target = '~'\n[map.config]\ntarget = '../mapped'");
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

#[test]
fn add_does_not_use_the_default_when_its_operands_have_no_context() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("default_overlay = 'one'")
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
        .write_str("default_overlay = 'one'")
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
        .write_str("default_overlay = 'two'")
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
        .write_str("default_overlay = 'two'")
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
fn data_infers_source_context_before_the_configured_default() {
    let fixture = Fixture::new();
    fixture.overlay("one", "target = '~'\n[data]\nowner = 'one'");
    fixture.overlay("two", "target = '~'\n[data]\nowner = 'two'");
    fixture
        .temp
        .child("repo/over.toml")
        .write_str("default_overlay = 'two'")
        .unwrap();
    fixture
        .command("data", &fixture.path("repo/one"))
        .assert()
        .success()
        .stdout(contains("\"owner\":\"one\""));
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
