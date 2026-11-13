use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use git2::{Repository, Signature};
use predicates::str::contains;
use rstest::rstest;
use tempfile::TempDir;

use over::actions::git::init::initialize;

const CANONICAL: &str = "git@example.invalid:user/dotfiles.git";

fn source(root: &Path, descriptor: &str, content: &str) -> Repository {
    fs::create_dir_all(root).unwrap();
    let repo = Repository::init(root).unwrap();
    fs::write(root.join(descriptor), content).unwrap();
    fs::write(root.join("dotfile"), "original\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(descriptor)).unwrap();
    index.add_path(Path::new("dotfile")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Over tests", "over@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    repo
}

fn command(home: &Path) -> Command {
    let mut command = Command::cargo_bin("over").unwrap();
    command
        .env_remove("OVER_HOME")
        .arg("--home")
        .arg(home)
        .arg("init");
    command
}

fn origin(root: &Path) -> String {
    Repository::open(root)
        .unwrap()
        .find_remote("origin")
        .unwrap()
        .url()
        .unwrap()
        .to_owned()
}

#[rstest]
#[case("over.toml", format!("repository = '{CANONICAL}'\n"))]
#[case("over.yaml", format!("repository: {CANONICAL}\n"))]
#[case("over.yml", format!("repository: {CANONICAL}\n"))]
fn local_bootstrap_sets_an_unreachable_canonical_origin_and_can_be_repeated(
    #[case] descriptor: &str,
    #[case] config: String,
) {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("source");
    source(&src, descriptor, &config);
    let dest = tmp.path().join("nested/dotfiles");
    command(&dest).arg(&src).assert().success();
    assert_eq!(origin(&dest), CANONICAL);
    // Git may convert LF to CRLF on checkout; retries must preserve those exact bytes.
    let checked_out = fs::read_to_string(dest.join("dotfile")).unwrap();
    assert_eq!(checked_out.lines().collect::<Vec<_>>(), ["original"]);
    command(&dest).arg(&src).assert().success();
    command(&dest).assert().success();
    assert_eq!(origin(&dest), CANONICAL);
    assert_eq!(
        fs::read_to_string(dest.join("dotfile")).unwrap(),
        checked_out
    );
}

#[test]
fn cloning_without_a_canonical_remote_preserves_the_source() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("source");
    source(&src, "over.toml", "target = '~'\n");
    let dest = tmp.path().join("dotfiles");
    fs::create_dir(&dest).unwrap();
    command(&dest).arg(&src).assert().success();
    assert_eq!(
        PathBuf::from(origin(&dest)).canonicalize().unwrap(),
        src.canonicalize().unwrap()
    );
    command(&dest).assert().success();
}

#[test]
fn an_existing_https_origin_requires_approval_and_preserves_other_configuration() {
    let tmp = TempDir::new().unwrap();
    let repo = source(
        tmp.path(),
        "over.toml",
        &format!("repository = '{CANONICAL}'"),
    );
    let https = "https://example.invalid/user/dotfiles.git";
    repo.remote("origin", https).unwrap();
    repo.remote("upstream", "https://example.invalid/upstream.git")
        .unwrap();
    repo.remote_set_pushurl("origin", Some("git@example.invalid:push.git"))
        .unwrap();
    repo.remote_add_fetch("origin", "+refs/notes/*:refs/notes/*")
        .unwrap();
    let refspecs = repo
        .find_remote("origin")
        .unwrap()
        .fetch_refspecs()
        .unwrap();
    fs::write(tmp.path().join("dotfile"), "dirty\n").unwrap();

    command(tmp.path())
        .assert()
        .failure()
        .stderr(contains("--set-origin"));
    assert_eq!(origin(tmp.path()), https);
    command(tmp.path()).arg("--set-origin").assert().success();
    assert_eq!(origin(tmp.path()), CANONICAL);
    assert_eq!(
        fs::read_to_string(tmp.path().join("dotfile")).unwrap(),
        "dirty\n"
    );
    let remote = repo.find_remote("origin").unwrap();
    assert_eq!(
        remote.pushurl().unwrap(),
        Some("git@example.invalid:push.git")
    );
    assert_eq!(
        remote.fetch_refspecs().unwrap().iter().collect::<Vec<_>>(),
        refspecs.iter().collect::<Vec<_>>()
    );
    assert_eq!(
        repo.find_remote("upstream").unwrap().url().unwrap(),
        "https://example.invalid/upstream.git"
    );
}

#[test]
fn a_missing_origin_is_created_only_with_approval() {
    let tmp = TempDir::new().unwrap();
    source(tmp.path(), "over.yaml", &format!("repository: {CANONICAL}"));
    command(tmp.path())
        .assert()
        .failure()
        .stderr(contains("--set-origin"));
    command(tmp.path()).arg("--set-origin").assert().success();
    assert_eq!(origin(tmp.path()), CANONICAL);
}

#[test]
fn origin_changes_during_confirmation_are_not_overwritten() {
    let tmp = TempDir::new().unwrap();
    let repo = source(tmp.path(), "over.yaml", &format!("repository: {CANONICAL}"));
    repo.remote("origin", "https://example.invalid/old.git")
        .unwrap();
    let mut pending = initialize(tmp.path().to_path_buf(), None).unwrap();
    repo.remote_set_url("origin", "https://example.invalid/new.git")
        .unwrap();
    assert!(
        pending
            .set_origin()
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    assert_eq!(origin(tmp.path()), "https://example.invalid/new.git");
}

#[test]
fn a_remote_write_failure_preserves_the_checkout_and_can_be_retried() {
    let tmp = TempDir::new().unwrap();
    let repo = source(tmp.path(), "over.yaml", &format!("repository: {CANONICAL}"));
    repo.remote("origin", "https://example.invalid/bootstrap.git")
        .unwrap();
    fs::write(repo.path().join("config.lock"), "locked").unwrap();
    command(tmp.path())
        .arg("--set-origin")
        .assert()
        .failure()
        .stderr(contains("failed to set origin"));
    assert_eq!(origin(tmp.path()), "https://example.invalid/bootstrap.git");
    assert!(tmp.path().join("dotfile").exists());
    fs::remove_file(repo.path().join("config.lock")).unwrap();
    command(tmp.path()).arg("--set-origin").assert().success();
}

#[rstest]
#[case("repository: 42")]
#[case("repository: true")]
#[case("repository: null")]
#[case("repository: []")]
#[case("repository: ''")]
#[case("repository: 'https://'")]
#[case("repository: 'https:///missing-host'")]
#[case("repository: 'user@host:'")]
#[case("repository: '{{ secret }}'")]
#[case("repository: \"bad\\nremote\"")]
#[case("repository: [")]
fn invalid_root_configuration_never_rewrites_origin(#[case] config: &str) {
    let tmp = TempDir::new().unwrap();
    let repo = source(tmp.path(), "over.yaml", config);
    repo.remote("origin", "https://example.invalid/keep.git")
        .unwrap();
    command(tmp.path()).arg("--set-origin").assert().failure();
    assert_eq!(origin(tmp.path()), "https://example.invalid/keep.git");
}

#[test]
fn root_declared_overlays_do_not_need_local_descriptors_and_nested_remotes_are_ignored() {
    let tmp = TempDir::new().unwrap();
    let repo = source(tmp.path(), "over.yaml", "overlays:\n  - 'apps/*'\n");
    fs::create_dir_all(tmp.path().join("apps/plain")).unwrap();
    fs::create_dir_all(tmp.path().join("apps/nested")).unwrap();
    fs::write(
        tmp.path().join("apps/nested/over.yaml"),
        format!("repository: {CANONICAL}\n"),
    )
    .unwrap();
    repo.remote("origin", "https://example.invalid/keep.git")
        .unwrap();
    command(tmp.path()).assert().success();
    assert_eq!(origin(tmp.path()), "https://example.invalid/keep.git");
}

#[test]
fn lint_checks_the_canonical_remote_even_when_only_children_are_discovered() {
    let tmp = TempDir::new().unwrap();
    source(tmp.path(), "over.yaml", "repository: false\n");
    fs::create_dir(tmp.path().join("child")).unwrap();
    fs::write(tmp.path().join("child/over.yaml"), "target: '~'\n").unwrap();
    let result =
        over::lint::lint_repository(&over::overlays::Repository::new(tmp.path().to_path_buf()));
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("repository"))
    );
    assert!(result.has_errors());
}

#[test]
fn unrelated_destinations_and_enclosing_repositories_are_preserved() {
    let tmp = TempDir::new().unwrap();
    let repo = source(tmp.path(), "over.toml", "target = '~'");
    repo.remote("origin", "https://example.invalid/keep.git")
        .unwrap();
    let dest = tmp.path().join("unrelated");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("keep"), "precious").unwrap();
    command(&dest)
        .arg(tmp.path())
        .arg("--set-origin")
        .assert()
        .failure();
    assert_eq!(fs::read_to_string(dest.join("keep")).unwrap(), "precious");
    assert_eq!(origin(tmp.path()), "https://example.invalid/keep.git");
    let file = tmp.path().join("file");
    fs::write(&file, "precious").unwrap();
    command(&file).arg(tmp.path()).assert().failure();
    assert_eq!(fs::read_to_string(file).unwrap(), "precious");
}

#[cfg(unix)]
#[test]
fn symlink_destinations_are_never_followed() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("source");
    source(&src, "over.toml", "target = '~'");
    let dest = tmp.path().join("link");
    std::os::unix::fs::symlink(&src, &dest).unwrap();
    command(&dest)
        .arg(&src)
        .assert()
        .failure()
        .stderr(contains("ordinary directory"));
    assert!(
        fs::symlink_metadata(&dest)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn bare_and_corrupt_checkouts_are_refused() {
    let tmp = TempDir::new().unwrap();
    let bare = tmp.path().join("bare");
    Repository::init_bare(&bare).unwrap();
    command(&bare).assert().failure();
    let corrupt = tmp.path().join("corrupt");
    fs::create_dir(&corrupt).unwrap();
    fs::write(corrupt.join(".git"), "invalid").unwrap();
    command(&corrupt).assert().failure();
    assert_eq!(fs::read_to_string(corrupt.join(".git")).unwrap(), "invalid");
}

#[test]
fn a_failed_clone_can_be_retried_without_destroying_data() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("missing-source");
    let dest = tmp.path().join("destination");
    command(&dest)
        .arg(&src)
        .assert()
        .failure()
        .stderr(contains("failed to clone"));
    source(&src, "over.toml", "target = '~'");
    // libgit2 normally cleans failed clones; if it leaves data, init must refuse it.
    if dest.exists() && fs::read_dir(&dest).unwrap().next().is_some() {
        command(&dest).arg(&src).assert().failure();
    } else {
        command(&dest).arg(&src).assert().success();
    }
}

#[test]
fn a_bad_cloned_descriptor_can_be_repaired_then_retried() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("source");
    source(&src, "over.yaml", "repository: false");
    let dest = tmp.path().join("destination");
    command(&dest)
        .arg(&src)
        .assert()
        .failure()
        .stderr(contains("invalid Over repository"));
    assert!(dest.join("dotfile").exists());
    fs::write(dest.join("over.yaml"), format!("repository: {CANONICAL}")).unwrap();
    command(&dest).arg("--set-origin").assert().success();
    assert_eq!(origin(&dest), CANONICAL);
}

#[test]
fn missing_or_ambiguous_root_descriptors_do_not_change_remotes() {
    let tmp = TempDir::new().unwrap();
    let repo = source(tmp.path(), "over.toml", "target = '~'");
    repo.remote("origin", "https://example.invalid/keep.git")
        .unwrap();
    fs::write(tmp.path().join("over.yaml"), "target: '~'").unwrap();
    command(tmp.path())
        .assert()
        .failure()
        .stderr(contains("exactly one root"));
    fs::remove_file(tmp.path().join("over.toml")).unwrap();
    fs::remove_file(tmp.path().join("over.yaml")).unwrap();
    command(tmp.path())
        .assert()
        .failure()
        .stderr(contains("--mode checkout"));
    assert_eq!(origin(tmp.path()), "https://example.invalid/keep.git");
}

#[rstest]
#[case("checkout")]
#[case("symlink")]
fn explicit_setup_persists_the_repository_default_without_touching_source_edits(
    #[case] mode: &str,
) {
    let tmp = TempDir::new().unwrap();
    let repo = source(tmp.path(), "over.toml", "");
    fs::remove_file(tmp.path().join("over.toml")).unwrap();
    fs::write(tmp.path().join("dotfile"), "dirty\n").unwrap();
    let index = fs::read(repo.path().join("index")).unwrap();
    command(tmp.path())
        .args(["--mode", mode])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("over.toml")).unwrap(),
        format!("mode = \"{mode}\"\n")
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("dotfile")).unwrap(),
        "dirty\n"
    );
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
    let repository = over::overlays::Repository::new(tmp.path().to_path_buf());
    let overlay = over::overlays::Overlay::new(&repository, tmp.path()).unwrap();
    let expected = if mode == "checkout" {
        over::overlays::MaterializationKind::Checkout
    } else {
        over::overlays::MaterializationKind::Symlink
    };
    assert_eq!(overlay.materialization_for(Path::new("dotfile")), expected);
}

#[rstest]
#[case("over.toml", "# legacy default\ntarget = '~'\n")]
#[case("over.yaml", "mode: symlink\n")]
#[case("over.yml", "mode: checkout\n")]
fn setup_flags_preserve_existing_configuration_and_report_its_precedence(
    #[case] descriptor: &str,
    #[case] config: &str,
) {
    let tmp = TempDir::new().unwrap();
    source(tmp.path(), descriptor, config);
    command(tmp.path())
        .args(["--mode", "checkout"])
        .assert()
        .success()
        .stdout(contains("--mode only creates missing configuration"));
    command(tmp.path()).arg("--no-config").assert().success();
    assert_eq!(
        fs::read_to_string(tmp.path().join(descriptor)).unwrap(),
        config
    );
    if descriptor != "over.toml" {
        assert!(!tmp.path().join("over.toml").exists());
    }
}

#[test]
fn explicit_skip_allows_descriptorless_initialization_without_writing_configuration() {
    let tmp = TempDir::new().unwrap();
    source(tmp.path(), "over.yaml", "");
    fs::remove_file(tmp.path().join("over.yaml")).unwrap();
    command(tmp.path())
        .arg("--no-config")
        .assert()
        .success()
        .stdout(contains("Implicit materialization remains symlink"));
    assert!(!tmp.path().join("over.toml").exists());
    command(tmp.path())
        .assert()
        .failure()
        .stderr(contains("--no-config"));
    assert!(!tmp.path().join("over.toml").exists());
}

#[rstest]
#[case("checkout")]
#[case("symlink")]
fn missing_configuration_can_be_created_after_a_fresh_clone(#[case] mode: &str) {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("source");
    // This ordinary tracked file deliberately is not a supported Over descriptor.
    source(&src, "readme.txt", "fixture");
    let dest = tmp.path().join("destination");
    command(&dest)
        .arg(&src)
        .args(["--mode", mode])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(dest.join("over.toml")).unwrap(),
        format!("mode = \"{mode}\"\n")
    );
    assert_eq!(
        PathBuf::from(origin(&dest)).canonicalize().unwrap(),
        src.canonicalize().unwrap()
    );
}

#[test]
fn configuration_created_while_setup_is_pending_is_never_overwritten() {
    let tmp = TempDir::new().unwrap();
    source(tmp.path(), "readme.txt", "fixture");
    let pending = over::actions::git::init::prepare(tmp.path().to_path_buf(), None).unwrap();
    fs::write(tmp.path().join("over.yml"), "mode: symlink\n").unwrap();
    assert!(
        pending
            .create_configuration(over::overlays::MaterializationKind::Checkout)
            .is_err()
    );
    assert!(!tmp.path().join("over.toml").exists());
    assert_eq!(
        fs::read_to_string(tmp.path().join("over.yml")).unwrap(),
        "mode: symlink\n"
    );
}

#[test]
fn invalid_setup_options_are_rejected() {
    let tmp = TempDir::new().unwrap();
    command(tmp.path())
        .args(["--mode", "checkout", "--no-config"])
        .assert()
        .failure();
    command(tmp.path())
        .args(["--mode", "symlink-directory"])
        .assert()
        .failure();
}

#[test]
fn generated_defaults_preserve_nearer_overlay_policy() {
    let tmp = TempDir::new().unwrap();
    source(tmp.path(), "readme.txt", "fixture");
    fs::create_dir(tmp.path().join("child")).unwrap();
    fs::write(
        tmp.path().join("child/over.toml"),
        "mode = 'symlink'\n[[rules]]\npath = 'directory'\nmode = 'symlink-directory'\n",
    )
    .unwrap();
    command(tmp.path())
        .args(["--mode", "checkout"])
        .assert()
        .success();
    let repository = over::overlays::Repository::new(tmp.path().to_path_buf());
    let child = over::overlays::Overlay::new(&repository, &tmp.path().join("child")).unwrap();
    assert_eq!(
        child.materialization_for(Path::new("file")),
        over::overlays::MaterializationKind::Symlink
    );
    assert_eq!(
        child.materialization_for(Path::new("directory")),
        over::overlays::MaterializationKind::SymlinkDirectory
    );
}

#[cfg(unix)]
#[test]
fn selecting_checkout_preserves_deployed_links_and_external_state() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("source");
    source(&src, "readme.txt", "fixture");
    let target = tmp.path().join("deployed");
    fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(src.join("dotfile"), target.join("dotfile")).unwrap();
    let state = tmp.path().join("state/over");
    fs::create_dir_all(&state).unwrap();
    // Init must not load, repair, or reinterpret deployment registrations at all.
    let registration = state.join("virtual_checkout.toml");
    fs::write(&registration, "deployment-state sentinel\n").unwrap();
    command(&src)
        .env("XDG_STATE_HOME", tmp.path().join("state"))
        .args(["--mode", "checkout"])
        .assert()
        .success();
    assert_eq!(
        fs::read_link(target.join("dotfile")).unwrap(),
        src.join("dotfile")
    );
    assert_eq!(
        fs::read_to_string(registration).unwrap(),
        "deployment-state sentinel\n"
    );
    assert_eq!(fs::read_dir(target).unwrap().count(), 1);
}

#[cfg(target_os = "linux")]
#[rstest]
#[case("\r", Some("checkout"))]
#[case("j\r", Some("symlink"))]
#[case("decline", None)]
#[case("cancel", None)]
fn interactive_setup_persists_only_an_accepted_mode(
    #[case] answer: &str,
    #[case] expected: Option<&str>,
) {
    let tmp = TempDir::new().unwrap();
    source(tmp.path(), "readme.txt", "fixture");
    // A controlling terminal exercises Dialoguer's actual default and key handling;
    // piped stdin cannot cover either because it deliberately disables setup prompts.
    let steps = if answer == "decline" {
        vec![("Create over.toml?", "n")]
    } else {
        vec![
            ("Create over.toml?", "y"),
            ("Symlink", if answer == "cancel" { "\u{3}" } else { answer }),
        ]
    };
    let driver = r#"
import errno, fcntl, json, os, pty, select, signal, struct, sys, time
steps = json.loads(sys.argv[1])
pid, fd = pty.fork()
if pid == 0:
    os.execv(sys.argv[2], sys.argv[2:])
fcntl.ioctl(fd, 0x5414, struct.pack('HHHH', 50, 120, 0, 0))
deadline = time.monotonic() + 20
output = b''
pending = b''
sent = 0
while time.monotonic() < deadline:
    if select.select([fd], [], [], 0.1)[0]:
        try:
            block = os.read(fd, 65536)
        except OSError as error:
            if error.errno == errno.EIO:
                break
            raise
        if not block:
            break
        output += block
        pending += block
        if sent < len(steps) and steps[sent][0].encode() in pending:
            if steps[sent][1] == '\x03':
                os.killpg(pid, signal.SIGINT)
            else:
                os.write(fd, steps[sent][1].encode())
            sent += 1
            pending = b''
else:
    os.killpg(pid, signal.SIGKILL)
    sys.exit(124)
_, status = os.waitpid(pid, 0)
sys.stdout.buffer.write(output)
if sent != len(steps):
    sys.exit(125)
sys.exit(os.waitstatus_to_exitcode(status))
"#;
    let output = std::process::Command::new("python3")
        .args(["-c", driver])
        .arg(serde_json::to_string(&steps).unwrap())
        .arg(assert_cmd::cargo::cargo_bin!("over"))
        .arg("--home")
        .arg(tmp.path())
        .arg("init")
        .env_remove("OVER_HOME")
        .env("NO_COLOR", "1")
        .env("TERM", "xterm")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert_ne!(
        output.status.code(),
        Some(124),
        "terminal timed out: {text}"
    );
    assert_ne!(output.status.code(), Some(125), "missing prompt: {text}");
    assert_eq!(output.status.success(), answer != "cancel", "{text}");
    if let Some(mode) = expected {
        assert!(text.contains("Virtual checkout (recommended)"), "{text}");
        assert_eq!(
            fs::read_to_string(tmp.path().join("over.toml")).unwrap(),
            format!("mode = \"{mode}\"\n")
        );
    } else {
        assert!(!tmp.path().join("over.toml").exists());
    }
}

#[cfg(unix)]
#[test]
fn dangling_descriptor_links_are_not_replaced_by_setup() {
    let tmp = TempDir::new().unwrap();
    source(tmp.path(), "readme.txt", "fixture");
    let descriptor = tmp.path().join("over.yaml");
    std::os::unix::fs::symlink("missing", &descriptor).unwrap();
    command(tmp.path())
        .args(["--mode", "checkout"])
        .assert()
        .failure();
    assert!(
        fs::symlink_metadata(descriptor)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!tmp.path().join("over.toml").exists());
}

#[test]
fn the_environment_selects_the_checkout_and_the_flag_takes_precedence() {
    let tmp = TempDir::new().unwrap();
    let valid = tmp.path().join("valid");
    source(&valid, "over.toml", "target = '~'");
    Command::cargo_bin("over")
        .unwrap()
        .env("OVER_HOME", &valid)
        .arg("init")
        .assert()
        .success();
    command(&valid)
        .env("OVER_HOME", tmp.path().join("missing"))
        .assert()
        .success();
    command(&tmp.path().join("missing"))
        .assert()
        .failure()
        .stderr(contains("supply a Git source"));
}

#[test]
fn initialization_neither_renders_templates_nor_applies_or_installs() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("target");
    let sentinel = tmp.path().join("installed");
    let src = tmp.path().join("source");
    let config = format!(
        "target: '{}'\ndefault_overlay: '{{{{ missing.variable }}}}'\nhooks:\n  install: ['touch \"{}\"']\n",
        target.display(),
        sentinel.display()
    );
    source(&src, "over.yaml", &config);
    let dest = tmp.path().join("destination");
    command(&dest).arg(&src).assert().success();
    assert!(!target.exists());
    assert!(!sentinel.exists());
}
