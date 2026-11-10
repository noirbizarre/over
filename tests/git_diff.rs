//! Compare Over's views with Git rather than duplicating its patch renderer.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pretty_assertions::assert_eq;
use rstest::rstest;
use tempfile::TempDir;

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
        fs::create_dir_all(source.join("ov")).unwrap();
        fs::write(
            source.join("ov/over.toml"),
            if checkout { "mode = 'checkout'\n" } else { "" },
        )
        .unwrap();
        fs::write(source.join("ov/file"), "base\n").unwrap();
        fs::write(source.join("ov/deleted"), "delete me\n").unwrap();
        fs::write(source.join("unrelated"), "unrelated base\n").unwrap();
        fs::write(temp.path().join("gitconfig"), "").unwrap();
        let fixture = Self {
            temp,
            source,
            target,
        };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.name", "Test"]);
        fixture.git(&["config", "user.email", "test@example.com"]);
        fixture.git(&["config", "color.ui", "false"]);
        fixture.git(&["config", "core.autocrlf", "false"]);
        fixture.commit();
        fixture.over("apply", &["--no-prompt"]);
        fixture
    }

    fn environment(&self, command: &mut Command) {
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("GIT_PAGER", "cat");
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_EXTERNAL_DIFF",
        ] {
            command.env_remove(name);
        }
    }

    fn git(&self, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        command.current_dir(&self.source).args(args);
        self.environment(&mut command);
        checked(command.output().unwrap())
    }

    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", "snapshot"]);
    }

    fn command(&self, subcommand: &str, args: &[&str]) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        self.environment(&mut command);
        command
            .arg("--home")
            .arg(&self.source)
            .arg(subcommand)
            .arg("ov")
            .arg("--target")
            .arg(&self.target)
            .args(args);
        command
    }

    fn over(&self, subcommand: &str, args: &[&str]) -> Output {
        checked(self.command(subcommand, args).output().unwrap())
    }

    fn add_target(&self, name: &str) {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        self.environment(&mut command);
        checked(
            command
                .arg("--home")
                .arg(&self.source)
                .arg("add")
                .arg("--overlay")
                .arg("ov")
                .arg("--target")
                .arg(&self.target)
                .arg(self.target.join(name))
                .output()
                .unwrap(),
        );
    }
}

fn checked(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

// Content inventories also detect unexpected new objects/refs/locks, unlike a
// HEAD/index-only assertion which misses otherwise invisible repository writes.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    kind: SnapshotKind,
    permissions: u32,
}

#[derive(Debug, PartialEq, Eq)]
enum SnapshotKind {
    File(Vec<u8>),
    Directory,
    Symlink(PathBuf),
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Snapshot> {
    if !root.exists() {
        return BTreeMap::new();
    }
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            #[cfg(unix)]
            let permissions = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o7777
            };
            #[cfg(not(unix))]
            let permissions = u32::from(metadata.permissions().readonly());
            let kind = if metadata.is_symlink() {
                SnapshotKind::Symlink(fs::read_link(entry.path()).unwrap())
            } else if metadata.is_dir() {
                SnapshotKind::Directory
            } else {
                SnapshotKind::File(fs::read(entry.path()).unwrap())
            };
            (
                entry.path().strip_prefix(root).unwrap().to_path_buf(),
                Snapshot { kind, permissions },
            )
        })
        .collect()
}

#[rstest]
#[case(false)]
#[case(true)]
fn staged_and_unstaged_views_match_git_and_leave_all_inputs_unchanged(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    let deployed = if checkout {
        fixture.target.clone()
    } else {
        fixture.source.join("ov")
    };
    fs::write(deployed.join("file"), "staged\n").unwrap();
    if checkout {
        fixture.add_target("file");
    } else {
        fixture.git(&["add", "ov/file"]);
    }
    fs::write(deployed.join("file"), "unstaged\n").unwrap();
    fs::remove_file(deployed.join("deleted")).unwrap();
    fs::write(fixture.source.join("unrelated"), "unrelated staged\n").unwrap();
    fixture.git(&["add", "unrelated"]);
    let before_source = inventory(&fixture.source);
    let before_target = inventory(&fixture.target);
    let before_state = inventory(&fixture.temp.path().join("state"));
    let staged = fixture.over("diff", &["--staged"]);
    let unstaged = fixture.over("diff", &[]);
    let expected_staged = fixture.git(&["diff", "--cached", "--", "ov/file", "ov/deleted"]);
    assert_eq!(staged.stdout, expected_staged.stdout);
    if checkout {
        // Project the independent target into a disposable real Git worktree
        // solely as a reference oracle; never change the fixture under test.
        let oracle = fixture.temp.path().join("oracle");
        fs::create_dir(&oracle).unwrap();
        for (path, snapshot) in &before_source {
            let destination = oracle.join(path);
            match &snapshot.kind {
                SnapshotKind::Directory => fs::create_dir_all(destination).unwrap(),
                SnapshotKind::File(content) => {
                    fs::create_dir_all(destination.parent().unwrap()).unwrap();
                    fs::write(destination, content).unwrap();
                }
                SnapshotKind::Symlink(target) => {
                    fs::create_dir_all(destination.parent().unwrap()).unwrap();
                    if fixture.source.join(path).is_dir() {
                        symlink::symlink_dir(target, destination).unwrap();
                    } else {
                        symlink::symlink_file(target, destination).unwrap();
                    }
                }
            }
        }
        fs::write(oracle.join("ov/file"), "unstaged\n").unwrap();
        fs::remove_file(oracle.join("ov/deleted")).unwrap();
        let mut command = Command::new("git");
        fixture.environment(&mut command);
        let expected = checked(
            command
                .current_dir(oracle)
                .args(["diff", "--", "ov/file", "ov/deleted"])
                .output()
                .unwrap(),
        );
        assert_eq!(unstaged.stdout, expected.stdout);
    } else {
        assert_eq!(
            unstaged.stdout,
            fixture.git(&["diff", "--", "ov/file", "ov/deleted"]).stdout
        );
    }
    let both = fixture.over("diff", &["--all-changes"]);
    assert_eq!(both.stdout, [staged.stdout, unstaged.stdout].concat());
    assert_eq!(before_source, inventory(&fixture.source));
    assert_eq!(before_target, inventory(&fixture.target));
    assert_eq!(before_state, inventory(&fixture.temp.path().join("state")));
}

#[test]
fn virtual_staged_changes_use_the_recorded_base_after_source_head_advances() {
    let fixture = Fixture::new(true);
    fs::write(fixture.source.join("ov/file"), "advanced source\n").unwrap();
    fixture.commit();
    let staged = fixture.over("diff", &["--cached"]);
    assert_eq!(
        staged.stdout,
        fixture
            .git(&["diff", "HEAD~1", "HEAD", "--", "ov/file"])
            .stdout
    );
    let unstaged = fixture.over("diff", &[]);
    assert_eq!(
        unstaged.stdout,
        fixture
            .git(&["diff", "HEAD", "HEAD~1", "--", "ov/file"])
            .stdout
    );
    assert!(String::from_utf8_lossy(&staged.stderr).contains("recorded base"));
}

#[test]
fn virtual_additions_deletions_binary_and_missing_newlines_have_real_git_patches() {
    let fixture = Fixture::new(true);
    fs::write(fixture.target.join("new"), "new without newline").unwrap();
    fs::write(fixture.target.join("file"), b"binary\0content").unwrap();
    fs::remove_file(fixture.target.join("deleted")).unwrap();
    let output = fixture.over("diff", &[]);
    let patch = String::from_utf8(output.stdout).unwrap();
    assert!(patch.contains("new file mode 100644"));
    assert!(patch.contains("deleted file mode 100644"));
    assert!(patch.contains("Binary files"));
    assert!(patch.contains("\\ No newline at end of file"));
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn executable_changes_survive_both_views(#[case] checkout: bool) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(checkout);
    let file = if checkout {
        fixture.target.join("file")
    } else {
        fixture.source.join("ov/file")
    };
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture.over("diff", &[]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("new mode 100755"));
    if checkout {
        fixture.add_target("file");
    } else {
        fixture.git(&["add", "ov/file"]);
    }
    let output = fixture.over("diff", &["--staged"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("new mode 100755"));
    assert!(fixture.over("diff", &[]).stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn misdirected_links_are_diagnostics_while_content_comes_from_the_source_repository() {
    let fixture = Fixture::new(false);
    fs::remove_file(fixture.target.join("file")).unwrap();
    std::os::unix::fs::symlink("elsewhere", fixture.target.join("file")).unwrap();
    fs::write(fixture.source.join("ov/file"), "source change\n").unwrap();
    let output = fixture.over("diff", &[]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("misdirected symlink"));
    assert_eq!(
        output.stdout,
        fixture.git(&["diff", "--", "ov/file", "ov/deleted"]).stdout
    );
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn configured_external_diff_and_difftool_receive_git_inputs(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    let file = if checkout {
        fixture.target.join("file")
    } else {
        fixture.source.join("ov/file")
    };
    fs::write(file, "changed\n").unwrap();
    fixture.git(&["config", "diff.tool", "over-test"]);
    fixture.git(&["config", "difftool.prompt", "false"]);
    fixture.git(&[
        "config",
        "difftool.over-test.cmd",
        "printf 'TOOL\\n'; cat \"$LOCAL\" \"$REMOTE\"",
    ]);
    let normal = fixture.over("diff", &[]);
    assert!(!String::from_utf8_lossy(&normal.stdout).contains("TOOL"));
    let tool = fixture.over("diff", &["--difftool"]);
    assert_eq!(tool.stdout, b"TOOL\nbase\nchanged\n");
    fixture.git(&[
        "config",
        "diff.external",
        "printf 'EXTERNAL\\n'; cat \"$2\" \"$5\"; :",
    ]);
    let external = fixture.over("diff", &[]);
    assert!(String::from_utf8_lossy(&external.stdout).starts_with("EXTERNAL\n"));
}

#[test]
fn literal_paths_and_empty_selections_never_expand_to_other_overlay_changes() {
    let fixture = Fixture::new(false);
    fs::write(fixture.source.join("ov/[special]"), "before\n").unwrap();
    fixture.commit();
    fs::write(fixture.source.join("ov/[special]"), "after\n").unwrap();
    fs::write(fixture.source.join("unrelated"), "hidden change\n").unwrap();
    assert_eq!(
        fixture.over("diff", &[]).stdout,
        fixture
            .git(&["--literal-pathspecs", "diff", "--", "ov/[special]"])
            .stdout
    );
    fs::write(fixture.source.join(".overignore"), "ov/**\n").unwrap();
    assert!(fixture.over("diff", &[]).stdout.is_empty());
}

#[test]
fn target_clean_filters_use_source_attributes_without_creating_false_changes() {
    let fixture = Fixture::new(true);
    fs::write(
        fixture.source.join(".gitattributes"),
        "ov/file text eol=lf\n",
    )
    .unwrap();
    fs::write(fixture.target.join("file"), "base\r\n").unwrap();
    assert!(fixture.over("diff", &[]).stdout.is_empty());
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn textconv_uses_git_configuration_without_persisting_cache_refs(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    fs::write(
        fixture.source.join(".gitattributes"),
        "ov/file diff=example\n",
    )
    .unwrap();
    fixture.git(&["config", "diff.example.textconv", "sed s/base/BASE/g"]);
    fixture.git(&["config", "diff.example.cachetextconv", "true"]);
    let file = if checkout {
        fixture.target.join("file")
    } else {
        fixture.source.join("ov/file")
    };
    fs::write(file, "changed\n").unwrap();
    let before = inventory(&fixture.source);
    let output = fixture.over("diff", &[]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("-BASE\n+changed"));
    assert_eq!(before, inventory(&fixture.source));
}

#[test]
fn a_missing_checkout_association_is_diagnostic_and_does_not_create_state() {
    let fixture = Fixture::new(true);
    fs::remove_dir_all(fixture.temp.path().join("state")).unwrap();
    let output = fixture.over("diff", &["--staged"]);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no recorded base"));
    assert!(!fixture.temp.path().join("state").exists());
}

#[test]
fn unborn_symlink_repositories_show_staged_additions_like_git() {
    let fixture = Fixture::new(false);
    let branch = String::from_utf8(fixture.git(&["symbolic-ref", "HEAD"]).stdout).unwrap();
    fixture.git(&["update-ref", "-d", branch.trim()]);
    assert_eq!(
        fixture.over("diff", &["--staged"]).stdout,
        fixture
            .git(&["diff", "--cached", "--", "ov/file", "ov/deleted"])
            .stdout
    );
}

#[test]
fn unrelated_index_conflicts_do_not_block_virtual_checkout_views() {
    let fixture = Fixture::new(true);
    let repo = git2::Repository::open(&fixture.source).unwrap();
    let mut index = repo.index().unwrap();
    let mut item = index.get_path(Path::new("unrelated"), 0).unwrap();
    index.remove_path(Path::new("unrelated")).unwrap();
    item.flags = (item.flags & !0x3000) | 0x1000;
    index.add(&item).unwrap();
    index.write().unwrap();
    fs::write(fixture.target.join("file"), "owned edit\n").unwrap();
    let before = fs::read(fixture.source.join(".git/index")).unwrap();
    let output = fixture.over("diff", &["--all-changes"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("+owned edit"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("unrelated"));
    assert_eq!(before, fs::read(fixture.source.join(".git/index")).unwrap());
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn failing_tools_report_errors_and_release_their_temporary_inputs(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    let file = if checkout {
        fixture.target.join("file")
    } else {
        fixture.source.join("ov/file")
    };
    fs::write(file, "changed\n").unwrap();
    fixture.git(&["config", "diff.tool", "failing"]);
    fixture.git(&["config", "difftool.prompt", "false"]);
    fixture.git(&["config", "difftool.trustExitCode", "true"]);
    let marker = fixture.temp.path().join("inputs");
    fixture.git(&[
        "config",
        "difftool.failing.cmd",
        &format!(
            "printf '%s\\n' \"$LOCAL\" \"$REMOTE\" > '{}'; exit 2",
            marker.display()
        ),
    ]);
    let before = inventory(&fixture.source);
    let output = fixture.command("diff", &["--difftool"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Git diff presentation failed"));
    for path in fs::read_to_string(marker).unwrap().lines() {
        if Path::new(path).is_absolute() {
            assert!(!Path::new(path).exists());
        }
    }
    assert_eq!(before, inventory(&fixture.source));
}

#[cfg(target_os = "linux")]
#[test]
fn pager_diff_overrides_core_pager_in_an_interactive_terminal() {
    let fixture = Fixture::new(true);
    fs::write(fixture.target.join("file"), "changed\n").unwrap();
    fixture.git(&["config", "core.pager", "printf 'CORE-PAGER\\n'; cat"]);
    fixture.git(&["config", "pager.diff", "printf 'DIFF-PAGER\\n'; cat"]);
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    let shell = format!(
        "{} --home {} diff ov --target {}",
        quote(Path::new(assert_cmd::cargo::cargo_bin!("over"))),
        quote(&fixture.source),
        quote(&fixture.target)
    );
    let mut command = Command::new("script");
    fixture.environment(&mut command);
    command.env_remove("GIT_PAGER").env("TERM", "xterm");
    let output = checked(
        command
            .args(["-q", "-e", "-c", &shell, "/dev/null"])
            .output()
            .unwrap(),
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("DIFF-PAGER"), "{text}");
    assert!(!text.contains("CORE-PAGER"));
}

#[rstest]
#[case(false)]
#[case(true)]
fn staged_additions_and_deletions_remain_distinct_from_later_target_edits(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    let deployed = if checkout {
        fixture.target.clone()
    } else {
        fixture.source.join("ov")
    };
    fs::write(deployed.join("new"), "staged addition\n").unwrap();
    fs::remove_file(deployed.join("deleted")).unwrap();
    if checkout {
        fixture.add_target(".");
    } else {
        fixture.git(&["add", "ov"]);
    }
    fs::write(deployed.join("new"), "later edit\n").unwrap();
    fs::write(deployed.join("deleted"), "recreated\n").unwrap();
    let staged = fixture.over("diff", &["--staged"]);
    assert_eq!(
        staged.stdout,
        fixture
            .git(&["diff", "--cached", "--", "ov/new", "ov/deleted"])
            .stdout
    );
    let output = fixture.over("diff", &[]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("-staged addition\n+later edit"));
    if checkout {
        assert!(text.contains("new file mode 100644"));
        assert!(text.contains("+recreated"));
    } else {
        // Recreating a staged deletion leaves an untracked file in a real
        // worktree, which ordinary git diff deliberately does not include.
        assert!(!text.contains("recreated"));
    }
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn read_only_sources_can_be_diffed_without_repository_or_target_writes(#[case] checkout: bool) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(checkout);
    let file = if checkout {
        fixture.target.join("file")
    } else {
        fixture.source.join("ov/file")
    };
    // This tests read-only inspection, not Git's stat cache: a different size
    // makes the edit observable even when filesystem timestamps are coarse.
    fs::write(file, "edited content\n").unwrap();
    let before = inventory(&fixture.source);
    let mut permissions = Vec::new();
    for entry in walkdir::WalkDir::new(&fixture.source) {
        let entry = entry.unwrap();
        let mode = entry.metadata().unwrap().permissions().mode();
        permissions.push((entry.path().to_path_buf(), mode));
        fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode & !0o222)).unwrap();
    }
    let read_only_before = inventory(&fixture.source);
    let output = fixture.command("diff", &["--all-changes"]).output();
    let read_only_after = inventory(&fixture.source);
    // Restore access before assertions so even a failing test cleans its fixture.
    for (path, mode) in permissions {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    let output = checked(output.unwrap());
    assert_eq!(read_only_before, read_only_after);
    assert!(String::from_utf8_lossy(&output.stdout).contains("+edited content"));
    assert_eq!(before, inventory(&fixture.source));
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn linked_worktrees_keep_their_own_index_and_conditional_git_configuration(#[case] checkout: bool) {
    let fixture = Fixture::new(checkout);
    let linked = fixture.temp.path().join("linked");
    fixture.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        linked.to_str().unwrap(),
    ]);
    let gitdir = git2::Repository::open(&linked)
        .unwrap()
        .path()
        .to_path_buf();
    let include = fixture.temp.path().join("conditional-config");
    fs::write(
        &include,
        "[diff]\nexternal = \"printf 'WORKTREE-CONFIG\\\\n'; :\"\n",
    )
    .unwrap();
    fixture.git(&[
        "config",
        &format!(
            "includeIf.gitdir:{}.path",
            gitdir.to_str().unwrap().trim_end_matches('/')
        ),
        include.to_str().unwrap(),
    ]);
    let fixture = Fixture {
        source: linked,
        target: fixture.temp.path().join("linked-target"),
        temp: fixture.temp,
    };
    fixture.over("apply", &["--no-prompt"]);
    let file = if checkout {
        fixture.target.join("file")
    } else {
        fixture.source.join("ov/file")
    };
    fs::write(file, "linked edit\n").unwrap();
    let output = fixture.over("diff", &[]);
    assert_eq!(output.stdout, b"WORKTREE-CONFIG\n");
}

#[test]
fn mixed_materializations_use_target_content_only_for_the_checkout_units() {
    let fixture = Fixture::new(false);
    fs::create_dir(fixture.source.join("ov/copied")).unwrap();
    fs::write(fixture.source.join("ov/copied/file"), "copied base\n").unwrap();
    fs::write(fixture.source.join("ov/over.toml"), "[[rules]]\npath = 'copied'\nmode = 'checkout'\n\n[[rules]]\npath = 'file'\nmode = 'checkout'\n").unwrap();
    fixture.commit();
    fixture.over("apply", &["--no-prompt"]);
    fs::write(fixture.target.join("copied/file"), "target edit\n").unwrap();
    fs::write(
        fixture.source.join("ov/copied/file"),
        "source edit must not appear\n",
    )
    .unwrap();
    fs::write(fixture.target.join("file"), "linked edit\n").unwrap();
    let output = fixture.over("diff", &[]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("+target edit"));
    assert!(text.contains("+linked edit"));
    assert!(!text.contains("source edit must not appear"));
    assert_eq!(
        text.matches("diff --git a/ov/copied/file b/ov/copied/file")
            .count(),
        1
    );
}

#[test]
fn selecting_all_overlays_diffs_shared_symlink_dependencies_once() {
    let fixture = Fixture::new(false);
    fs::create_dir(fixture.source.join("dep")).unwrap();
    fs::write(fixture.source.join("dep/over.toml"), "target = '~/dep'\n").unwrap();
    fs::write(fixture.source.join("dep/file"), "dependency base\n").unwrap();
    fs::write(fixture.source.join("ov/over.toml"), "uses = ['dep']\n").unwrap();
    fixture.commit();
    fixture.over("apply", &["--no-prompt"]);
    // Keep deduplication independent of same-size edits hitting Git's stat cache.
    fs::write(
        fixture.source.join("dep/file"),
        "dependency edited content\n",
    )
    .unwrap();
    let mut command = fixture.command("diff", &[]);
    // `--all` conflicts with an explicit overlay name, so construct that selector.
    let mut all = Command::new(assert_cmd::cargo::cargo_bin!("over"));
    fixture.environment(&mut all);
    let output = checked(
        all.arg("--home")
            .arg(&fixture.source)
            .args(["diff", "--all", "--target"])
            .arg(&fixture.target)
            .output()
            .unwrap(),
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .matches("+dependency edited content")
            .count(),
        1
    );
    let no_uses = checked(command.arg("--no-uses").output().unwrap());
    assert!(no_uses.stdout.is_empty());
}

#[test]
fn directory_links_use_source_git_and_report_breakage_after_source_deletion() {
    let fixture = Fixture::new(false);
    fs::create_dir(fixture.source.join("ov/bundle")).unwrap();
    fs::write(fixture.source.join("ov/bundle/file"), "directory base\n").unwrap();
    fs::write(
        fixture.source.join("ov/over.toml"),
        "[[rules]]\npath = 'bundle'\nmode = 'symlink-directory'\n",
    )
    .unwrap();
    fixture.commit();
    fixture.over("apply", &["--no-prompt"]);
    assert!(fixture.target.join("bundle").is_symlink());
    fs::write(fixture.target.join("bundle/file"), "directory edit\n").unwrap();
    assert_eq!(
        fixture.over("diff", &[]).stdout,
        fixture.git(&["diff", "--", "ov/bundle"]).stdout
    );
    fs::remove_dir_all(fixture.source.join("ov/bundle")).unwrap();
    let output = fixture.over("diff", &[]);
    assert_eq!(
        output.stdout,
        fixture.git(&["diff", "--", "ov/bundle"]).stdout
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("broken symlink"));
}

#[test]
fn intent_to_add_stays_unstaged_in_virtual_checkout_views() {
    let fixture = Fixture::new(true);
    fs::write(fixture.source.join("ov/new"), "intent content\n").unwrap();
    fs::write(fixture.target.join("new"), "intent content\n").unwrap();
    fixture.git(&["add", "--intent-to-add", "ov/new"]);
    let before = inventory(&fixture.source);
    assert_eq!(
        fixture.over("diff", &["--staged"]).stdout,
        fixture.git(&["diff", "--cached", "--", "ov/new"]).stdout
    );
    assert_eq!(
        fixture.over("diff", &[]).stdout,
        fixture.git(&["diff", "--", "ov/new"]).stdout
    );
    assert_eq!(before, inventory(&fixture.source));
}

#[rstest]
#[case(true)]
#[case(false)]
fn virtual_file_directory_replacements_show_deleted_and_added_content(
    #[case] directory_replaces_file: bool,
) {
    let fixture = Fixture::new(true);
    if !directory_replaces_file {
        fixture.over("unapply", &[]);
        fs::remove_file(fixture.source.join("ov/file")).unwrap();
        fs::create_dir(fixture.source.join("ov/file")).unwrap();
        fs::write(fixture.source.join("ov/file/child"), "base\n").unwrap();
        fixture.commit();
        // Reapply after removing the clean old deployment to record this baseline.
        fixture.over("apply", &["--no-prompt"]);
    }
    if directory_replaces_file {
        fs::remove_file(fixture.target.join("file")).unwrap();
        fs::create_dir(fixture.target.join("file")).unwrap();
        fs::write(fixture.target.join("file/child"), "replacement\n").unwrap();
    } else {
        fs::remove_dir_all(fixture.target.join("file")).unwrap();
        fs::write(fixture.target.join("file"), "replacement\n").unwrap();
    }
    let before = inventory(&fixture.target);
    let output = fixture.over("diff", &[]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("deleted file mode 100644"));
    assert!(text.contains("new file mode 100644"));
    assert!(text.contains("-base\n"));
    assert!(text.contains("+replacement\n"));
    assert_eq!(before, inventory(&fixture.target));
}
