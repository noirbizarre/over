//! First initialization merges committed paths into an existing container.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use git2::{Oid, Repository};

use crate::actions::fs::{ConflictChoice, ensure_unprotected_target, prompt_conflict, show_diff};
use crate::desired::{CheckoutScope, DesiredEntry, Provenance};
use crate::exec::Context;
use crate::materialize::disposal::{PathGuard, validate_relative_ancestors};

use super::{git, snapshot, state::VirtualCheckoutRecord};

#[derive(PartialEq, Eq)]
enum Node {
    Directory,
    File(Vec<u8>, i32),
    Link(PathBuf),
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Directory => f.write_str("Directory"),
            Self::File(_, mode) => f
                .debug_tuple("File")
                .field(&"[REDACTED]")
                .field(mode)
                .finish(),
            Self::Link(path) => f.debug_tuple("Link").field(path).finish(),
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Self::File(bytes, _) = self {
            use zeroize::Zeroize;
            bytes.zeroize();
        }
    }
}

type Inventory = BTreeMap<PathBuf, Node>;

fn join_relative(root: &Path, relative: &Path) -> PathBuf {
    // Joining an empty relative path adds a trailing separator, which makes
    // regular file reads/writes fail with a directory requirement on Unix.
    if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    }
}

// Inventories include empty directories and links, so consent cannot silently
// authorize new children or a changed symlink after the prompt.
fn inventory(path: &Path) -> Result<Inventory> {
    let mut result = BTreeMap::new();
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .follow_root_links(false)
    {
        let entry = entry?;
        let relative = entry.path().strip_prefix(path)?.to_path_buf();
        let node = if entry.file_type().is_symlink() {
            Node::Link(fs::read_link(entry.path())?)
        } else if entry.file_type().is_dir() {
            Node::Directory
        } else {
            ensure!(
                entry.file_type().is_file(),
                "unsupported entry '{}'",
                entry.path().display()
            );
            Node::File(
                fs::read(entry.path())?,
                i32::from(git::on_disk_filemode(entry.path())?),
            )
        };
        result.insert(relative, node);
    }
    Ok(result)
}

struct Group {
    path: PathBuf,
    before: Inventory,
    conflict: bool,
}

struct Initial {
    repo: Repository,
    managed: PathBuf,
    base: Oid,
    entries: BTreeMap<PathBuf, (Oid, i32)>,
    groups: Vec<Group>,
}

impl Initial {
    fn assess(source: &Path, target: &Path, scope: &CheckoutScope) -> Result<Self> {
        match target.symlink_metadata() {
            Ok(metadata) => ensure!(
                metadata.is_dir() && !metadata.is_symlink(),
                "virtual checkout root is not a real directory"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let (repo, managed) = git::discover_source(source)?;
        let base = repo.head()?.peel_to_commit()?.id();
        let entries = {
            let head = git::base_tree(&repo, Some(&base.to_string()))?;
            // Classification must not publish synthetic trees into the source
            // object database just to decide whether a prompt is necessary.
            git::scoped_entries(&repo, &head, &managed, scope)?
        };
        let mut groups: Vec<Group> = Vec::new();
        for (path, expected) in &entries {
            // Sorted tree paths keep descendants contiguous, so only the last
            // obstruction can cover this entry (avoid a quadratic full scan).
            if groups
                .last()
                .is_some_and(|group| path.starts_with(&group.path))
            {
                continue;
            }
            let mut current = PathBuf::new();
            let mut obstruction = None;
            for component in path.components() {
                current.push(component);
                match target.join(&current).symlink_metadata() {
                    Ok(metadata) if metadata.is_dir() && current != *path => {
                        // A shared container may hold repositories, but its nested
                        // checkout units must never become initialization scaffolds.
                        let directory = target.join(&current);
                        ensure!(
                            !directory.join(".git").try_exists()?
                                && !directory.join(".git").is_symlink()
                                && super::state::record_for_blocking(&directory)?.is_none(),
                            "virtual checkout path crosses protected checkout '{}'",
                            directory.display()
                        );
                    }
                    Ok(_) => {
                        obstruction = Some(current.clone());
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(error) => return Err(error.into()),
                }
            }
            if let Some(obstruction) = obstruction {
                let before = inventory(&target.join(&obstruction))?;
                if obstruction == *path && matches!(before.get(Path::new("")), Some(Node::File(..)))
                {
                    let actual = snapshot::canonical_target_entry(
                        &repo,
                        target,
                        path,
                        &managed.join(path),
                        &[Some(*expected)],
                    )?;
                    if git::entries_match(actual, Some(*expected)) {
                        continue;
                    }
                }
                groups.push(Group {
                    path: obstruction,
                    before,
                    conflict: true,
                });
            } else {
                groups.push(Group {
                    path: path.clone(),
                    before: BTreeMap::new(),
                    conflict: false,
                });
            }
        }
        Ok(Self {
            repo,
            managed,
            base,
            entries,
            groups,
        })
    }

    fn committed_group(&self, group: &Group) -> Result<Inventory> {
        let mut result = BTreeMap::new();
        for (path, (oid, mode)) in self
            .entries
            .range(group.path.clone()..)
            .take_while(|(path, _)| path.starts_with(&group.path))
        {
            let relative = path.strip_prefix(&group.path)?;
            result.insert(
                relative.to_path_buf(),
                Node::File(
                    crate::crypt::policy::materialized_blob(
                        &self.repo,
                        &self.managed.join(path),
                        *oid,
                    )?,
                    *mode,
                ),
            );
        }
        Ok(result)
    }
}

pub(super) fn has_conflicts(entry: &DesiredEntry) -> Result<bool> {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!()
    };
    Ok(
        Initial::assess(source, &entry.target, &entry.intent.checkout_scope())?
            .groups
            .iter()
            .any(|group| group.conflict),
    )
}

// Check ancestors of holes too: replacing `a` must not delete a mapping at `a/b`.
fn validate_group(root: &Path, group: &Group, scope: &CheckoutScope, source: bool) -> Result<()> {
    let excludes = if source {
        &scope.source_excludes
    } else {
        &scope.target_excludes
    };
    ensure!(
        !excludes
            .iter()
            .any(|hole| hole.starts_with(&group.path) || group.path.starts_with(hole)),
        "conflict '{}' overlaps an independent mapping",
        group.path.display()
    );
    validate_relative_ancestors(root, &root.join(&group.path))?;
    ensure_unprotected_target(&root.join(&group.path))?;
    let metadata = root.join(&group.path).symlink_metadata()?;
    ensure!(
        metadata.is_symlink() || !metadata.permissions().readonly(),
        "conflict '{}' is read-only",
        group.path.display()
    );
    for (relative, node) in &group.before {
        let path = join_relative(&group.path, relative);
        ensure!(
            scope.owns_source_kind(&path, matches!(node, Node::Directory))
                && scope.owns_target_kind(&path, matches!(node, Node::Directory)),
            "conflict contains ignored or unmanaged path '{}'",
            path.display()
        );
        ensure!(
            !path.components().any(|part| part.as_os_str() == ".git"),
            "conflict contains Git metadata"
        );
    }
    Ok(())
}

pub(super) fn initialize(
    ctx: &Context,
    target: &Path,
    overlay: &str,
    source: &Path,
    scope: CheckoutScope,
) -> Result<VirtualCheckoutRecord> {
    initialize_with(
        ctx,
        target,
        overlay,
        source,
        scope,
        prompt_conflict,
        show_diff,
    )
}

fn initialize_with(
    ctx: &Context,
    target: &Path,
    overlay: &str,
    source: &Path,
    scope: CheckoutScope,
    mut prompt: impl FnMut(&Path, &Path) -> Result<ConflictChoice>,
    mut diff: impl FnMut(&Path, &Path) -> Result<()>,
) -> Result<VirtualCheckoutRecord> {
    let initial = Initial::assess(source, target, &scope)?;
    let guard = PathGuard::capture(target)?;
    let source_guard = PathGuard::capture(source)?;
    let target_guards = initial
        .groups
        .iter()
        .map(|group| PathGuard::capture(&target.join(&group.path)))
        .collect::<Result<Vec<_>>>()?;
    let source_location = crate::overlays::mapping::physical(source)?;
    let target_location = crate::overlays::mapping::location(target)?;
    ensure!(
        !source_location.starts_with(&target_location)
            && !target_location.starts_with(&source_location),
        "virtual checkout source and target overlap"
    );
    let mut decisions = Vec::new();
    for group in &initial.groups {
        let mut protected = false;
        for path in initial
            .entries
            .keys()
            .filter(|path| path.starts_with(&group.path))
        {
            protected |=
                crate::crypt::policy::protected(&initial.repo, &initial.managed.join(path))?;
        }
        ensure!(
            !protected
                || !group.conflict
                || (group.before.len() == 1
                    && matches!(group.before.get(Path::new("")), Some(Node::File(..)))),
            "protected directory-shape conflict requires manual resolution before apply"
        );
        let choice = if !group.conflict || ctx.force {
            ConflictChoice::Overwrite
        } else {
            ensure!(
                !ctx.no_prompt,
                "virtual checkout conflict: '{}' already exists (use --force to overwrite or run interactively to choose)",
                target.join(&group.path).display()
            );
            loop {
                let choice = prompt(&source.join(&group.path), &target.join(&group.path))?;
                if choice != ConflictChoice::Diff {
                    break choice;
                }
                ensure!(
                    !protected,
                    "protected conflicts cannot create plaintext comparison directories; inspect the intended destination explicitly"
                );
                ensure!(
                    !group
                        .before
                        .values()
                        .any(|node| matches!(node, Node::Link(_))),
                    "cannot diff through a symlink conflict"
                );
                validate_group(target, group, &scope, false)?;
                let comparison = tempfile::tempdir()?;
                let committed = comparison.path().join("committed");
                write_inventory(&committed, &initial.committed_group(group)?)?;
                diff(&committed, &target.join(&group.path))?;
            }
        };
        if matches!(choice, ConflictChoice::Overwrite | ConflictChoice::Absorb) && group.conflict {
            validate_group(target, group, &scope, false)?;
        }
        decisions.push(choice);
    }
    guard.verify()?;
    source_guard.verify()?;
    // Prompt callbacks may run arbitrary programs; no previously observed path
    // may gain new deletion authority while the user is deciding.
    for group in &initial.groups {
        ensure!(
            inventory(&target.join(&group.path))? == group.before,
            "virtual checkout target changed during conflict resolution"
        );
    }
    let mut regular = git::SourceTransaction::for_repository(&initial.repo)?;
    let mut replacements = Vec::new();
    let mut source_observations = Vec::new();
    let prepared = (|| -> Result<()> {
        for (group, choice) in initial.groups.iter().zip(decisions) {
            match choice {
                ConflictChoice::Skip => {}
                ConflictChoice::Overwrite => {
                    let contents = initial.committed_group(group)?;
                    prepare_update(
                        &mut regular,
                        &mut replacements,
                        target.join(&group.path),
                        &contents,
                        false,
                    )?;
                }
                ConflictChoice::Absorb => {
                    let ov = ctx.repository.get(overlay)?;
                    crate::secrets::validate_ingestion(ctx, &ov, &[target.join(&group.path)])?;
                    let source_group = Group {
                        path: group.path.clone(),
                        before: inventory(&source.join(&group.path))?,
                        conflict: true,
                    };
                    validate_group(source, &source_group, &scope, true)?;
                    validate_source(&initial, source, &source_group)?;
                    source_observations.push(source_group);
                    let scratch = crate::actions::git::diff::Scratch::new(&initial.repo)?;
                    let mut contents = BTreeMap::new();
                    for (relative, node) in &group.before {
                        let node = match node {
                            Node::File(bytes, mode) => {
                                let path = join_relative(&group.path, relative);
                                let logical = initial.managed.join(&path);
                                let (oid, mode) = snapshot::canonical_entry(
                                    &initial.repo,
                                    &scratch,
                                    &logical,
                                    bytes,
                                    *mode,
                                    &[initial.entries.get(&path).copied()],
                                )?;
                                // Publishing is intentional only after an explicit Absorb;
                                // the real index and HEAD remain untouched.
                                let canonical = git::read_blob(&scratch.repo, oid)?;
                                let oid = initial.repo.blob(&canonical)?;
                                Node::File(
                                    git::source_worktree_blob(&initial.repo, &logical, oid)?,
                                    mode,
                                )
                            }
                            Node::Directory => Node::Directory,
                            Node::Link(_) => anyhow::bail!(
                                "cannot absorb symlinks into a virtual checkout source"
                            ),
                        };
                        contents.insert(relative.clone(), node);
                    }
                    prepare_update(
                        &mut regular,
                        &mut replacements,
                        source.join(&group.path),
                        &contents,
                        true,
                    )?;
                }
                ConflictChoice::Diff => unreachable!(),
            }
        }
        Ok(())
    })();
    if let Err(error) = prepared {
        regular
            .rollback()
            .with_context(|| format!("{error:#}; preparation cleanup also failed"))?;
        return Err(error);
    }
    // Every blob, permission check and source ingestion is prepared before the
    // first target write; a late obstruction must not leave earlier files applied.
    let applied = (|| -> Result<()> {
        guard.verify()?;
        source_guard.verify()?;
        for path_guard in &target_guards {
            path_guard.verify()?;
        }
        for group in &source_observations {
            validate_group(source, group, &scope, true)?;
            ensure!(
                inventory(&source.join(&group.path))? == group.before,
                "overlay source changed before absorb"
            );
            validate_source(&initial, source, group)?;
        }
        for group in &initial.groups {
            validate_relative_ancestors(target, &target.join(&group.path))?;
            ensure!(
                inventory(&target.join(&group.path))? == group.before,
                "virtual checkout target changed before initialization"
            );
        }
        regular.apply()?;
        for replacement in &mut replacements {
            replacement.apply()?;
        }
        fs::create_dir_all(target)?;
        Ok(())
    })();
    if let Err(error) = applied {
        let mut failures = Vec::new();
        for replacement in replacements.iter_mut().rev() {
            if let Err(rollback) = replacement.rollback() {
                failures.push(format!("{rollback:#}"));
            }
        }
        if let Err(rollback) = regular.rollback() {
            failures.push(format!("{rollback:#}"));
        }
        ensure!(
            failures.is_empty(),
            "{error:#}; rollback also failed: {}",
            failures.join("; ")
        );
        return Err(error);
    }
    if let Err(error) = regular.cleanup() {
        // The content is installed; reporting failure would conceal a completed
        // initialization and prevent persistence of its comparison base.
        tracing::warn!("virtual checkout initialized, but backup cleanup failed: {error:#}");
    }
    Ok(VirtualCheckoutRecord {
        overlay: overlay.into(),
        managed_path: initial.managed,
        base_oid: initial.base.to_string(),
        created_at: super::now(),
        last_commit_at: None,
        scope,
        source_repository: initial.repo.workdir().map(Path::to_path_buf),
    })
}

fn validate_source(initial: &Initial, source: &Path, group: &Group) -> Result<()> {
    ensure!(
        initial.repo.head()?.peel_to_commit()?.id() == initial.base,
        "source HEAD changed during virtual checkout initialization"
    );
    let mut index = initial.repo.index()?;
    index.read(true)?;
    let head = git::base_tree(&initial.repo, Some(&initial.base.to_string()))?;
    let head = git::tracked_entries(&head)?;
    let mut paths = std::collections::BTreeSet::new();
    for path in head.keys() {
        if path.starts_with(initial.managed.join(&group.path)) {
            paths.insert(path.clone());
        }
    }
    for (relative, node) in &group.before {
        if !matches!(node, Node::Directory) {
            paths.insert(join_relative(&initial.managed.join(&group.path), relative));
        }
    }
    for entry in index.iter() {
        let path = PathBuf::from(std::str::from_utf8(&entry.path)?);
        if path.starts_with(initial.managed.join(&group.path)) {
            ensure!(
                entry.flags & 0x3000 == 0,
                "cannot absorb over an unmerged source index entry"
            );
            paths.insert(path);
        }
    }
    for path in paths {
        let expected = head.get(&path).copied();
        let indexed = index
            .get_path(&path, 0)
            .map(|entry| (entry.id, entry.mode as i32));
        ensure!(
            git::entries_match(expected, indexed)
                && git::entries_match(
                    expected,
                    snapshot::canonical_source_entry(&initial.repo, &path)?
                ),
            "cannot absorb over independently modified source '{}'",
            source.join(path.strip_prefix(&initial.managed)?).display()
        );
    }
    Ok(())
}

fn write_inventory(destination: &Path, contents: &Inventory) -> Result<()> {
    for (relative, node) in contents {
        let path = join_relative(destination, relative);
        match node {
            Node::Directory => fs::create_dir_all(&path)?,
            Node::File(bytes, mode) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, bytes)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(
                        &path,
                        fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 }),
                    )?;
                }
                #[cfg(not(unix))]
                let _ = mode;
            }
            Node::Link(_) => anyhow::bail!("cannot materialize a symlink in a virtual checkout"),
        }
    }
    Ok(())
}

// Regular files use the existing metadata-preserving transaction. Structural
// replacements retain an entire original group for rollback instead of deleting it.
fn prepare_update(
    regular: &mut git::SourceTransaction,
    replacements: &mut Vec<Replacement>,
    destination: PathBuf,
    contents: &Inventory,
    private: bool,
) -> Result<()> {
    if let Some(Node::File(bytes, mode)) = contents.get(Path::new(""))
        && destination
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file())
    {
        regular.prepare(destination, Some((bytes.clone(), *mode as u32)))?;
    } else {
        let scratch = tempfile::tempdir()?;
        write_inventory(&scratch.path().join("new"), contents)?;
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::PermissionsExt;
            // Newly ingested source paths follow SourceTransaction's private
            // creation policy rather than inheriting public target access bits.
            for entry in walkdir::WalkDir::new(scratch.path().join("new")) {
                let entry = entry?;
                let mode = entry.metadata()?.permissions().mode();
                fs::set_permissions(
                    entry.path(),
                    fs::Permissions::from_mode(
                        if entry.file_type().is_dir() || mode & 0o111 != 0 {
                            0o700
                        } else {
                            0o600
                        },
                    ),
                )?;
            }
        }
        #[cfg(not(unix))]
        let _ = private;
        replacements.push(Replacement {
            destination,
            scratch,
            backup: None,
        });
    }
    Ok(())
}

struct Replacement {
    destination: PathBuf,
    scratch: tempfile::TempDir,
    backup: Option<tempfile::TempDir>,
}

impl Replacement {
    fn apply(&mut self) -> Result<()> {
        let parent = self
            .destination
            .parent()
            .context("replacement has no parent")?;
        fs::create_dir_all(parent)?;
        // Stage on the destination filesystem so rename works across mounts.
        let backup = tempfile::tempdir_in(parent)?;
        if self.destination.symlink_metadata().is_ok() {
            fs::rename(&self.destination, backup.path().join("old"))?;
        }
        self.backup = Some(backup);
        copy_prepared(&self.scratch.path().join("new"), &self.destination)?;
        Ok(())
    }

    fn rollback(&mut self) -> Result<()> {
        if let Some(backup) = self.backup.take() {
            let restored = (|| -> Result<()> {
                if self.destination.symlink_metadata().is_ok() {
                    crate::actions::fs::remove_target(&self.destination)?;
                }
                if backup.path().join("old").symlink_metadata().is_ok() {
                    fs::rename(backup.path().join("old"), &self.destination)?;
                }
                Ok(())
            })();
            if let Err(error) = restored {
                let retained = backup.keep();
                anyhow::bail!(
                    "{error:#}; original retained at '{}'",
                    retained.join("old").display()
                );
            }
        }
        Ok(())
    }
}

fn copy_prepared(source: &Path, target: &Path) -> Result<()> {
    if source.is_dir() {
        fs::create_dir(target)?;
        fs::set_permissions(target, fs::metadata(source)?.permissions())?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_prepared(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        // Exclusive creation prevents a raced-in link from redirecting a write
        // outside the approved group after its original was backed up.
        let mut input = fs::File::open(source)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(target)?;
        std::io::copy(&mut input, &mut output)?;
        output.set_permissions(input.metadata()?.permissions())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    struct Fixture {
        home: tempfile::TempDir,
        target: tempfile::TempDir,
        source: PathBuf,
        ctx: std::sync::Arc<Context>,
    }

    impl Fixture {
        fn new(files: &[(&str, &str)]) -> Self {
            let home = tempfile::tempdir().unwrap();
            let target = tempfile::tempdir().unwrap();
            let source = home.path().join("ov");
            fs::create_dir(&source).unwrap();
            for (path, content) in files {
                let path = source.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, content).unwrap();
            }
            let repo = Repository::init(home.path()).unwrap();
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let signature = git2::Signature::now("Test", "test@example.com").unwrap();
            repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
                .unwrap();
            let ctx = Context::builder()
                .repository(crate::overlays::Repository::new(home.path().into()))
                .root(target.path().into())
                .build();
            Self {
                home,
                target,
                source,
                ctx,
            }
        }

        fn run(&self, choice: ConflictChoice) -> Result<VirtualCheckoutRecord> {
            initialize_with(
                &self.ctx,
                self.target.path(),
                "ov",
                &self.source,
                CheckoutScope::default(),
                |_, _| Ok(choice),
                |_, _| panic!("unexpected diff"),
            )
        }
    }

    #[test]
    fn existing_containers_and_identical_files_need_no_prompt() {
        let fixture = Fixture::new(&[("dir/new", "new"), ("same", "same")]);
        fs::create_dir(fixture.target.path().join("dir")).unwrap();
        fs::write(fixture.target.path().join("same"), "same").unwrap();
        fs::write(fixture.target.path().join("extra"), "keep me").unwrap();
        initialize_with(
            &fixture.ctx,
            fixture.target.path(),
            "ov",
            &fixture.source,
            CheckoutScope::default(),
            |_, _| panic!("no collision"),
            |_, _| panic!("no diff"),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("dir/new")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("extra")).unwrap(),
            "keep me"
        );
        assert!(!fixture.target.path().join(".git").exists());
    }

    #[rstest]
    #[case(ConflictChoice::Skip, "local", "committed")]
    #[case(ConflictChoice::Overwrite, "committed", "committed")]
    #[case(ConflictChoice::Absorb, "local", "local")]
    fn resolving_a_collision_preserves_siblings_and_source_staging(
        #[case] choice: ConflictChoice,
        #[case] target: &str,
        #[case] source: &str,
    ) {
        let fixture = Fixture::new(&[("collision", "committed"), ("missing", "new")]);
        fs::write(fixture.target.path().join("collision"), "local").unwrap();
        fs::write(fixture.target.path().join("extra"), "keep").unwrap();
        let repo = Repository::open(fixture.home.path()).unwrap();
        let head = repo.head().unwrap().target().unwrap();
        let index = fs::read(repo.path().join("index")).unwrap();
        let record = fixture.run(choice).unwrap();
        assert_eq!(record.base_oid, head.to_string());
        assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("collision")).unwrap(),
            target
        );
        assert_eq!(
            fs::read_to_string(fixture.source.join("collision")).unwrap(),
            source
        );
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("missing")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("extra")).unwrap(),
            "keep"
        );
    }

    #[rstest]
    #[case(ConflictChoice::Skip)]
    #[case(ConflictChoice::Overwrite)]
    #[case(ConflictChoice::Absorb)]
    fn a_file_obstructing_descendants_is_resolved_as_one_group(#[case] choice: ConflictChoice) {
        let fixture = Fixture::new(&[("dir/a", "a"), ("dir/b", "b"), ("sibling", "new")]);
        fs::write(fixture.target.path().join("dir"), "local file").unwrap();
        let mut prompts = 0;
        initialize_with(
            &fixture.ctx,
            fixture.target.path(),
            "ov",
            &fixture.source,
            CheckoutScope::default(),
            |_, path| {
                prompts += 1;
                assert_eq!(path, fixture.target.path().join("dir"));
                Ok(choice)
            },
            |_, _| panic!("no diff"),
        )
        .unwrap();
        assert_eq!(prompts, 1);
        assert!(fixture.target.path().join("sibling").is_file());
        if choice == ConflictChoice::Overwrite {
            assert_eq!(
                fs::read_to_string(fixture.target.path().join("dir/b")).unwrap(),
                "b"
            );
        } else {
            assert_eq!(
                fs::read_to_string(fixture.target.path().join("dir")).unwrap(),
                "local file"
            );
        }
        if choice == ConflictChoice::Absorb {
            assert_eq!(
                fs::read_to_string(fixture.source.join("dir")).unwrap(),
                "local file"
            );
        }
    }

    #[rstest]
    #[case(ConflictChoice::Skip)]
    #[case(ConflictChoice::Overwrite)]
    #[case(ConflictChoice::Absorb)]
    fn a_directory_at_a_committed_leaf_is_one_collision(#[case] choice: ConflictChoice) {
        let fixture = Fixture::new(&[("leaf", "committed")]);
        fs::create_dir(fixture.target.path().join("leaf")).unwrap();
        fs::write(fixture.target.path().join("leaf/local"), "local").unwrap();
        fixture.run(choice).unwrap();
        if choice == ConflictChoice::Overwrite {
            assert_eq!(
                fs::read_to_string(fixture.target.path().join("leaf")).unwrap(),
                "committed"
            );
        } else {
            assert_eq!(
                fs::read_to_string(fixture.target.path().join("leaf/local")).unwrap(),
                "local"
            );
        }
        if choice == ConflictChoice::Absorb {
            assert_eq!(
                fs::read_to_string(fixture.source.join("leaf/local")).unwrap(),
                "local"
            );
        }
    }

    #[test]
    fn diff_uses_committed_content_and_returns_to_the_prompt() {
        let fixture = Fixture::new(&[("file", "committed")]);
        fs::write(fixture.source.join("file"), "dirty source").unwrap();
        fs::write(fixture.target.path().join("file"), "local").unwrap();
        let mut choices = [ConflictChoice::Diff, ConflictChoice::Skip].into_iter();
        let mut shown = false;
        initialize_with(
            &fixture.ctx,
            fixture.target.path(),
            "ov",
            &fixture.source,
            CheckoutScope::default(),
            |_, _| Ok(choices.next().unwrap()),
            |committed, target| {
                assert_eq!(fs::read_to_string(committed)?, "committed");
                assert_eq!(fs::read_to_string(target)?, "local");
                shown = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(shown);
    }

    #[test]
    fn noninteractive_conflicts_do_not_write_earlier_missing_files() {
        let mut fixture = Fixture::new(&[("a-missing", "new"), ("z-conflict", "committed")]);
        fs::write(fixture.target.path().join("z-conflict"), "local").unwrap();
        std::sync::Arc::make_mut(&mut fixture.ctx).no_prompt = true;
        assert!(fixture.run(ConflictChoice::Overwrite).is_err());
        assert!(!fixture.target.path().join("a-missing").exists());
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("z-conflict")).unwrap(),
            "local"
        );
    }

    #[test]
    fn absorb_refuses_independent_source_edits_before_target_writes() {
        let fixture = Fixture::new(&[("a-missing", "new"), ("z-conflict", "committed")]);
        fs::write(fixture.target.path().join("z-conflict"), "local").unwrap();
        fs::write(fixture.source.join("z-conflict"), "source edit").unwrap();
        assert!(fixture.run(ConflictChoice::Absorb).is_err());
        assert!(!fixture.target.path().join("a-missing").exists());
        assert_eq!(
            fs::read_to_string(fixture.source.join("z-conflict")).unwrap(),
            "source edit"
        );
    }

    #[test]
    fn changed_target_content_invalidates_the_prompt_decision() {
        let fixture = Fixture::new(&[("file", "committed")]);
        fs::write(fixture.target.path().join("file"), "local").unwrap();
        let result = initialize_with(
            &fixture.ctx,
            fixture.target.path(),
            "ov",
            &fixture.source,
            CheckoutScope::default(),
            |_, path| {
                fs::write(path, "changed during prompt")?;
                Ok(ConflictChoice::Overwrite)
            },
            |_, _| panic!("no diff"),
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("file")).unwrap(),
            "changed during prompt"
        );
    }

    #[test]
    fn force_cannot_replace_a_group_containing_an_independent_mapping() {
        let mut fixture = Fixture::new(&[("leaf", "committed")]);
        fs::create_dir(fixture.target.path().join("leaf")).unwrap();
        fs::write(fixture.target.path().join("leaf/hole"), "independent").unwrap();
        std::sync::Arc::make_mut(&mut fixture.ctx).force = true;
        let scope = CheckoutScope {
            target_excludes: vec!["leaf/hole".into()],
            ..Default::default()
        };
        let result = initialize(
            &fixture.ctx,
            fixture.target.path(),
            "ov",
            &fixture.source,
            scope,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("leaf/hole")).unwrap(),
            "independent"
        );
    }

    #[test]
    fn initialization_preserves_unrelated_repositories_but_cannot_write_inside_them() {
        let fixture = Fixture::new(&[("new", "committed")]);
        Repository::init(fixture.target.path().join("unrelated")).unwrap();
        fixture.run(ConflictChoice::Overwrite).unwrap();
        assert!(fixture.target.path().join("unrelated/.git").is_dir());

        let fixture = Fixture::new(&[("nested/new", "committed")]);
        Repository::init(fixture.target.path().join("nested")).unwrap();
        assert!(fixture.run(ConflictChoice::Overwrite).is_err());
        assert!(!fixture.target.path().join("nested/new").exists());
    }

    #[cfg(unix)]
    #[test]
    fn absorb_cleans_target_content_and_smudges_it_for_the_source_worktree() {
        let fixture = Fixture::new(&[("file", "COMMITTED")]);
        let repo = Repository::open(fixture.home.path()).unwrap();
        repo.config()
            .unwrap()
            .set_str("filter.upper.clean", "tr '[:lower:]' '[:upper:]'")
            .unwrap();
        repo.config()
            .unwrap()
            .set_str("filter.upper.smudge", "tr '[:upper:]' '[:lower:]'")
            .unwrap();
        fs::write(fixture.source.join(".gitattributes"), "file filter=upper\n").unwrap();
        fs::write(fixture.source.join("file"), "committed").unwrap();
        fs::write(fixture.target.path().join("file"), "LOCAL").unwrap();
        let index = fs::read(repo.path().join("index")).unwrap();
        fixture.run(ConflictChoice::Absorb).unwrap();
        assert_eq!(
            fs::read_to_string(fixture.source.join("file")).unwrap(),
            "local"
        );
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("file")).unwrap(),
            "LOCAL"
        );
        assert_eq!(fs::read(repo.path().join("index")).unwrap(), index);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_identical_content_does_not_prompt_for_line_ending_differences() {
        let fixture = Fixture::new(&[("file", "committed\n")]);
        fs::write(
            fixture.source.join(".gitattributes"),
            "file text eol=crlf\n",
        )
        .unwrap();
        fs::write(fixture.target.path().join("file"), "committed\r\n").unwrap();
        initialize_with(
            &fixture.ctx,
            fixture.target.path(),
            "ov",
            &fixture.source,
            CheckoutScope::default(),
            |_, _| panic!("canonical identical file must be adopted"),
            |_, _| panic!("no diff"),
        )
        .unwrap();
        assert_eq!(
            fs::read(fixture.target.path().join("file")).unwrap(),
            b"committed\r\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn retargeting_a_missing_paths_parent_during_a_prompt_cannot_redirect_writes() {
        let fixture = Fixture::new(&[("dir/new", "new"), ("z-conflict", "committed")]);
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.target.path().join("dir")).unwrap();
        fs::write(fixture.target.path().join("z-conflict"), "local").unwrap();
        let result = initialize_with(
            &fixture.ctx,
            fixture.target.path(),
            "ov",
            &fixture.source,
            CheckoutScope::default(),
            |_, _| {
                fs::remove_dir(fixture.target.path().join("dir"))?;
                std::os::unix::fs::symlink(outside.path(), fixture.target.path().join("dir"))?;
                Ok(ConflictChoice::Skip)
            },
            |_, _| panic!("no diff"),
        );
        assert!(result.is_err());
        assert!(!outside.path().join("new").exists());
    }

    #[cfg(unix)]
    #[rstest]
    #[case(false)]
    #[case(true)]
    fn overwrite_unlinks_a_leaf_symlink_without_touching_its_destination(#[case] directory: bool) {
        let fixture = Fixture::new(&[("file", "committed")]);
        let outside = tempfile::tempdir().unwrap();
        let marker = outside.path().join("marker");
        fs::write(&marker, "outside").unwrap();
        let destination = if directory { outside.path() } else { &marker };
        std::os::unix::fs::symlink(destination, fixture.target.path().join("file")).unwrap();
        fixture.run(ConflictChoice::Overwrite).unwrap();
        assert_eq!(fs::read_to_string(marker).unwrap(), "outside");
        assert!(!fixture.target.path().join("file").is_symlink());
        assert_eq!(
            fs::read_to_string(fixture.target.path().join("file")).unwrap(),
            "committed"
        );
    }
}
