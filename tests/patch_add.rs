//! Exercise the actual command, including its terminal interaction.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    directory: tempfile::TempDir,
    source: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_worktree(false)
    }

    fn with_worktree(linked: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        fs::create_dir_all(source.join("ov")).unwrap();
        fs::write(source.join("ov/over.toml"), "mode = 'checkout'\n").unwrap();
        fs::write(
            source.join("ov/file"),
            (0..30)
                .map(|line| format!("line {line}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let mut fixture = Self {
            directory,
            source,
            target,
        };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.name", "Test"]);
        fixture.git(&["config", "user.email", "test@example.com"]);
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "initial"]);
        if linked {
            let worktree = fixture.directory.path().join("linked");
            fixture.git(&[
                "worktree",
                "add",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ]);
            fixture.source = worktree;
        }
        let mut apply = fixture.command();
        checked(
            apply
                .args(["apply", "ov", "--no-prompt", "--target"])
                .arg(&fixture.target)
                .output()
                .unwrap(),
        );
        fixture
    }

    fn environment(&self, command: &mut Command) {
        command
            .env("XDG_STATE_HOME", self.directory.path().join("state"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.directory.path().join("gitconfig"))
            .env("TERM", "xterm");
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_EXTERNAL_DIFF",
        ] {
            command.env_remove(name);
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        self.environment(&mut command);
        command.arg("--home").arg(&self.source);
        command
    }

    fn git(&self, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        self.environment(&mut command);
        checked(
            command
                .current_dir(&self.source)
                .args(args)
                .output()
                .unwrap(),
        )
    }

    fn changed(&self) -> String {
        let original = fs::read_to_string(self.target.join("file")).unwrap();
        let changed = original
            .replace("line 0\n", "accepted\n")
            .replace("line 29\n", "skipped\n");
        fs::write(self.target.join("file"), &changed).unwrap();
        changed
    }

    #[cfg(target_os = "linux")]
    fn interact(&self, keys: &str) {
        // A controlling PTY is necessary: dialoguer reads the terminal, not a
        // scripted stdin pipe. The deadline turns prompt regressions into failures.
        let driver = r#"
import errno, fcntl, os, pty, select, signal, struct, sys, time, tty
pid, fd = pty.fork()
if pid == 0:
    os.execv(sys.argv[2], sys.argv[2:])
tty.setraw(fd)
fcntl.ioctl(fd, 0x5414, struct.pack('HHHH', 50, 120, 0, 0))
deadline = time.monotonic() + 20
output = b''
sent = False
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
        if not sent and b'Stage this change?' in output and b'Cancel' in output:
            os.write(fd, sys.argv[1].encode())
            sent = True
else:
    os.kill(pid, signal.SIGKILL)
    raise RuntimeError('patch prompt timed out: ' + repr(output))
_, status = os.waitpid(pid, 0)
sys.stdout.buffer.write(output)
sys.exit(os.waitstatus_to_exitcode(status))
"#;
        let mut command = Command::new("python3");
        self.environment(&mut command);
        checked(
            command
                .args(["-c", driver, keys])
                .arg(assert_cmd::cargo::cargo_bin!("over"))
                .arg("--home")
                .arg(&self.source)
                .args(["add", "--patch", "--overlay", "ov", "--target"])
                .arg(&self.target)
                .arg(self.target.join("file"))
                .output()
                .unwrap(),
        );
    }
}

fn checked(output: Output) -> Output {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn dry_run_displays_hunks_without_a_terminal_or_index_writes() {
    let fixture = Fixture::new();
    let changed = fixture.changed();
    let original = fs::read(fixture.source.join(".git/index")).unwrap();
    let output = checked(
        fixture
            .command()
            .args(["add", "-p", "--dry-run", "--overlay", "ov", "--target"])
            .arg(&fixture.target)
            .arg(fixture.target.join("file"))
            .output()
            .unwrap(),
    );
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("candidate"));
    assert!(text.contains("+accepted"));
    assert!(text.contains("+skipped"));
    assert_eq!(
        fs::read(fixture.source.join(".git/index")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(fixture.target.join("file")).unwrap(),
        changed
    );
}

#[test]
fn patch_mode_requires_a_terminal_instead_of_accepting_piped_choices() {
    let fixture = Fixture::new();
    fixture.changed();
    let output = fixture
        .command()
        .args(["add", "-p", "--overlay", "ov", "--target"])
        .arg(&fixture.target)
        .arg(fixture.target.join("file"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires a terminal"));
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_choices_stage_one_hunk_and_keep_the_other_unstaged() {
    let fixture = Fixture::new();
    let changed = fixture.changed();
    // Default is Skip; k selects Accept, then space confirms each selection.
    fixture.interact("k  ");
    let staged = fixture.git(&["diff", "--cached", "--no-ext-diff"]);
    let staged = String::from_utf8(staged.stdout).unwrap();
    assert!(staged.contains("+accepted"));
    assert!(!staged.contains("+skipped"));
    assert_eq!(
        fs::read_to_string(fixture.target.join("file")).unwrap(),
        changed
    );
    let diff = checked(
        fixture
            .command()
            .args(["diff", "ov", "--target"])
            .arg(&fixture.target)
            .output()
            .unwrap(),
    );
    let unstaged = String::from_utf8(diff.stdout).unwrap();
    assert!(unstaged.contains("+skipped"));
    assert!(!unstaged.contains("+accepted"));
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_cancellation_discards_an_earlier_accepted_hunk() {
    let fixture = Fixture::new();
    fixture.changed();
    let original = fs::read(fixture.source.join(".git/index")).unwrap();
    fixture.interact("k jj ");
    assert_eq!(
        fs::read(fixture.source.join(".git/index")).unwrap(),
        original
    );
}

#[cfg(target_os = "linux")]
#[test]
fn patch_staging_updates_a_linked_worktree_index_without_touching_the_main_index() {
    let fixture = Fixture::with_worktree(true);
    let main_index = fixture.directory.path().join("source/.git/index");
    let original = fs::read(&main_index).unwrap();
    fixture.changed();
    fixture.interact("k  ");
    assert_eq!(fs::read(main_index).unwrap(), original);
    let diff = fixture.git(&["diff", "--cached", "--no-ext-diff"]);
    assert!(String::from_utf8_lossy(&diff.stdout).contains("+accepted"));
    assert!(!String::from_utf8_lossy(&diff.stdout).contains("+skipped"));
}

#[cfg(target_os = "linux")]
#[rstest::rstest]
#[case(false)]
#[case(true)]
fn terminal_whole_file_choices_stage_binary_changes_and_deletions(#[case] binary: bool) {
    let fixture = Fixture::new();
    let original = fs::read(fixture.source.join("ov/file")).unwrap();
    if binary {
        fs::write(fixture.target.join("file"), b"binary\0change").unwrap();
    } else {
        fs::remove_file(fixture.target.join("file")).unwrap();
    }
    fixture.interact("k ");
    if binary {
        assert_eq!(fixture.git(&["show", ":ov/file"]).stdout, b"binary\0change");
        assert_eq!(
            fs::read(fixture.target.join("file")).unwrap(),
            b"binary\0change"
        );
    } else {
        let changes = fixture.git(&["diff", "--cached", "--name-status"]);
        assert!(String::from_utf8_lossy(&changes.stdout).contains("D\tov/file"));
        assert!(!fixture.target.join("file").exists());
    }
    assert_eq!(fs::read(fixture.source.join("ov/file")).unwrap(), original);
}
