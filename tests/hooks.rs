//! Shell execution fixtures are Unix-only; schema tests also run on Windows.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use git2::{Repository, Signature};
use predicates::str::contains;
use rstest::rstest;
use serde_json::json;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    source: PathBuf,
    target: PathBuf,
    trace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("repository with spaces");
        let target = temp.path().join("target with spaces");
        let trace = temp.path().join("trace");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        Self {
            _temp: temp,
            source,
            target,
            trace,
        }
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.source.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn config(&self, path: &str, value: serde_json::Value) {
        // JSON is a YAML subset and avoids manual quoting of shell commands.
        self.write(path, &serde_json::to_string(&value).unwrap());
    }

    fn command(&self) -> Command {
        self.command_at(&self.source)
    }

    fn command_at(&self, home: &Path) -> Command {
        let mut command = Command::cargo_bin("over").unwrap();
        command
            .env_remove("OVER_HOME")
            .env_remove("OVER_HOOK_ACTIVE")
            .env("HOOK_TRACE", &self.trace)
            .env("TEST_TARGET", &self.target)
            .arg("--home")
            .arg(home);
        command
    }

    fn apply(&self, overlay: &str) -> Command {
        let mut command = self.command();
        command
            .arg("apply")
            .arg(overlay)
            .arg("--root")
            .arg(&self.target)
            .arg("--no-prompt");
        command
    }

    fn trace(&self) -> String {
        fs::read_to_string(&self.trace).unwrap_or_default()
    }

    fn commit(&self) -> Repository {
        let repository = Repository::init(&self.source).unwrap();
        let mut index = repository.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = Signature::now("Hook tests", "hooks@example.invalid").unwrap();
        repository
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        drop(tree);
        repository
    }
}

#[test]
fn hooks_run_root_first_then_once_per_overlay_in_dependency_order_after_all_files_exist() {
    let fixture = Fixture::new();
    fixture.config("over.yaml", json!({"target": "~", "hooks": {"post-apply": [
        "test -f \"$TEST_TARGET/selected\" && test -f \"$TEST_TARGET/shared\" && echo root >> \"$HOOK_TRACE\"",
        "echo root-second >> \"$HOOK_TRACE\""
    ]}}));
    for (name, uses) in [
        ("shared", vec![]),
        ("left", vec!["shared"]),
        ("right", vec!["shared"]),
        ("selected", vec!["left", "right", "shared", "left"]),
    ] {
        fixture.config(
            &format!("{name}/over.yaml"),
            json!({"uses": uses, "hooks": {"post-apply": [
                format!("echo {name} >> \"$HOOK_TRACE\"")
            ]}}),
        );
        fixture.write(&format!("{name}/{name}"), name);
    }
    fixture.apply("selected").assert().success();
    let expected = "root\nroot-second\nshared\nleft\nright\nselected\n";
    assert_eq!(fixture.trace(), expected);
    // Lifecycle events still run when reconciliation has no changes to make.
    fixture.apply("selected").assert().success();
    assert_eq!(fixture.trace(), expected.repeat(2));
}

#[test]
fn no_uses_and_metadata_only_dependencies_do_not_participate_in_hooks() {
    let fixture = Fixture::new();
    fixture.config(
        "over.yaml",
        json!({"target": "~", "default_overlay": "host", "hooks": {"post-apply": [
            "echo root >> \"$HOOK_TRACE\""
        ]}}),
    );
    for (name, uses) in [
        ("host", vec!["metadata"]),
        ("metadata", vec![]),
        ("selected", vec!["dependency"]),
        ("dependency", vec![]),
    ] {
        fixture.config(
            &format!("{name}/over.yaml"),
            json!({"uses": uses, "hooks": {"post-apply": [
                format!("echo {name} >> \"$HOOK_TRACE\"")
            ]}}),
        );
    }
    fixture
        .apply("selected")
        .arg("--no-uses")
        .assert()
        .success();
    assert_eq!(fixture.trace(), "root\nselected\n");
    fixture.apply("selected").assert().success();
    assert_eq!(
        fixture.trace(),
        "root\nselected\nroot\ndependency\nselected\n"
    );
}

#[test]
fn ancestor_hooks_are_not_inherited_and_root_overlay_hooks_are_not_duplicated() {
    let fixture = Fixture::new();
    fixture.config(
        "over.yaml",
        json!({"target": "~", "hooks": {"post-apply": ["echo root >> \"$HOOK_TRACE\""]}}),
    );
    fixture.config(
        "parent/over.yaml",
        json!({"hooks": {"post-apply": ["echo parent >> \"$HOOK_TRACE\""]}}),
    );
    fixture.config("parent/child/over.yaml", json!({}));
    fixture.apply("parent/child").assert().success();
    assert_eq!(fixture.trace(), "root\n");
    // Isolate the root-as-overlay case from child descriptors and content.
    fs::remove_dir_all(fixture.source.join("parent")).unwrap();
    fixture.apply("").assert().success();
    assert_eq!(fixture.trace(), "root\nroot\n");
}

#[rstest]
#[case(false)]
#[case(true)]
fn hook_context_has_absolute_paths_correct_cwd_and_inherited_environment(#[case] aliased: bool) {
    let mut fixture = Fixture::new();
    if aliased {
        // Reproduce macOS's /var -> /private/var alias without requiring a Mac.
        let alias = fixture._temp.path().join("alias");
        std::os::unix::fs::symlink(fixture._temp.path(), &alias).unwrap();
        fixture.source = alias.join(fixture.source.file_name().unwrap());
        fixture.target = alias.join(fixture.target.file_name().unwrap());
    }
    // A shell may canonicalize PWD even when Over receives a lexical absolute path.
    let root_hook = r#"test "$PWD" -ef "$OVER_REPOSITORY_ROOT" && test "$OVER_HOOK_EVENT" = post-apply && test "$OVER_HOOK_ACTIVE" = 1 && test "$OVER_OVERLAY_NAME" = selected && test "$OVER_TARGET_ROOT" = "$TEST_TARGET" && echo root >> "$HOOK_TRACE""#;
    let overlay_hook = r#"test "$PWD" -ef "$OVER_OVERLAY_ROOT" && test "$OVER_OVERLAY_ROOT" = "$OVER_REPOSITORY_ROOT/selected" && test "$OVER_OVERLAY_TARGET" = "$TEST_TARGET/config" && test "$INHERITED_VALUE" = available && echo overlay >> "$HOOK_TRACE""#;
    fixture.config("over.yaml", json!({"hooks": {"post-apply": [root_hook]}}));
    fixture.config(
        "selected/over.yaml",
        json!({"target": "~/config", "hooks": {"post-apply": [overlay_hook]}}),
    );
    fixture
        .apply("selected")
        .env("INHERITED_VALUE", "available")
        .assert()
        .success();
    assert_eq!(fixture.trace(), "root\noverlay\n");
}

#[test]
fn a_later_materialization_failure_suppresses_all_hooks() {
    let fixture = Fixture::new();
    fixture.config(
        "over.yaml",
        json!({"target": "~", "hooks": {"post-apply": ["echo root >> \"$HOOK_TRACE\""]}}),
    );
    fixture.config(
        "dependency/over.yaml",
        json!({"hooks": {"post-apply": ["echo dependency >> \"$HOOK_TRACE\""]}}),
    );
    fixture.write("dependency/first", "applied");
    fixture.config("selected/over.yaml", json!({"uses": ["dependency"]}));
    fixture.write("selected/conflict", "desired");
    fs::write(fixture.target.join("conflict"), "existing").unwrap();
    fixture.apply("selected").assert().failure();
    assert_eq!(
        fs::read_to_string(fixture.target.join("first")).unwrap(),
        "applied"
    );
    assert_eq!(fixture.trace(), "");
}

#[rstest]
#[case(true)]
#[case(false)]
fn hook_failure_preserves_exit_status_and_files_and_stops_later_hooks(#[case] root_fails: bool) {
    let fixture = Fixture::new();
    let failing = "secret_sentinel=never_log_this_hook_secret; exit 23";
    fixture.config(
        "over.yaml",
        json!({"target": "~", "hooks": {"post-apply": [if root_fails { failing } else { ":" }]}}),
    );
    fixture.config("dependency/over.yaml", json!({"hooks": {"post-apply": [if root_fails { "echo unexpected >> \"$HOOK_TRACE\"" } else { failing }]}}));
    fixture.config("selected/over.yaml", json!({"uses": ["dependency"], "hooks": {"post-apply": ["echo unexpected >> \"$HOOK_TRACE\""]}}));
    fixture.write("selected/file", "applied");
    let output = fixture
        .apply("selected")
        .arg("--debug")
        .arg("--verbose")
        .assert()
        .code(23)
        .stderr(contains("post-apply hook #1"))
        .get_output()
        .clone();
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("never_log_this_hook_secret"));
    }
    assert_eq!(fixture.trace(), "");
    assert_eq!(
        fs::read_to_string(fixture.target.join("file")).unwrap(),
        "applied"
    );
}

#[test]
fn dry_run_and_read_only_commands_never_execute_hooks() {
    let fixture = Fixture::new();
    fixture.config(
        "over.yaml",
        json!({"target": "~", "hooks": {"post-apply": ["echo root >> \"$HOOK_TRACE\""]}}),
    );
    fixture.config(
        "selected/over.yaml",
        json!({"hooks": {"post-apply": ["echo overlay >> \"$HOOK_TRACE\""]}}),
    );
    fixture
        .apply("selected")
        .arg("--dry-run")
        .assert()
        .success();
    for operation in ["status", "show", "diff"] {
        fixture
            .command()
            .arg(operation)
            .arg("selected")
            .arg("--root")
            .arg(&fixture.target)
            .assert()
            .success();
    }
    fixture.command().arg("lint").assert().success();
    assert_eq!(fixture.trace(), "");
}

#[test]
fn status_and_show_report_sections_without_exposing_or_executing_secret_values() {
    let fixture = Fixture::new();
    fixture.config(
        "over.yaml",
        json!({"target": "~", "hooks": {"post-apply": ["echo ROOT_SECRET >> \"$HOOK_TRACE\""]}}),
    );
    fixture.config("selected/over.yaml", json!({
        "data": {"token": "DATA_SECRET"},
        "hooks": {"post-apply": ["echo HOOK_SECRET >> \"$HOOK_TRACE\""]},
        "install": {"pre": ["echo INSTALL_SECRET >> \"$HOOK_TRACE\""], "apt": ["PACKAGE_SECRET"]},
        "git": {"plugin": {"url": "https://user:REMOTE_SECRET@example.invalid/plugin.git", "config": {"custom.token": "CONFIG_SECRET"}}}
    }));
    for verb in ["status", "show"] {
        let output = fixture
            .command()
            .args(["--debug", verb, "selected", "--verbose", "--root"])
            .arg(&fixture.target)
            .assert()
            .code(1)
            .stdout(contains("install: configured"))
            .stdout(contains("post-apply: 1"))
            .stdout(contains("git: plugin"))
            .get_output()
            .clone();
        for bytes in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(bytes);
            for secret in [
                "ROOT_SECRET",
                "DATA_SECRET",
                "HOOK_SECRET",
                "INSTALL_SECRET",
                "PACKAGE_SECRET",
                "REMOTE_SECRET",
                "CONFIG_SECRET",
            ] {
                assert!(!text.contains(secret), "leaked {secret}: {text}");
            }
        }
    }
    assert_eq!(fixture.trace(), "");
    fixture.config(
        "selected/over.yaml",
        json!({"install": {"apt": {"packages": 42, "pre": ["INVALID_SECRET"]}}}),
    );
    let output = fixture
        .command()
        .args(["--debug", "status", "selected", "--root"])
        .arg(&fixture.target)
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("INVALID_SECRET"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("INVALID_SECRET"));
}

#[test]
fn nested_over_operations_do_not_recursively_trigger_hooks() {
    let fixture = Fixture::new();
    fixture.config("over.yaml", json!({"target": "~", "hooks": {"post-apply": [
        "echo root >> \"$HOOK_TRACE\"; \"$OVER_TEST_BINARY\" --home \"$OVER_REPOSITORY_ROOT\" apply selected --root \"$OVER_TARGET_ROOT\" --no-prompt"
    ]}}));
    fixture.config(
        "selected/over.yaml",
        json!({"hooks": {"post-apply": ["echo overlay >> \"$HOOK_TRACE\""]}}),
    );
    fixture
        .apply("selected")
        .env("OVER_TEST_BINARY", assert_cmd::cargo::cargo_bin!("over"))
        .assert()
        .success();
    assert_eq!(fixture.trace(), "root\noverlay\n");
}

#[test]
fn hook_enabled_apply_requires_a_valid_root_before_materialization() {
    let fixture = Fixture::new();
    fixture.config(
        "selected/over.yaml",
        json!({"target": "~", "hooks": {"post-apply": ["echo unexpected >> \"$HOOK_TRACE\""]}}),
    );
    fixture.write("selected/file", "must not apply");
    fixture
        .apply("selected")
        .assert()
        .failure()
        .stderr(contains("exactly one root"));
    assert!(!fixture.target.join("file").exists());
    assert_eq!(fixture.trace(), "");
}

#[rstest]
#[case(
    "over.toml",
    "[hooks]\npost-apply = ['echo toml >> \"$HOOK_TRACE\"']\n"
)]
#[case(
    "over.yaml",
    "hooks:\n  post-apply: ['echo yaml >> \"$HOOK_TRACE\"']\n"
)]
#[case("over.yml", "hooks:\n  post-apply: ['echo yml >> \"$HOOK_TRACE\"']\n")]
fn root_and_overlay_hooks_work_in_every_descriptor_format(
    #[case] descriptor: &str,
    #[case] config: &str,
) {
    let fixture = Fixture::new();
    fixture.write(descriptor, config);
    fixture.write(&format!("selected/{descriptor}"), config);
    fixture.apply("selected").assert().success();
    assert_eq!(fixture.trace().lines().count(), 2);
}

#[test]
fn clone_then_init_hooks_see_canonical_origin_without_reading_encrypted_child_descriptors() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.config("over.yaml", json!({"repository": "git@example.invalid:user/dotfiles.git", "hooks": {
        "post-clone": ["test \"$(git remote get-url origin)\" = git@example.invalid:user/dotfiles.git && git-crypt unlock && echo clone >> \"$HOOK_TRACE\""],
        "post-init": ["test \"$PWD\" -ef \"$OVER_REPOSITORY_ROOT\" && test \"$OVER_HOOK_EVENT\" = post-init && test -z \"${OVER_OVERLAY_ROOT+x}\" && echo init >> \"$HOOK_TRACE\""]
    }}));
    fixture.write(
        "selected/over.yaml",
        "\0GITCRYPT unreadable encrypted descriptor",
    );
    fixture.write("selected/file", "unlocked");
    fixture.commit();
    let tools = fixture._temp.path().join("tools");
    fs::create_dir(&tools).unwrap();
    let unlock = tools.join("git-crypt");
    fs::write(&unlock, "#!/bin/sh\ntest \"$1\" = unlock || exit 9\nprintf 'target: \"~\"\\nhooks:\\n  post-clone: [exit 99]\\n  post-init: [exit 98]\\n' > selected/over.yaml\n").unwrap();
    fs::set_permissions(&unlock, fs::Permissions::from_mode(0o755)).unwrap();
    let destination = fixture._temp.path().join("clone");
    let path = std::env::join_paths(
        std::iter::once(tools).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let init = || {
        let mut command = Command::cargo_bin("over").unwrap();
        command
            .env_remove("OVER_HOOK_ACTIVE")
            .env("PATH", &path)
            .env("HOOK_TRACE", &fixture.trace)
            .env("OVER_OVERLAY_ROOT", "stale")
            .arg("--home")
            .arg(&destination)
            .arg("init");
        command
    };
    init().arg(&fixture.source).assert().success();
    assert_eq!(fixture.trace(), "clone\ninit\n");
    init().assert().success();
    assert_eq!(fixture.trace(), "clone\ninit\ninit\n");
    let mut apply = Command::cargo_bin("over").unwrap();
    apply
        .env_remove("OVER_HOOK_ACTIVE")
        .arg("--home")
        .arg(&destination)
        .arg("apply")
        .arg("selected")
        .arg("--root")
        .arg(&fixture.target)
        .arg("--no-prompt")
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(fixture.target.join("file")).unwrap(),
        "unlocked"
    );
}

#[rstest]
#[case("post-clone")]
#[case("post-init")]
fn failed_init_hooks_preserve_the_checkout_and_retries_only_run_post_init(
    #[case] failing_event: &str,
) {
    let fixture = Fixture::new();
    let failure = "echo failure >> \"$HOOK_TRACE\"; exit 23";
    fixture.config("over.yaml", json!({"hooks": {
        "post-clone": [if failing_event == "post-clone" { failure } else { "echo clone >> \"$HOOK_TRACE\"" }],
        "post-init": [if failing_event == "post-init" { failure } else { "echo init >> \"$HOOK_TRACE\"" }]
    }}));
    fixture.commit();
    let destination = fixture._temp.path().join("clone");
    fixture
        .command_at(&destination)
        .arg("init")
        .arg(&fixture.source)
        .assert()
        .code(23);
    assert!(destination.join(".git").is_dir());
    let first = if failing_event == "post-clone" {
        "failure\n"
    } else {
        "clone\nfailure\n"
    };
    assert_eq!(fixture.trace(), first);
    fs::write(
        destination.join("over.yaml"),
        serde_json::to_string(&json!({"hooks": {
            "post-clone": ["exit 99"], "post-init": ["echo retry >> \"$HOOK_TRACE\""]
        }}))
        .unwrap(),
    )
    .unwrap();
    fixture
        .command_at(&destination)
        .arg("init")
        .assert()
        .success();
    assert_eq!(fixture.trace(), format!("{first}retry\n"));
}

#[test]
fn invalid_repositories_and_unapproved_origin_changes_never_run_init_hooks() {
    let fixture = Fixture::new();
    fixture.config("over.yaml", json!({"repository": "git@example.invalid:canonical.git", "hooks": {
        "post-clone": ["echo clone >> \"$HOOK_TRACE\""], "post-init": ["echo init >> \"$HOOK_TRACE\""]
    }}));
    let repository = fixture.commit();
    repository
        .remote("origin", "https://example.invalid/old.git")
        .unwrap();
    fixture
        .command()
        .arg("init")
        .assert()
        .failure()
        .stderr(contains("--set-origin"));
    assert_eq!(fixture.trace(), "");
    fixture
        .command()
        .arg("init")
        .arg("--set-origin")
        .assert()
        .success();
    assert_eq!(fixture.trace(), "init\n");
    fixture.config("over.yml", json!({}));
    fixture
        .command()
        .arg("init")
        .assert()
        .failure()
        .stderr(contains("exactly one root"));
    assert_eq!(fixture.trace(), "init\n");
}

#[rstest]
#[case("hooks: {unknown-event: [never_log_this_hook_secret]}")]
#[case("hooks: {post-apply: never_log_this_hook_secret}")]
#[case("hooks: {post-apply: [{never_log_this_hook_secret: value}]}")]
#[case("hooks: {post-apply: [\"never_log_this_hook_secret}")]
fn invalid_hook_diagnostics_do_not_expose_command_values(#[case] descriptor: &str) {
    let fixture = Fixture::new();
    fixture.write("over.yaml", descriptor);
    fixture.config("selected/over.yaml", json!({}));
    let output = fixture
        .apply("selected")
        .arg("--debug")
        .assert()
        .failure()
        .get_output()
        .clone();
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("never_log_this_hook_secret"));
    }
}

#[test]
fn a_missing_shell_reports_the_hook_scope_without_exposing_commands() {
    let fixture = Fixture::new();
    fixture.config(
        "over.yaml",
        json!({"hooks": {"post-apply": ["never_log_this_hook_secret"]}}),
    );
    fixture.config("selected/over.yaml", json!({}));
    let output = fixture
        .apply("selected")
        .env("PATH", "")
        .assert()
        .code(1)
        .stderr(contains(
            "could not start post-apply hook #1 in repository root",
        ))
        .stderr(contains("ensure sh is available on PATH"))
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("never_log_this_hook_secret"));
}

#[test]
fn signal_termination_of_a_hook_returns_a_nonzero_fallback() {
    let fixture = Fixture::new();
    fixture.config(
        "over.yaml",
        json!({"hooks": {"post-apply": ["kill -TERM $$"]}}),
    );
    fixture.config("selected/over.yaml", json!({}));
    fixture
        .apply("selected")
        .assert()
        .code(1)
        .stderr(contains("post-apply hook #1"));
}
