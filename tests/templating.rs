use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use rstest::rstest;
use tempfile::TempDir;

#[rstest]
#[case(false)]
#[case(true)]
fn templates_consume_recursively_merged_inherited_and_dependency_data(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    fs::write(
        fixture.source.join("over.yaml"),
        "default_overlay: app\ndata: {config: {root: inherited}}\n",
    )
    .unwrap();
    fs::create_dir_all(fixture.source.join("base")).unwrap();
    fs::write(
        fixture.source.join("base/over.yaml"),
        "data: {config: {dependency: shared}}\n",
    )
    .unwrap();
    let descriptor = fixture.source.join("app/over.yaml");
    let configuration = fs::read_to_string(&descriptor).unwrap().replace(
        "data: {hostname: first}",
        "uses: [base]\ndata: {hostname: first, config: {selected: local}}",
    );
    fs::write(descriptor, configuration).unwrap();
    let template = "{{ data.config.root }}/{{ data.config.dependency }}/{{ data.config.selected }}";
    fs::write(fixture.source.join("app/config.jinja"), template).unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        "inherited/shared/local"
    );
    assert_eq!(
        fs::read_to_string(fixture.source.join("app/config.jinja")).unwrap(),
        template
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn explicitly_composed_roots_render_from_one_recursively_merged_context(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    fs::write(fixture.source.join("over.yaml"), format!("mode: {}\ndata: {{config: {{inherited: root}}}}\nrules:\n  - {{path: '**/*.jinja', template: true, strip_extension: true}}\n", if checkout { "checkout" } else { "symlink" })).unwrap();
    for (name, data) in [("left", "left: l"), ("right", "right: r")] {
        let root = fixture.source.join(name);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("over.yaml"),
            format!("data: {{config: {{{data}}}}}\n"),
        )
        .unwrap();
        fs::write(
            root.join(format!("{name}.jinja")),
            "{{ data.config.inherited }} {{ data.config.left }} {{ data.config.right }}",
        )
        .unwrap();
    }
    commit_all(&fixture.repo());
    for _ in 0..2 {
        fixture
            .command("apply")
            .args(["left", "right", "--no-prompt", "--target"])
            .arg(&fixture.target)
            .assert()
            .success();
    }
    for name in ["left", "right"] {
        assert_eq!(
            fs::read_to_string(fixture.target.join(name)).unwrap(),
            "root l r"
        );
    }
    assert!(!fixture.target.join("config").exists());
}

struct Fixture {
    temp: TempDir,
    source: PathBuf,
    target: PathBuf,
}

#[rstest]
#[case(false)]
#[case(true)]
fn reapply_keeps_unsynchronized_historical_checkout_leaves_excluded(#[case] local_edit: bool) {
    let fixture = Fixture::new(true);
    fs::write(fixture.source.join("app/over.yaml"), "mode: checkout\n").unwrap();
    commit_all(&fixture.repo());
    let host = git2::Repository::init(&fixture.target).unwrap();
    fixture.apply();
    assert!(host.is_path_ignored(Path::new("plain")).unwrap());
    if local_edit {
        fs::write(fixture.target.join("plain"), "local\n").unwrap();
    }
    fs::remove_file(fixture.source.join("app/plain")).unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    assert!(fixture.target.join("plain").exists());
    assert!(host.is_path_ignored(Path::new("plain")).unwrap());
    fs::write(fixture.target.join("neighbor"), "host-owned\n").unwrap();
    assert!(!host.is_path_ignored(Path::new("neighbor")).unwrap());
}

#[cfg(unix)]
#[rstest]
#[case(false, false)]
#[case(false, true)]
#[case(true, false)]
#[case(true, true)]
fn checkout_sync_refuses_read_only_leaves_before_other_updates(
    #[case] generated: bool,
    #[case] deletion: bool,
) {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(true);
    if !generated {
        fs::write(fixture.source.join("app/over.yaml"), "mode: checkout\n").unwrap();
    }
    fs::write(fixture.source.join("app/aaa"), "before\n").unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    fs::set_permissions(
        fixture.target.join("plain"),
        fs::Permissions::from_mode(0o400),
    )
    .unwrap();
    fs::write(fixture.source.join("app/aaa"), "after\n").unwrap();
    if deletion {
        fs::remove_file(fixture.source.join("app/plain")).unwrap();
    } else {
        fs::write(fixture.source.join("app/plain"), "changed\n").unwrap();
    }
    commit_all(&fixture.repo());
    fixture.sync().arg("--pull-only").assert().failure();
    assert_eq!(
        fs::read(fixture.target.join("plain")).unwrap(),
        b"ordinary\n"
    );
    assert_eq!(fs::read(fixture.target.join("aaa")).unwrap(), b"before\n");
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn checkout_sync_retains_private_access_when_a_file_becomes_executable(#[case] generated: bool) {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(true);
    if !generated {
        fs::write(fixture.source.join("app/over.yaml"), "mode: checkout\n").unwrap();
    }
    commit_all(&fixture.repo());
    fixture.apply();
    fs::set_permissions(
        fixture.target.join("plain"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::set_permissions(
        fixture.source.join("app/plain"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_all(&fixture.repo());
    fixture.sync().arg("--pull-only").assert().success();
    assert_eq!(
        fs::metadata(fixture.target.join("plain"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[cfg(target_os = "linux")]
#[rstest]
#[case(false, false)]
#[case(false, true)]
#[case(true, false)]
#[case(true, true)]
fn checkout_sync_preserves_native_metadata_without_editing_other_hard_links(
    #[case] generated: bool,
    #[case] shared: bool,
) {
    use std::os::unix::fs::MetadataExt;

    let fixture = Fixture::new(true);
    if !generated {
        fs::write(fixture.source.join("app/over.yaml"), "mode: checkout\n").unwrap();
    }
    commit_all(&fixture.repo());
    fixture.apply();
    let target = fixture.target.join("plain");
    xattr::set(&target, "user.over-regression", b"retained").unwrap();
    let inode = fs::metadata(&target).unwrap().ino();
    let neighbor = fixture.temp.path().join("neighbor");
    if shared {
        fs::hard_link(&target, &neighbor).unwrap();
    }
    fs::write(fixture.source.join("app/plain"), "changed\n").unwrap();
    commit_all(&fixture.repo());
    fixture.sync().arg("--pull-only").assert().success();
    assert_eq!(
        xattr::get(&target, "user.over-regression")
            .unwrap()
            .unwrap(),
        b"retained"
    );
    assert_eq!(fs::read(&target).unwrap(), b"changed\n");
    if shared {
        assert_eq!(fs::read(neighbor).unwrap(), b"ordinary\n");
    } else {
        assert_eq!(fs::metadata(target).unwrap().ino(), inode);
    }
}

impl Fixture {
    fn git(&self, arguments: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(&self.source)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn protect(&self) {
        self.git(&["config", "user.name", "Snapshot tests"]);
        self.git(&["config", "user.email", "snapshots@example.invalid"]);
        self.command("user")
            .current_dir(&self.source)
            .args(["add", "owner", "--identity-output"])
            .arg(self.temp.path().join("identity.age"))
            .assert()
            .success();
        // Use new content: the ordinary fixture's public initial commit is intentionally retained.
        fs::write(self.source.join("app/config.jinja"), "host={{ data.hostname }}\nfirst=protected-snapshot-sentinel\nmiddle=stable\nlast=base\n").unwrap();
        fs::write(
            self.source.join("app/plain"),
            "protected-ordinary-sentinel\n",
        )
        .unwrap();
        self.command("crypt")
            .current_dir(&self.source)
            .args(["add", "app/config.jinja", "app/plain"])
            .assert()
            .success();
        self.git(&["add", "--all"]);
        self.git(&["commit", "-qm", "protect snapshot sources"]);
    }

    fn assert_no_protected_plaintext(&self) {
        let repo = self.repo();
        let odb = repo.odb().unwrap();
        odb.foreach(&mut |id: &git2::Oid| {
            let object = odb.read(*id).unwrap();
            if object.kind() == git2::ObjectType::Blob {
                for sentinel in [
                    "protected-snapshot-sentinel",
                    "protected-ordinary-sentinel",
                    "protected-reverse-sentinel",
                ] {
                    assert!(
                        !object
                            .data()
                            .windows(sentinel.len())
                            .any(|bytes| bytes == sentinel.as_bytes()),
                        "persistent plaintext blob {id}"
                    );
                }
            }
            true
        })
        .unwrap();
    }

    fn new(checkout: bool) -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::create_dir_all(source.join("app")).unwrap();
        fs::write(source.join("over.yaml"), "default_overlay: app\n").unwrap();
        fs::write(source.join("app/over.yaml"), format!("mode: {}\ndata: {{hostname: first}}\nrules:\n  - {{path: '**/*.jinja', template: true, strip_extension: true}}\n", if checkout { "checkout" } else { "symlink" })).unwrap();
        fs::write(
            source.join("app/config.jinja"),
            "host={{ data.hostname }}\nfirst=base\nmiddle=stable\nlast=base\n",
        )
        .unwrap();
        fs::write(source.join("app/plain"), "ordinary\n").unwrap();
        let repo = git2::Repository::init(&source).unwrap();
        commit_all(&repo);
        Self {
            temp,
            source,
            target,
        }
    }

    fn command(&self, verb: &str) -> Command {
        self.command_at(&self.source, verb)
    }

    fn command_at(&self, source: &Path, verb: &str) -> Command {
        let mut command = Command::cargo_bin("over").unwrap();
        command
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .arg("--home")
            .arg(source)
            .arg(verb);
        command
    }

    fn apply(&self) {
        self.command("apply")
            .args(["app", "--no-prompt", "--target"])
            .arg(&self.target)
            .assert()
            .success();
    }

    fn sync(&self) -> Command {
        let mut command = self.command("sync");
        command
            .args(["app", "--no-prompt", "--target"])
            .arg(&self.target);
        command
    }

    fn commit(&self) -> Command {
        let mut command = self.command("commit");
        command
            .args(["--overlay", "app", "--no-prompt", "--target"])
            .arg(&self.target);
        command
    }

    fn stage(&self, name: &str) {
        self.command("add")
            .args(["--overlay", "app", "--target"])
            .arg(&self.target)
            .arg(self.target.join(name))
            .assert()
            .success();
    }

    fn restore(&self, name: &str, options: &[&str]) {
        self.command("restore")
            .args(["--overlay", "app", "--target"])
            .arg(&self.target)
            .args(options)
            .arg(self.target.join(name))
            .assert()
            .success();
    }

    fn repo(&self) -> git2::Repository {
        git2::Repository::open(&self.source).unwrap()
    }

    fn snapshot(&self) -> git2::Oid {
        let repo = self.repo();
        let ids: Vec<_> = repo
            .references_glob("refs/over/generated/*")
            .unwrap()
            .map(|reference| reference.unwrap().target().unwrap())
            .collect();
        assert_eq!(ids.len(), 1);
        ids[0]
    }
}

#[test]
fn generated_snapshots_protect_templates_and_ordinary_siblings_through_lock_and_unlock() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    assert_eq!(
        fs::read_to_string(fixture.target.join("plain")).unwrap(),
        "protected-ordinary-sentinel\n"
    );
    let repo = fixture.repo();
    let snapshot = fixture.snapshot();
    let tree = repo.find_commit(snapshot).unwrap().tree().unwrap();
    for path in ["config", "plain"] {
        let blob = repo
            .find_blob(tree.get_path(Path::new(path)).unwrap().id())
            .unwrap();
        assert!(over::crypt::format::recognized(blob.content()));
    }
    fixture.assert_no_protected_plaintext();
    fixture.sync().assert().success();
    assert_eq!(
        fixture.snapshot(),
        snapshot,
        "deterministic protected publication must be a no-op"
    );
    fixture.command("lock").assert().success();
    assert!(!fixture.source.join(".git/over-crypt/key").exists());
    fixture.sync().assert().failure();
    fixture
        .command("unlock")
        .args(["--identity"])
        .arg(fixture.temp.path().join("identity.age"))
        .assert()
        .success();
    fixture.sync().assert().success();
    assert_eq!(fixture.snapshot(), snapshot);
    fixture.assert_no_protected_plaintext();
}

#[test]
fn protected_reconciliation_merges_plaintext_and_publishes_only_ciphertext() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let target = fixture.target.join("config");
    let edited = fs::read_to_string(&target)
        .unwrap()
        .replace("last=base", "last=local");
    fs::write(&target, edited).unwrap();
    let source = fixture.source.join("app/config.jinja");
    fs::write(
        &source,
        fs::read_to_string(&source).unwrap().replace(
            "first=protected-snapshot-sentinel",
            "first=protected-snapshot-sentinel-upstream",
        ),
    )
    .unwrap();
    fixture.git(&["add", "app/config.jinja"]);
    fixture.git(&["commit", "-qm", "update protected source"]);
    fixture.sync().assert().success();
    let merged = fs::read_to_string(&target).unwrap();
    assert!(merged.contains("first=protected-snapshot-sentinel-upstream"));
    assert!(merged.contains("last=local"));
    fixture.assert_no_protected_plaintext();
    fixture.restore("config", &[]);
    assert!(!fs::read_to_string(&target).unwrap().contains("last=local"));
    fixture.assert_no_protected_plaintext();
}

#[test]
fn reverse_patches_keep_protected_sources_and_both_generated_journal_trees_encrypted() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let target = fixture.target.join("config");
    fs::write(
        &target,
        fs::read_to_string(&target)
            .unwrap()
            .replace("last=base", "last=protected-reverse-sentinel"),
    )
    .unwrap();
    fixture
        .commit()
        .args(["-m", "reverse protected edit"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(fixture.source.join("app/config.jinja"))
            .unwrap()
            .contains("last=protected-reverse-sentinel")
    );
    let repo = fixture.repo();
    let source_tree = repo.head().unwrap().peel_to_tree().unwrap();
    let blob = repo
        .find_blob(
            source_tree
                .get_path(Path::new("app/config.jinja"))
                .unwrap()
                .id(),
        )
        .unwrap();
    assert!(over::crypt::format::recognized(blob.content()));
    fixture.assert_no_protected_plaintext();
    let head = repo.head().unwrap().target().unwrap();
    fixture
        .commit()
        .args(["-m", "do not replay"])
        .assert()
        .success();
    assert_eq!(fixture.repo().head().unwrap().target().unwrap(), head);
    fixture.sync().assert().success();
    fixture.command("lock").assert().success();
    fixture
        .command("unlock")
        .arg("--identity")
        .arg(fixture.temp.path().join("identity.age"))
        .assert()
        .success();
    fixture.assert_no_protected_plaintext();
}

#[test]
fn key_rotation_refuses_to_strand_protected_snapshot_bases_before_publishing_anything() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let key = fs::read(fixture.source.join(".git/over-crypt/key")).unwrap();
    let registry = fs::read(fixture.source.join(".over/crypt/access.json")).unwrap();
    let index = fs::read(fixture.source.join(".git/index")).unwrap();
    let snapshot = fixture.snapshot();
    fixture
        .command("key")
        .args(["rotate"])
        .assert()
        .failure()
        .stderr(contains("would strand protected generated snapshots"));
    assert_eq!(
        fs::read(fixture.source.join(".git/over-crypt/key")).unwrap(),
        key
    );
    assert_eq!(
        fs::read(fixture.source.join(".over/crypt/access.json")).unwrap(),
        registry
    );
    assert_eq!(fs::read(fixture.source.join(".git/index")).unwrap(), index);
    assert_eq!(fixture.snapshot(), snapshot);
}

fn legacy_snapshot(fixture: &Fixture) -> git2::Oid {
    let repo = fixture.repo();
    let name = repo
        .references_glob("refs/over/generated/*")
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .name()
        .unwrap()
        .to_owned();
    legacy_snapshot_ref(fixture, &name)
}

fn legacy_snapshot_ref(fixture: &Fixture, reference: &str) -> git2::Oid {
    fn rewrite(
        repo: &git2::Repository,
        id: git2::Oid,
        key: Option<&over::crypt::format::RepositoryKey>,
        replacements: &mut Vec<(git2::Oid, git2::Oid)>,
    ) -> git2::Oid {
        if let Some((_, new)) = replacements.iter().find(|(old, _)| *old == id) {
            return *new;
        }
        let commit = repo.find_commit(id).unwrap();
        let parents = commit
            .parent_ids()
            .map(|id| rewrite(repo, id, key, replacements))
            .collect::<Vec<_>>();
        let tree = commit.tree().unwrap();
        let mut builder = git2::build::TreeUpdateBuilder::new();
        for leaf in tree.iter() {
            let blob = repo.find_blob(leaf.id()).unwrap();
            if over::crypt::format::recognized(blob.content()) {
                let plaintext = key.unwrap().decrypt(blob.content(), 0).unwrap();
                builder.upsert(
                    leaf.name().unwrap(),
                    repo.blob(&plaintext).unwrap(),
                    git2::FileMode::Blob,
                );
            }
        }
        let tree = repo
            .find_tree(builder.create_updated(repo, &tree).unwrap())
            .unwrap();
        let parents = parents
            .iter()
            .map(|id| repo.find_commit(*id).unwrap())
            .collect::<Vec<_>>();
        let new = repo
            .commit(
                None,
                &commit.author(),
                &commit.committer(),
                commit.summary().unwrap().unwrap(),
                &tree,
                &parents.iter().collect::<Vec<_>>(),
            )
            .unwrap();
        replacements.push((id, new));
        new
    }
    let repo = fixture.repo();
    let key = over::crypt::access::local_key(&repo).ok();
    let old = repo.refname_to_id(reference).unwrap();
    let mut replacements = Vec::new();
    let new = rewrite(&repo, old, key.as_ref(), &mut replacements);
    repo.reference(reference, new, true, "legacy fixture")
        .unwrap();
    let state = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let mut text = fs::read_to_string(&state).unwrap();
    for (old, new) in replacements {
        text = text.replace(&old.to_string(), &new.to_string());
    }
    fs::write(state, text).unwrap();
    new
}

#[rstest]
#[case("loose")]
#[case("packed")]
#[case("alternate")]
fn legacy_migration_rewrites_history_but_does_not_claim_unreachable_plaintext_was_erased(
    #[case] storage: &str,
) {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let legacy = legacy_snapshot(&fixture);
    if storage == "packed" {
        fixture.git(&["repack", "-ad"]);
    } else if storage == "alternate" {
        let alternate =
            git2::Repository::init_bare(fixture.temp.path().join("alternate.git")).unwrap();
        let repo = fixture.repo();
        let odb = repo.odb().unwrap();
        odb.foreach(&mut |id: &git2::Oid| {
            let object = odb.read(*id).unwrap();
            if object.kind() == git2::ObjectType::Blob
                && object
                    .data()
                    .windows(b"sentinel".len())
                    .any(|bytes| bytes == b"sentinel")
            {
                alternate.blob(object.data()).unwrap();
            }
            true
        })
        .unwrap();
        fs::write(
            fixture.source.join(".git/objects/info/alternates"),
            format!("{}\n", alternate.path().join("objects").display()),
        )
        .unwrap();
    }
    let key = fs::read(fixture.source.join(".git/over-crypt/key")).unwrap();
    fixture
        .command("lock")
        .assert()
        .failure()
        .stderr(contains("known protected plaintext Git objects remain"));
    assert_ne!(fixture.snapshot(), legacy);
    assert_eq!(
        fs::read(fixture.source.join(".git/over-crypt/key")).unwrap(),
        key
    );
    let migrated = fixture.snapshot();
    fixture
        .command("unlock")
        .assert()
        .failure()
        .stderr(contains("known protected plaintext Git objects remain"));
    assert_eq!(
        fixture.snapshot(),
        migrated,
        "migration must be idempotent while cleanup is pending"
    );
    fixture
        .command("crypt")
        .arg("list")
        .assert()
        .success()
        .stdout(contains("known protected plaintext Git objects remain"));
    // Only this isolated fixture deliberately expires Git recovery data to verify actual erasure.
    fixture.git(&["reflog", "expire", "--expire=now", "--all"]);
    fixture.git(&["gc", "--prune=now"]);
    if storage == "alternate" {
        fixture
            .command("lock")
            .assert()
            .failure()
            .stderr(contains("known protected plaintext Git objects remain"));
        fs::remove_file(fixture.source.join(".git/objects/info/alternates")).unwrap();
    }
    fixture.command("lock").assert().success();
    fixture
        .command("unlock")
        .arg("--identity")
        .arg(fixture.temp.path().join("identity.age"))
        .assert()
        .success();
    fixture.sync().assert().success();
    fixture.assert_no_protected_plaintext();
}

#[test]
fn protected_reverse_patches_preserve_independent_unstaged_source_edits() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let target = fixture.target.join("config");
    fs::write(
        &target,
        fs::read_to_string(&target).unwrap().replace(
            "first=protected-snapshot-sentinel",
            "first=protected-reverse-sentinel",
        ),
    )
    .unwrap();
    let source = fixture.source.join("app/config.jinja");
    fs::write(
        &source,
        fs::read_to_string(&source)
            .unwrap()
            .replace("last=base", "last=unstaged"),
    )
    .unwrap();
    fixture
        .commit()
        .args(["-m", "preserve unstaged source edits"])
        .assert()
        .success();
    let physical = fs::read_to_string(&source).unwrap();
    assert!(physical.contains("first=protected-reverse-sentinel"));
    assert!(physical.contains("last=unstaged"));
    let repo = fixture.repo();
    let index = repo.index().unwrap();
    let blob = repo
        .find_blob(index.get_path(Path::new("app/config.jinja"), 0).unwrap().id)
        .unwrap();
    let key = over::crypt::access::local_key(&repo).unwrap();
    let canonical = key.decrypt(blob.content(), 0).unwrap();
    let text = std::str::from_utf8(&canonical).unwrap();
    assert!(text.contains("first=protected-reverse-sentinel"));
    assert!(text.contains("last=base"));
    fixture.assert_no_protected_plaintext();
}

#[rstest]
#[case(false)]
#[case(true)]
fn failed_protected_reverse_patches_do_not_publish_plaintext_or_overwrite_sources(
    #[case] publication_failure: bool,
) {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let source = fs::read(fixture.source.join("app/config.jinja")).unwrap();
    let index = fs::read(fixture.source.join(".git/index")).unwrap();
    let snapshot = fixture.snapshot();
    let target = fixture.target.join("config");
    let text = fs::read_to_string(&target).unwrap();
    let edited = if publication_failure {
        let repo = fixture.repo();
        let reference = repo.head().unwrap().name().unwrap().to_owned();
        fs::write(
            repo.path().join(format!("{reference}.lock")),
            b"concurrent writer",
        )
        .unwrap();
        text.replace(
            "first=protected-snapshot-sentinel",
            "first=protected-reverse-sentinel",
        )
    } else {
        text.replace("host=first", "host=protected-reverse-sentinel")
    };
    fs::write(&target, &edited).unwrap();
    fixture
        .commit()
        .args(["-m", "rejected protected edit"])
        .assert()
        .failure();
    assert_eq!(
        fs::read(fixture.source.join("app/config.jinja")).unwrap(),
        source
    );
    assert_eq!(fs::read(fixture.source.join(".git/index")).unwrap(), index);
    assert_eq!(fixture.snapshot(), snapshot);
    assert_eq!(fs::read_to_string(target).unwrap(), edited);
    fixture.assert_no_protected_plaintext();
}

#[rstest]
#[case(false, false, false)]
#[case(true, false, false)]
#[case(false, true, false)]
#[case(true, true, false)]
#[case(true, true, true)]
fn interrupted_snapshot_migration_recovers_both_ref_and_state_publication(
    #[case] ref_published: bool,
    #[case] state_published: bool,
    #[case] cleanup_locked: bool,
) {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let old = legacy_snapshot(&fixture);
    let state_path = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let old_text = fs::read_to_string(&state_path).unwrap();
    let old_state: toml::Value = toml::from_str(&old_text).unwrap();
    fixture.command("lock").assert().failure();
    let new = fixture.snapshot();
    let new_text = fs::read_to_string(&state_path).unwrap();
    let new_state: toml::Value = toml::from_str(&new_text).unwrap();
    let repo = fixture.repo();
    let reference = repo
        .references_glob("refs/over/generated/*")
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .name()
        .unwrap()
        .to_owned();
    let ledger_path = fixture
        .source
        .join(".git/over-crypt/generated-snapshots.json");
    let mut ledger: serde_json::Value =
        serde_json::from_slice(&fs::read(&ledger_path).unwrap()).unwrap();
    let mut refs = serde_json::Map::new();
    refs.insert(
        reference.clone(),
        serde_json::json!([old.to_string(), new.to_string()]),
    );
    let retained = format!(
        "refs/over/generated-retained/{}/{old}",
        reference.strip_prefix("refs/over/generated/").unwrap()
    );
    let retained_lock = repo.path().join(format!("{retained}.lock"));
    if cleanup_locked {
        repo.reference(&retained, new, true, "retained ref awaiting cleanup")
            .unwrap();
        refs.insert(
            retained.clone(),
            serde_json::json!([old.to_string(), new.to_string()]),
        );
        fs::write(&retained_lock, b"cleanup is temporarily blocked").unwrap();
    }
    let records: serde_json::Map<String, serde_json::Value> = old_state["state"]["checkouts"]
        .as_table()
        .unwrap()
        .iter()
        .map(|(name, old)| {
            (
                name.clone(),
                serde_json::json!([old, new_state["state"]["checkouts"][name].clone()]),
            )
        })
        .collect();
    ledger["pending"] = serde_json::json!({
        "administrative": fs::canonicalize(repo.commondir()).unwrap(),
        "refs": refs,
        "records": records,
        "replacements": [new.to_string()],
    });
    fs::write(&ledger_path, serde_json::to_vec(&ledger).unwrap()).unwrap();
    repo.reference(
        &format!("refs/over/crypt-migration/{new}"),
        new,
        true,
        "migration recovery fixture",
    )
    .unwrap();
    if !ref_published {
        repo.reference(&reference, old, true, "interrupted migration")
            .unwrap();
    }
    if !state_published {
        fs::write(&state_path, old_text).unwrap();
    }
    let before_ref = fixture.snapshot();
    let before_state = fs::read(&state_path).unwrap();
    for verb in ["sync", "commit", "unapply"] {
        let mut command = match verb {
            "sync" => fixture.sync(),
            "commit" => fixture.commit(),
            _ => {
                let mut command = fixture.command(verb);
                command.args(["app", "--target"]).arg(&fixture.target);
                command
            }
        };
        command
            .assert()
            .failure()
            .stderr(contains("generated snapshot migration interrupted"));
        assert_eq!(fixture.snapshot(), before_ref);
        assert_eq!(fs::read(&state_path).unwrap(), before_state);
    }
    let recovered = fixture.command("crypt").arg("recover").assert().failure();
    if !cleanup_locked {
        recovered.stderr(contains("known protected plaintext Git objects remain"));
    }
    assert_eq!(fixture.snapshot(), new);
    assert_eq!(fs::read_to_string(&state_path).unwrap(), new_text);
    let ledger: serde_json::Value =
        serde_json::from_slice(&fs::read(ledger_path).unwrap()).unwrap();
    assert!(ledger["pending"].is_null());
    if cleanup_locked {
        assert!(
            repo.find_reference(&retained).is_ok(),
            "unfinished cleanup must not invalidate a pending intent"
        );
        fs::remove_file(retained_lock).unwrap();
        fixture
            .command("unlock")
            .assert()
            .failure()
            .stderr(contains("known protected plaintext Git objects remain"));
        assert!(repo.find_reference(&retained).is_err());
    }
    assert!(
        repo.find_reference(&format!("refs/over/crypt-migration/{new}"))
            .is_err()
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn a_renamed_public_template_cannot_declassify_older_protected_renderings(
    #[case] lost_source_ancestry: bool,
) {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let original = fs::read(fixture.target.join("config")).unwrap();
    let exposed = git2::Oid::hash_object(git2::ObjectType::Blob, &original).unwrap();
    fs::remove_file(fixture.source.join("app/config.jinja")).unwrap();
    fs::write(
        fixture.source.join("app/config.tmpl"),
        "host={{ data.hostname }}\npublic=output\n",
    )
    .unwrap();
    let descriptor = fixture.source.join("app/over.yaml");
    fs::write(
        &descriptor,
        fs::read_to_string(&descriptor)
            .unwrap()
            .replace("**/*.jinja", "**/*.tmpl"),
    )
    .unwrap();
    if lost_source_ancestry {
        fixture
            .command("crypt")
            .current_dir(&fixture.source)
            .args(["remove", "app/plain", "--yes"])
            .assert()
            .success();
        fs::write(fixture.source.join("app/plain"), "a new public sibling\n").unwrap();
    }
    fixture.git(&["add", "--all"]);
    if lost_source_ancestry {
        let repo = fixture.repo();
        let tree = repo
            .find_tree(repo.index().unwrap().write_tree().unwrap())
            .unwrap();
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        let orphan = repo
            .commit(
                None,
                &signature,
                &signature,
                "replacement source history",
                &tree,
                &[],
            )
            .unwrap();
        let mut head = repo.head().unwrap();
        head.set_target(orphan, "simulate replaced source ancestry")
            .unwrap();
    } else {
        fixture.git(&[
            "commit",
            "-qm",
            "replace the protected template with a public source",
        ]);
    }
    fixture.sync().assert().success();
    let legacy = legacy_snapshot(&fixture);
    fixture
        .command("lock")
        .assert()
        .failure()
        .stderr(contains("known protected plaintext Git objects remain"));
    assert_ne!(fixture.snapshot(), legacy);
    assert!(fixture.source.join(".git/over-crypt/key").exists());
    let repo = fixture.repo();
    let tip = repo.find_commit(fixture.snapshot()).unwrap();
    let blob = repo
        .find_blob(
            tip.tree()
                .unwrap()
                .get_path(Path::new("config"))
                .unwrap()
                .id(),
        )
        .unwrap();
    assert!(
        over::crypt::format::recognized(blob.content()),
        "unknown historical renderings must be conservatively protected"
    );
    let ledger: serde_json::Value = serde_json::from_slice(
        &fs::read(
            fixture
                .source
                .join(".git/over-crypt/generated-snapshots.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        ledger["leaked"]
            .as_array()
            .unwrap()
            .contains(&exposed.to_string().into())
    );
}

#[test]
fn legacy_snapshot_migration_uses_each_linked_worktrees_own_source_root() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    legacy_snapshot(&fixture);
    let linked = fixture.temp.path().join("linked");
    fixture.git(&["worktree", "add", "-qb", "linked", linked.to_str().unwrap()]);
    let descriptor = linked.join("app/over.yaml");
    fs::write(
        &descriptor,
        fs::read_to_string(&descriptor)
            .unwrap()
            .replace("hostname: first", "hostname: linked"),
    )
    .unwrap();
    let output = std::process::Command::new("git")
        .current_dir(&linked)
        .args(["commit", "-qam", "linked rendering context"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = fixture.temp.path().join("linked-target");
    fixture
        .command_at(&linked, "apply")
        .args(["app", "--no-prompt", "--target"])
        .arg(&target)
        .assert()
        .success();
    let repo = fixture.repo();
    let references = repo
        .references_glob("refs/over/generated/*")
        .unwrap()
        .map(|reference| reference.unwrap().name().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 2);
    for reference in &references {
        legacy_snapshot_ref(&fixture, reference);
    }
    fixture
        .command("lock")
        .assert()
        .failure()
        .stderr(contains("known protected plaintext Git objects remain"));
    fixture.git(&["reflog", "expire", "--expire=now", "--all"]);
    fixture.git(&["gc", "--prune=now"]);
    fixture.command_at(&linked, "lock").assert().success();
    fixture
        .command_at(&linked, "unlock")
        .arg("--identity")
        .arg(fixture.temp.path().join("identity.age"))
        .assert()
        .success();
    fixture.sync().assert().success();
    fixture
        .command_at(&linked, "sync")
        .args(["app", "--no-prompt", "--target"])
        .arg(&target)
        .assert()
        .success();
    assert!(
        fs::read_to_string(target.join("config"))
            .unwrap()
            .contains("host=linked")
    );
    fixture.assert_no_protected_plaintext();
}

#[cfg(unix)]
#[test]
fn legacy_snapshot_provenance_resolves_logical_source_aliases() {
    let mut fixture = Fixture::new(true);
    let alias = fixture.temp.path().join("source-alias");
    std::os::unix::fs::symlink(&fixture.source, &alias).unwrap();
    fixture.source = alias;
    fixture.protect();
    fixture.apply();
    legacy_snapshot(&fixture);
    fixture
        .command("lock")
        .assert()
        .failure()
        .stderr(contains("known protected plaintext Git objects remain"));
}

#[test]
fn unencrypted_legacy_snapshots_support_no_op_ordinary_and_reverse_commits_without_a_key() {
    let fixture = Fixture::new(true);
    fixture.apply();
    legacy_snapshot(&fixture);
    fixture
        .commit()
        .assert()
        .success()
        .stdout(contains("nothing to commit"));
    fs::write(fixture.target.join("plain"), "ordinary sibling edit\n").unwrap();
    fixture.stage("plain");
    fixture
        .commit()
        .arg(fixture.target.join("plain"))
        .assert()
        .success();
    legacy_snapshot(&fixture);
    let target = fixture.target.join("config");
    fs::write(
        &target,
        fs::read_to_string(&target)
            .unwrap()
            .replace("first=base", "first=edited"),
    )
    .unwrap();
    fixture.commit().arg(&target).assert().success();
    assert!(
        fs::read_to_string(fixture.source.join("app/config.jinja"))
            .unwrap()
            .contains("first=edited")
    );
    assert!(!fixture.source.join(".over/crypt/access.json").exists());
    assert!(!fixture.source.join(".git/over-crypt/key").exists());
}

#[test]
fn interrupted_unencrypted_legacy_sync_finishes_without_enabling_encryption() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let source = fixture.source.join("app/config.jinja");
    fs::write(
        &source,
        fs::read_to_string(&source)
            .unwrap()
            .replace("last=base", "last=upstream"),
    )
    .unwrap();
    commit_all(&fixture.repo());
    fixture.sync().assert().success();
    let pending = legacy_snapshot(&fixture);
    let repo = fixture.repo();
    let base = repo.find_commit(pending).unwrap().parent_id(0).unwrap();
    let path = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let mut metadata: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let generated = metadata["state"]["checkouts"]
        .as_table_mut()
        .unwrap()
        .iter_mut()
        .next()
        .unwrap()
        .1["generated"]
        .as_table_mut()
        .unwrap();
    generated.insert("base".into(), base.to_string().into());
    generated.insert("pending".into(), pending.to_string().into());
    fs::write(path, toml::to_string(&metadata).unwrap()).unwrap();
    repo.references_glob("refs/over/generated/*")
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .set_target(base, "interrupted legacy pending sync")
        .unwrap();
    fixture.sync().assert().success();
    assert_eq!(fixture.snapshot(), pending);
    fixture.sync().assert().success();
    assert!(
        fs::read_to_string(fixture.target.join("config"))
            .unwrap()
            .contains("last=upstream")
    );
    assert!(!fixture.source.join(".over/crypt/access.json").exists());
}

#[test]
fn an_orphan_legacy_snapshot_blocks_lifecycle_without_guessing_its_protection() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let legacy = legacy_snapshot(&fixture);
    fs::remove_file(fixture.temp.path().join("state/over/virtual_checkout.toml")).unwrap();
    fixture
        .command("lock")
        .assert()
        .failure()
        .stderr(contains("orphan legacy generated snapshot"));
    assert_eq!(fixture.snapshot(), legacy);
    assert!(fixture.source.join(".git/over-crypt/key").exists());
    fixture
        .command("crypt")
        .arg("list")
        .assert()
        .success()
        .stdout(contains(
            "legacy generated snapshots need provenance migration",
        ));
}

#[test]
fn an_unreferenced_legacy_snapshot_left_by_failed_publication_is_still_diagnosed() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let legacy = legacy_snapshot(&fixture);
    fs::remove_file(fixture.temp.path().join("state/over/virtual_checkout.toml")).unwrap();
    let repo = fixture.repo();
    repo.references_glob("refs/over/generated/*")
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .delete()
        .unwrap();
    assert!(repo.find_commit(legacy).is_ok());
    fixture
        .command("lock")
        .assert()
        .failure()
        .stderr(contains("orphan legacy generated snapshot").and(contains("object database")));
    assert!(fixture.source.join(".git/over-crypt/key").exists());
}

#[test]
fn legacy_migration_preserves_reverse_snapshot_parentage_and_source_history() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let target = fixture.target.join("config");
    fs::write(
        &target,
        fs::read_to_string(&target)
            .unwrap()
            .replace("last=base", "last=protected-reverse-sentinel"),
    )
    .unwrap();
    fixture.commit().assert().success();
    let head = fixture.repo().head().unwrap().target();
    let legacy = legacy_snapshot(&fixture);
    assert_eq!(
        fixture.repo().find_commit(legacy).unwrap().parent_count(),
        2
    );
    fixture
        .command("lock")
        .assert()
        .failure()
        .stderr(contains("known protected plaintext Git objects remain"));
    assert_eq!(
        fixture
            .repo()
            .find_commit(fixture.snapshot())
            .unwrap()
            .parent_count(),
        2
    );
    assert_eq!(fixture.repo().head().unwrap().target(), head);
    fixture.git(&["reflog", "expire", "--expire=now", "--all"]);
    fixture.git(&["gc", "--prune=now"]);
    fixture.command("lock").assert().success();
    fixture
        .command("unlock")
        .arg("--identity")
        .arg(fixture.temp.path().join("identity.age"))
        .assert()
        .success();
    fixture.sync().assert().success();
    fixture.assert_no_protected_plaintext();
}

#[test]
fn pending_protected_snapshots_survive_object_cleanup_until_publication_finishes() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let repo = fixture.repo();
    let parent = repo.find_commit(fixture.snapshot()).unwrap();
    let tree = parent.tree().unwrap();
    let key = over::crypt::access::local_key(&repo).unwrap();
    let rendered = fs::read_to_string(fixture.target.join("config"))
        .unwrap()
        .replace("middle=stable", "middle=pending");
    let mut builder = git2::build::TreeUpdateBuilder::new();
    builder.upsert(
        "config",
        repo.blob(&key.encrypt(rendered.as_bytes(), 0).unwrap())
            .unwrap(),
        git2::FileMode::Blob,
    );
    let tree = repo
        .find_tree(builder.create_updated(&repo, &tree).unwrap())
        .unwrap();
    let pending = repo
        .commit(
            None,
            &parent.author(),
            &parent.committer(),
            parent.message().unwrap(),
            &tree,
            &[&parent],
        )
        .unwrap();
    let path = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let mut state: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let generated = &mut state["state"]["checkouts"]
        .as_table_mut()
        .unwrap()
        .iter_mut()
        .next()
        .unwrap()
        .1["generated"];
    generated
        .as_table_mut()
        .unwrap()
        .insert("pending".into(), pending.to_string().into());
    fs::write(&path, toml::to_string(&state).unwrap()).unwrap();
    fixture.command("lock").assert().success();
    fixture.git(&["reflog", "expire", "--expire=now", "--all"]);
    fixture.git(&["gc", "--prune=now"]);
    assert!(
        fixture.repo().find_commit(pending).is_ok(),
        "XDG-only pending content needs a retention ref during cleanup"
    );
    fixture
        .command("unlock")
        .arg("--identity")
        .arg(fixture.temp.path().join("identity.age"))
        .assert()
        .success();
    fixture.sync().assert().success();
    assert_eq!(fixture.snapshot(), pending);
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        rendered
    );
    // Completing a pending operation precedes reconciliation with the current source/context.
    fixture.sync().assert().success();
    fixture
        .command("unapply")
        .args(["app", "--target"])
        .arg(&fixture.target)
        .assert()
        .success();
    assert_eq!(
        fixture
            .repo()
            .references_glob("refs/over/generated-retained/*")
            .unwrap()
            .count(),
        0
    );
    fixture.assert_no_protected_plaintext();
}

#[test]
fn ciphertext_authentication_failure_never_replaces_local_rendered_content() {
    let fixture = Fixture::new(true);
    fixture.protect();
    fixture.apply();
    let target = fs::read(fixture.target.join("config")).unwrap();
    let repo = fixture.repo();
    let parent = repo.find_commit(fixture.snapshot()).unwrap();
    let old_tree = parent.tree().unwrap();
    let mut ciphertext = repo
        .find_blob(old_tree.get_path(Path::new("config")).unwrap().id())
        .unwrap()
        .content()
        .to_vec();
    *ciphertext.last_mut().unwrap() ^= 1;
    let mut builder = git2::build::TreeUpdateBuilder::new();
    builder.upsert(
        "config",
        repo.blob(&ciphertext).unwrap(),
        git2::FileMode::Blob,
    );
    let tree = repo
        .find_tree(builder.create_updated(&repo, &old_tree).unwrap())
        .unwrap();
    let corrupt = repo
        .commit(
            None,
            &parent.author(),
            &parent.committer(),
            parent.message().unwrap(),
            &tree,
            &[&parent],
        )
        .unwrap();
    let mut reference = repo
        .references_glob("refs/over/generated/*")
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    reference.set_target(corrupt, "tampered fixture").unwrap();
    let state_path = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let state = fs::read_to_string(&state_path)
        .unwrap()
        .replace(&parent.id().to_string(), &corrupt.to_string());
    fs::write(&state_path, &state).unwrap();
    fixture.sync().assert().failure();
    fixture.command("lock").assert().failure();
    assert_eq!(fs::read(fixture.target.join("config")).unwrap(), target);
    assert_eq!(fs::read_to_string(state_path).unwrap(), state);
    fixture.assert_no_protected_plaintext();
}

fn commit_all(repo: &git2::Repository) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "source",
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap();
}

#[test]
fn committing_a_rendered_literal_updates_only_the_template_and_is_not_replayed() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let edited = "host=first\nfirst=edited\nmiddle=stable\nlast=base";
    fs::write(fixture.target.join("config"), edited).unwrap();
    fixture
        .commit()
        .assert()
        .success()
        .stdout(contains("committed"));
    let repo = fixture.repo();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert_eq!(
        repo.find_blob(tree.get_path(Path::new("app/config.jinja")).unwrap().id())
            .unwrap()
            .content(),
        b"host={{ data.hostname }}\nfirst=edited\nmiddle=stable\nlast=base\n"
    );
    assert!(tree.get_path(Path::new("app/config")).is_err());
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        edited
    );
    fixture
        .commit()
        .assert()
        .success()
        .stdout(contains("nothing to commit"));
    assert_eq!(fixture.repo().head().unwrap().target(), Some(head.id()));
    fixture.sync().assert().success();
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        edited
    );
}

#[rstest]
#[case("expression", "host=changed\nfirst=base\nmiddle=stable\nlast=base\n")]
#[case("binary", "host=first\nfirst=\0binary\nmiddle=stable\nlast=base\n")]
fn incompatible_rendered_edits_preserve_both_files(#[case] _reason: &str, #[case] edited: &str) {
    let fixture = Fixture::new(true);
    fixture.apply();
    let original = fs::read(fixture.source.join("app/config.jinja")).unwrap();
    let head = fixture.repo().head().unwrap().target();
    fs::write(fixture.target.join("config"), edited).unwrap();
    fixture
        .commit()
        .assert()
        .failure()
        .stdout(contains("source template").and(contains("config.jinja")));
    assert_eq!(
        fs::read(fixture.source.join("app/config.jinja")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        edited
    );
    assert_eq!(fixture.repo().head().unwrap().target(), head);
}

#[rstest]
#[case(false)]
#[case(true)]
fn disjoint_source_worktree_edits_remain_unstaged_after_a_reverse_commit(#[case] staged: bool) {
    let fixture = Fixture::new(true);
    fixture.apply();
    let repo = fixture.repo();
    let source = fixture.source.join("app/config.jinja");
    let original = fs::read_to_string(&source).unwrap();
    if staged {
        fs::write(&source, original.replace("middle=stable", "middle=staged")).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("app/config.jinja")).unwrap();
        index.write().unwrap();
    }
    fs::write(
        &source,
        fs::read_to_string(&source)
            .unwrap()
            .replace("last=base", "last=unstaged"),
    )
    .unwrap();
    fs::write(
        fixture.target.join("config"),
        "host=first\nfirst=edited\nmiddle=stable\nlast=base",
    )
    .unwrap();
    fixture.commit().assert().success();
    let committed = repo
        .find_blob(
            fixture
                .repo()
                .head()
                .unwrap()
                .peel_to_tree()
                .unwrap()
                .get_path(Path::new("app/config.jinja"))
                .unwrap()
                .id(),
        )
        .unwrap();
    let committed = std::str::from_utf8(committed.content()).unwrap();
    assert!(committed.contains("first=edited"));
    assert!(!committed.contains("unstaged"));
    assert_eq!(committed.contains("middle=staged"), staged);
    let worktree = fs::read_to_string(&source).unwrap();
    assert!(worktree.contains("first=edited") && worktree.contains("last=unstaged"));
    let indexed = repo
        .find_blob(
            fixture
                .repo()
                .index()
                .unwrap()
                .get_path(Path::new("app/config.jinja"), 0)
                .unwrap()
                .id,
        )
        .unwrap();
    assert_eq!(indexed.content(), committed.as_bytes());
    let head = fixture.repo().head().unwrap().target();
    fixture
        .commit()
        .assert()
        .success()
        .stdout(contains("nothing to commit"));
    assert_eq!(fixture.repo().head().unwrap().target(), head);
    fixture.sync().assert().success();
    let physical = fs::read_to_string(fixture.target.join("config")).unwrap();
    assert!(physical.contains("first=edited"));
    assert_eq!(physical.contains("middle=staged"), staged);
    assert!(!physical.contains("unstaged"));
}

#[test]
fn committed_disjoint_source_changes_are_preserved_by_reverse_patches() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let source = fixture.source.join("app/config.jinja");
    fs::write(
        &source,
        fs::read_to_string(&source)
            .unwrap()
            .replace("last=base", "last=upstream"),
    )
    .unwrap();
    commit_all(&fixture.repo());
    fs::write(
        fixture.target.join("config"),
        "host=first\nfirst=edited\nmiddle=stable\nlast=base",
    )
    .unwrap();
    fixture.commit().assert().success();
    let source = fs::read_to_string(source).unwrap();
    assert!(source.contains("first=edited") && source.contains("last=upstream"));
}

#[rstest]
#[case("worktree")]
#[case("index")]
#[case("HEAD")]
fn overlapping_source_changes_are_never_overwritten(#[case] layer: &str) {
    let fixture = Fixture::new(true);
    fixture.apply();
    let source = fixture.source.join("app/config.jinja");
    fs::write(
        &source,
        fs::read_to_string(&source)
            .unwrap()
            .replace("first=base", "first=source"),
    )
    .unwrap();
    if layer == "HEAD" {
        commit_all(&fixture.repo());
    }
    if layer == "index" {
        let repo = fixture.repo();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("app/config.jinja")).unwrap();
        index.write().unwrap();
    }
    let before = fs::read(&source).unwrap();
    let repo = fixture.repo();
    let index = fs::read(repo.path().join("index")).unwrap();
    let head = repo.head().unwrap().target();
    let edited = "host=first\nfirst=target\nmiddle=stable\nlast=base";
    fs::write(fixture.target.join("config"), edited).unwrap();
    fixture
        .commit()
        .assert()
        .failure()
        .stdout(contains("overlap"));
    assert_eq!(fs::read(&source).unwrap(), before);
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
    assert_eq!(fixture.repo().head().unwrap().target(), head);
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        edited
    );
}

#[test]
fn successful_and_conflicting_templates_are_reported_independently() {
    let fixture = Fixture::new(true);
    fs::write(fixture.source.join("app/third.jinja"), "literal=old\n").unwrap();
    fs::write(
        fixture.source.join("app/other.jinja"),
        "name={{ data.hostname }}\nvalue=old\n",
    )
    .unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    let good = "host=first\nfirst=edited\nmiddle=stable\nlast=base";
    let bad = "name=untranslatable\nvalue=old\n";
    fs::write(fixture.target.join("config"), good).unwrap();
    fs::write(fixture.target.join("other"), bad).unwrap();
    fs::write(fixture.target.join("third"), "literal=new").unwrap();
    fs::write(fixture.target.join("plain"), "ordinary staged change\n").unwrap();
    fixture.stage("plain");
    fixture
        .commit()
        .assert()
        .failure()
        .stdout(contains("committed 3").and(contains("other.jinja")));
    assert_eq!(
        fs::read_to_string(fixture.source.join("app/third.jinja")).unwrap(),
        "literal=new\n"
    );
    assert!(
        fs::read_to_string(fixture.source.join("app/config.jinja"))
            .unwrap()
            .contains("first=edited")
    );
    assert_eq!(
        fs::read_to_string(fixture.source.join("app/other.jinja")).unwrap(),
        "name={{ data.hostname }}\nvalue=old\n"
    );
    let head = fixture.repo().head().unwrap().target();
    fixture.commit().assert().failure();
    assert_eq!(fixture.repo().head().unwrap().target(), head);
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        good
    );
    assert_eq!(
        fs::read_to_string(fixture.target.join("other")).unwrap(),
        bad
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn reverse_patches_follow_directory_mappings_for_both_operand_namespaces(
    #[case] source_operand: bool,
) {
    let fixture = Fixture::new(true);
    fs::create_dir(fixture.source.join("app/sub")).unwrap();
    fs::rename(
        fixture.source.join("app/config.jinja"),
        fixture.source.join("app/sub/config.jinja"),
    )
    .unwrap();
    let descriptor = fixture.source.join("app/over.yaml");
    let config = fs::read_to_string(&descriptor).unwrap() + "  - {path: sub, target: '~/mapped'}\n";
    fs::write(descriptor, config).unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    let target = fixture.target.join("mapped/config");
    let source = fixture.source.join("app/sub/config.jinja");
    fs::write(
        &target,
        fs::read_to_string(&target)
            .unwrap()
            .replace("first=base", "first=mapped"),
    )
    .unwrap();
    fixture
        .commit()
        .arg(if source_operand { &source } else { &target })
        .assert()
        .success();
    assert!(fs::read_to_string(source).unwrap().contains("first=mapped"));
    assert!(fs::read_to_string(target).unwrap().contains("first=mapped"));
}

#[cfg(unix)]
#[test]
fn reverse_patches_accept_an_aliased_source_collection() {
    let mut fixture = Fixture::new(true);
    let alias = fixture.temp.path().join("source-alias");
    std::os::unix::fs::symlink(&fixture.source, &alias).unwrap();
    fixture.source = alias;
    fixture.apply();
    let target = fixture.target.join("config");
    fs::write(
        &target,
        fs::read_to_string(&target)
            .unwrap()
            .replace("first=base", "first=aliased"),
    )
    .unwrap();
    fixture.commit().arg(target).assert().success();
    assert!(
        fs::read_to_string(fixture.source.join("app/config.jinja"))
            .unwrap()
            .contains("first=aliased")
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn rendered_commit_selection_accepts_source_and_destination_paths(#[case] source_operand: bool) {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(
        fixture.target.join("config"),
        "host=first\nfirst=edited\nmiddle=stable\nlast=base",
    )
    .unwrap();
    fs::write(fixture.target.join("plain"), "staged ordinary\n").unwrap();
    fixture.stage("plain");
    let path = if source_operand {
        fixture.source.join("app/config.jinja")
    } else {
        fixture.target.join("config")
    };
    fixture.commit().arg(path).assert().success();
    let repo = fixture.repo();
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    assert_eq!(
        repo.find_blob(tree.get_path(Path::new("app/plain")).unwrap().id())
            .unwrap()
            .content(),
        b"ordinary\n"
    );
    assert_eq!(
        repo.find_blob(
            repo.index()
                .unwrap()
                .get_path(Path::new("app/plain"), 0)
                .unwrap()
                .id
        )
        .unwrap()
        .content(),
        b"staged ordinary\n"
    );
}

#[rstest]
#[case(
    "host={{ data.hostname }}\nsame\nsame\nend\n",
    "host=first\nchanged\nsame\nend",
    "ambiguous"
)]
#[case("{% if true %}literal{% endif %}\n", "changed", "control structures")]
#[case("{{ '}}' +\n'literal' }}\n", "changed", "multiline")]
fn ambiguous_or_structurally_transformed_output_requires_manual_source_edits(
    #[case] template: &str,
    #[case] edited: &str,
    #[case] reason: &str,
) {
    let fixture = Fixture::new(true);
    let source = fixture.source.join("app/config.jinja");
    fs::write(&source, template).unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    fs::write(fixture.target.join("config"), edited).unwrap();
    fixture
        .commit()
        .assert()
        .failure()
        .stdout(contains(reason).and(contains("hunk")));
    assert_eq!(fs::read_to_string(source).unwrap(), template);
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        edited
    );
}

#[test]
fn extension_stripping_without_rendering_propagates_literal_jinja_bytes_as_text() {
    let fixture = Fixture::new(true);
    let descriptor = fixture.source.join("app/over.yaml");
    fs::write(
        &descriptor,
        fs::read_to_string(&descriptor)
            .unwrap()
            .replace("template: true", "template: false"),
    )
    .unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    let edited = "host={{ another.expression }}\nfirst=edited\n";
    fs::write(fixture.target.join("config"), edited).unwrap();
    fixture.commit().assert().success();
    assert_eq!(
        fs::read_to_string(fixture.source.join("app/config.jinja")).unwrap(),
        edited
    );
}

#[test]
fn a_changed_rendering_context_does_not_copy_expression_values_into_source_or_diagnostics() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let descriptor = fixture.source.join("app/over.yaml");
    fs::write(
        &descriptor,
        fs::read_to_string(&descriptor)
            .unwrap()
            .replace("hostname: first", "hostname: PRIVATE_REVERSE_SENTINEL"),
    )
    .unwrap();
    let edited = "host=PRIVATE_LOCAL_SENTINEL\nfirst=edited\nmiddle=stable\nlast=base";
    fs::write(fixture.target.join("config"), edited).unwrap();
    let assertion = fixture
        .commit()
        .assert()
        .failure()
        .stdout(contains("context or rules"));
    let output = assertion.get_output();
    for sentinel in ["PRIVATE_REVERSE_SENTINEL", "PRIVATE_LOCAL_SENTINEL"] {
        assert!(!String::from_utf8_lossy(&output.stdout).contains(sentinel));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(sentinel));
    }
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        edited
    );
    assert!(
        fs::read_to_string(fixture.source.join("app/config.jinja"))
            .unwrap()
            .contains("first=base")
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn a_published_reverse_commit_recovers_its_checkpoint_without_replaying_the_edit(
    #[case] ref_advanced: bool,
) {
    let fixture = Fixture::new(true);
    fixture.apply();
    let repo = fixture.repo();
    let parent = repo.head().unwrap().target().unwrap();
    let old_snapshot = fixture.snapshot();
    let state = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let mut before: toml::Value = toml::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    let edited = "host=first\nfirst=edited\nmiddle=stable\nlast=base";
    fs::write(fixture.target.join("config"), edited).unwrap();
    fixture.commit().assert().success();
    let committed = fixture.repo().head().unwrap().target().unwrap();
    let new_snapshot = fixture.snapshot();
    let generated = before["state"]["checkouts"]
        .as_table_mut()
        .unwrap()
        .iter_mut()
        .next()
        .unwrap()
        .1["generated"]
        .as_table_mut()
        .unwrap();
    generated.insert(
        "reverse_pending".into(),
        toml::Value::Table(toml::map::Map::from_iter([
            ("parent".into(), parent.to_string().into()),
            ("source_commit".into(), committed.to_string().into()),
            ("generated_commit".into(), new_snapshot.to_string().into()),
            (
                "destinations".into(),
                toml::Value::Array(vec!["config".into()]),
            ),
            ("observed".into(), toml::Value::Table(Default::default())),
            ("index_before".into(), "unused after publication".into()),
            (
                "sources_before".into(),
                toml::Value::Table(Default::default()),
            ),
        ])),
    );
    fs::write(&state, toml::to_string(&before).unwrap()).unwrap();
    if !ref_advanced {
        let mut reference = repo
            .references_glob("refs/over/generated/*")
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        reference
            .set_target(old_snapshot, "simulate interrupted checkpoint")
            .unwrap();
    }
    fixture
        .commit()
        .assert()
        .success()
        .stdout(contains("nothing to commit"));
    assert_eq!(fixture.repo().head().unwrap().target(), Some(committed));
    assert_eq!(fixture.snapshot(), new_snapshot);
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        edited
    );
    assert!(
        !fs::read_to_string(state)
            .unwrap()
            .contains("reverse_pending")
    );
}

#[test]
fn a_failed_reverse_commit_restores_source_and_index_before_retrying() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let repo = fixture.repo();
    let source = fixture.source.join("app/config.jinja");
    let original = fs::read(&source).unwrap();
    let index = fs::read(repo.path().join("index")).unwrap();
    let head = repo.head().unwrap().target();
    let snapshot = fixture.snapshot();
    let reference = repo.head().unwrap().name().unwrap().to_owned();
    let lock = repo.path().join(format!("{reference}.lock"));
    fs::write(&lock, b"").unwrap();
    let edited = "host=first\nfirst=edited\nmiddle=stable\nlast=base";
    fs::write(fixture.target.join("config"), edited).unwrap();
    fixture.commit().assert().failure();
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
    assert_eq!(fixture.repo().head().unwrap().target(), head);
    assert_eq!(fixture.snapshot(), snapshot);
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        edited
    );
    fs::remove_file(lock).unwrap();
    fixture.commit().assert().success();
    assert!(fs::read_to_string(source).unwrap().contains("first=edited"));
}

#[test]
fn symlink_templates_use_reconstructible_artifacts_without_touching_source() {
    let fixture = Fixture::new(false);
    let original = fs::read(fixture.source.join("app/config.jinja")).unwrap();
    fixture.apply();
    let destination = fixture.target.join("config");
    assert!(destination.is_symlink());
    let artifact = fs::read_link(&destination).unwrap();
    assert!(artifact.starts_with(fixture.temp.path().join("state/over/rendered")));
    assert!(
        fs::read_to_string(&destination)
            .unwrap()
            .starts_with("host=first\n")
    );
    let modified = fs::metadata(&artifact).unwrap().modified().unwrap();
    fixture.apply();
    assert_eq!(
        fs::metadata(&artifact).unwrap().modified().unwrap(),
        modified
    );
    fs::remove_file(&artifact).unwrap();
    fixture.apply();
    assert!(artifact.is_file());
    assert_eq!(
        fs::read(fixture.source.join("app/config.jinja")).unwrap(),
        original
    );
    fixture
        .command("add")
        .args(["--overlay", "app", "--target"])
        .arg(&fixture.target)
        .arg(&destination)
        .assert()
        .failure()
        .stderr(contains("source template"));
}

#[test]
fn checkout_templates_store_complete_snapshots_and_merge_disjoint_local_edits() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let repo = fixture.repo();
    let head = repo.head().unwrap().target();
    let index = fs::read(repo.path().join("index")).unwrap();
    let first = fixture.snapshot();
    let tree = repo.find_commit(first).unwrap().tree().unwrap();
    assert!(tree.get_path(Path::new("config")).is_ok());
    assert!(tree.get_path(Path::new("plain")).is_ok());
    assert!(tree.get_path(Path::new("config.jinja")).is_err());
    assert!(!fixture.target.join("config").is_symlink());
    assert!(!fixture.target.join(".git").exists());
    fixture.sync().assert().success();
    assert_eq!(first, fixture.snapshot());
    let source = fixture.source.join("app/config.jinja");
    let original = fs::read_to_string(&source).unwrap();
    fs::write(&source, original.replace("last=base", "last=upstream")).unwrap();
    // A dirty template is not yet checkout source content.
    fixture.sync().assert().success();
    assert!(
        fs::read_to_string(fixture.target.join("config"))
            .unwrap()
            .contains("last=base")
    );
    commit_all(&repo);
    let head_after_commit = repo.head().unwrap().target();
    let index_after_commit = fs::read(repo.path().join("index")).unwrap();
    let local = fs::read_to_string(fixture.target.join("config"))
        .unwrap()
        .replace("first=base", "first=local");
    fs::write(fixture.target.join("config"), local).unwrap();
    fixture
        .sync()
        .assert()
        .success()
        .stdout(contains("source template"));
    let merged = fs::read_to_string(fixture.target.join("config")).unwrap();
    assert!(merged.contains("first=local"));
    assert!(merged.contains("last=upstream"));
    let second = fixture.snapshot();
    assert_eq!(
        repo.find_commit(second).unwrap().parent_id(0).unwrap(),
        first
    );
    assert_eq!(repo.head().unwrap().target(), head_after_commit);
    assert_ne!(head, head_after_commit);
    assert_eq!(
        fs::read(repo.path().join("index")).unwrap(),
        index_after_commit
    );
    assert_ne!(index, fs::read(repo.path().join("index")).unwrap());
    let desired_blob = repo
        .find_blob(
            repo.find_commit(second)
                .unwrap()
                .tree()
                .unwrap()
                .get_path(Path::new("config"))
                .unwrap()
                .id(),
        )
        .unwrap();
    assert!(
        std::str::from_utf8(desired_blob.content())
            .unwrap()
            .contains("first=base")
    );
    fixture.sync().assert().success();
    assert_eq!(
        fs::read_to_string(fixture.target.join("config")).unwrap(),
        merged
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn overlapping_checkout_edits_remain_conflicted_after_the_desired_ref_advances(
    #[case] protected: bool,
) {
    let fixture = Fixture::new(true);
    if protected {
        fixture.protect();
    }
    fixture.apply();
    let first = fixture.snapshot();
    let source = fixture.source.join("app/config.jinja");
    let original = fs::read_to_string(&source).unwrap();
    let first_line = if protected {
        "first=protected-snapshot-sentinel"
    } else {
        "first=base"
    };
    fs::write(&source, original.replace(first_line, "first=upstream")).unwrap();
    if protected {
        fixture.git(&["add", "app/config.jinja"]);
        fixture.git(&["commit", "-qm", "update protected source"]);
    } else {
        commit_all(&fixture.repo());
    }
    let destination = fixture.target.join("config");
    let local = fs::read_to_string(&destination)
        .unwrap()
        .replace(first_line, "first=local");
    fs::write(&destination, &local).unwrap();
    fixture
        .sync()
        .assert()
        .failure()
        .stdout(contains("source template"));
    let second = fixture.snapshot();
    assert_ne!(first, second);
    assert_eq!(fs::read_to_string(&destination).unwrap(), local);
    if protected {
        fixture.command("lock").assert().success();
        fixture
            .command("unlock")
            .arg("--identity")
            .arg(fixture.temp.path().join("identity.age"))
            .assert()
            .success();
        fixture.assert_no_protected_plaintext();
    }
    fixture.sync().assert().failure();
    assert_eq!(fixture.snapshot(), second);
    assert_eq!(fs::read_to_string(&destination).unwrap(), local);
    fixture
        .command("status")
        .args(["app", "--target"])
        .arg(&fixture.target)
        .assert()
        .failure()
        .stdout(contains("conflict"));
    fixture
        .command("restore")
        .args(["--overlay", "app", "--target"])
        .arg(&fixture.target)
        .arg(&destination)
        .assert()
        .success();
    assert!(
        fs::read_to_string(&destination)
            .unwrap()
            .contains("first=upstream")
    );
    fixture.sync().assert().success();
}

#[rstest]
#[case(false)]
#[case(true)]
fn reporting_and_previews_do_not_write_generated_objects_or_state(#[case] protected: bool) {
    let fixture = Fixture::new(true);
    if protected {
        fixture.protect();
    }
    let repo = fixture.repo();
    let count = || {
        let mut count = 0;
        repo.odb()
            .unwrap()
            .foreach(|_| {
                count += 1;
                true
            })
            .unwrap();
        count
    };
    let before = count();
    fixture
        .command("status")
        .args(["app", "--target"])
        .arg(&fixture.target)
        .assert()
        .failure();
    fixture
        .command("apply")
        .args(["app", "--dry-run", "--target"])
        .arg(&fixture.target)
        .assert()
        .success();
    assert_eq!(count(), before);
    if !protected {
        assert!(!fixture.temp.path().join("state").exists());
    }
    fixture.apply();
    let before = count();
    let state = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let metadata = fs::read(&state).unwrap();
    let source = fixture.source.join("app/config.jinja");
    fs::write(
        &source,
        fs::read_to_string(&source)
            .unwrap()
            .replace("last=base", "last=upstream"),
    )
    .unwrap();
    if protected {
        fixture.git(&["add", "app/config.jinja"]);
        fixture.git(&["commit", "-qm", "update protected source"]);
    } else {
        commit_all(&repo);
    }
    let after_commit = count();
    assert!(after_commit > before);
    fixture
        .command("status")
        .args(["app", "--target"])
        .arg(&fixture.target)
        .assert()
        .failure();
    fixture
        .command("diff")
        .args(["app", "--target"])
        .arg(&fixture.target)
        .assert()
        .success()
        .stdout(contains("last=upstream"));
    fixture.sync().arg("--dry-run").assert().success();
    assert_eq!(count(), after_commit);
    assert_eq!(fs::read(state).unwrap(), metadata);
    if protected {
        fixture.assert_no_protected_plaintext();
    }
    assert!(
        fs::read_to_string(fixture.target.join("config"))
            .unwrap()
            .contains("last=base")
    );
}

#[test]
fn generated_checkout_unapply_removes_only_clean_owned_files_and_its_ref() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.target.join("foreign"), "keep").unwrap();
    fixture
        .command("unapply")
        .args(["app", "--target"])
        .arg(&fixture.target)
        .assert()
        .success();
    assert!(!fixture.target.join("config").exists());
    assert!(!fixture.target.join("plain").exists());
    assert_eq!(fs::read(fixture.target.join("foreign")).unwrap(), b"keep");
    assert_eq!(
        fixture
            .repo()
            .references_glob("refs/over/generated/*")
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn binary_conflicts_preserve_the_local_file_and_template_source() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let destination = fixture.target.join("config");
    fs::write(&destination, b"local\0private").unwrap();
    let source = fixture.source.join("app/config.jinja");
    let updated = fs::read_to_string(&source)
        .unwrap()
        .replace("first=base", "first=upstream");
    fs::write(&source, &updated).unwrap();
    commit_all(&fixture.repo());
    fixture.sync().assert().failure();
    assert_eq!(fs::read(destination).unwrap(), b"local\0private");
    assert_eq!(fs::read_to_string(source).unwrap(), updated);
}

#[test]
fn ordinary_siblings_keep_their_staging_workflow_and_generated_selections_fail_before_staging() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.target.join("plain"), "ordinary edit\n").unwrap();
    let index = fs::read(fixture.repo().path().join("index")).unwrap();
    for patch in [false, true] {
        let mut command = fixture.command("add");
        command
            .args(["--overlay", "app", "--target"])
            .arg(&fixture.target)
            .arg(&fixture.target);
        if patch {
            command.arg("--patch");
        }
        command
            .assert()
            .failure()
            .stderr(contains("source template"));
        assert_eq!(
            fs::read(fixture.repo().path().join("index")).unwrap(),
            index
        );
    }
    fixture
        .command("add")
        .args(["--overlay", "app", "--target"])
        .arg(&fixture.target)
        .arg(fixture.target.join("plain"))
        .assert()
        .success();
    fixture
        .command("commit")
        .args(["--overlay", "app", "--target"])
        .arg(&fixture.target)
        .args(["-m", "ordinary edit"])
        .arg(fixture.target.join("plain"))
        .assert()
        .success();
    assert_eq!(
        fs::read(fixture.source.join("app/plain")).unwrap(),
        b"ordinary edit\n"
    );
    assert!(
        fs::read_to_string(fixture.source.join("app/config.jinja"))
            .unwrap()
            .contains("{{ data.hostname }}")
    );
    fixture.sync().assert().success();
}

#[test]
fn a_directory_link_with_templates_expands_to_generated_leaf_links() {
    let fixture = Fixture::new(false);
    fs::create_dir_all(fixture.source.join("app/nested")).unwrap();
    fs::rename(
        fixture.source.join("app/config.jinja"),
        fixture.source.join("app/nested/config.jinja"),
    )
    .unwrap();
    let descriptor = fixture.source.join("app/over.yaml");
    let config = fs::read_to_string(&descriptor)
        .unwrap()
        .replace("mode: symlink", "mode: symlink-directory");
    fs::write(descriptor, config).unwrap();
    fixture.apply();
    assert!(fixture.target.join("nested").is_dir());
    assert!(!fixture.target.join("nested").is_symlink());
    assert!(fixture.target.join("nested/config").is_symlink());
    assert!(
        fs::read_to_string(fixture.target.join("nested/config"))
            .unwrap()
            .contains("host=first")
    );
}

#[test]
fn a_literal_target_rule_keeps_its_explicit_destination_for_a_template() {
    let fixture = Fixture::new(false);
    let descriptor = fixture.source.join("app/over.yaml");
    let mut config = fs::read_to_string(&descriptor).unwrap();
    config.push_str("  - {path: config.jinja, target: '~/renamed'}\n");
    fs::write(descriptor, config).unwrap();
    fixture.apply();
    assert!(fixture.target.join("renamed").is_symlink());
    assert!(
        fs::read_to_string(fixture.target.join("renamed"))
            .unwrap()
            .contains("host=first")
    );
    assert!(!fixture.target.join("config").exists());
}

#[test]
fn a_source_template_deletion_preserves_local_edits_until_explicit_restore() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let destination = fixture.target.join("config");
    let local = "keep my local generated edit\n";
    fs::write(&destination, local).unwrap();
    fs::remove_file(fixture.source.join("app/config.jinja")).unwrap();
    commit_all(&fixture.repo());
    fixture.sync().assert().failure();
    let snapshot = fixture.snapshot();
    for _ in 0..2 {
        fixture
            .command("status")
            .args(["app", "--target"])
            .arg(&fixture.target)
            .assert()
            .failure()
            .stdout(contains("conflict"));
        fixture
            .sync()
            .assert()
            .failure()
            .stdout(contains("config.jinja"));
        assert_eq!(fixture.snapshot(), snapshot);
    }
    assert_eq!(fs::read_to_string(&destination).unwrap(), local);
    fixture
        .command("add")
        .args(["--overlay", "app", "--target"])
        .arg(&fixture.target)
        .arg(&destination)
        .assert()
        .failure()
        .stderr(contains("source template"));
    fixture
        .command("restore")
        .args(["--overlay", "app", "--target"])
        .arg(&fixture.target)
        .arg(&destination)
        .assert()
        .success();
    assert!(!destination.exists());
    fixture.sync().assert().success();
}

#[test]
fn an_interrupted_ref_publication_does_not_apply_a_local_merge_twice() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let first = fixture.snapshot();
    let destination = fixture.target.join("config");
    fs::write(
        &destination,
        fs::read_to_string(&destination)
            .unwrap()
            .replace("first=base", "first=local"),
    )
    .unwrap();
    let source = fixture.source.join("app/config.jinja");
    fs::write(
        &source,
        fs::read_to_string(&source)
            .unwrap()
            .replace("last=base", "last=upstream"),
    )
    .unwrap();
    commit_all(&fixture.repo());
    fixture.sync().assert().success();
    let second = fixture.snapshot();
    let merged = fs::read(&destination).unwrap();
    let state = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let mut metadata: toml::Value = toml::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    let record = metadata["state"]["checkouts"]
        .as_table_mut()
        .unwrap()
        .iter_mut()
        .next()
        .unwrap()
        .1;
    let generated = record["generated"].as_table_mut().unwrap();
    generated.insert("base".into(), first.to_string().into());
    generated.insert("pending".into(), second.to_string().into());
    generated.insert(
        "applied".into(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "config".into(),
            format!(
                "{}:{}",
                git2::Oid::hash_object(git2::ObjectType::Blob, &merged).unwrap(),
                0o100644
            )
            .into(),
        )])),
    );
    fs::write(&state, toml::to_string(&metadata).unwrap()).unwrap();
    fixture.sync().assert().success();
    assert_eq!(fs::read(destination).unwrap(), merged);
    assert_eq!(fixture.snapshot(), second);
    let metadata: toml::Value = toml::from_str(&fs::read_to_string(state).unwrap()).unwrap();
    let record = metadata["state"]["checkouts"]
        .as_table()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert!(record["generated"].get("pending").is_none());
    assert_eq!(
        record["generated"]["base"].as_str(),
        Some(second.to_string().as_str())
    );
}

#[test]
fn enabling_templates_migrates_an_existing_source_based_checkout_without_changing_its_source() {
    let fixture = Fixture::new(true);
    let descriptor = fixture.source.join("app/over.yaml");
    let config = fs::read_to_string(&descriptor).unwrap();
    fs::write(&descriptor, "mode: checkout\ndata: {hostname: first}\n").unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    let original = fs::read(fixture.source.join("app/config.jinja")).unwrap();
    assert_eq!(
        fs::read(fixture.target.join("config.jinja")).unwrap(),
        original
    );
    fs::write(descriptor, config).unwrap();
    commit_all(&fixture.repo());
    fixture.sync().assert().success();
    assert!(!fixture.target.join("config.jinja").exists());
    assert!(
        fs::read_to_string(fixture.target.join("config"))
            .unwrap()
            .contains("host=first")
    );
    assert_eq!(
        fs::read(fixture.source.join("app/config.jinja")).unwrap(),
        original
    );
    assert!(fixture.repo().find_commit(fixture.snapshot()).is_ok());
}

#[rstest]
#[case(false)]
#[case(true)]
fn rendered_destination_collisions_fail_before_materialization(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    fs::write(fixture.source.join("app/config"), "a different source\n").unwrap();
    commit_all(&fixture.repo());
    fixture
        .command("apply")
        .args(["app", "--no-prompt", "--target"])
        .arg(&fixture.target)
        .assert()
        .failure();
    assert!(!fixture.target.exists());
    assert!(!fixture.temp.path().join("state").exists());
    assert_eq!(
        fixture
            .repo()
            .references_glob("refs/over/generated/*")
            .unwrap()
            .count(),
        0
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn force_cannot_render_over_a_tracked_source_file(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    let descriptor = fixture.source.join("app/over.yaml");
    let config = fs::read_to_string(&descriptor)
        .unwrap()
        .replace("strip_extension: true", "strip_extension: false");
    fs::write(descriptor, config).unwrap();
    commit_all(&fixture.repo());
    let source = fixture.source.join("app/config.jinja");
    let original = fs::read(&source).unwrap();
    fixture
        .command("apply")
        .args(["app", "--force", "--no-prompt", "--target"])
        .arg(fixture.source.join("app"))
        .assert()
        .failure()
        .stderr(contains("source template"));
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(!source.is_symlink());
}

#[test]
fn rendering_directory_components_does_not_materialize_literal_jinja_scaffolds() {
    let fixture = Fixture::new(false);
    let directory = fixture.source.join("app/{{ data.hostname }}");
    fs::create_dir_all(&directory).unwrap();
    fs::rename(
        fixture.source.join("app/config.jinja"),
        directory.join("config.jinja"),
    )
    .unwrap();
    fixture.apply();
    assert!(fixture.target.join("first/config").is_file());
    assert!(!fixture.target.join("{{ data.hostname }}").exists());
}

#[test]
fn changing_only_context_rerenders_checkout_and_metadata_does_not_duplicate_values() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let first = fixture.snapshot();
    let descriptor = fixture.source.join("app/over.yaml");
    let contents = fs::read_to_string(&descriptor)
        .unwrap()
        .replace("hostname: first", "hostname: PRIVATE_RENDER_SENTINEL");
    fs::write(descriptor, contents).unwrap();
    let head = fixture.repo().head().unwrap().target();
    fixture.sync().assert().success();
    assert_ne!(first, fixture.snapshot());
    assert!(
        fs::read_to_string(fixture.target.join("config"))
            .unwrap()
            .contains("PRIVATE_RENDER_SENTINEL")
    );
    assert_eq!(fixture.repo().head().unwrap().target(), head);
    let metadata =
        fs::read_to_string(fixture.temp.path().join("state/over/virtual_checkout.toml")).unwrap();
    assert!(!metadata.contains("PRIVATE_RENDER_SENTINEL"));
    assert!(!metadata.contains("first=base"));
    let assertion = fixture
        .command("apply")
        .args(["app", "--debug", "--no-prompt", "--target"])
        .arg(&fixture.target)
        .assert()
        .success();
    let output = assertion.get_output();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE_RENDER_SENTINEL"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE_RENDER_SENTINEL"));
}

#[rstest]
#[case("~/config")]
#[case("~/config/child")]
fn rendered_checkout_leaves_cannot_overlap_independently_mapped_targets(#[case] target: &str) {
    let fixture = Fixture::new(true);
    fs::write(fixture.source.join("app/special"), "independent source").unwrap();
    let descriptor = fixture.source.join("app/over.yaml");
    let config = format!(
        "{}  - {{path: special, target: '{target}', mode: symlink}}\n",
        fs::read_to_string(&descriptor).unwrap()
    );
    fs::write(descriptor, config).unwrap();
    commit_all(&fixture.repo());
    fixture
        .command("apply")
        .args(["app", "--no-prompt", "--target"])
        .arg(&fixture.target)
        .assert()
        .failure()
        .stderr(contains("overlaps independently managed target"));
    assert!(!fixture.target.exists());
    assert!(!fixture.temp.path().join("state").exists());
}

#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
fn disabling_rendering_restores_ordinary_source_ingestion_after_safe_resolution(
    #[case] conflicting: bool,
    #[case] interrupted: bool,
) {
    let fixture = Fixture::new(true);
    let source = fixture.source.join("app/config");
    fs::rename(fixture.source.join("app/config.jinja"), &source).unwrap();
    let descriptor = fixture.source.join("app/over.yaml");
    fs::write(
        &descriptor,
        fs::read_to_string(&descriptor)
            .unwrap()
            .replace("**/*.jinja", "config"),
    )
    .unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    let state_path = fixture.temp.path().join("state/over/virtual_checkout.toml");
    let initial: toml::Value = toml::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let initial_generated = initial["state"]["checkouts"]
        .as_table()
        .unwrap()
        .values()
        .next()
        .unwrap()["generated"]
        .clone();
    if conflicting {
        let target = fixture.target.join("config");
        fs::write(
            &target,
            fs::read_to_string(&target)
                .unwrap()
                .replace("host=first", "host=local"),
        )
        .unwrap();
    }
    fs::write(descriptor, "mode: checkout\ndata: {hostname: first}\n").unwrap();
    commit_all(&fixture.repo());
    if conflicting {
        fixture.sync().assert().failure();
        fixture
            .command("add")
            .args(["--overlay", "app", "--target"])
            .arg(&fixture.target)
            .arg(fixture.target.join("config"))
            .assert()
            .failure()
            .stderr(contains("source template"));
        fixture.restore("config", &[]);
    } else {
        fixture.sync().assert().success();
    }
    if interrupted {
        let snapshot = fixture.snapshot();
        let mut document: toml::Value =
            toml::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        let record = document["state"]["checkouts"]
            .as_table_mut()
            .unwrap()
            .iter_mut()
            .next()
            .unwrap()
            .1;
        let generated = record["generated"].as_table_mut().unwrap();
        // Recreate the journal after file/ref publication but before provenance retirement.
        generated.insert("base".into(), initial_generated["base"].clone());
        generated.insert("sources".into(), initial_generated["sources"].clone());
        generated.insert("pending".into(), snapshot.to_string().into());
        generated.insert(
            "pending_sources".into(),
            toml::Value::Table(Default::default()),
        );
        fs::write(&state_path, toml::to_string(&document).unwrap()).unwrap();
        fixture
            .command("add")
            .args(["--overlay", "app", "--target"])
            .arg(&fixture.target)
            .arg(fixture.target.join("config"))
            .assert()
            .failure()
            .stderr(contains("source template"));
        fixture.sync().assert().success();
        assert_eq!(fixture.snapshot(), snapshot);
    }
    assert_eq!(
        fs::read(fixture.target.join("config")).unwrap(),
        fs::read(&source).unwrap()
    );
    fs::write(fixture.target.join("config"), "ordinary local edit").unwrap();
    fixture.stage("config");
    fixture
        .command("commit")
        .args(["--overlay", "app", "--target"])
        .arg(&fixture.target)
        .args(["-m", "record ordinary conversion"])
        .arg(fixture.target.join("config"))
        .assert()
        .success();
    assert_eq!(fs::read(source).unwrap(), b"ordinary local edit");
}

#[test]
fn staged_ordinary_changes_keep_a_generated_checkout_modified() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.target.join("plain"), "staged ordinary edit").unwrap();
    fixture.stage("plain");
    fixture
        .command("unapply")
        .args(["app", "--dry-run", "--target"])
        .arg(&fixture.target)
        .assert()
        .failure()
        .stdout(contains("modified"));
    assert!(fixture.target.join("config").exists());
    assert_eq!(
        fs::read(fixture.target.join("plain")).unwrap(),
        b"staged ordinary edit"
    );
}

#[cfg(unix)]
#[test]
fn restoring_generated_executables_preserves_snapshot_executable_bits() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(true);
    fs::set_permissions(
        fixture.source.join("app/config.jinja"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    commit_all(&fixture.repo());
    fixture.apply();
    fs::write(fixture.target.join("config"), "local script edit").unwrap();
    fixture.restore("config", &[]);
    assert_eq!(
        fs::metadata(fixture.target.join("config"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o711
    );
    fixture.sync().assert().success();
}

#[test]
fn ordinary_restore_keeps_index_and_source_semantics_in_a_template_checkout() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let snapshot = fixture.snapshot();
    fs::write(fixture.target.join("plain"), "staged ordinary edit").unwrap();
    fixture.stage("plain");
    fs::write(fixture.target.join("plain"), "later unstaged edit").unwrap();
    fs::write(fixture.target.join("config"), "preserved generated edit").unwrap();
    let index = fs::read(fixture.repo().path().join("index")).unwrap();
    fixture.restore("plain", &[]);
    assert_eq!(
        fs::read(fixture.target.join("plain")).unwrap(),
        b"staged ordinary edit"
    );
    assert_eq!(
        fs::read(fixture.repo().path().join("index")).unwrap(),
        index
    );
    fixture.restore("plain", &["--staged"]);
    assert_eq!(
        fs::read(fixture.target.join("plain")).unwrap(),
        b"staged ordinary edit"
    );
    let repo = fixture.repo();
    let indexed = repo
        .index()
        .unwrap()
        .get_path(Path::new("app/plain"), 0)
        .unwrap();
    assert_eq!(repo.find_blob(indexed.id).unwrap().content(), b"ordinary\n");
    fixture.restore("plain", &["--source", "HEAD"]);
    assert_eq!(
        fs::read(fixture.target.join("plain")).unwrap(),
        b"ordinary\n"
    );
    assert_eq!(
        fs::read(fixture.target.join("config")).unwrap(),
        b"preserved generated edit"
    );
    assert_eq!(fixture.snapshot(), snapshot);
}

#[rstest]
#[case(false)]
#[case(true)]
fn syncing_staged_ordinary_files_requires_consent_even_when_generated_edits_exist(
    #[case] push_only: bool,
) {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.target.join("plain"), "staged ordinary edit").unwrap();
    fixture.stage("plain");
    fs::write(fixture.target.join("config"), "preserved generated edit").unwrap();
    let head = fixture.repo().head().unwrap().target();
    let mut command = fixture.sync();
    if push_only {
        command.arg("--push-only");
    }
    command
        .assert()
        .failure()
        .stdout(contains("no-prompt disables the sync-time"));
    assert_eq!(fixture.repo().head().unwrap().target(), head);
    assert_eq!(
        fs::read(fixture.target.join("config")).unwrap(),
        b"preserved generated edit"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn sync_can_commit_an_ordinary_sibling_with_consent_without_recording_generated_edits() {
    let fixture = Fixture::new(true);
    fixture.apply();
    fs::write(fixture.target.join("plain"), "staged ordinary edit").unwrap();
    fixture.stage("plain");
    fs::write(fixture.target.join("config"), "preserved generated edit").unwrap();
    let snapshot = fixture.snapshot();
    let output = Command::new("script")
        .args(["--quiet", "--return", "--echo", "never", "--command", "exec \"$OVER_TEST_BINARY\" --home \"$OVER_TEST_SOURCE\" sync app --push-only --target \"$OVER_TEST_TARGET\"", "/dev/null"])
        .env("OVER_TEST_BINARY", assert_cmd::cargo::cargo_bin!("over"))
        .env("OVER_TEST_SOURCE", &fixture.source)
        .env("OVER_TEST_TARGET", &fixture.target)
        .env("XDG_STATE_HOME", fixture.temp.path().join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", fixture.temp.path().join("gitconfig"))
        .env("NO_COLOR", "1").env("TERM", "dumb")
        .write_stdin(b"\nrecord ordinary sibling\n")
        .timeout(std::time::Duration::from_secs(10)).output().unwrap();
    let transcript = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{transcript}");
    assert!(transcript.contains("1 staged file"), "{transcript}");
    assert_eq!(
        fixture
            .repo()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .message(),
        Ok("record ordinary sibling")
    );
    assert_eq!(
        fs::read(fixture.source.join("app/plain")).unwrap(),
        b"staged ordinary edit"
    );
    assert_eq!(
        fs::read(fixture.target.join("config")).unwrap(),
        b"preserved generated edit"
    );
    assert!(
        fs::read_to_string(fixture.source.join("app/config.jinja"))
            .unwrap()
            .contains("{{ data.hostname }}")
    );
    assert_eq!(fixture.snapshot(), snapshot);
}
