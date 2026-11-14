use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
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

impl Fixture {
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
        let mut command = Command::cargo_bin("over").unwrap();
        command
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .arg("--home")
            .arg(&self.source)
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

#[test]
fn overlapping_checkout_edits_remain_conflicted_after_the_desired_ref_advances() {
    let fixture = Fixture::new(true);
    fixture.apply();
    let first = fixture.snapshot();
    let source = fixture.source.join("app/config.jinja");
    let original = fs::read_to_string(&source).unwrap();
    fs::write(&source, original.replace("first=base", "first=upstream")).unwrap();
    commit_all(&fixture.repo());
    let destination = fixture.target.join("config");
    let local = fs::read_to_string(&destination)
        .unwrap()
        .replace("first=base", "first=local");
    fs::write(&destination, &local).unwrap();
    fixture
        .sync()
        .assert()
        .failure()
        .stdout(contains("source template"));
    let second = fixture.snapshot();
    assert_ne!(first, second);
    assert_eq!(fs::read_to_string(&destination).unwrap(), local);
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

#[test]
fn reporting_and_previews_do_not_write_generated_objects_or_state() {
    let fixture = Fixture::new(true);
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
    assert!(!fixture.temp.path().join("state").exists());
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
    commit_all(&repo);
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
