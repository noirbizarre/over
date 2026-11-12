//! Bootstrap encryption through real CLI processes, including Linux terminal input.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const PLAINTEXT: &[u8] = b"bootstrap protected file sentinel\n";
const SECRET: &[u8] = b"bootstrap native secret sentinel";
#[cfg(target_os = "linux")]
const PASSPHRASE: &str = "bootstrap-hidden-passphrase-sentinel";

struct Fixture {
    directory: tempfile::TempDir,
    source: PathBuf,
    clone: PathBuf,
    identity: PathBuf,
}

impl Fixture {
    fn new(passphrase: bool, hooks: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let fixture = Self {
            source,
            clone: directory.path().join("clone"),
            identity: directory.path().join("identity.age"),
            directory,
        };
        fixture.git(&fixture.source, &["init", "-q"]);
        fixture.git(&fixture.source, &["config", "core.autocrlf", "false"]);
        fixture.git(&fixture.source, &["config", "user.name", "Bootstrap test"]);
        fixture.git(
            &fixture.source,
            &["config", "user.email", "bootstrap@example.invalid"],
        );
        if passphrase {
            #[cfg(target_os = "linux")]
            {
                // The API installs a test-executable filter; crypt add below replaces it
                // with the actual CLI before any protected content reaches Git.
                let repo = git2::Repository::open(&fixture.source).unwrap();
                over::crypt::access::add_key(
                    &repo,
                    "alice",
                    "default",
                    None,
                    Some(PASSPHRASE.into()),
                    true,
                )
                .unwrap();
            }
            #[cfg(not(target_os = "linux"))]
            panic!("passphrase fixtures require the Linux terminal driver");
        } else {
            checked(run(fixture.over(&fixture.source).args([
                "user",
                "add",
                "alice",
                "--identity-output",
                fixture.identity.to_str().unwrap(),
            ])));
        }
        fs::write(fixture.source.join("token"), PLAINTEXT).unwrap();
        checked(run(fixture
            .over(&fixture.source)
            .args(["crypt", "add", "token"])));
        let mut secret = fixture.over(&fixture.source);
        secret.args(["secret", "add", "TOKEN"]);
        checked(
            assert_cmd::Command::from_std(secret)
                .timeout(Duration::from_secs(45))
                .write_stdin(SECRET)
                .output()
                .unwrap(),
        );
        fixture.write_descriptor(&fixture.source, hooks);
        fixture.git(&fixture.source, &["add", "."]);
        fixture.git(
            &fixture.source,
            &["commit", "-qm", "protected bootstrap fixture"],
        );
        over::crypt::format::inspect(&fixture.git(&fixture.source, &["show", "HEAD:token"]).stdout)
            .unwrap();
        fixture
    }

    fn write_descriptor(&self, root: &Path, hooks: bool) {
        let mut descriptor = serde_json::json!({"target": "~"});
        if hooks {
            // Both independent hook processes must observe plaintext, not merely a key file.
            let hook = |event| {
                format!(
                    "cat token > .git/{event}.token && \"$CRYPT_INIT_OVER\" --home \"$OVER_REPOSITORY_ROOT\" secret get TOKEN > .git/{event}.secret && printf '{event}\\n' >> .git/bootstrap-events"
                )
            };
            descriptor["hooks"] = serde_json::json!({
                "post-clone": [hook("post-clone")],
                "post-init": [hook("post-init")],
            });
        }
        fs::write(
            // JSON is a YAML subset; serialization safely quotes shell command strings.
            root.join("over.yaml"),
            serde_json::to_vec(&descriptor).unwrap(),
        )
        .unwrap();
    }

    fn environment(&self, command: &mut Command) {
        // Remove the entire inherited Git namespace, including indexed config overrides
        // and object/alternate routing; individual removals miss caller-specific entries.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_")
                || name.to_string_lossy().starts_with("OVER_")
                || name.to_string_lossy().starts_with("XDG_")
            {
                command.env_remove(name);
            }
        }
        command
            .env("HOME", self.directory.path())
            .env("USERPROFILE", self.directory.path())
            .env("XDG_CONFIG_HOME", self.directory.path().join("config"))
            .env("XDG_DATA_HOME", self.directory.path().join("data"))
            .env("XDG_STATE_HOME", self.directory.path().join("state"))
            .env("XDG_CACHE_HOME", self.directory.path().join("cache"))
            .env("XDG_RUNTIME_DIR", self.directory.path().join("runtime"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.directory.path().join("gitconfig"))
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("NO_COLOR", "1")
            .env("TERM", "xterm")
            .env("CRYPT_INIT_OVER", assert_cmd::cargo::cargo_bin!("over"))
            .stdin(Stdio::null());
    }

    fn over(&self, home: &Path) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        self.environment(&mut command);
        // A stable existing cwd also covers init before the destination exists.
        command.current_dir(&self.source).arg("--home").arg(home);
        command
    }

    fn git(&self, root: &Path, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        self.environment(&mut command);
        checked(run(command.current_dir(root).args(args)))
    }

    fn init(&self) -> Command {
        let mut command = self.over(&self.clone);
        command.arg("init").arg(&self.source);
        command
    }

    fn assert_locked(&self) {
        assert!(!self.clone.join(".git/over-crypt/key").exists());
        over::crypt::format::inspect(&fs::read(self.clone.join("token")).unwrap()).unwrap();
        assert!(!self.clone.join(".git/bootstrap-events").exists());
        let rejected = run(self.over(&self.clone).args(["secret", "get", "TOKEN"]));
        assert!(!rejected.status.success());
    }

    fn assert_hooks(&self, events: &[&str]) {
        for event in events {
            assert_eq!(
                fs::read(self.clone.join(format!(".git/{event}.token"))).unwrap(),
                PLAINTEXT
            );
            assert_eq!(
                fs::read(self.clone.join(format!(".git/{event}.secret"))).unwrap(),
                SECRET
            );
        }
        assert_eq!(
            fs::read_to_string(self.clone.join(".git/bootstrap-events")).unwrap(),
            events
                .iter()
                .map(|event| format!("{event}\n"))
                .collect::<String>()
        );
    }

    fn assert_persistent_unlock(&self) {
        assert_eq!(fs::read(self.clone.join("token")).unwrap(), PLAINTEXT);
        assert_eq!(
            checked(run(self.over(&self.clone).args(["secret", "get", "TOKEN"]))).stdout,
            SECRET
        );
        // Force an independent Git smudge, then clean, rather than trusting cached stat data.
        fs::remove_file(self.clone.join("token")).unwrap();
        self.git(&self.clone, &["checkout", "--", "token"]);
        assert_eq!(fs::read(self.clone.join("token")).unwrap(), PLAINTEXT);
        self.git(&self.clone, &["add", "token"]);
        over::crypt::format::inspect(&self.git(&self.clone, &["show", ":token"]).stdout).unwrap();
        assert!(
            self.git(&self.clone, &["status", "--porcelain"])
                .stdout
                .is_empty()
        );
    }

    #[cfg(target_os = "linux")]
    fn interact(&self, command: &mut Command, steps: &[(&str, &str)]) -> Output {
        // Like patch_add.rs, use a controlling PTY and wait for each prompt before
        // sending input. A second password prompt receives no answer and times out.
        let driver = r#"
import errno, fcntl, json, os, pty, select, signal, struct, sys, time, termios
steps = json.loads(sys.argv[1])
pid, fd = pty.fork()
if pid == 0:
    os.execv(sys.argv[2], sys.argv[2:])
fcntl.ioctl(fd, 0x5414, struct.pack('HHHH', 50, 120, 0, 0))
deadline = time.monotonic() + 30
output = b''
pending = b''
sent = 0
timed_out = False
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
            if 'passphrase' in steps[sent][0]:
                # Dialoguer prints the label before disabling echo and flushing input.
                # Wait for its secure terminal mode instead of racing that flush.
                ready = time.monotonic() + 2
                while termios.tcgetattr(fd)[3] & termios.ECHO:
                    if time.monotonic() >= ready:
                        raise RuntimeError('password prompt did not disable echo')
                    time.sleep(0.01)
            if steps[sent][1] == '\x03':
                # Cancel at the prompt without delivering or logging a credential.
                os.killpg(pid, signal.SIGINT)
            else:
                os.write(fd, steps[sent][1].encode())
            sent += 1
            pending = b''
else:
    timed_out = True
    os.killpg(pid, signal.SIGKILL)
_, status = os.waitpid(pid, 0)
sys.stdout.buffer.write(output)
if timed_out:
    sys.stderr.write('terminal interaction timed out\n')
    sys.exit(124)
if sent != len(steps):
    sys.stderr.write('command exited before all expected prompts\n')
    sys.exit(125)
sys.exit(os.waitstatus_to_exitcode(status))
"#;
        let mut python = Command::new("python3");
        self.environment(&mut python);
        python.current_dir(&self.source).args(["-c", driver]);
        python.arg(serde_json::to_string(steps).unwrap());
        python.arg(command.get_program()).args(command.get_args());
        let output = run(&mut python);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !text.contains(PASSPHRASE),
            "hidden passphrase appeared in terminal output"
        );
        assert!(
            !text.contains("incorrect-password-sentinel"),
            "hidden wrong password appeared in terminal output"
        );
        assert_ne!(output.status.code(), Some(124), "{text}");
        assert_ne!(output.status.code(), Some(125), "{text}");
        output
    }
}

fn run(command: &mut Command) -> Output {
    assert_cmd::Command::from_std(std::mem::replace(command, Command::new("unused")))
        .timeout(Duration::from_secs(45))
        .output()
        .unwrap()
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
fn explicit_identity_unlocks_a_fresh_noninteractive_init_before_both_bootstrap_hooks() {
    let fixture = Fixture::new(false, true);
    checked(run(fixture
        .init()
        .args(["--unlock", "--identity"])
        .arg(&fixture.identity)));
    fixture.assert_hooks(&["post-clone", "post-init"]);
    fixture.assert_persistent_unlock();
}

#[test]
fn noninteractive_init_keeps_ciphertext_until_explicit_identity_unlock_before_post_init() {
    let fixture = Fixture::new(false, false);
    checked(run(&mut fixture.init()));
    fixture.assert_locked();
    // Install hooks only after proving the default clone remains locked, so the
    // retry can demonstrate unlock ordering independently of the fresh-clone path.
    fixture.write_descriptor(&fixture.clone, true);
    fixture.git(&fixture.clone, &["add", "over.yaml"]);
    fixture.git(
        &fixture.clone,
        &[
            "-c",
            "user.name=Bootstrap test",
            "-c",
            "user.email=bootstrap@example.invalid",
            "commit",
            "-qm",
            "enable retry hook",
        ],
    );
    checked(run(fixture
        .over(&fixture.clone)
        .args(["init", "--unlock", "--identity"])
        .arg(&fixture.identity)));
    fixture.assert_hooks(&["post-init"]);
    fixture.assert_persistent_unlock();
}

#[test]
fn noninteractive_init_unlock_without_a_credential_rejects_without_installing_a_local_key() {
    let fixture = Fixture::new(false, true);
    let output = run(fixture.init().arg("--unlock"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("identity"));
    fixture.assert_locked();
}

#[test]
fn no_argument_unlock_repairs_broken_filters_even_with_an_existing_local_key() {
    let fixture = Fixture::new(false, false);
    checked(run(fixture
        .init()
        .args(["--unlock", "--identity"])
        .arg(&fixture.identity)));
    for name in [
        "filter.over.clean",
        "filter.over.smudge",
        "filter.over.process",
    ] {
        fixture.git(
            &fixture.clone,
            &["config", name, "over-intentionally-missing-filter"],
        );
    }
    fixture.git(&fixture.clone, &["config", "filter.over.required", "false"]);
    // A checkout without working smudge filters can leave ciphertext beside a
    // valid local key; unlock must repair that user-visible state too.
    fs::write(
        fixture.clone.join("token"),
        fixture.git(&fixture.clone, &["show", "HEAD:token"]).stdout,
    )
    .unwrap();
    checked(run(fixture.over(&fixture.clone).arg("unlock")));
    assert_eq!(
        fixture
            .git(&fixture.clone, &["config", "--get", "filter.over.required"])
            .stdout,
        b"true\n"
    );
    fixture.assert_persistent_unlock();
}

#[cfg(target_os = "linux")]
#[test]
fn interactive_fresh_init_offers_unlock_and_accepts_one_hidden_passphrase_before_hooks() {
    let fixture = Fixture::new(true, true);
    checked(fixture.interact(
        &mut fixture.init(),
        &[
            ("Unlock repository encryption", "y"),
            ("passphrase: ", &format!("{PASSPHRASE}\n")),
        ],
    ));
    fixture.assert_hooks(&["post-clone", "post-init"]);
    fixture.assert_persistent_unlock();
}

#[cfg(target_os = "linux")]
#[rstest::rstest]
#[case::wrong_password("incorrect-password-sentinel\n")]
#[case::cancelled_password("\u{3}")]
fn failed_or_cancelled_interactive_init_leaves_ciphertext_locked_and_skips_hooks(
    #[case] answer: &str,
) {
    let fixture = Fixture::new(true, true);
    let output = fixture.interact(
        &mut fixture.init(),
        &[
            ("Unlock repository encryption", "y"),
            ("passphrase: ", answer),
        ],
    );
    assert!(!output.status.success());
    fixture.assert_locked();
}

#[cfg(target_os = "linux")]
#[test]
fn interrupted_state_refuses_unlock_before_asking_for_another_credential() {
    let fixture = Fixture::new(true, false);
    checked(run(&mut fixture.init()));
    fs::write(
        fixture.clone.join(".git/over-crypt/transaction.json"),
        b"pending recovery",
    )
    .unwrap();
    let output = fixture.interact(fixture.over(&fixture.clone).arg("unlock"), &[]);
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("over crypt recover"));
    assert!(!text.contains("Age key passphrase"));
    fixture.assert_locked();
}

#[test]
fn a_locked_noninteractive_init_does_not_start_bootstrap_commands() {
    let fixture = Fixture::new(false, true);
    let output = run(&mut fixture.init());
    assert!(!output.status.success());
    fixture.assert_locked();
    assert!(
        !fixture.clone.join(".git/post-clone.token").exists(),
        "bootstrap must not consume ciphertext before discovering that secrets are locked"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn declining_init_unlock_leaves_bootstrap_commands_unexecuted() {
    let fixture = Fixture::new(false, true);
    let output = fixture.interact(
        &mut fixture.init(),
        &[("Unlock repository encryption", "n")],
    );
    assert!(!output.status.success());
    fixture.assert_locked();
    assert!(!fixture.clone.join(".git/post-clone.token").exists());
}

#[test]
fn init_repairs_a_persisted_unlock_before_running_hooks_again() {
    let fixture = Fixture::new(false, true);
    checked(run(fixture
        .init()
        .args(["--unlock", "--identity"])
        .arg(&fixture.identity)));
    fs::write(
        fixture.clone.join("token"),
        fixture.git(&fixture.clone, &["show", "HEAD:token"]).stdout,
    )
    .unwrap();
    for file in ["post-init.token", "post-init.secret", "bootstrap-events"] {
        fs::remove_file(fixture.clone.join(".git").join(file)).unwrap();
    }
    checked(run(fixture.over(&fixture.clone).arg("init")));
    fixture.assert_hooks(&["post-init"]);
    fixture.assert_persistent_unlock();
}

#[test]
fn repairing_an_existing_unlock_preserves_plaintext_working_edits() {
    let fixture = Fixture::new(false, false);
    checked(run(fixture
        .init()
        .args(["--unlock", "--identity"])
        .arg(&fixture.identity)));
    fs::write(fixture.clone.join("token"), b"independent working edit").unwrap();
    checked(run(fixture.over(&fixture.clone).arg("init")));
    assert_eq!(
        fs::read(fixture.clone.join("token")).unwrap(),
        b"independent working edit"
    );
}
