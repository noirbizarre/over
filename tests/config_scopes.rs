use std::fs;
use std::path::Path;

use assert_cmd::Command;
use over::exec::Context;
use over::overlays::{FileMode, MaterializationKind, Repository};
use predicates::str::contains;
use rstest::rstest;
use tempfile::TempDir;

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[rstest]
#[case("mode: checkout\n", "mode: symlink\n", MaterializationKind::Symlink)]
#[case(
    "rules:\n  - {path: group/ov/config, mode: checkout}\n",
    "rules: []\n",
    MaterializationKind::Checkout
)]
fn empty_child_rules_preserve_ancestors_and_explicit_baselines_override_them(
    #[case] root_config: &str,
    #[case] child_config: &str,
    #[case] expected: MaterializationKind,
) {
    let temp = TempDir::new().unwrap();
    write(temp.path(), "over.yaml", root_config);
    write(temp.path(), "group/over.yaml", "permissions: '600'\n");
    write(temp.path(), "group/ov/over.yaml", child_config);
    let overlay = Repository::new(temp.path().into()).get("group/ov").unwrap();
    assert_eq!(
        overlay.materialization_for(Path::new("config/editor")),
        expected
    );
    assert_eq!(
        overlay.permission_for(Path::new("config/partial")),
        Some(FileMode::parse("600").unwrap())
    );
}

#[test]
fn inherited_destinations_and_mapping_entries_keep_independent_origins() {
    let temp = TempDir::new().unwrap();
    write(
        temp.path(),
        "over.yaml",
        "target: ../deployed\nmap:\n  group/ov/config:\n    target: ../root-config\n  group/ov/etc:\n    target: ../root-etc\nrules:\n  - {path: group/ov/config, mode: checkout}\nexclude: [group/ov/secret]\n",
    );
    write(
        temp.path(),
        "group/over.yaml",
        "map:\n  ov/config:\n    target: ../../group-config\n",
    );
    fs::create_dir_all(temp.path().join("group/ov")).unwrap();
    let repo = Repository::new(temp.path().into());
    let overlay = repo.get("group/ov").unwrap();
    let ctx = Context::builder()
        .repository(repo)
        .root(temp.path().join("home"))
        .build();
    let routing = overlay.resolve_mappings(&ctx).unwrap();
    assert_eq!(routing.base, temp.path().parent().unwrap().join("deployed"));
    assert_eq!(
        routing.target_for(Path::new("config/file")),
        temp.path().parent().unwrap().join("group-config/file")
    );
    assert_eq!(
        routing.target_for(Path::new("etc/file")),
        temp.path().parent().unwrap().join("root-etc/file")
    );
    assert_eq!(
        overlay.materialization_for(Path::new("config")),
        MaterializationKind::Checkout
    );
    assert!(overlay.is_excluded(Path::new("secret/file")));
    assert!(!overlay.is_excluded(Path::new("nested/secret/file")));
}

#[test]
fn inherited_local_git_sources_keep_the_declaring_root() {
    let temp = TempDir::new().unwrap();
    write(
        temp.path(),
        "over.yaml",
        "git:\n  ov/vendor:\n    url: local/repository\n    remotes:\n      mirror:\n        url: ../mirror\n",
    );
    write(temp.path(), "ov/over.yaml", "description: child\n");
    let overlay = Repository::new(temp.path().into()).get("ov").unwrap();
    let git = overlay.git.unwrap();
    assert_eq!(
        Path::new(&git["vendor"].url),
        temp.path().join("local/repository")
    );
}

#[rstest]
#[case("install: {apt: [curl]}\n", "migrate to `hooks.install`")]
#[case("defaults: {materialization: checkout}\n", "no longer supported")]
#[case("link_dirs: [config/*]\n", "no longer supported")]
#[case("permissions: [{path: secret, mode: '600'}]\n", "octal string")]
#[case("rules: [{path: config, materialization: checkout}]\n", "mode")]
#[case("rules: [{path: config}]\n", "must set mode or permissions")]
#[case(
    "rules: [{path: 'config/*', mode: checkout}]\n",
    "requires mode: symlink-directory"
)]
#[case(
    "rules: [{path: 'config/*', permissions: '600'}]\n",
    "requires mode: symlink-directory"
)]
fn obsolete_or_invalid_ancestor_rules_are_not_hidden_by_children(
    #[case] config: &str,
    #[case] message: &str,
) {
    let temp = TempDir::new().unwrap();
    write(temp.path(), "over.yaml", config);
    write(
        temp.path(),
        "ov/over.yaml",
        "mode: symlink\npermissions: '644'\nrules: []\n",
    );
    let error = Repository::new(temp.path().into()).get("ov").unwrap_err();
    assert!(format!("{error:#}").contains(message), "{error:#}");
}

#[test]
fn apply_resolves_descriptor_and_sidecar_paths_identically_from_unrelated_directories() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("source");
    write(
        &repo,
        "over.yaml",
        "target: ../deployed\nmap:\n  ov/config:\n    target: ../mapped\nexclude: [ov/secret]\n",
    );
    write(&repo, "ov/file", "base");
    write(&repo, "ov/config/file", "mapped");
    write(&repo, "ov/secret/file", "excluded");
    write(
        &repo,
        "ov/nested/extra.link.toml",
        "target = '../../referent'\n",
    );
    write(
        &repo,
        "ov/nested/block.partial.toml",
        "target = '../../partial'\ncontent = 'managed'\n",
    );
    write(temp.path(), "referent", "external");
    for directory in ["first", "second"] {
        fs::create_dir_all(temp.path().join(directory)).unwrap();
        Command::cargo_bin("over")
            .unwrap()
            .current_dir(temp.path().join(directory))
            .env("XDG_STATE_HOME", temp.path().join("state"))
            .arg("--home")
            .arg(&repo)
            .args(["apply", "ov", "--no-prompt"])
            .assert()
            .success();
        assert_eq!(
            fs::read_link(temp.path().join("deployed/file")).unwrap(),
            repo.join("ov/file")
        );
        assert_eq!(
            fs::read_link(temp.path().join("mapped/file")).unwrap(),
            repo.join("ov/config/file")
        );
        assert_eq!(
            fs::read(temp.path().join("deployed/nested/extra")).unwrap(),
            b"external"
        );
        assert!(
            fs::read_to_string(temp.path().join("partial"))
                .unwrap()
                .contains("managed")
        );
        assert!(!temp.path().join("deployed/secret").exists());
    }
}

#[test]
fn lint_reports_cross_scope_glob_boundaries_and_exits_unsuccessfully() {
    let temp = TempDir::new().unwrap();
    write(
        temp.path(),
        "over.yaml",
        "rules:\n  - {path: ov/config/*, mode: symlink-directory}\n",
    );
    write(
        temp.path(),
        "ov/over.yaml",
        "rules:\n  - {path: config/editor/nested, mode: symlink}\n",
    );
    write(temp.path(), "ov/config/editor/nested/file", "content");
    Command::cargo_bin("over")
        .unwrap()
        .arg("--home")
        .arg(temp.path())
        .args(["lint", "ov"])
        .assert()
        .failure()
        .stdout(contains("unreachable"))
        .stdout(contains("symlink-directory"));
}

#[rstest]
#[case("mode: checkout\n", "rules: [{path: config, mode: symlink}]\n", true)]
#[case(
    "mode: symlink-directory\n",
    "rules: [{path: config/editor, mode: checkout}]\n",
    true
)]
#[case(
    "rules: [{path: ov/config, mode: symlink-directory}]\n",
    "mode: symlink\nrules: [{path: config/editor, mode: checkout}]\n",
    false
)]
#[case(
    "mode: checkout\n",
    "map: {config: {target: '~/mapped'}}\nrules: [{path: config, mode: symlink}]\n",
    false
)]
#[case(
    "mode: checkout\n",
    "rules: [{path: config/editor, permissions: '600'}]\n",
    false
)]
#[case(
    "mode: symlink\n",
    "rules: [{path: 'config/**', mode: symlink-directory}]\n",
    false
)]
fn lint_uses_final_boundaries_and_respects_mapping_and_permission_ownership(
    #[case] root_config: &str,
    #[case] child_config: &str,
    #[case] unreachable: bool,
) {
    let temp = TempDir::new().unwrap();
    write(temp.path(), "over.yaml", root_config);
    write(temp.path(), "ov/over.yaml", child_config);
    write(temp.path(), "ov/config/editor/nested/file", "content");
    let repo = Repository::new(temp.path().into());
    let result = over::lint::lint_overlay(&repo, "ov", false).unwrap();
    assert_eq!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unreachable")),
        unreachable,
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn root_and_overlay_rule_paths_share_the_same_normalized_source_identity() {
    let temp = TempDir::new().unwrap();
    write(
        temp.path(),
        "over.yaml",
        "rules: [{path: ov/config, mode: checkout}]\n",
    );
    write(
        temp.path(),
        "ov/over.yaml",
        "rules: [{path: './unused/../config', permissions: '600'}]\n",
    );
    let overlay = Repository::new(temp.path().into()).get("ov").unwrap();
    assert_eq!(
        overlay.materialization_for(Path::new("config/child")),
        MaterializationKind::Checkout
    );
    assert_eq!(
        overlay.permission_for(Path::new("config/child")),
        Some(FileMode::parse("600").unwrap())
    );
    assert_eq!(
        overlay.materialization_for(Path::new("configuration")),
        MaterializationKind::Symlink
    );
}

#[rstest]
#[case(
    "rules: [{path: config, materialization: checkout}]\n",
    "replace it with `mode`"
)]
#[case(
    "permissions: [{path: config, mode: '600'}]\n",
    "move permission overrides into `rules`"
)]
fn lint_retains_migration_guidance_for_removed_nested_syntax(
    #[case] config: &str,
    #[case] hint: &str,
) {
    let temp = TempDir::new().unwrap();
    write(temp.path(), "over.yaml", config);
    write(temp.path(), "ov/over.yaml", "mode: symlink\n");
    let result =
        over::lint::lint_overlay(&Repository::new(temp.path().into()), "ov", false).unwrap();
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(hint)),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn absolute_exclusions_select_the_same_source_as_scoped_relative_paths() {
    let temp = TempDir::new().unwrap();
    write(
        temp.path(),
        "ov/over.yaml",
        &format!(
            "exclude: [{}]\n",
            serde_json::to_string(&temp.path().join("ov/secret")).unwrap()
        ),
    );
    let overlay = Repository::new(temp.path().into()).get("ov").unwrap();
    assert!(overlay.is_excluded(Path::new("secret/file")));
    assert!(!overlay.is_excluded(Path::new("secrets/file")));
}
