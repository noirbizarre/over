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

fn worktree_identity(repo: &git2::Repository) -> std::path::PathBuf {
    // libgit2 resolves macOS temporary-directory aliases and omits a Windows verbatim prefix.
    repo.workdir().unwrap().components().collect()
}

#[test]
fn status_show_and_doctor_report_obstructed_scaffolds_without_panicking() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    fs::create_dir_all(source.join("selected/nested")).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(
        source.join("selected/over.toml"),
        "target='~/blocked'\n[[rules]]\npath='nested'\nmode='checkout'\n",
    )
    .unwrap();
    fs::write(source.join("selected/nested/file"), "managed").unwrap();
    fs::write(root.join("blocked"), "preserve").unwrap();
    for verb in ["status", "show", "doctor"] {
        command(&source, &root, verb)
            .arg("selected")
            .env("XDG_STATE_HOME", temp.path().join("state"))
            .assert()
            .code(1)
            .stderr(contains("panicked").not());
    }
    assert_eq!(fs::read(root.join("blocked")).unwrap(), b"preserve");
}

#[test]
fn globally_ignored_overlay_files_need_no_managed_exclusion_in_status_or_doctor() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    let state = temp.path().join("state");
    fs::create_dir_all(source.join("selected")).unwrap();
    fs::write(source.join("selected/over.toml"), "target = '~'\n").unwrap();
    fs::write(source.join("selected/mise.toml"), "# managed").unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    symlink::symlink_file(source.join("selected/mise.toml"), root.join("mise.toml")).unwrap();
    let global = temp.path().join(".gitconfig");
    let ignore = temp.path().join("global-ignore");
    fs::write(&ignore, "mise.toml\n").unwrap();
    fs::write(&global, "").unwrap();
    // Git quotes native Windows backslashes rather than interpreting them as escapes.
    git2::Config::open(&global)
        .unwrap()
        .set_str("core.excludesFile", ignore.to_str().unwrap())
        .unwrap();
    let before = fs::read(repo.path().join("info/exclude")).unwrap();
    for verb in ["status", "doctor"] {
        command(&source, &root, verb)
            .arg("selected")
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("HOME", temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("XDG_STATE_HOME", &state)
            .assert()
            .success()
            .stdout(contains("exclude missing").not());
    }
    assert_eq!(fs::read(repo.path().join("info/exclude")).unwrap(), before);
    fs::write(&ignore, "").unwrap();
    command(&source, &root, "status")
        .arg("selected")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("XDG_STATE_HOME", &state)
        .assert()
        .code(1)
        .stdout(contains("Git configuration:"))
        .stdout(contains("exclude missing: mise.toml"));
    command(&source, &root, "doctor")
        .arg("selected")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("XDG_STATE_HOME", &state)
        .assert()
        .success()
        .stdout(contains("1 warning found"))
        .stdout(contains("selected: ⚠ 1 warning(s)"))
        .stdout(contains("exclude missing").not());
}

#[test]
fn doctor_marks_every_affected_composition_but_counts_a_shared_problem_once() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    fs::create_dir_all(&root).unwrap();
    for name in ["shared", "alpha", "beta"] {
        fs::create_dir_all(source.join(name)).unwrap();
        fs::write(
            source.join(name).join("over.toml"),
            if name == "shared" {
                "target = '~'\n"
            } else {
                "target = '~'\nuses = ['shared']\n"
            },
        )
        .unwrap();
    }
    fs::write(source.join("shared/file"), "content").unwrap();
    command(&source, &root, "doctor")
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .assert()
        .success()
        .stdout(contains("alpha: ⚠ 1 warning(s)"))
        .stdout(contains("beta: ⚠ 1 warning(s)"))
        .stdout(contains("shared: ⚠ 1 warning(s)"))
        .stdout(contains("1 warning found"));
    command(&source, &root, "doctor")
        .args(["alpha", "--no-uses"])
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .assert()
        .success()
        .stdout(contains("warning(s)").not());
}

#[rstest::rstest]
#[case("base")]
#[case("./base")]
fn invalid_shared_dependencies_keep_lints_original_error_and_consumer_warnings(
    #[case] dependency: &str,
) {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    fs::create_dir_all(&root).unwrap();
    for name in ["base", "alpha", "beta"] {
        fs::create_dir_all(source.join(name)).unwrap();
        fs::write(
            source.join(name).join("over.toml"),
            if name == "base" {
                "broken [[[".into()
            } else {
                format!("target = '~'\nuses = ['{dependency}']\n")
            },
        )
        .unwrap();
    }
    command(&source, &root, "doctor")
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .assert()
        .code(1)
        .stdout(contains("base: ✘ 1 error(s)"))
        .stdout(contains("alpha: ⚠ 1 warning(s)"))
        .stdout(contains("beta: ⚠ 1 warning(s)"))
        .stdout(contains("1 error, 2 warnings found"));
}

#[test]
fn doctor_inspects_a_shared_checkout_once_across_consumers() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    let state = temp.path().join("state");
    for name in ["base", "alpha", "beta"] {
        fs::create_dir_all(source.join(name)).unwrap();
        let descriptor = if name == "base" {
            "target: '~/shared'\nmode: checkout\n".to_owned()
        } else {
            format!("target: '~/{name}'\nuses: [base]\n")
        };
        fs::write(source.join(name).join("over.yaml"), descriptor).unwrap();
    }
    fs::write(source.join("base/file.txt"), "original\n").unwrap();
    fs::write(source.join(".gitattributes"), "*.txt filter=probe text\n").unwrap();
    let repo = git2::Repository::init(&source).unwrap();
    commit_all(&repo);
    for name in ["alpha", "beta"] {
        command(&source, &root, "apply")
            .args([name, "--no-prompt"])
            .env("XDG_STATE_HOME", &state)
            .assert()
            .success();
    }
    let calls = temp.path().join("calls");
    repo.config()
        .unwrap()
        .set_str(
            "filter.probe.clean",
            &format!("printf x >> '{}'; cat", calls.display()),
        )
        .unwrap();
    fs::write(root.join("shared/file.txt"), "edited\n").unwrap();
    command(&source, &root, "doctor")
        .env("XDG_STATE_HOME", &state)
        .assert()
        .success()
        .stdout(contains("No issues found"))
        .stdout(contains("warning(s)").not());
    assert_eq!(fs::read_to_string(calls).unwrap(), "x");
}

#[test]
fn doctor_summarizes_mixed_deployment_and_static_health_while_status_keeps_details() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    fs::create_dir_all(&root).unwrap();
    for name in ["healthy", "missing", "conflict", "invalid"] {
        fs::create_dir_all(source.join(name)).unwrap();
        fs::write(
            source.join(name).join("over.toml"),
            if name == "invalid" {
                "broken [[["
            } else {
                "target = '~'\n"
            },
        )
        .unwrap();
    }
    fs::write(source.join("missing/not-deployed"), "content").unwrap();
    fs::write(source.join("conflict/collision"), "overlay").unwrap();
    fs::write(root.join("collision"), "user content").unwrap();
    command(&source, &root, "doctor")
        .arg("--verbose")
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .assert()
        .code(1)
        .stdout(contains("healthy: ✔"))
        .stdout(contains("missing: ⚠ 1 warning(s)"))
        .stdout(contains("conflict: ✘ 1 error(s)"))
        .stdout(contains("invalid: ✘ 1 error(s)"))
        .stdout(contains("collision").not())
        .stdout(contains("not-deployed").not());
    command(&source, &root, "status")
        .arg("conflict")
        .assert()
        .code(1)
        .stdout(contains("conflict:"))
        .stdout(contains("collision"));
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
        "uses: [base]\nrules:\n  - {path: config, target: '~/mapped'}\n",
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
    mounted_repo
        .config()
        .unwrap()
        .set_str("over.overlay", "selected")
        .unwrap();
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
        .success()
        .stdout(contains("M selected/project/managed.txt (unstaged)"))
        .stdout(contains("repository.txt").not());
    assert_eq!(fs::read_to_string(&calls).unwrap(), "x");
    fs::write(&calls, "").unwrap();
    command(&source, &root, "doctor")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .success()
        .stdout(contains("warning(s)").not())
        .stdout(contains("project/managed.txt").not());
    assert_eq!(fs::read_to_string(&calls).unwrap(), "x");
    fs::write(mounted.join("managed.txt"), "managed\n").unwrap();
    fs::remove_file(root.join("overlay.txt")).unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .success()
        .stdout(contains("D selected/overlay.txt (unstaged)"));
    fs::write(root.join("overlay.txt"), "overlay\n").unwrap();

    mounted_repo
        .remote_set_url("origin", "https://example.com/drift.git")
        .unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .code(1)
        .stdout(contains("config incorrect: remote.origin.url"))
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
        .success()
        .stdout(contains("A selected/new (staged)"))
        .stdout(contains("new (unstaged)").not());
    fs::write(root.join("new"), "edited after staging\n").unwrap();
    command(&source, &root, "status")
        .env("XDG_STATE_HOME", &state)
        .arg("selected")
        .assert()
        .success()
        .stdout(contains("A selected/new (staged)"))
        .stdout(contains("M selected/new (unstaged)"));
}

struct CheckoutFixture {
    temp: TempDir,
    source: std::path::PathBuf,
    root: std::path::PathBuf,
    repo: git2::Repository,
}

impl CheckoutFixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let root = temp.path().join("home");
        fs::create_dir_all(source.join("selected")).unwrap();
        fs::write(
            source.join("selected/over.yaml"),
            "target: '~'\nmode: checkout\n",
        )
        .unwrap();
        fs::write(source.join("selected/file"), "original\n").unwrap();
        let repo = git2::Repository::init(&source).unwrap();
        commit_all(&repo);
        let fixture = Self {
            temp,
            source,
            root,
            repo,
        };
        fixture
            .command("apply")
            .args(["selected", "--no-prompt"])
            .assert()
            .success();
        fixture
    }

    fn command(&self, verb: &str) -> Command {
        let mut cmd = command(&self.source, &self.root, verb);
        cmd.env("XDG_STATE_HOME", self.temp.path().join("state"));
        cmd
    }

    fn stage(&self, name: &str) {
        self.command("add")
            .args(["--overlay", "selected"])
            .arg(self.root.join(name))
            .assert()
            .success();
    }

    fn row(&self, xy: &str, path: &str) -> String {
        format!(
            "{xy}\t{}\t{}\t{}\n",
            serde_json::to_string(&worktree_identity(&self.repo)).unwrap(),
            serde_json::to_string(&format!("selected/{path}")).unwrap(),
            serde_json::to_string(&self.root).unwrap()
        )
    }
}

#[test]
fn adding_a_file_reports_staging_only_in_the_final_changes_section() {
    let fixture = CheckoutFixture::new();
    fs::write(fixture.root.join("new file"), "added\n").unwrap();
    fixture.stage("new file");
    let output = fixture
        .command("status")
        .arg("selected")
        .assert()
        .success()
        .stdout(contains("Fully applied"))
        .stdout(contains("Needs attention").not())
        .stdout(contains("A selected/new file (staged)"))
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    let (health, changes) = output.split_once("Changes:").unwrap();
    assert!(!health.contains("(staged)"));
    assert!(!changes.contains("Reconciliation:"));
    for verb in ["status", "show"] {
        fixture
            .command(verb)
            .args(["selected", "--short", "--verbose"])
            .env("CLICOLOR_FORCE", "1")
            .assert()
            .success()
            .stdout(fixture.row("A ", "new file"))
            .stderr("");
    }
    fixture
        .command("doctor")
        .arg("selected")
        .assert()
        .success()
        .stdout(contains("warning(s)").not());
}

#[test]
fn short_status_preserves_staged_and_further_unstaged_snapshots() {
    let fixture = CheckoutFixture::new();
    fixture
        .command("status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout("");
    fs::write(fixture.root.join("file"), "staged\n").unwrap();
    fixture.stage("file");
    fixture
        .command("status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout(fixture.row("M ", "file"));
    fs::write(fixture.root.join("file"), "edited again\n").unwrap();
    fixture
        .command("status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout(fixture.row("MM", "file"));
    fs::remove_file(fixture.root.join("file")).unwrap();
    fixture
        .command("status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout(fixture.row("MD", "file"));
    fixture.stage("file");
    fixture
        .command("status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout(fixture.row("D ", "file"));
}

#[test]
fn intent_to_add_is_only_an_unstaged_addition() {
    let fixture = CheckoutFixture::new();
    fs::write(fixture.root.join("reserved"), "new\n").unwrap();
    fs::write(fixture.source.join("selected/reserved"), "new\n").unwrap();
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&fixture.source)
            .args(["add", "--intent-to-add", "selected/reserved"])
            .output()
            .unwrap()
            .status
            .success()
    );
    fixture
        .command("status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout(fixture.row(" A", "reserved"));
}

#[test]
fn short_status_keeps_distinct_targets_for_the_same_source_path() {
    let fixture = CheckoutFixture::new();
    // Independent invocations may materialize the same source into different target roots.
    // Use separate state homes so the live-target guard does not reinterpret an existing association.
    let other = fixture.temp.path().join("other");
    command(&fixture.source, &other, "apply")
        .args(["selected", "--no-prompt"])
        .env("XDG_STATE_HOME", fixture.temp.path().join("other-state"))
        .assert()
        .success();
    fs::write(fixture.root.join("file"), "first\n").unwrap();
    fs::write(other.join("file"), "second\n").unwrap();
    fixture
        .command("status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout(fixture.row(" M", "file"));
    command(&fixture.source, &other, "status")
        .args(["selected", "-s"])
        .env("XDG_STATE_HOME", fixture.temp.path().join("other-state"))
        .assert()
        .success()
        .stdout(format!(
            " M\t{}\t\"selected/file\"\t{}\n",
            serde_json::to_string(&worktree_identity(&fixture.repo)).unwrap(),
            serde_json::to_string(&other).unwrap()
        ));
}

#[test]
fn source_updates_remain_actionable_independently_of_staging() {
    let fixture = CheckoutFixture::new();
    fs::write(fixture.source.join("selected/file"), "upstream\n").unwrap();
    let mut index = fixture.repo.index().unwrap();
    index.add_path(Path::new("selected/file")).unwrap();
    let tree = fixture.repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = fixture.repo.head().unwrap().peel_to_commit().unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    fixture
        .repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "advance",
            &tree,
            &[&parent],
        )
        .unwrap();
    index.write().unwrap();
    fixture
        .command("status")
        .arg("selected")
        .assert()
        .code(1)
        .stdout(contains("behind:"))
        .stdout(contains("Changes:"));
    fs::write(fixture.root.join("file"), "independent\n").unwrap();
    fixture
        .command("status")
        .arg("selected")
        .assert()
        .code(1)
        .stdout(contains("conflict:"));
    // Converged bytes need no content reconciliation even while the recorded base is older.
    fs::write(fixture.root.join("file"), "upstream\n").unwrap();
    fixture
        .command("status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn all_overlay_changes_follow_the_overall_summary_without_duplicating_dependencies() {
    let fixture = CheckoutFixture::new();
    for name in ["alpha", "beta"] {
        fs::create_dir_all(fixture.source.join(name)).unwrap();
        fs::write(
            fixture.source.join(name).join("over.yaml"),
            format!("target: '~/{name}'\nuses: [selected]\n"),
        )
        .unwrap();
        fixture
            .command("apply")
            .args([name, "--no-prompt"])
            .assert()
            .success();
    }
    fs::write(fixture.root.join("file"), "edited\n").unwrap();
    let output = fixture
        .command("status")
        .arg("--all")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    assert!(output.find("Overall:").unwrap() < output.find("Changes:").unwrap());
    assert_eq!(output.matches("M selected/file (unstaged)").count(), 1);
    fixture
        .command("status")
        .args(["--all", "-s"])
        .assert()
        .success()
        .stdout(fixture.row(" M", "file"));
}

#[cfg(unix)]
#[test]
fn short_status_escapes_control_characters_in_git_paths() {
    let fixture = CheckoutFixture::new();
    let name = "space tab\tquote\"line\n";
    fs::write(fixture.root.join(name), "added\n").unwrap();
    fixture.stage(name);
    fixture
        .command("status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout(fixture.row("A ", name));
}

#[test]
fn source_context_avoids_unrelated_overlay_resolution_and_administration_discovery() {
    let fixture = CheckoutFixture::new();
    for number in 0..100 {
        let dir = fixture.source.join(format!("unrelated-{number}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("over.yaml"),
            "target: '{{ env.OVER_STATUS_MISSING }}'\nunknown_key: true\n",
        )
        .unwrap();
    }
    fs::create_dir_all(fixture.source.join(".git/unrelated")).unwrap();
    fs::write(fixture.source.join(".git/unrelated/over.yaml"), "[broken").unwrap();
    for number in 0..100 {
        let dir = fixture.root.join(format!("untracked-{number}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("file"), "unrelated\n").unwrap();
    }
    fixture
        .command("status")
        .current_dir(fixture.source.join("selected"))
        .arg("-s")
        .assert()
        .success()
        .stdout("")
        .stderr("");
}

#[cfg(unix)]
#[test]
fn non_utf8_index_paths_only_fail_the_overlay_that_owns_them() {
    let fixture = CheckoutFixture::new();
    let mut index = fixture.repo.index().unwrap();
    let mut entry = index.get_path(Path::new("selected/file"), 0).unwrap();
    entry.path = b"unrelated-\xff".to_vec();
    index.add(&entry).unwrap();
    index.write().unwrap();
    for short in [false, true] {
        let mut command = fixture.command("status");
        command.arg("selected");
        if short {
            command.arg("--short");
        }
        command.assert().success();
    }
    entry.path = b"selected/invalid-\xff".to_vec();
    index.add(&entry).unwrap();
    index.write().unwrap();
    fixture
        .command("status")
        .args(["selected", "--short"])
        .assert()
        .failure()
        .stdout("");
}

#[cfg(unix)]
#[test]
fn source_aliases_preserve_git_changes_and_ignore_unrelated_non_utf8_paths() {
    let fixture = CheckoutFixture::new();
    let alias = fixture.temp.path().join("source-alias");
    std::os::unix::fs::symlink(&fixture.source, &alias).unwrap();
    let mut index = fixture.repo.index().unwrap();
    let mut entry = index.get_path(Path::new("selected/file"), 0).unwrap();
    entry.path = b"unrelated-\xff".to_vec();
    index.add(&entry).unwrap();
    index.write().unwrap();
    fs::write(fixture.root.join("file"), "target edit\n").unwrap();
    command(&alias, &fixture.root, "status")
        .args(["selected", "--short"])
        .env("XDG_STATE_HOME", fixture.temp.path().join("state"))
        .assert()
        .success()
        .stdout(fixture.row(" M", "file"));
}

#[cfg(unix)]
#[test]
fn native_source_changes_remain_selected_through_directory_aliases() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let alias = temp.path().join("source-alias");
    let root = temp.path().join("home");
    fs::create_dir_all(source.join("selected")).unwrap();
    fs::write(source.join("selected/over.yaml"), "target: '~'\n").unwrap();
    fs::write(source.join("selected/file"), "original\n").unwrap();
    let repo = git2::Repository::init(&source).unwrap();
    commit_all(&repo);
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    command(&alias, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    fs::write(source.join("selected/file"), "source edit\n").unwrap();
    command(&alias, &root, "status")
        .args(["selected", "--short"])
        .assert()
        .success()
        .stdout(format!(
            " M\t{}\t\"selected/file\"\tnull\n",
            serde_json::to_string(&worktree_identity(&repo)).unwrap()
        ));
}

#[test]
fn short_source_status_distinguishes_index_and_worktree_changes_without_unrelated_paths() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let root = temp.path().join("home");
    fs::create_dir_all(source.join("selected")).unwrap();
    fs::write(source.join("selected/over.yaml"), "target: '~'\n").unwrap();
    fs::write(source.join("selected/file"), "original\n").unwrap();
    fs::write(source.join("unrelated"), "original\n").unwrap();
    let repo = git2::Repository::init(&source).unwrap();
    commit_all(&repo);
    command(&source, &root, "apply")
        .args(["selected", "--no-prompt"])
        .assert()
        .success();
    fs::write(source.join("selected/file"), "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("selected/file")).unwrap();
    index.write().unwrap();
    fs::write(source.join("selected/file"), "edited again\n").unwrap();
    fs::write(source.join("unrelated"), "unrelated\n").unwrap();
    command(&source, &root, "status")
        .args(["selected", "-s"])
        .assert()
        .success()
        .stdout(format!(
            "MM\t{}\t\"selected/file\"\tnull\n",
            serde_json::to_string(&worktree_identity(&repo)).unwrap()
        ));
    // Structural drift still fails without putting prose on short stdout.
    fs::remove_file(root.join("file")).unwrap();
    command(&source, &root, "status")
        .args(["selected", "-s"])
        .assert()
        .code(1)
        .stdout(format!(
            "MM\t{}\t\"selected/file\"\tnull\n",
            serde_json::to_string(&worktree_identity(&repo)).unwrap()
        ));
}
