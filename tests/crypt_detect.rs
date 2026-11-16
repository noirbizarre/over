//! Detection reads committed objects independently of checkout, local policy, and keys.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use git2::{Index, IndexEntry, IndexTime, Oid, Repository, Signature};
use over::crypt::access::{self, Registry};
use over::crypt::detect::{self, DiagnosticCode, Metadata};
use over::crypt::format::{self, RepositoryKey};
use pretty_assertions::assert_eq;
use rstest::rstest;

const ACCESS: &str = ".over/crypt/access.json";

fn registry(key: &RepositoryKey) -> Registry {
    let identity = age::x25519::Identity::generate();
    let access = access::envelope(key, Some(&identity.to_public().to_string()), None).unwrap();
    Registry {
        version: 1,
        repository: key.repository,
        generation: key.generation,
        users: BTreeMap::from([("alice".into(), BTreeMap::from([("laptop".into(), access)]))]),
    }
}

#[rstest]
#[case(".gitattributes")]
#[case(".over/crypt/access.json")]
#[case(".over/crypt/extra")]
fn committed_policy_cannot_protect_bootstrap_files(#[case] protected: &str) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let policy = format!("{protected} filter=over\n");
    commit(
        &repo,
        &[
            (
                ACCESS,
                &serde_json::to_vec(&registry(&key)).unwrap(),
                0o100644,
            ),
            (".gitattributes", policy.as_bytes(), 0o100644),
            (".over/crypt/extra", b"public bootstrap", 0o100644),
        ],
    );
    let result = detect::inspect(&repo).unwrap();
    assert!(result.diagnostics.iter().any(|diagnostic| diagnostic.code
        == DiagnosticCode::ProtectedBootstrap
        && Path::new(&diagnostic.location) == Path::new(protected)));
    assert!(result.validate_checkout().is_err());
}

#[rstest]
#[case(false)]
#[case(true)]
fn protected_symlinks_and_gitlinks_are_rejected(#[case] gitlink: bool) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let oid = commit(
        &repo,
        &[
            (
                ACCESS,
                &serde_json::to_vec(&registry(&key)).unwrap(),
                0o100644,
            ),
            (".gitattributes", b"special filter=over\n", 0o100644),
            ("special", b"destination", 0o120000),
        ],
    );
    let commit = repo.find_commit(oid).unwrap();
    let mut builder = repo.treebuilder(Some(&commit.tree().unwrap())).unwrap();
    if gitlink {
        builder.insert("special", oid, 0o160000).unwrap();
    }
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let result = detect::inspect_tree(&repo, &tree).unwrap();
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::ProtectedNonregularPath)
    );
    assert!(result.validate_checkout().is_err());
}

#[rstest]
#[case("additive fields", Metadata::Supported)]
#[case("user name", Metadata::Malformed)]
#[case("method name", Metadata::Malformed)]
#[case("envelope size", Metadata::Malformed)]
fn registry_version_one_enforces_its_documented_compatibility_contract(
    #[case] change: &str,
    #[case] expected: Metadata,
) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let mut metadata = serde_json::to_value(registry(&key)).unwrap();
    match change {
        "additive fields" => {
            metadata["future"] = serde_json::json!({"ignored": true});
            metadata["users"]["alice"]["laptop"]["future"] = serde_json::json!(true);
        }
        "user name" => {
            let users = metadata["users"].as_object_mut().unwrap();
            let value = users.remove("alice").unwrap();
            users.insert("invalid user".into(), value);
        }
        "method name" => {
            let keys = metadata["users"]["alice"].as_object_mut().unwrap();
            let value = keys.remove("laptop").unwrap();
            keys.insert("invalid method".into(), value);
        }
        "envelope size" => {
            metadata["users"]["alice"]["laptop"]["envelope"] =
                serde_json::json!(vec![0u8; 16 * 1024 + 1])
        }
        _ => unreachable!(),
    }
    commit(
        &repo,
        &[(ACCESS, &serde_json::to_vec(&metadata).unwrap(), 0o100644)],
    );
    let result = detect::inspect(&repo).unwrap();
    assert_eq!(result.metadata, expected);
    assert_eq!(result.can_offer_unlock(), change == "additive fields");
}

#[test]
fn batch_inspection_retains_only_headers_while_preserving_subsequent_blob_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let ciphertext = key.encrypt(&vec![42; 2 * 1024 * 1024], 0).unwrap();
    commit(
        &repo,
        &[
            (
                ACCESS,
                &serde_json::to_vec(&registry(&key)).unwrap(),
                0o100644,
            ),
            (".gitattributes", b"protected* filter=over\n", 0o100644),
            ("ordinary", &vec![b'x'; 2 * 1024 * 1024], 0o100644),
            ("protected-one", &ciphertext, 0o100644),
            ("protected-two", &ciphertext, 0o100644),
        ],
    );
    let result = detect::inspect(&repo).unwrap();
    assert!(result.diagnostics.is_empty(), "{result:?}");
    assert_eq!(
        result.ciphertext_paths,
        vec![
            PathBuf::from("protected-one"),
            PathBuf::from("protected-two")
        ]
    );
}

fn commit(repo: &Repository, files: &[(&str, &[u8], u32)]) -> Oid {
    // Build raw objects without invoking clean filters or relying on filesystem materialization.
    let mut index = Index::new().unwrap();
    for (path, bytes, mode) in files {
        let oid = repo.blob(bytes).unwrap();
        index
            .add(&IndexEntry {
                ctime: IndexTime::new(0, 0),
                mtime: IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: *mode,
                uid: 0,
                gid: 0,
                file_size: bytes.len() as u32,
                id: oid,
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            })
            .unwrap();
    }
    let tree = repo.find_tree(index.write_tree_to(repo).unwrap()).unwrap();
    let signature = Signature::now("Detection test", "detection@example.invalid").unwrap();
    let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "detection fixture",
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry.path().strip_prefix(root).unwrap().to_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn a_no_checkout_clone_is_detected_without_keys_filters_or_source_mutation() {
    let directory = tempfile::tempdir().unwrap();
    // The separator and quote exercise Git alternate-directory quoting rather than shell quoting.
    #[cfg(not(windows))]
    let source = directory.path().join("source:with\"quote.git");
    #[cfg(windows)]
    let source = directory.path().join("source with spaces.git");
    let repo = Repository::init_bare(&source).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let metadata = serde_json::to_vec(&registry(&key)).unwrap();
    let ciphertext = key.encrypt(b"private sentinel", 0).unwrap();
    commit(
        &repo,
        &[
            (ACCESS, &metadata, 0o100644),
            (".gitattributes", b"token filter=over\n", 0o100644),
            ("token", &ciphertext, 0o100644),
        ],
    );
    let clone = directory.path().join("clone");
    let output = Command::new("git")
        .args(["clone", "--no-checkout", "--no-local"])
        .arg(&source)
        .arg(&clone)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repo = Repository::open(&clone).unwrap();
    let sentinel = directory.path().join("driver-ran");
    let mut config = repo.config().unwrap();
    for name in [
        "filter.over.smudge",
        "filter.over.clean",
        "diff.over.textconv",
    ] {
        config
            .set_str(name, &format!("touch {}", sentinel.display()))
            .unwrap();
    }
    let before = snapshot(&clone);
    let detected = detect::inspect(&repo).unwrap();
    assert!(detected.is_crypt());
    assert!(detected.can_offer_unlock());
    assert_eq!(detected.protected_paths, vec![PathBuf::from("token")]);
    assert_eq!(detected.ciphertext_paths, vec![PathBuf::from("token")]);
    assert!(
        detected.diagnostics.is_empty(),
        "{:?}",
        detected.diagnostics
    );
    assert_eq!(detected, detect::inspect(&repo).unwrap());
    assert_eq!(snapshot(&clone), before);
    assert!(!clone.join("token").exists());
    assert!(!repo.path().join("over-crypt").exists());
    assert!(!sentinel.exists());
    assert!(!format!("{detected:?}").contains("private sentinel"));
    // Bare inspection reads the same tree without requiring a workdir at all.
    assert_eq!(
        detect::inspect(&Repository::open_bare(&source).unwrap()).unwrap(),
        detected
    );
}

#[test]
fn committed_nested_attributes_and_macros_ignore_local_policy_and_index() {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let metadata = serde_json::to_vec(&registry(&key)).unwrap();
    let ciphertext = key.encrypt(b"nested private sentinel", 0).unwrap();
    commit(&repo, &[
        (ACCESS, &metadata, 0o100644),
        (".gitattributes", b"[attr]secret filter=over diff=over -text\nprotected/** secret\n\"quoted name\" secret\n", 0o100644),
        ("protected/.gitattributes", b".gitattributes -filter\npublic -filter\nreset !filter\n", 0o100644),
        ("protected/nested/token", &ciphertext, 0o100644),
        ("protected/public", b"public", 0o100644),
        ("protected/reset", b"reset", 0o100644),
        ("quoted name", &ciphertext, 0o100644),
    ]);
    fs::write(directory.path().join(".gitattributes"), "* -filter\n").unwrap();
    fs::create_dir_all(repo.path().join("info")).unwrap();
    fs::write(repo.path().join("info/attributes"), "* -filter\n").unwrap();
    let global = directory.path().join("global-attributes");
    fs::write(&global, "* -filter\n[attr]secret -filter\n").unwrap();
    repo.config()
        .unwrap()
        .set_str("core.attributesFile", global.to_str().unwrap())
        .unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(".gitattributes")).unwrap();
    index.write().unwrap();
    let before = snapshot(directory.path());
    let detected = detect::inspect(&repo).unwrap();
    assert_eq!(
        detected.protected_paths,
        vec![
            PathBuf::from("protected/nested/token"),
            PathBuf::from("quoted name")
        ]
    );
    assert!(
        detected.diagnostics.is_empty(),
        "{:?}",
        detected.diagnostics
    );
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn inherited_git_configuration_cannot_invent_committed_protection() {
    const CHILD_REPO: &str = "OVER_DETECTION_CHILD_REPOSITORY";
    if let Some(path) = std::env::var_os(CHILD_REPO) {
        let repo = Repository::open_bare(path).unwrap();
        let result = detect::inspect(&repo).unwrap();
        assert!(!result.is_crypt(), "{result:?}");
        assert!(result.diagnostics.is_empty());
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path().join("source.git")).unwrap();
    commit(
        &repo,
        &[
            (".gitattributes", b"token secret\n", 0o100644),
            ("token", b"not encrypted", 0o100644),
        ],
    );
    fs::create_dir_all(repo.path().join("info")).unwrap();
    fs::write(repo.path().join("info/attributes"), "* filter=over\n").unwrap();
    let attributes = directory.path().join("attributes");
    fs::write(&attributes, "[attr]secret filter=over\n* filter=over\n").unwrap();
    let config = directory.path().join("global-config");
    fs::write(
        &config,
        format!("[core]\nattributesFile = {}\n", attributes.display()),
    )
    .unwrap();
    // Child-process environment avoids process-global mutation racing other Rust tests.
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "inherited_git_configuration_cannot_invent_committed_protection",
            "--nocapture",
        ])
        .env(CHILD_REPO, repo.path())
        .env("GIT_CONFIG_SYSTEM", &config)
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.attributesFile")
        .env("GIT_CONFIG_VALUE_0", &attributes)
        .env("GIT_ATTR_SOURCE", "nonexistent-ref")
        .env("GIT_INDEX_FILE", directory.path().join("nonexistent-index"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn committed_policy_detection_supports_git_without_check_attr_source() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD_REPO: &str = "OVER_DETECTION_OLD_GIT_REPOSITORY";
    if let Some(path) = std::env::var_os(CHILD_REPO) {
        let repo = Repository::open_bare(path).unwrap();
        let result = detect::inspect(&repo).unwrap();
        assert_eq!(result.protected_paths, vec![PathBuf::from("token")]);
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path().join("source.git")).unwrap();
    commit(
        &repo,
        &[
            (".gitattributes", b"token filter=over\n", 0o100644),
            ("token", b"partial encryption setup", 0o100644),
        ],
    );
    let bin = directory.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let shim = bin.join("git");
    // Reproduce Git 2.39's unsupported option in a child, without changing global PATH.
    fs::write(&shim, "#!/bin/sh\nfor arg in \"$@\"; do\ncase \"$arg\" in --source|--source=*) exit 129 ;; esac\ndone\nexec \"$OVER_DETECTION_REAL_GIT\" \"$@\"\n").unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "committed_policy_detection_supports_git_without_check_attr_source",
            "--nocapture",
        ])
        .env(CHILD_REPO, repo.path())
        .env("OVER_DETECTION_REAL_GIT", which::which("git").unwrap())
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[rstest]
#[case(b"".as_slice(), Metadata::Malformed)]
#[case(b"not json".as_slice(), Metadata::Malformed)]
#[case(br#"{"version":2,"future":"different format"}"#.as_slice(), Metadata::Unsupported(2))]
#[case(br#"{"version":1}"#.as_slice(), Metadata::Malformed)]
fn invalid_metadata_has_stable_diagnostics_without_checkout(
    #[case] bytes: &[u8],
    #[case] expected: Metadata,
) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    commit(&repo, &[(ACCESS, bytes, 0o100644)]);
    let before = snapshot(directory.path());
    let result = detect::inspect(&repo).unwrap();
    assert_eq!(result.metadata, expected);
    assert!(result.is_crypt());
    assert!(!result.can_offer_unlock());
    assert!(result.validate_checkout().is_err());
    assert_eq!(result, detect::inspect(&repo).unwrap());
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn empty_and_noncrypt_repositories_need_no_crypt_setup() {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let empty = detect::inspect(&repo).unwrap();
    assert_eq!(empty.tree, None);
    assert!(!empty.is_crypt());
    // Dormant policy is not effective protection, and a short binary prefix is not ciphertext.
    commit(
        &repo,
        &[
            (".gitattributes", b"future/** filter=over\n", 0o100644),
            ("binary", b"\0OV", 0o100644),
        ],
    );
    let result = detect::inspect(&repo).unwrap();
    assert!(!result.is_crypt());
    assert!(result.diagnostics.is_empty());
    result.validate_checkout().unwrap();
}

#[test]
fn configured_encryption_is_detected_even_without_protected_files() {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let metadata = serde_json::to_vec(&registry(&key)).unwrap();
    commit(&repo, &[(ACCESS, &metadata, 0o100644)]);
    let result = detect::inspect(&repo).unwrap();
    assert!(result.is_crypt());
    assert!(result.can_offer_unlock());
    assert!(result.protected_paths.is_empty());
    assert!(result.diagnostics.is_empty());
}

#[test]
fn packed_shared_objects_and_linked_worktrees_use_the_committed_tree() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let repo = Repository::init(&source).unwrap();
    let key = RepositoryKey::generate().unwrap();
    commit(
        &repo,
        &[(
            ACCESS,
            &serde_json::to_vec(&registry(&key)).unwrap(),
            0o100644,
        )],
    );
    let expected = detect::inspect(&repo).unwrap();
    for args in [
        vec!["repack", "-ad"],
        vec!["worktree", "add", "--detach", "--no-checkout", "../linked"],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let shared = directory.path().join("shared.git");
    let output = Command::new("git")
        .args(["clone", "--bare", "--shared"])
        .arg(&source)
        .arg(&shared)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for path in [shared, directory.path().join("linked")] {
        let repo = Repository::open(&path).unwrap();
        let before = snapshot(&path);
        assert_eq!(detect::inspect(&repo).unwrap(), expected);
        assert_eq!(snapshot(&path), before);
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_git_names_do_not_turn_an_ordinary_repository_into_a_detection_error() {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let mut index = Index::new().unwrap();
    let oid = repo.blob(b"ordinary content").unwrap();
    index
        .add(&IndexEntry {
            ctime: IndexTime::new(0, 0),
            mtime: IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            file_size: 16,
            id: oid,
            flags: 0,
            flags_extended: 0,
            path: b"file-\xff".to_vec(),
        })
        .unwrap();
    let tree = repo.find_tree(index.write_tree_to(&repo).unwrap()).unwrap();
    let result = detect::inspect_tree(&repo, &tree).unwrap();
    assert!(!result.is_crypt());
    assert!(result.diagnostics.is_empty());
}

#[rstest]
#[case(false)]
#[case(true)]
fn missing_access_metadata_does_not_hide_policy_or_ciphertext(#[case] policy: bool) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let ciphertext = RepositoryKey::generate()
        .unwrap()
        .encrypt(b"sentinel", 0)
        .unwrap();
    let content = if policy {
        b"plaintext".as_slice()
    } else {
        ciphertext.as_slice()
    };
    let mut files = vec![("token", content, 0o100644)];
    if policy {
        files.push((".gitattributes", b"token filter=over\n", 0o100644));
    }
    commit(&repo, &files);
    let result = detect::inspect(&repo).unwrap();
    assert!(result.is_crypt());
    assert!(!result.can_offer_unlock());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::MissingMetadata)
    );
    assert!(result.validate_checkout().is_err());
}

#[test]
fn a_damaged_alternative_envelope_does_not_disable_a_usable_method() {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let mut registry = registry(&key);
    let mut damaged = registry.users["alice"]["laptop"].clone();
    damaged.envelope.clear();
    registry
        .users
        .get_mut("alice")
        .unwrap()
        .insert("damaged".into(), damaged);
    let metadata = serde_json::to_vec(&registry).unwrap();
    commit(&repo, &[(ACCESS, &metadata, 0o100644)]);
    let result = detect::inspect(&repo).unwrap();
    assert_eq!(result.usable_access_methods, 1);
    assert!(result.can_offer_unlock());
    assert_eq!(
        result.diagnostics[0].code,
        DiagnosticCode::InvalidAccessMethod
    );
    result.validate_checkout().unwrap();
}

#[rstest]
#[case("users")]
#[case("keys")]
#[case("envelope")]
#[case("payload")]
#[case("recipient")]
fn incomplete_access_cannot_offer_unlock(#[case] missing: &str) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let mut registry = registry(&key);
    match missing {
        "users" => registry.users.clear(),
        "keys" => registry.users.get_mut("alice").unwrap().clear(),
        "envelope" => registry
            .users
            .get_mut("alice")
            .unwrap()
            .get_mut("laptop")
            .unwrap()
            .envelope
            .clear(),
        "payload" => {
            let key = registry
                .users
                .get_mut("alice")
                .unwrap()
                .get_mut("laptop")
                .unwrap();
            key.envelope.truncate(key.envelope.len() - 112);
        }
        "recipient" => {
            registry
                .users
                .get_mut("alice")
                .unwrap()
                .get_mut("laptop")
                .unwrap()
                .recipient = Some("age1pq1invalid".into())
        }
        _ => unreachable!(),
    }
    commit(
        &repo,
        &[(ACCESS, &serde_json::to_vec(&registry).unwrap(), 0o100644)],
    );
    let result = detect::inspect(&repo).unwrap();
    assert!(!result.can_offer_unlock());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::NoAccessMethod)
    );
    assert!(result.validate_checkout().is_err());
}

#[rstest]
#[case("secrets")]
#[case("secrets.yaml")]
fn passphrase_access_and_native_secret_ciphertext_need_no_credential(#[case] filename: &str) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let mut registry = registry(&key);
    registry.users.get_mut("alice").unwrap().insert(
        "laptop".into(),
        access::envelope(&key, None, Some("hidden passphrase sentinel".into())).unwrap(),
    );
    commit(
        &repo,
        &[
            (ACCESS, &serde_json::to_vec(&registry).unwrap(), 0o100644),
            (
                ".over/.gitattributes",
                format!("{filename} filter=over\n").as_bytes(),
                0o100644,
            ),
            (
                &format!(".over/{filename}"),
                &key.encrypt(b"native secret sentinel", 1).unwrap(),
                0o100644,
            ),
        ],
    );
    let result = detect::inspect(&repo).unwrap();
    assert!(result.can_offer_unlock());
    assert!(result.diagnostics.is_empty());
    assert_eq!(
        result.ciphertext_paths,
        vec![PathBuf::from(format!(".over/{filename}"))]
    );
    assert!(!format!("{result:?}").contains("sentinel"));
}

#[rstest]
#[case("symlink")]
#[case("tree")]
#[case("oversized")]
fn metadata_must_be_a_bounded_regular_blob(#[case] kind: &str) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let oversized;
    let files = match kind {
        "symlink" => vec![(ACCESS, b"elsewhere".as_slice(), 0o120000)],
        "tree" => vec![(
            ".over/crypt/access.json/child",
            b"child".as_slice(),
            0o100644,
        )],
        "oversized" => {
            oversized = vec![b' '; 16 * 1024 * 1024 + 1];
            vec![(ACCESS, oversized.as_slice(), 0o100644)]
        }
        _ => unreachable!(),
    };
    commit(&repo, &files);
    let result = detect::inspect(&repo).unwrap();
    assert_eq!(result.metadata, Metadata::Malformed);
    assert_eq!(result.diagnostics[0].code, DiagnosticCode::InvalidMetadata);
}

#[rstest]
#[case("truncated", DiagnosticCode::InvalidCiphertext)]
#[case("version", DiagnosticCode::InvalidCiphertext)]
#[case("domain", DiagnosticCode::InvalidCiphertext)]
#[case("generation", DiagnosticCode::GenerationMismatch)]
fn invalid_committed_ciphertext_is_reported_without_authentication(
    #[case] fault: &str,
    #[case] expected: DiagnosticCode,
) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_bare(directory.path()).unwrap();
    let key = RepositoryKey::generate().unwrap();
    let mut ciphertext = key.encrypt(b"private", 0).unwrap();
    match fault {
        "truncated" => ciphertext.truncate(format::MAGIC.len()),
        "version" => ciphertext[8] = 2,
        "domain" => ciphertext[9] = 2,
        "generation" => ciphertext[10] ^= 1,
        _ => unreachable!(),
    }
    commit(
        &repo,
        &[
            (
                ACCESS,
                &serde_json::to_vec(&registry(&key)).unwrap(),
                0o100644,
            ),
            (".gitattributes", b"token filter=over\n", 0o100644),
            ("token", &ciphertext, 0o100644),
        ],
    );
    let result = detect::inspect(&repo).unwrap();
    assert_eq!(result.diagnostics[0].code, expected);
    assert!(result.validate_checkout().is_err());
}
