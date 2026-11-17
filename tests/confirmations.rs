//! Removal consent must precede every mutation, including implicit commits.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use rstest::rstest;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::create_dir_all(source.join("ov")).unwrap();
        fs::create_dir(&target).unwrap();
        let repo = git2::Repository::init(&source).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Confirmation test").unwrap();
        config
            .set_str("user.email", "confirmation@example.invalid")
            .unwrap();
        config.set_bool("commit.gpgSign", false).unwrap();
        config.set_i32("gc.auto", 0).unwrap();
        fs::write(source.join("ov/over.toml"), "mode = 'checkout'\n").unwrap();
        fs::write(source.join("ov/file"), "original\n").unwrap();
        let fixture = Self {
            temp,
            source,
            target,
        };
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "initial"]);
        fixture
    }

    fn environment(&self, command: &mut Command) {
        command
            .current_dir(&self.source)
            .env_remove("OVER_HOME")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("XDG_CACHE_HOME", self.temp.path().join("cache"));
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        self.environment(&mut command);
        command
            .arg("--home")
            .arg(&self.source)
            .args(args)
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        success(self.command(args).output().unwrap())
    }

    fn git(&self, args: &[&str]) {
        let mut command = Command::new("git");
        self.environment(&mut command);
        success(command.args(args).output().unwrap());
    }

    fn enroll(&self) {
        for user in ["alice", "bob"] {
            let recipient = age::x25519::Identity::generate().to_public().to_string();
            self.run(&["user", "add", user, "--recipient", &recipient]);
        }
    }

    fn apply(&self) {
        self.run(&[
            "apply",
            "ov",
            "--target",
            self.target.to_str().unwrap(),
            "--no-prompt",
        ]);
    }

    fn snapshot(&self) -> BTreeMap<PathBuf, (bool, Vec<u8>)> {
        walkdir::WalkDir::new(self.temp.path())
            .into_iter()
            // Python may be a mise shim; its own usage metadata is unrelated to Over's state.
            .filter_entry(|entry| {
                !entry
                    .path()
                    .starts_with(self.temp.path().join("state/mise"))
            })
            .map(Result::unwrap)
            .map(|entry| {
                let symlink = entry.file_type().is_symlink();
                let bytes = if symlink {
                    fs::read_link(entry.path())
                        .unwrap()
                        .into_os_string()
                        .into_encoded_bytes()
                } else if entry.file_type().is_file() {
                    fs::read(entry.path()).unwrap()
                } else {
                    Vec::new()
                };
                (
                    entry
                        .path()
                        .strip_prefix(self.temp.path())
                        .unwrap()
                        .to_path_buf(),
                    (symlink, bytes),
                )
            })
            .collect()
    }

    fn assert_unchanged(&self, before: &BTreeMap<PathBuf, (bool, Vec<u8>)>) {
        let after = self.snapshot();
        for path in before.keys().chain(after.keys()) {
            assert!(
                before.get(path) == after.get(path),
                "state changed at {}",
                path.display()
            );
        }
    }
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[rstest]
#[case(&["user", "revoke", "alice"])]
#[case(&["user", "delete", "alice"])]
#[case(&["user", "key", "revoke", "alice", "default"])]
#[case(&["user", "key", "detach", "alice", "default"])]
#[case(&["secret", "remove", "token"])]
#[case(&["crypt", "remove", "ov/file"])]
fn unattended_removal_preserves_all_repository_and_local_state(#[case] args: &[&str]) {
    use std::io::Write;

    let fixture = Fixture::new();
    fixture.enroll();
    fixture.run(&["crypt", "add", "ov/file"]);
    let mut command = fixture.command(&["secret", "add", "token"]);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"private value")
        .unwrap();
    success(child.wait_with_output().unwrap());
    let before = fixture.snapshot();
    let output = fixture.command(args).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("confirmation required"));
    fixture.assert_unchanged(&before);
}

#[test]
fn explicit_key_revocation_rotates_current_state_for_remaining_users() {
    let fixture = Fixture::new();
    fixture.enroll();
    fixture.run(&["user", "key", "revoke", "alice", "default", "--yes"]);
    let users = fixture.run(&["user", "show", "alice"]);
    assert_eq!(users.stdout, b"alice\n");
    let users = fixture.run(&["user", "show", "bob"]);
    assert!(String::from_utf8_lossy(&users.stdout).contains("default"));
}

#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
#[case(true, true)]
fn restore_refusal_preserves_staged_and_worktree_changes(
    #[case] dry_run: bool,
    #[case] generated: bool,
) {
    let fixture = Fixture::new();
    if generated {
        fs::write(fixture.source.join("ov/over.toml"), "mode = 'checkout'\n[[rules]]\npath = '**/*.jinja'\ntemplate = true\nstrip_extension = true\n").unwrap();
        fs::write(fixture.source.join("ov/config.jinja"), "generated\n").unwrap();
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "add generated sibling"]);
    }
    fixture.apply();
    fs::write(fixture.source.join("ov/file"), "staged\n").unwrap();
    fixture.git(&["add", "ov/file"]);
    fs::write(fixture.target.join("file"), "local edit\n").unwrap();
    if generated {
        fs::write(fixture.target.join("config"), "generated edit\n").unwrap();
    }
    let before = fixture.snapshot();
    let mut command = fixture.command(&["restore", "--overlay", "ov", "--target"]);
    command
        .arg(&fixture.target)
        .arg(fixture.target.join("file"));
    if generated {
        command.arg(fixture.target.join("config"));
    } else {
        command.args(["--staged", "--worktree"]);
    }
    if dry_run {
        command.arg("--dry-run");
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.success(),
        dry_run,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if !dry_run {
        assert!(String::from_utf8_lossy(&output.stderr).contains("confirmation required"));
    }
    fixture.assert_unchanged(&before);
}

#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(true, true)]
fn unapply_requires_approval_before_removing_files_or_excludes(
    #[case] no_prompt: bool,
    #[case] dry_run: bool,
) {
    let fixture = Fixture::new();
    git2::Repository::init(&fixture.target).unwrap();
    fixture.apply();
    let before = fixture.snapshot();
    let mut command = fixture.command(&["unapply", "ov", "--target"]);
    command.arg(&fixture.target);
    if no_prompt {
        command.arg("--no-prompt");
    }
    if dry_run {
        command.arg("--dry-run");
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.success(), dry_run);
    if !dry_run {
        assert!(String::from_utf8_lossy(&output.stderr).contains("confirmation required"));
    }
    fixture.assert_unchanged(&before);
}

#[cfg(unix)]
#[rstest]
#[case("\n", false)]
#[case("n\n", false)]
#[case("y\n", true)]
fn terminal_removal_defaults_to_no_and_obeys_explicit_consent(
    #[case] answer: &str,
    #[case] approved: bool,
) {
    let fixture = Fixture::new();
    fixture.apply();
    let before = fixture.snapshot();
    let mut command = Command::new("python3");
    fixture.environment(&mut command);
    let output = command
        .args(["-c", include_str!("fixtures/age-pty.py")])
        .arg(serde_json::to_string(&[("materializations from", answer)]).unwrap())
        .arg(assert_cmd::cargo::cargo_bin!("over"))
        .arg("--home")
        .arg(&fixture.source)
        .args(["unapply", "ov", "--target"])
        .arg(&fixture.target)
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        approved,
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    if approved {
        assert!(!fixture.target.join("file").exists());
    } else {
        fixture.assert_unchanged(&before);
    }
}
