//! A deterministic sudo stand-in exercises the real helper protocol without root or prompts.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::json;

struct Fixture {
    temp: tempfile::TempDir,
    source: PathBuf,
    target: PathBuf,
    bin: PathBuf,
    trace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source with spaces");
        let target = temp.path().join("target with spaces");
        let bin = temp.path().join("bin");
        let trace = temp.path().join("sudo.trace");
        fs::create_dir_all(source.join("overlay")).unwrap();
        fs::write(source.join("over.yaml"), "{}\n").unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let sudo = bin.join("sudo");
        fs::write(&sudo, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_SUDO_TRACE\"\nwhile [ \"$1\" = -n ] || [ \"$1\" = -b ] || [ \"$1\" = -- ]; do shift; done\nenv -i HOME=/sudo-home PATH=/usr/bin:/bin \"$@\" &\nexit 0\n").unwrap();
        fs::set_permissions(sudo, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            temp,
            source,
            target,
            bin,
            trace,
        }
    }

    fn config(&self, value: serde_json::Value) {
        fs::write(
            self.source.join("overlay/over.yaml"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }

    fn command(&self, verb: &str) -> Command {
        self.selected_command(verb, true)
    }

    fn selected_command(&self, verb: &str, named: bool) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        let path = std::env::join_paths(std::iter::once(self.bin.clone()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        command
            .env("PATH", path)
            .env("FAKE_SUDO_TRACE", &self.trace)
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("XDG_CACHE_HOME", self.temp.path().join("cache"))
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .env_remove("OVER_HOME")
            .env_remove("OVER_HOOK_ACTIVE")
            .arg("--home")
            .arg(&self.source)
            .arg(verb);
        if verb == "unapply" {
            command.arg("--yes");
        }
        if named {
            command.arg("overlay");
        }
        command.arg("--target").arg(&self.target);
        command
    }

    fn starts(&self) -> usize {
        fs::read_to_string(&self.trace)
            .unwrap_or_default()
            .lines()
            .count()
    }

    fn commit(&self) {
        let repo = git2::Repository::init(&self.source).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Sudo tests", "sudo@example.invalid").unwrap();
        let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
        let parents = parent.iter().collect::<Vec<_>>();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "fixture",
            &tree,
            &parents,
        )
        .unwrap();
    }
}

#[test]
fn target_operations_and_multiple_hooks_share_one_helper_with_real_standard_streams() {
    let fixture = Fixture::new();
    fs::write(fixture.source.join("overlay/file"), "source").unwrap();
    fixture.config(json!({"sudo": true, "hooks": {"post-apply": [
        {"command": "read value; printf 'input=%s home=%s explicit=%s\\n' \"$value\" \"$HOME\" \"$EXPLICIT\"", "sudo": true, "env": {"EXPLICIT": "present"}},
        {"command": "printf 'second=%s\\n' \"$HOME\"", "sudo": true},
        {"command": "printf 'ordinary=%s\\n' \"$HOME\""}
    ]}}));
    let output = fixture
        .command("apply")
        .arg("--no-prompt")
        .env("HOME", "/caller-home")
        .write_stdin("hello\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    assert!(
        output.contains("input=hello home=/sudo-home explicit=present"),
        "{output}"
    );
    assert!(output.contains("second=/sudo-home"), "{output}");
    assert!(output.contains("ordinary=/caller-home"), "{output}");
    assert_eq!(fixture.starts(), 1);
    assert!(
        fs::read_to_string(&fixture.trace)
            .unwrap()
            .starts_with("-n -b -- ")
    );
    assert_eq!(
        fs::read_link(fixture.target.join("file")).unwrap(),
        fixture.source.join("overlay/file")
    );
    fixture
        .command("unapply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert!(!fixture.target.join("file").exists());
}

#[test]
fn skipped_and_unmarked_hooks_never_start_sudo() {
    let fixture = Fixture::new();
    fixture.config(json!({"hooks": {"post-apply": [
        {"command": "exit 89", "sudo": true, "when": "false"},
        "true"
    ]}}));
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert_eq!(fixture.starts(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn elevated_and_ordinary_hooks_can_read_the_callers_terminal_in_sequence() {
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let fixture = Fixture::new();
    fixture.config(json!({"sudo": true, "hooks": {"post-apply": [
        {"command": "printf 'ROOT_READY\\n'; IFS= read -r value; printf 'ROOT_GOT:%s\\n' \"$value\"", "sudo": true},
        {"command": "printf 'USER_READY\\n'; IFS= read -r value; printf 'USER_GOT:%s\\n' \"$value\""}
    ]}}));
    // A fresh controlling terminal exercises job control without sudo credentials.
    let fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    assert!(fd >= 0);
    let mut master = unsafe { fs::File::from_raw_fd(fd) };
    assert_eq!(unsafe { libc::grantpt(fd) }, 0);
    assert_eq!(unsafe { libc::unlockpt(fd) }, 0);
    let mut name = [0_i8; 512];
    assert_eq!(
        unsafe { libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) },
        0
    );
    let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
        .to_str()
        .unwrap();
    let slave = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut selected = fixture.command("apply");
    selected.arg("--no-prompt");
    let mut command = std::process::Command::new(selected.get_program());
    command.args(selected.get_args());
    for (key, value) in selected.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    command
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    // SAFETY: the child performs only async-signal-safe session/terminal syscalls before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(command);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut output = String::new();
    let mut first = false;
    let mut second = false;
    let status = loop {
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("interactive hook timed out: {output}");
        }
        let mut poll = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll borrows one initialized descriptor for the bounded wait.
        if unsafe { libc::poll(&mut poll, 1, 20) } > 0 && poll.revents & libc::POLLIN != 0 {
            let mut bytes = [0_u8; 4096];
            let count = master.read(&mut bytes).unwrap();
            output.push_str(&String::from_utf8_lossy(&bytes[..count]));
        }
        if !first && output.contains("ROOT_READY") {
            master.write_all(b"one\n").unwrap();
            first = true;
        }
        if !second && output.contains("USER_READY") {
            master.write_all(b"two\n").unwrap();
            second = true;
        }
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
    };
    assert!(status.success(), "{output}");
    assert!(output.contains("ROOT_GOT:one"), "{output}");
    assert!(output.contains("USER_GOT:two"), "{output}");
    assert_eq!(fixture.starts(), 1);
}

#[test]
fn dry_runs_authenticate_for_reads_without_deploying_files_or_running_hooks() {
    let fixture = Fixture::new();
    fixture.config(
        json!({"sudo": true, "hooks": {"post-apply": [{"command": "exit 88", "sudo": true}]}}),
    );
    fs::write(fixture.source.join("overlay/file"), "source").unwrap();
    fixture
        .command("apply")
        .args(["--dry-run", "--no-prompt"])
        .assert()
        .success();
    assert_eq!(fixture.starts(), 1);
    assert!(
        fs::read_to_string(&fixture.trace)
            .unwrap()
            .contains("read-only")
    );
    assert!(!fixture.target.join("file").exists());
}

#[rstest::rstest]
#[case(true, true)]
#[case(false, true)]
#[case(true, false)]
#[case(false, false)]
fn sync_preserves_prompt_and_preview_policy_from_selection_to_execution(
    #[case] named: bool,
    #[case] dry_run: bool,
) {
    let fixture = Fixture::new();
    fixture.config(json!({"sudo": true, "mode": "checkout"}));
    fs::write(fixture.source.join("overlay/file"), "initial").unwrap();
    fixture.commit();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    fs::write(fixture.source.join("overlay/file"), "updated").unwrap();
    fixture.commit();
    fs::write(&fixture.trace, "").unwrap();
    let index = fs::read(fixture.source.join(".git/index")).unwrap();
    let state = fs::read(fixture.temp.path().join("state/over/virtual_checkout.toml")).unwrap();
    let mut command = fixture.selected_command("sync", named);
    command.current_dir(&fixture.target).arg("--no-prompt");
    if dry_run {
        command.arg("--dry-run");
    }
    command.assert().success();
    let trace = fs::read_to_string(&fixture.trace).unwrap();
    assert_eq!(trace.lines().count(), 1, "{trace}");
    assert!(trace.starts_with("-n "), "{trace}");
    assert_eq!(trace.contains("read-only"), dry_run, "{trace}");
    assert_eq!(
        fs::read_to_string(fixture.target.join("file")).unwrap(),
        if dry_run { "initial" } else { "updated" }
    );
    if dry_run {
        assert_eq!(fs::read(fixture.source.join(".git/index")).unwrap(), index);
        assert_eq!(
            fs::read(fixture.temp.path().join("state/over/virtual_checkout.toml")).unwrap(),
            state
        );
    }
}

#[test]
fn sudo_startup_failure_is_not_retried_and_precedes_target_mutation() {
    let fixture = Fixture::new();
    fixture.config(json!({"sudo": true}));
    fs::write(fixture.source.join("overlay/file"), "source").unwrap();
    fs::write(
        fixture.bin.join("sudo"),
        "#!/bin/sh\nprintf 'start\\n' >> \"$FAKE_SUDO_TRACE\"\nexit 1\n",
    )
    .unwrap();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .failure();
    assert_eq!(fixture.starts(), 1);
    assert!(!fixture.target.join("file").exists());
}

#[test]
fn shared_partial_blocks_are_written_before_restrictive_permissions_are_finalized() {
    let fixture = Fixture::new();
    fixture.config(json!({"sudo": true, "permissions": "0444", "uses": ["dependency"]}));
    fs::create_dir_all(fixture.source.join("dependency")).unwrap();
    fs::write(
        fixture.source.join("dependency/over.yaml"),
        "sudo: true\npermissions: '0444'\n",
    )
    .unwrap();
    for (directory, marker) in [("overlay", "first"), ("dependency", "second")] {
        fs::write(
            fixture.source.join(directory).join("settings.partial.yaml"),
            serde_json::to_vec(&json!({
                "target": fixture.target.join("settings"), "marker": marker, "content": marker
            }))
            .unwrap(),
        )
        .unwrap();
    }
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    let content = fs::read_to_string(fixture.target.join("settings")).unwrap();
    assert!(content.contains("first") && content.contains("second"));
    assert_eq!(
        fs::metadata(fixture.target.join("settings"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o444
    );
}

#[test]
fn privileged_partial_files_keep_unrelated_bytes_and_enforce_structured_modes() {
    let fixture = Fixture::new();
    fixture.config(json!({"sudo": true, "permissions": {"mode": "0600"}}));
    fs::write(
        fixture.source.join("overlay/settings.partial.yaml"),
        serde_json::to_vec(&json!({
            "target": fixture.target.join("settings"), "marker": "settings", "content": "managed"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(fixture.target.join("settings"), "unrelated\n").unwrap();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert!(
        fs::read_to_string(fixture.target.join("settings"))
            .unwrap()
            .contains("managed")
    );
    assert_eq!(
        fs::metadata(fixture.target.join("settings"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fixture
        .command("unapply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(fixture.target.join("settings")).unwrap(),
        "unrelated\n"
    );
}

#[test]
fn elevated_hook_exit_codes_survive_the_helper_protocol() {
    let fixture = Fixture::new();
    fixture.config(json!({"hooks": {"post-apply": [{"command": "exit 37", "sudo": true}]}}));
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .code(37);
    assert_eq!(fixture.starts(), 1);
}

#[test]
fn mapped_rendered_targets_use_rule_elevation_while_artifacts_remain_caller_owned() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let target = fixture.target.join("protected/deep/config");
    fixture.config(json!({"rules": [{"path": "config.jinja", "target": target, "template": true, "sudo": true}]}));
    fs::write(
        fixture.source.join("overlay/config.jinja"),
        "rendered {{ 1 + 1 }}\n",
    )
    .unwrap();
    fixture.commit();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert_eq!(fixture.starts(), 1);
    assert_eq!(fs::read_to_string(&target).unwrap(), "rendered 2");
    let artifact = fs::read_link(&target).unwrap();
    // Target elevation must never create root-owned user state or artifact files.
    assert_eq!(
        fs::metadata(artifact).unwrap().uid(),
        fs::metadata(&fixture.source).unwrap().uid()
    );
    fixture.command("status").assert().success();
    fixture.command("diff").assert().success();
    fixture
        .command("unapply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert!(!target.exists());
}

#[test]
fn virtual_checkouts_share_target_access_across_apply_inspection_sync_and_unapply() {
    let fixture = Fixture::new();
    fixture.config(
        json!({"sudo": true, "mode": "checkout", "rules": [{"path": "local", "sudo": false}]}),
    );
    fs::write(fixture.source.join("overlay/file"), "first").unwrap();
    fs::write(fixture.source.join("overlay/local"), "local").unwrap();
    fixture.commit();
    fixture
        .command("apply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert_eq!(fixture.starts(), 1);
    assert_eq!(
        fs::read_to_string(fixture.target.join("file")).unwrap(),
        "first"
    );
    fixture.command("status").assert().success();
    fixture.command("diff").assert().success();
    fs::write(fixture.source.join("overlay/file"), "second").unwrap();
    fixture.commit();
    fixture
        .command("sync")
        .args(["--no-prompt", "--no-pull", "--no-push", "--pull-only"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(fixture.target.join("file")).unwrap(),
        "second"
    );
    fixture
        .command("unapply")
        .arg("--no-prompt")
        .assert()
        .success();
    assert!(!fixture.target.join("file").exists());
    assert!(!fixture.target.join("local").exists());
}
