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
