//! Exercise the standard plugin boundary without requiring a hardware device in CI.
#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use age::secrecy::SecretString;
use rstest::rstest;

use over::crypt::{access, format::RepositoryKey};

const RECIPIENT: &str = "age1overfixture1qq79m20a";

struct Fixture {
    directory: tempfile::TempDir,
    repo: PathBuf,
    bin: PathBuf,
    identity: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        git2::Repository::init(&repo).unwrap();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let plugin = bin.join("age-plugin-overfixture");
        fs::write(&plugin, include_str!("fixtures/age-plugin-overfixture.py")).unwrap();
        fs::set_permissions(plugin, fs::Permissions::from_mode(0o700)).unwrap();
        let identity = directory.path().join("identities");
        fs::write(
            &identity,
            format!(
                "{}\n",
                age::plugin::Identity::default_for_plugin("overfixture").unwrap()
            ),
        )
        .unwrap();
        let log = directory.path().join("invocations");
        Self {
            directory,
            repo,
            bin,
            identity,
            log,
        }
    }

    fn environment(&self, command: &mut Command) {
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command
            .env_remove("OVER_HOME")
            .env("XDG_STATE_HOME", self.directory.path().join("state"))
            .env("XDG_CACHE_HOME", self.directory.path().join("cache"))
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("OVER_TEST_PLUGIN_LOG", &self.log)
            .env("HOME", self.directory.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        command
            .current_dir(&self.repo)
            .args(args)
            .stdin(Stdio::null());
        self.environment(&mut command);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        success(self.command(args).output().unwrap())
    }

    fn enroll(&self) {
        self.run(&["user", "add", "alice", "--recipient", RECIPIENT]);
    }

    fn software(&self) -> PathBuf {
        self.software_named("backup")
    }

    fn software_named(&self, name: &str) -> PathBuf {
        let path = self.directory.path().join("software");
        self.run(&[
            "user",
            "key",
            "add",
            "alice",
            "--name",
            name,
            "--identity-output",
            path.to_str().unwrap(),
        ]);
        path
    }

    fn terminal(&self, args: &[&str], steps: &[(&str, &str)], mode: &str) -> Output {
        let mut command = Command::new("python3");
        self.environment(&mut command);
        let output = command
            .current_dir(&self.repo)
            .env("OVER_TEST_PLUGIN_MODE", mode)
            .args(["-c", include_str!("fixtures/age-pty.py")])
            .arg(serde_json::to_string(steps).unwrap())
            .arg(assert_cmd::cargo::cargo_bin!("over"))
            .args(args)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(!text.contains("pin-sentinel"), "PIN was echoed");
        assert!(!text.contains("recovery-sentinel"), "passphrase was echoed");
        assert_ne!(output.status.code(), Some(124), "PTY timed out: {text}");
        assert_ne!(
            output.status.code(),
            Some(125),
            "missing PTY prompt: {text}"
        );
        output
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

#[test]
fn plugin_envelopes_unlock_once_and_filters_do_not_invoke_the_plugin() {
    let fixture = Fixture::new();
    fixture.enroll();
    fixture.run(&["lock"]);
    fixture.run(&["unlock", "--identity", fixture.identity.to_str().unwrap()]);
    let log = fs::read(&fixture.log).unwrap();
    assert!(String::from_utf8_lossy(&log).contains("identity-v1"));
    let mut command = fixture.command(&["git", "filter", "clean"]);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"secret payload")
        .unwrap();
    let encrypted = success(child.wait_with_output().unwrap()).stdout;
    over::crypt::format::inspect(&encrypted).unwrap();
    let mut child = fixture
        .command(&["git", "filter", "smudge"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&encrypted).unwrap();
    assert_eq!(
        success(child.wait_with_output().unwrap()).stdout,
        b"secret payload"
    );
    fs::write(fixture.repo.join("protected"), b"filtered content").unwrap();
    fixture.run(&["crypt", "add", "protected"]);
    for _ in 0..3 {
        let mut git = Command::new("git");
        fixture.environment(&mut git);
        success(
            git.current_dir(&fixture.repo)
                .args(["add", "protected"])
                .output()
                .unwrap(),
        );
    }
    let home = fixture.repo.to_str().unwrap();
    let mut child = fixture
        .command(&["--home", home, "secret", "add", "native"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"native content")
        .unwrap();
    success(child.wait_with_output().unwrap());
    assert_eq!(
        fixture
            .run(&["--home", home, "secret", "get", "native"])
            .stdout,
        b"native content"
    );
    assert_eq!(fs::read(&fixture.log).unwrap(), log);
    fixture.run(&["lock"]);
    let mut child = fixture
        .command(&["git", "filter", "clean"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"secret payload")
        .unwrap();
    assert!(!child.wait_with_output().unwrap().status.success());
    assert_eq!(fs::read(&fixture.log).unwrap(), log);
}

#[rstest]
#[case("absent")]
#[case("pin")]
#[case("cancel")]
#[case("malformed")]
#[case("interactive")]
fn a_failed_plugin_does_not_block_an_independent_software_identity(#[case] mode: &str) {
    let fixture = Fixture::new();
    fixture.enroll();
    // Force the device envelope before software recovery, regardless of identity-file order.
    let software = fixture.software_named("z-backup");
    for plugin_first in [true, false] {
        let plugin = fs::read_to_string(&fixture.identity).unwrap();
        let key = fs::read_to_string(&software).unwrap();
        fs::write(
            &fixture.identity,
            if plugin_first {
                format!("{plugin}{key}")
            } else {
                format!("{key}{plugin}")
            },
        )
        .unwrap();
        fixture.run(&["lock"]);
        let before = fs::read_to_string(&fixture.log).unwrap().len();
        success(
            fixture
                .command(&["unlock", "--identity", fixture.identity.to_str().unwrap()])
                .env("OVER_TEST_PLUGIN_MODE", mode)
                .output()
                .unwrap(),
        );
        let log = fs::read_to_string(&fixture.log).unwrap();
        assert!(log[before..].contains("--age-plugin=identity-v1"));
        fs::write(
            &fixture.identity,
            plugin.lines().next().unwrap().to_owned() + "\n",
        )
        .unwrap();
    }
}

#[test]
fn a_missing_plugin_in_a_shared_identity_file_does_not_block_software_recovery() {
    let fixture = Fixture::new();
    fixture.enroll();
    let software = fixture.software();
    let plugin = fs::read_to_string(&fixture.identity).unwrap();
    fs::write(
        &fixture.identity,
        plugin + &fs::read_to_string(software).unwrap(),
    )
    .unwrap();
    fs::remove_file(fixture.bin.join("age-plugin-overfixture")).unwrap();
    fixture.run(&["lock"]);
    fixture.run(&["unlock", "--identity", fixture.identity.to_str().unwrap()]);
}

#[test]
fn an_explicit_key_selection_never_escapes_to_another_configured_key() {
    let fixture = Fixture::new();
    fixture.enroll();
    let software = fixture.software();
    let plugin = fs::read_to_string(&fixture.identity).unwrap();
    fs::write(
        &fixture.identity,
        plugin + &fs::read_to_string(software).unwrap(),
    )
    .unwrap();
    fixture.run(&["lock"]);
    let output = fixture
        .command(&[
            "unlock",
            "--user",
            "alice",
            "--key",
            "default",
            "--identity",
            fixture.identity.to_str().unwrap(),
        ])
        .env("OVER_TEST_PLUGIN_MODE", "absent")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!fixture.repo.join(".git/over-crypt/key").exists());
}

#[test]
fn recipient_lists_publish_separate_keys_or_nothing() {
    let fixture = Fixture::new();
    let software = age::x25519::Identity::generate();
    let list = fixture.directory.path().join("recipients");
    fs::write(
        &list,
        format!(
            "# Passage recipients\r\n\r\n{RECIPIENT}\r\n{}\r\n",
            software.to_public()
        ),
    )
    .unwrap();
    fixture.run(&[
        "user",
        "add",
        "alice",
        "--recipients-file",
        list.to_str().unwrap(),
        "--name-prefix",
        "device",
    ]);
    let repo = git2::Repository::open(&fixture.repo).unwrap();
    let registry = access::load(&repo).unwrap();
    assert_eq!(
        registry.users["alice"].keys().cloned().collect::<Vec<_>>(),
        ["device-1", "device-2"]
    );
    let before = fs::read(access::metadata_path(&repo).unwrap()).unwrap();
    let additional = age::x25519::Identity::generate();
    let output = fixture
        .command(&[
            "user",
            "add",
            "bob",
            "--name-prefix",
            "import",
            "--recipient",
            &additional.to_public().to_string(),
            "--recipient",
            RECIPIENT,
        ])
        .env("OVER_TEST_PLUGIN_MODE", "absent")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read(access::metadata_path(&repo).unwrap()).unwrap(),
        before
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-error-sentinel"));
}

#[test]
fn stale_hints_are_skipped_but_explicit_identity_inputs_do_not_use_other_hints() {
    let fixture = Fixture::new();
    fixture.enroll();
    let software = fixture.software();
    fixture.run(&["lock"]);
    // Explicit input restricts identity sources, even when another local key is remembered.
    let unavailable = fixture.directory.path().join("missing");
    assert!(
        !fixture
            .command(&["unlock", "--identity", unavailable.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
    fixture.run(&[
        "unlock",
        "--identity",
        unavailable.to_str().unwrap(),
        "--identity",
        software.to_str().unwrap(),
    ]);
    let second = fixture.directory.path().join("second-software");
    fixture.run(&[
        "user",
        "key",
        "add",
        "alice",
        "--name",
        "second",
        "--identity-output",
        second.to_str().unwrap(),
    ]);
    fs::remove_file(software).unwrap();
    fixture.run(&["lock"]);
    fixture.run(&["unlock"]);
}

#[derive(Clone, Default)]
struct TestCallbacks(Arc<Mutex<Vec<&'static str>>>);

impl age::Callbacks for TestCallbacks {
    fn display_message(&self, _: &str) {
        self.0.lock().unwrap().push("message");
    }
    fn confirm(&self, _: &str, _: &str, _: Option<&str>) -> Option<bool> {
        self.0.lock().unwrap().push("confirm");
        Some(true)
    }
    fn request_public_string(&self, _: &str) -> Option<String> {
        self.0.lock().unwrap().push("public");
        Some("device".into())
    }
    fn request_passphrase(&self, _: &str) -> Option<SecretString> {
        self.0.lock().unwrap().push("secret");
        Some("pin-sentinel".into())
    }
}

#[test]
fn standard_plugin_callbacks_and_passphrase_recovery_are_independent() {
    if std::env::var_os("OVER_TEST_PLUGIN_CHILD").is_none() {
        let fixture = Fixture::new();
        let mut child = Command::new(std::env::current_exe().unwrap());
        fixture.environment(&mut child);
        success(
            child
                .args([
                    "--exact",
                    "standard_plugin_callbacks_and_passphrase_recovery_are_independent",
                ])
                .env("OVER_TEST_PLUGIN_CHILD", "1")
                .env("OVER_TEST_PLUGIN_MODE", "interactive")
                .output()
                .unwrap(),
        );
        return;
    }
    let callbacks = TestCallbacks::default();
    let key = RepositoryKey::generate().unwrap();
    let envelope = access::envelope_with(&key, Some(RECIPIENT), None, callbacks.clone()).unwrap();
    let stub = age::plugin::Identity::default_for_plugin("overfixture").unwrap();
    let identity =
        age::plugin::IdentityPluginV1::new("overfixture", &[stub], callbacks.clone()).unwrap();
    let mut reader = age::Decryptor::new(&envelope.envelope[..])
        .unwrap()
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .unwrap();
    let mut output = Vec::new();
    reader.read_to_end(&mut output).unwrap();
    assert_eq!(output.len(), 96);
    assert_eq!(output[..16], key.repository);
    assert_eq!(output[16..32], key.generation);
    assert_eq!(
        *callbacks.0.lock().unwrap(),
        ["message", "confirm", "public", "secret"]
    );

    let directory = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(directory.path()).unwrap();
    access::add_key(
        &repo,
        "alice",
        "password",
        None,
        Some("recovery".into()),
        true,
    )
    .unwrap();
    access::add_key(&repo, "alice", "device", Some(RECIPIENT), None, false).unwrap();
    let missing = directory.path().join("missing-identities");
    fs::write(
        &missing,
        format!(
            "{}\n",
            age::plugin::Identity::default_for_plugin("overmissingfixture").unwrap()
        ),
    )
    .unwrap();
    let recovered = access::unwrap_sources(&repo, &[missing], None, None, age::NoCallbacks, |_| {
        Ok("recovery".into())
    })
    .unwrap();
    assert_eq!(
        recovered.encrypt(b"recovery proof", 0).unwrap(),
        access::local_key(&repo)
            .unwrap()
            .encrypt(b"recovery proof", 0)
            .unwrap()
    );
}

#[rstest]
#[case("absent")]
#[case("pin")]
#[case("cancel")]
#[case("malformed")]
#[case("interactive")]
fn failed_hardware_attempts_leave_the_repository_locked_and_report_the_key(#[case] mode: &str) {
    let fixture = Fixture::new();
    fixture.enroll();
    fixture.run(&["lock"]);
    let output = fixture
        .command(&["unlock", "--identity", fixture.identity.to_str().unwrap()])
        .env("OVER_TEST_PLUGIN_MODE", mode)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("alice/default"), "{stderr}");
    assert!(!stderr.contains("private-error-sentinel"));
    assert!(!fixture.repo.join(".git/over-crypt/key").exists());
}

#[test]
fn failed_plugin_rewrapping_cannot_publish_a_partial_rotation() {
    let fixture = Fixture::new();
    fixture.enroll();
    fixture.software();
    let repo = git2::Repository::open(&fixture.repo).unwrap();
    let before = fs::read(access::metadata_path(&repo).unwrap()).unwrap();
    let proof = access::local_key(&repo)
        .unwrap()
        .encrypt(b"rotation proof", 0)
        .unwrap();
    let output = fixture
        .command(&["key", "rotate"])
        .env("OVER_TEST_PLUGIN_MODE", "absent")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read(access::metadata_path(&repo).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        access::local_key(&repo)
            .unwrap()
            .encrypt(b"rotation proof", 0)
            .unwrap(),
        proof
    );
}

#[rstest]
#[case("v1.2.0", "sys.exit(0)")]
#[case("v1.3.2", "sys.exit(1)")]
#[case("v1.3.2", "sys.stdout.write('x' * 17000)")]
#[case("v1.3.2", "sys.stdout.write('invalid ciphertext')")]
fn unavailable_or_invalid_native_pq_clients_cannot_publish_a_user(
    #[case] version: &str,
    #[case] action: &str,
) {
    let fixture = Fixture::new();
    let client = fixture.bin.join("age");
    fs::write(&client, format!("#!/usr/bin/env python3\nimport sys\nif '--version' in sys.argv:\n print('{version}')\nelse:\n {action}\n")).unwrap();
    fs::set_permissions(client, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        !fixture
            .command(&["user", "add", "alice", "--recipient", "age1pq1invalid"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!fixture.repo.join(".over/crypt/access.json").exists());
    assert!(!fixture.repo.join(".git/over-crypt/key").exists());
}

#[rstest]
#[case(false)]
#[case(true)]
#[ignore = "requires OVER_TEST_AGE_BIN pointing to standard age >= 1.3 with sibling age-keygen"]
fn standard_age_clients_can_rewrap_software_and_native_pq_envelopes(#[case] pq: bool) {
    let fixture = Fixture::new();
    let client =
        PathBuf::from(std::env::var_os("OVER_TEST_AGE_BIN").expect("set OVER_TEST_AGE_BIN"));
    std::os::unix::fs::symlink(&client, fixture.bin.join("age")).unwrap();
    let keygen = client.parent().unwrap().join("age-keygen");
    let identity = fixture.directory.path().join("external-identity");
    if pq {
        success(
            Command::new(&keygen)
                .args(["-pq", "-o"])
                .arg(&identity)
                .output()
                .unwrap(),
        );
        let recipient = success(
            Command::new(&keygen)
                .arg("-y")
                .arg(&identity)
                .output()
                .unwrap(),
        );
        let recipient = String::from_utf8(recipient.stdout).unwrap();
        fixture.run(&["user", "add", "alice", "--recipient", recipient.trim()]);
    } else {
        fixture.run(&[
            "user",
            "add",
            "alice",
            "--identity-output",
            identity.to_str().unwrap(),
        ]);
    }
    fixture.run(&["lock"]);
    fixture.run(&["unlock", "--identity", identity.to_str().unwrap()]);
    let repo = git2::Repository::open(&fixture.repo).unwrap();
    let mut registry = access::load(&repo).unwrap();
    let envelope = fixture.directory.path().join("envelope.age");
    fs::write(&envelope, &registry.users["alice"]["default"].envelope).unwrap();
    let key = success(
        Command::new(&client)
            .args(["-d", "-i"])
            .arg(&identity)
            .arg(envelope)
            .output()
            .unwrap(),
    )
    .stdout;
    assert_eq!(key.len(), 96);
    assert_eq!(key[..16], registry.repository);
    assert_eq!(key[16..32], registry.generation);
    let mut encrypt = Command::new(&client)
        .args([
            "-r",
            registry.users["alice"]["default"]
                .recipient
                .as_deref()
                .unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    encrypt.stdin.take().unwrap().write_all(&key).unwrap();
    registry
        .users
        .get_mut("alice")
        .unwrap()
        .get_mut("default")
        .unwrap()
        .envelope = success(encrypt.wait_with_output().unwrap()).stdout;
    fixture.run(&["lock"]);
    fs::write(
        access::metadata_path(&repo).unwrap(),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    fixture.run(&["unlock", "--identity", identity.to_str().unwrap()]);
}

#[test]
fn an_unused_encrypted_identity_file_does_not_prompt_before_software_unlock() {
    let fixture = Fixture::new();
    fixture.enroll();
    let software = fixture.software();
    let encrypted = fixture.directory.path().join("encrypted-identities");
    // The file need not be decrypted at all when the preceding supplied key succeeds.
    fs::write(
        &encrypted,
        b"age-encryption.org/v1\nunused encrypted identity",
    )
    .unwrap();
    fixture.run(&["lock"]);
    success(fixture.terminal(
        &[
            "unlock",
            "--identity",
            software.to_str().unwrap(),
            "--identity",
            encrypted.to_str().unwrap(),
        ],
        &[],
        "success",
    ));
}

#[rstest]
#[case(false)]
#[case(true)]
fn encrypted_passage_identity_files_unlock_or_allow_an_independent_source_to_recover(
    #[case] wrong_password: bool,
) {
    let fixture = Fixture::new();
    fixture.enroll();
    let software = fixture.software();
    let missing = age::plugin::Identity::default_for_plugin("overmissingfixture").unwrap();
    let contents = zeroize::Zeroizing::new(format!(
        "{missing}\n{}",
        fs::read_to_string(&software).unwrap()
    ));
    let mut password = age::scrypt::Recipient::new("pin-sentinel".into());
    password.set_work_factor(1);
    let mut ciphertext = Vec::new();
    let mut writer =
        age::Encryptor::with_recipients(std::iter::once(&password as &dyn age::Recipient))
            .unwrap()
            .wrap_output(&mut ciphertext)
            .unwrap();
    writer.write_all(contents.as_bytes()).unwrap();
    writer.finish().unwrap();
    let encrypted = fixture.directory.path().join("passage-identities.age");
    fs::write(&encrypted, ciphertext).unwrap();
    fixture.run(&["lock"]);
    let mut args = vec!["unlock", "--identity", encrypted.to_str().unwrap()];
    if wrong_password {
        args.extend(["--identity", software.to_str().unwrap()]);
    }
    success(fixture.terminal(
        &args,
        &[(
            "Age identity file passphrase",
            if wrong_password {
                "recovery-sentinel\n"
            } else {
                "pin-sentinel\n"
            },
        )],
        "success",
    ));
}

#[rstest]
#[case("")]
#[case("pin-sentinel")]
#[ignore = "requires ssh-keygen for standard SSH identity interoperability"]
fn standard_ssh_identities_can_unlock_imported_ssh_recipients(#[case] password: &str) {
    let fixture = Fixture::new();
    let identity = fixture.directory.path().join("ssh-identity");
    success(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", password, "-f"])
            .arg(&identity)
            .output()
            .unwrap(),
    );
    let recipient = fs::read_to_string(identity.with_extension("pub")).unwrap();
    fixture.run(&["user", "add", "alice", "--recipient", recipient.trim()]);
    let repo = git2::Repository::open(&fixture.repo).unwrap();
    let proof = access::local_key(&repo)
        .unwrap()
        .encrypt(b"SSH proof", 0)
        .unwrap();
    fixture.run(&["lock"]);
    let recovered = access::unwrap_sources(
        &repo,
        &[identity],
        None,
        None,
        TestCallbacks::default(),
        |_| unreachable!(),
    )
    .unwrap();
    assert_eq!(recovered.encrypt(b"SSH proof", 0).unwrap(), proof);
}

#[test]
fn generated_software_recipients_remain_standard_age_recipients() {
    let fixture = Fixture::new();
    fixture.enroll();
    let software = fixture.software();
    let identity: age::x25519::Identity = fs::read_to_string(software)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let registry = access::load(&git2::Repository::open(&fixture.repo).unwrap()).unwrap();
    let entry = &registry.users["alice"]["backup"];
    assert_eq!(
        entry.recipient.as_deref(),
        Some(identity.to_public().to_string().as_str())
    );
    let mut reader = age::Decryptor::new(&entry.envelope[..])
        .unwrap()
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes.len(), 96);
}

#[test]
fn plugin_pin_and_confirmation_requests_reach_the_terminal_without_echoing_secrets() {
    let fixture = Fixture::new();
    fixture.enroll();
    fixture.run(&["lock"]);
    success(fixture.terminal(
        &["unlock", "--identity", fixture.identity.to_str().unwrap()],
        &[
            ("Allow decryption?", "\x1b[A\n"),
            ("Device selector", "device\n"),
            ("Device PIN", "pin-sentinel\n"),
        ],
        "interactive",
    ));
}

#[rstest]
#[case("pin", "Device PIN", "recovery-sentinel\n", "request-secret")]
#[case("cancel", "Allow decryption?", "\n", "confirm")]
fn wrong_pin_and_denied_confirmation_allow_software_recovery(
    #[case] mode: &str,
    #[case] prompt: &str,
    #[case] answer: &str,
    #[case] request: &str,
) {
    let fixture = Fixture::new();
    fixture.enroll();
    let software = fixture.software_named("z-backup");
    fixture.run(&["lock"]);
    let before = fs::read_to_string(&fixture.log).unwrap().len();
    success(fixture.terminal(
        &[
            "unlock",
            "--identity",
            fixture.identity.to_str().unwrap(),
            "--identity",
            software.to_str().unwrap(),
        ],
        &[(prompt, answer)],
        mode,
    ));
    let log = fs::read_to_string(&fixture.log).unwrap();
    assert!(log[before..].contains(request));
    assert!(log[before..].contains("error"));
    assert!(fixture.repo.join(".git/over-crypt/key").exists());
}

#[rstest]
#[case(false)]
#[case(true)]
fn a_passphrase_key_remains_usable_after_a_missing_or_failed_plugin(#[case] missing: bool) {
    let fixture = Fixture::new();
    fixture.enroll();
    success(fixture.terminal(
        &[
            "user",
            "key",
            "add",
            "alice",
            "--name",
            "recovery",
            "--passphrase",
        ],
        &[("New Age key passphrase", "recovery-sentinel\n")],
        "success",
    ));
    fixture.run(&["lock"]);
    if missing {
        fs::remove_file(fixture.bin.join("age-plugin-overfixture")).unwrap();
    }
    success(fixture.terminal(
        &["unlock", "--identity", fixture.identity.to_str().unwrap()],
        &[("Age key passphrase: ", "recovery-sentinel\n")],
        "absent",
    ));
    assert!(fixture.repo.join(".git/over-crypt/key").exists());
}

#[test]
fn interactive_user_creation_publishes_its_first_generated_key_together() {
    let fixture = Fixture::new();
    let identity = fixture.directory.path().join("generated");
    success(fixture.terminal(
        &["user", "add", "alice"],
        &[
            ("First key type", "\n"),
            (
                "Private identity output path",
                &format!("{}\n", identity.display()),
            ),
        ],
        "success",
    ));
    let registry = access::load(&git2::Repository::open(&fixture.repo).unwrap()).unwrap();
    assert_eq!(registry.users["alice"].len(), 1);
    assert!(identity.exists());
}

#[test]
fn cancelled_or_failed_interactive_first_key_setup_cannot_persist_a_user() {
    for after_type in [false, true] {
        let fixture = Fixture::new();
        let steps = if after_type {
            vec![
                ("First key type", "\n"),
                ("Private identity output path", "\x03"),
            ]
        } else {
            vec![("First key type", "\x03")]
        };
        assert!(
            !fixture
                .terminal(&["user", "add", "alice"], &steps, "success")
                .status
                .success()
        );
        assert!(!fixture.repo.join(".over/crypt/access.json").exists());
    }
    let fixture = Fixture::new();
    assert!(
        !fixture
            .terminal(
                &["user", "add", "alice"],
                &[
                    ("First key type", "\x1b[B\n"),
                    ("Age recipient", &format!("{RECIPIENT}\n")),
                ],
                "absent"
            )
            .status
            .success()
    );
    assert!(!fixture.repo.join(".over/crypt/access.json").exists());
}
