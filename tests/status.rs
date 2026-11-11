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
        .arg("--target")
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
        .stdout(contains("composition:\n    ├─→ base\n    └─→ selected"))
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
        .stdout(contains("no-uses"))
        .stdout(contains("target"))
        .stdout(contains("--root").not());
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
        .stdout(contains("--target"));
}

fn commit_all(repo: &git2::Repository) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
}

#[rstest::rstest]
#[case::checkout(false)]
#[case::linked_worktree(true)]
fn status_checks_overlay_files_and_mount_configuration_without_reading_mount_content(
    #[case] linked_worktree: bool,
) {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    let state = temp.path().join("state");
    let upstream = temp.path().join("upstream");
    fs::create_dir_all(source.join("selected/project")).unwrap();
    fs::create_dir_all(&root).unwrap();
    let upstream_repo = git2::Repository::init(&upstream).unwrap();
    fs::write(upstream.join("repository.txt"), "repository content\n").unwrap();
    commit_all(&upstream_repo);
    fs::write(
        source.join("selected/over.yaml"),
        "target: '~'\nmode: checkout\n",
    )
    .unwrap();
    fs::write(source.join("selected/overlay.txt"), "overlay\n").unwrap();
    fs::write(source.join("selected/project/managed.txt"), "managed\n").unwrap();
    fs::write(source.join(".gitattributes"), "*.txt filter=probe text\n").unwrap();
    let source_repo = git2::Repository::init(&source).unwrap();
    commit_all(&source_repo);
    command(&source, &root, "apply")
        .env("XDG_STATE_HOME", &state)
        .args(["selected", "--no-prompt"])
        .assert()
        .success();

    // Mount after applying: initialization intentionally protects existing Git
    // boundaries, while read-only status must still inspect overlay-tracked paths.
    let mounted = root.join("project");
    fs::remove_file(mounted.join("managed.txt")).unwrap();
    fs::remove_dir(&mounted).unwrap();
    let mounted_repo = if linked_worktree {
        upstream_repo.worktree("mounted", &mounted, None).unwrap();
        let repo = git2::Repository::open(&mounted).unwrap();
        repo.remote("origin", upstream.to_str().unwrap()).unwrap();
        repo
    } else {
        git2::Repository::clone(upstream.to_str().unwrap(), &mounted).unwrap()
    };
    fs::write(mounted.join("managed.txt"), "managed\n").unwrap();
    fs::write(
        source.join("selected/over.yaml"),
        format!(
            "target: '~'\nmode: checkout\ngit:\n  project:\n    url: '{}'\n",
            upstream.display(),
        ),
    )
    .unwrap();
    let repository = over::overlays::Repository::new(source.clone());
    let overlay = repository.get("selected").unwrap();
    let ctx = over::exec::Context::builder()
        .root(root.clone())
        .repository(repository)
        .overlay(overlay.clone())
        .build();
    let desired = over::desired::DesiredTree::build(&ctx, &overlay).unwrap();
    over::git_exclude::reconcile_desired(&desired).unwrap();

    // If status touches repository-owned content, Git's required filter fails.
    // Raw unchanged overlay blobs bypass it, so this detects scanning deterministically.
    source_repo
        .config()
        .unwrap()
        .set_str("filter.probe.clean", "exit 42")
        .unwrap();
    source_repo
        .config()
        .unwrap()
        .set_bool("filter.probe.required", true)
        .unwrap();
    fs::write(mounted.join("repository.txt"), "dirty\r\n").unwrap();
    fs::write(mounted.join("untracked.txt"), "untracked\r\n").unwrap();
    fs::write(root.join("unrelated.txt"), "unrelated\r\n").unwrap();
    for verb in ["status", "show"] {
        command(&source, &root, verb)
            .env("XDG_STATE_HOME", &state)
            .args(["selected", "--verbose"])
            .assert()
            .success()
            .stdout(contains("Fully applied"))
            .stdout(contains("repository.txt").not())
            .stdout(contains("untracked.txt").not())
            .stdout(contains("unrelated.txt").not())
            .stdout(contains(
                "    configuration: present\n    latest contributing scope:",
            ))
            .stderr("");
    }

    let calls = temp.path().join("filter-calls");
    source_repo
        .config()
        .unwrap()
        .set_str(
            "filter.probe.clean",
            &format!("printf x >> '{}'; cat", calls.display()),
        )
        .unwrap();
    fs::write(mounted.join("managed.txt"), "overlay edit\n").unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .code(1)
        .stdout(contains("M project/managed.txt (unstaged)"))
        .stdout(contains("repository.txt").not());
    assert_eq!(fs::read_to_string(&calls).unwrap(), "x");
    fs::write(mounted.join("managed.txt"), "managed\n").unwrap();
    fs::remove_file(root.join("overlay.txt")).unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .code(1)
        .stdout(contains("D overlay.txt (unstaged)"));
    fs::write(root.join("overlay.txt"), "overlay\n").unwrap();

    mounted_repo
        .remote_set_url("origin", "https://example.com/drift.git")
        .unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .code(1)
        .stdout(contains(format!("conflict: {}", mounted.display())))
        .stdout(contains("repository.txt").not());
    fs::rename(&mounted, temp.path().join("removed-mount")).unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .code(1)
        .stdout(contains(format!("missing: {}", mounted.display())));
}

#[test]
fn status_defers_unknown_key_warnings_to_configuration_diagnostics() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    fs::create_dir_all(source.join("selected")).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(
        source.join("selected/over.yaml"),
        "target: '~'\nunknown_option: true\n",
    )
    .unwrap();
    for verb in ["status", "show"] {
        command(&source, &root, verb)
            .arg("selected")
            .assert()
            .success()
            .stdout(contains("unknown key").not())
            .stderr("");
    }
    command(&source, &root, "lint")
        .arg("selected")
        .assert()
        .stdout(contains("unknown_option"));
}

#[test]
fn index_only_overlay_additions_remain_visible_after_target_discovery_is_disabled() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    let state = temp.path().join("state");
    fs::create_dir_all(source.join("selected")).unwrap();
    fs::write(
        source.join("selected/over.yaml"),
        "target: '~'\nmode: checkout\n",
    )
    .unwrap();
    fs::write(source.join("selected/file"), "original\n").unwrap();
    let repo = git2::Repository::init(&source).unwrap();
    commit_all(&repo);
    command(&source, &root, "apply")
        .env("XDG_STATE_HOME", &state)
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    fs::write(root.join("new"), "added\n").unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .success();

    // The new file has no base/HEAD entry: only the index can select it for inspection.
    fs::write(source.join("selected/new"), "added\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("selected/new")).unwrap();
    index.write().unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .code(1)
        .stdout(contains("A new (staged)"))
        .stdout(contains("new (unstaged)").not());
    fs::write(root.join("new"), "edited after staging\n").unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .code(1)
        .stdout(contains("A new (staged)"))
        .stdout(contains("M new (unstaged)"));
}
