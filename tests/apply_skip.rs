use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::str::contains;
use rstest::rstest;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(TempDir::new().unwrap());
        for path in ["repo", "target", "outside"] {
            fixture.0.child(path).create_dir_all().unwrap();
        }
        fixture
    }

    fn write(&self, path: &str, content: &str) {
        self.0
            .child(format!("repo/{path}"))
            .write_str(content)
            .unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        command
            .current_dir(self.0.child("outside"))
            .env_remove("OVER_HOME")
            .arg("--home")
            .arg(self.0.child("repo").path())
            .args(["apply", "--no-prompt", "--target"])
            .arg(self.0.child("target").path());
        command
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn repeatable_exclusions_filter_canonical_roots_before_loading_or_planning(#[case] dry_run: bool) {
    let fixture = Fixture::new();
    fixture.write("base/over.toml", "target = '~'");
    fixture.write("base/base-file", "base");
    // A skipped descriptor must not need to be parseable for explicit root selection.
    fixture.write("neovim/over.toml", "not valid toml!");
    fixture.write("workspace/private/over.toml", "not valid toml!");
    let mut command = fixture.command();
    command.args([
        "base",
        "./neovim/",
        "workspace/private",
        "--skip",
        "neovim",
        "--skip",
        "workspace/*",
        "--skip",
        "workspace/**",
    ]);
    if dry_run {
        command.arg("--dry-run");
    }
    let output = command
        .assert()
        .success()
        .stdout(contains("Skipped overlay 'neovim'"))
        .stdout(contains("Skipped overlay 'workspace/private'"))
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("matched no overlay"));
    if dry_run {
        assert!(String::from_utf8_lossy(&output.stdout).contains("base-file"));
    }
    assert_eq!(
        fixture.0.child("target/base-file").path().exists(),
        !dry_run
    );
}

#[test]
fn unmatched_patterns_warn_without_preventing_remaining_deployment() {
    let fixture = Fixture::new();
    fixture.write("base/over.toml", "target = '~'");
    fixture.write("base/file", "base");
    fixture
        .command()
        .args(["base", "--skip", "typo", "--skip", "absent/*"])
        .assert()
        .success()
        .stderr(contains(
            "--skip 'typo' matched no overlay in this apply selection",
        ))
        .stderr(contains(
            "--skip 'absent/*' matched no overlay in this apply selection",
        ));
    assert!(fixture.0.child("target/file").path().exists());
}

#[test]
fn invalid_patterns_fail_before_any_root_is_applied() {
    let fixture = Fixture::new();
    fixture.write("base/over.toml", "target = '~'");
    fixture.write("base/file", "base");
    fixture
        .command()
        .args(["base", "--skip", "["])
        .assert()
        .failure()
        .stderr(contains("invalid --skip pattern '['"));
    assert!(!fixture.0.child("target/file").path().exists());
}

#[test]
fn a_shared_skipped_dependency_is_not_reincluded_even_when_named_as_a_root() {
    let fixture = Fixture::new();
    for name in ["one", "two"] {
        fixture.write(&format!("{name}/over.toml"), "uses = ['private']");
        fixture.write(&format!("{name}/{name}"), "must not deploy");
    }
    fixture.write("private/over.toml", "target = '~/private'");
    fixture
        .command()
        .args([
            "one",
            "private",
            "two",
            "--skip",
            "private",
            "--force",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(contains("one -> private"))
        .stdout(contains("Skipped overlay 'private'"));
    assert!(!fixture.0.child("target/one").path().exists());
    assert!(!fixture.0.child("target/two").path().exists());
}

#[rstest]
#[case(false)]
#[case(true)]
fn transitive_conflicts_fail_before_any_roots_content_or_files_are_prepared(#[case] reverse: bool) {
    let fixture = Fixture::new();
    fixture.write(
        "base/over.toml",
        "target = '~'\nrules = [{ path = '**', template = true }]",
    );
    fixture.write("base/file", "{{ undefined_function() }}");
    fixture.write("app/over.toml", "uses = ['tools']");
    fixture.write("tools/over.toml", "uses = ['./workspace/private/']");
    fixture.write("workspace/private/over.toml", "target = '~/private'");
    let roots = if reverse {
        ["app", "base"]
    } else {
        ["base", "app"]
    };
    fixture
        .command()
        .args(roots)
        .args(["--skip", "workspace/*"])
        .assert()
        .failure()
        .stderr(contains("requires skipped overlay 'workspace/private'"))
        .stderr(contains("app -> tools -> workspace/private"))
        .stderr(contains("--no-uses"));
    assert!(!fixture.0.child("target/file").path().exists());
}

#[test]
fn no_uses_keeps_skipped_dependency_metadata_without_preparing_its_content_or_git() {
    let fixture = Fixture::new();
    fixture.write("private/over.toml", "target = '~/private'\nrules = [{ path = '**', template = true }]\n[data]\nvalue = 'available'\n[git]\nrepo = '/nonexistent/skip-must-not-clone'\n");
    fixture.write("private/file", "{{ undefined_function() }}");
    fixture.write(
        "base/over.toml",
        "target = '~'\nuses = ['private']\nrules = [{ path = '**', template = true }]",
    );
    fixture.write(
        "base/file",
        "{{ overlays.private.data.value }}|{{ overlays.private.target }}",
    );
    fixture
        .command()
        .args(["base", "private", "--no-uses", "--skip", "private"])
        .assert()
        .success()
        .stdout(contains("Skipped overlay 'private'"));
    let contents = std::fs::read_to_string(fixture.0.child("target/file")).unwrap();
    assert!(contents.starts_with("available|"), "{contents}");
    assert!(!fixture.0.child("target/private").path().exists());
}

#[test]
fn target_inference_excludes_private_content_before_preparing_candidate_trees() {
    let fixture = Fixture::new();
    fixture.write("base/over.toml", "target = '~'");
    fixture.write("base/file", "base");
    fixture.write(
        "private/over.toml",
        "target = '~'\nrules = [{ path = '**', template = true }]",
    );
    fixture.write("private/broken", "{{ undefined_function() }}");
    fixture
        .command()
        .current_dir(fixture.0.child("target"))
        .args(["--skip", "private"])
        .assert()
        .success()
        .stdout(contains("Skipped overlay 'private'"));
    assert!(fixture.0.child("target/file").path().exists());
    assert!(!fixture.0.child("target/broken").path().exists());
}

#[test]
fn target_inference_keeps_a_relevant_dependency_conflict_instead_of_hiding_it() {
    let fixture = Fixture::new();
    fixture.write(
        "base/over.toml",
        "target = '~'\nuses = ['private']\nrules = [{ path = '**', template = true }]",
    );
    fixture.write("base/file", "{{ undefined_function() }}");
    fixture.write("private/over.toml", "target = '~/private'");
    fixture
        .command()
        .current_dir(fixture.0.child("target"))
        .args(["--skip", "private"])
        .assert()
        .failure()
        .stderr(contains("requires skipped overlay 'private'"));
    assert!(!fixture.0.child("target/file").path().exists());
}

#[test]
fn a_blocked_candidate_cannot_silently_lose_sibling_evidence_to_another_root() {
    let fixture = Fixture::new();
    fixture.write("one/over.toml", "target = '~'\nuses = ['private']");
    fixture.write("one/first", "must not deploy");
    fixture.write("two/over.toml", "target = '~'");
    fixture.write("two/second", "must not deploy");
    fixture.write("private/over.toml", "target = '~/private'");
    fixture
        .command()
        .current_dir(fixture.0.child("target"))
        .args(["--skip", "private"])
        .assert()
        .failure()
        .stderr(contains("cannot safely infer an overlay"))
        .stderr(contains("requires skipped overlay 'private'"))
        .stderr(contains("specify NAME explicitly"));
    assert!(!fixture.0.child("target/first").path().exists());
    assert!(!fixture.0.child("target/second").path().exists());
}

#[test]
fn skipping_the_configured_host_preserves_its_metadata_for_other_roots() {
    let fixture = Fixture::new();
    fixture.write("over.toml", "host_overlay = 'host'");
    fixture.write(
        "host/over.toml",
        "target = '~/host'\n[host]\nvalue = 'host metadata'",
    );
    fixture.write("host/file", "must not deploy");
    fixture.write(
        "base/over.toml",
        "target = '~'\nrules = [{ path = '**', template = true }]",
    );
    fixture.write("base/file", "{{ host.value }}");
    fixture
        .command()
        .args(["host", "base", "--skip", "host"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(fixture.0.child("target/file")).unwrap(),
        "host metadata"
    );
    assert!(!fixture.0.child("target/host").path().exists());
}

#[rstest]
#[case("source")]
#[case("git")]
#[case("host")]
#[case("explicit")]
#[case("target")]
fn excluded_authoritative_selection_is_a_noop_without_host_fallback(#[case] context: &str) {
    let fixture = Fixture::new();
    fixture.write(
        "over.toml",
        "host_overlay = 'host'\n[apply]\ndefault_to_host = true",
    );
    fixture.write("host/over.toml", "target = '~/host'");
    fixture.write("host/file", "host");
    fixture.write("selected/over.toml", "target = '~/selected'");
    fixture.write("selected/file", "selected");
    let mut command = fixture.command();
    match context {
        "source" => {
            command.current_dir(fixture.0.child("repo/selected"));
        }
        "git" => {
            let repository = git2::Repository::init(fixture.0.child("outside")).unwrap();
            repository
                .config()
                .unwrap()
                .set_str("over.overlay", "selected")
                .unwrap();
        }
        "host" => {}
        "target" => {
            fixture.0.child("target/selected").create_dir_all().unwrap();
            command.current_dir(fixture.0.child("target/selected"));
        }
        "explicit" => {
            command.arg("selected");
        }
        _ => unreachable!(),
    }
    let skipped = if context == "host" {
        "host"
    } else {
        "selected"
    };
    command
        .args(["--skip", skipped])
        .assert()
        .success()
        .stdout(contains("No overlays to apply after exclusions"));
    assert!(!fixture.0.child("target/host").path().exists());
    assert!(!fixture.0.child("target/selected/file").path().exists());
}
