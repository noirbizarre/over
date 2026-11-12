//! Prepare the entire interaction before publishing any selected index changes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use git2::{IndexEntryExtendedFlag, Repository};

use crate::actions::git::diff::Scratch;
use crate::actions::git::index::{IndexLock, read_index};
use crate::actions::git::patch::{Session, Snapshot, Unit};
use crate::desired::{DesiredEntry, DesiredTree, Provenance};
use crate::exec::Ctx;
use crate::materialize::virtual_checkout::git;
use crate::materialize::virtual_checkout::snapshot::{
    canonical_entry, canonical_source_entry, target_content,
};
use crate::materialize::virtual_checkout::state;
use crate::overlays::Overlay;

pub(crate) enum Decision {
    Accept,
    Skip,
    Finish,
    Cancel,
}

struct Prepared {
    repo: Repository,
    index_path: PathBuf,
    original: Option<Vec<u8>>,
    _snapshot_directory: tempfile::TempDir,
    index: git2::Index,
    scratch: Scratch,
    before: Snapshot,
    after: Snapshot,
    session: Option<Session>,
    accepted: BTreeSet<usize>,
}

/// The injected selector keeps tests and future restore callers independent of a TTY.
pub(crate) fn run(
    ctx: &Ctx,
    overlay: &Overlay,
    selections: &[PathBuf],
    select: impl FnMut(&Unit, bool) -> Result<Decision>,
) -> Result<()> {
    crate::secrets::validate_ingestion(ctx, overlay, selections)?;
    run_with(
        ctx,
        overlay,
        selections,
        select,
        &state::record_for_blocking,
    )
}

fn run_with(
    ctx: &Ctx,
    overlay: &Overlay,
    selections: &[PathBuf],
    mut select: impl FnMut(&Unit, bool) -> Result<Decision>,
    record_for: &impl Fn(&Path) -> Result<Option<state::VirtualCheckoutRecord>>,
) -> Result<()> {
    let tree = DesiredTree::build_own(ctx, overlay)?;
    // Legacy un-mapped root checkouts do not encode all independent entries in
    // their persisted scope. Enforce those live boundaries without changing the
    // recorded association or reinterpreting its ownership during verification.
    let protected: Vec<_> = tree
        .entries()
        .iter()
        .filter(|entry| {
            !entry.intent.is_virtual_checkout()
                && !matches!(
                    entry.intent,
                    crate::desired::MaterializationIntent::Directory
                )
        })
        .map(|entry| entry.target.clone())
        .collect();
    let entries: Vec<_> = tree
        .entries()
        .iter()
        .filter(|entry| entry.intent.is_virtual_checkout())
        .cloned()
        .collect();
    let routing = overlay.resolve_mappings(ctx)?;
    let policy = crate::overlays::ignore::SourcePolicy::new(&ctx.repository.root, overlay)?;
    let selections = selections
        .iter()
        .map(|path| {
            routing
                .logical_path_for(path)
                .map(|logical| routing.target_for(&logical))
        })
        .collect::<Result<Vec<_>>>()?;
    for path in &selections {
        let logical = routing.logical_path_for(path)?;
        ensure!(
            policy.includes(&overlay.root.join(logical), path.is_dir()),
            "'{}' is ignored or is control metadata",
            path.display()
        );
        ensure!(
            entries.iter().any(|entry| super::owns(entry, path)
                || (path.is_dir() && entry.target.starts_with(path)))
                && !protected.iter().any(|boundary| path.starts_with(boundary)),
            "'{}' is not owned by a virtual checkout in this overlay",
            path.display()
        );
        if path.is_dir() {
            crate::ui::info(format!(
                "{}: select owned virtual-checkout changes only; other materializations are skipped",
                path.display()
            ))?;
        }
    }
    let mut repositories: BTreeMap<PathBuf, Prepared> = BTreeMap::new();
    let mut checked = BTreeSet::new();
    for entry in &entries {
        if !selections
            .iter()
            .any(|path| super::owns(entry, path) || entry.target.starts_with(path))
        {
            continue;
        }
        let record = record_for(&entry.target)?.with_context(|| {
            format!(
                "no checkout at '{}'; run over apply first",
                entry.target.display()
            )
        })?;
        prepare_entry(
            entry,
            &selections,
            &protected,
            &record,
            &mut repositories,
            &mut checked,
        )?;
    }
    let checked: Vec<_> = checked.into_iter().collect();
    for selection in &selections {
        ensure!(
            selection.exists() || checked.iter().any(|path| path.starts_with(selection)),
            "{} does not match any tracked checkout path",
            selection.display()
        );
    }
    // Snapshot traversal rejects links before ordinary staging's preflight can
    // read target bytes; reuse its safety checks, never its ingestion path.
    super::stage_entries_with(
        &entries,
        &checked,
        true,
        overlay,
        false,
        record_for,
        &canonical_source_entry,
    )?;
    for prepared in repositories.values_mut() {
        ensure!(
            read_index(&prepared.index_path)? == prepared.original,
            "source index changed while preparing; rerun over add --patch"
        );
        prepared.session = Some(Session::prepare(
            &prepared.scratch.repo,
            &prepared.before,
            &prepared.after,
        )?);
    }
    let mut finished = false;
    for prepared in repositories.values_mut() {
        let session = prepared
            .session
            .as_ref()
            .context("missing prepared patch")?;
        for (id, unit) in session.units.iter().enumerate() {
            if finished {
                break;
            }
            match select(unit, ctx.dry_run)? {
                Decision::Accept => {
                    prepared.accepted.insert(id);
                }
                Decision::Skip => {}
                Decision::Finish => finished = true,
                Decision::Cancel => return Ok(()),
            }
        }
    }
    if ctx.dry_run {
        return Ok(());
    }
    let mut results = Vec::new();
    for prepared in repositories.values() {
        let updates = prepared
            .session
            .as_ref()
            .context("missing prepared patch")?
            .apply(&prepared.scratch.repo, &prepared.accepted)?;
        if !updates.is_empty() {
            results.push((prepared, updates));
        }
    }
    if results.is_empty() {
        return Ok(());
    }
    // Reserve all participating indexes before validation, closing the prompt-time
    // lost-update race without pretending that multiple renames form a transaction.
    let mut locks = results
        .iter()
        .map(|(prepared, _)| IndexLock::acquire(&prepared.index_path))
        .collect::<Result<Vec<_>>>()?;
    for (prepared, _) in &results {
        ensure!(
            read_index(&prepared.index_path)? == prepared.original,
            "source index changed during selection; rerun over add --patch"
        );
    }
    super::stage_entries_with(
        &entries,
        &checked,
        true,
        overlay,
        false,
        record_for,
        &canonical_source_entry,
    )?;
    for ((prepared, updates), lock) in results.into_iter().zip(&mut locks) {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("index");
        if let Some(original) = &prepared.original {
            fs::write(&path, original)?;
        }
        let mut index = git2::Index::open(&path)?;
        for (path, update) in updates {
            if let Some((oid, mode)) = update {
                let blob = prepared.scratch.repo.find_blob(oid)?;
                let oid = prepared.repo.blob(blob.content())?;
                let bytes = path
                    .to_str()
                    .context("source path is not UTF-8")?
                    .replace('\\', "/")
                    .as_bytes()
                    .to_vec();
                let entry = git2::IndexEntry {
                    ctime: git2::IndexTime::new(0, 0),
                    mtime: git2::IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    uid: 0,
                    gid: 0,
                    mode: mode as u32,
                    file_size: blob.size().try_into().context("file too large to stage")?,
                    id: oid,
                    flags: 0,
                    flags_extended: 0,
                    path: bytes,
                };
                index.add(&entry)?;
            } else {
                index.remove_path(&path)?;
            }
        }
        index.write()?;
        lock.publish(
            &prepared.index_path,
            &fs::read(temporary.path().join("index"))?,
        )?;
    }
    Ok(())
}

fn prepare_entry(
    entry: &DesiredEntry,
    selections: &[PathBuf],
    protected: &[PathBuf],
    record: &state::VirtualCheckoutRecord,
    repositories: &mut BTreeMap<PathBuf, Prepared>,
    checked: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        anyhow::bail!("checkout has no overlay source");
    };
    let (repo, managed) = git::discover_source(source)?;
    let key = repo.path().to_path_buf();
    if !repositories.contains_key(&key) {
        let index_path = repo
            .index()?
            .path()
            .context("source index has no path")?
            .to_path_buf();
        let original = read_index(&index_path)?;
        // Project exactly the bytes used by the freshness check, rather than a
        // repository-cached index that may predate that snapshot.
        let snapshot_directory = tempfile::tempdir()?;
        let snapshot_path = snapshot_directory.path().join("index");
        if let Some(original) = &original {
            fs::write(&snapshot_path, original)?;
        }
        let index = git2::Index::open(&snapshot_path)?;
        let scratch = Scratch::new(&repo)?;
        repositories.insert(
            key.clone(),
            Prepared {
                repo,
                index_path,
                original,
                _snapshot_directory: snapshot_directory,
                index,
                scratch,
                before: Snapshot::new(),
                after: Snapshot::new(),
                session: None,
                accepted: BTreeSet::new(),
            },
        );
    }
    let prepared = repositories
        .get_mut(&key)
        .context("missing source repository")?;
    let repo = &prepared.repo;
    let index = &prepared.index;
    ensure!(
        !index.has_conflicts(),
        "source index has unresolved conflicts"
    );
    let scope = entry.intent.checkout_scope();
    let head = git::base_tree(repo, None)?;
    let mut candidates: BTreeSet<_> = super::checkout_entries(entry, record, repo, &managed)?
        .keys()
        .cloned()
        .collect();
    match entry.target.symlink_metadata() {
        Ok(metadata) => ensure!(
            metadata.is_dir() && !metadata.is_symlink(),
            "virtual checkout '{}' is not an ordinary directory",
            entry.target.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if entry.target.exists() {
        for item in walkdir::WalkDir::new(&entry.target)
            .min_depth(1)
            .into_iter()
            .filter_entry(|item| {
                let relative = item
                    .path()
                    .strip_prefix(&entry.target)
                    .expect("walk stays below root");
                scope.owns_source_kind(relative, item.file_type().is_dir())
                    && scope.owns_target_kind(relative, item.file_type().is_dir())
                    && !git::is_overlay_metadata(relative)
                    && !protected
                        .iter()
                        .any(|boundary| item.path().starts_with(boundary))
                    && selections.iter().any(|selection| {
                        item.path().starts_with(selection) || selection.starts_with(item.path())
                    })
            })
        {
            let item = item?;
            ensure!(
                item.file_type().is_file() || item.file_type().is_dir(),
                "unsupported virtual checkout target entry '{}'",
                item.path().display()
            );
            if item.file_type().is_file() {
                candidates.insert(item.path().strip_prefix(&entry.target)?.to_path_buf());
            }
        }
    }
    for item in index.iter() {
        let path = PathBuf::from(std::str::from_utf8(&item.path)?);
        if let Ok(relative) = path.strip_prefix(&managed) {
            candidates.insert(relative.to_path_buf());
        }
    }
    for relative in candidates {
        let target = entry.target.join(&relative);
        if !scope.owns_source(&relative)
            || !scope.owns_target(&relative)
            || git::is_overlay_metadata(&relative)
            || protected
                .iter()
                .any(|boundary| target.starts_with(boundary))
            || !selections.iter().any(|path| target.starts_with(path))
        {
            continue;
        }
        let path = managed.join(&relative);
        checked.insert(target.clone());
        git::ensure_supported_path(&head, index, &path)?;
        // Structural replacements cannot be chosen independently without risking
        // removal of descendants; leave those to ordinary full-file staging.
        ensure!(
            !target.is_dir(),
            "cannot patch a file/directory replacement at '{}'",
            target.display()
        );
        let old = index
            .get_path(&path, 0)
            .filter(|item| {
                !IndexEntryExtendedFlag::from_bits_truncate(item.flags_extended).is_intent_to_add()
            })
            .map(|item| (item.id, item.mode as i32));
        if let Some(old) = old {
            prepared.before.insert(path.clone(), old);
        }
        if let Some((content, mode)) = target_content(&entry.target, &relative)? {
            let canonical =
                canonical_entry(repo, &prepared.scratch, &path, &content, mode, &[old])?;
            prepared.after.insert(path, canonical);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Context;
    use crate::overlays::Repository as Overlays;

    struct Fixture {
        _directory: tempfile::TempDir,
        ctx: Ctx,
        overlay: Overlay,
        repo: Repository,
        target: PathBuf,
        records: BTreeMap<PathBuf, state::VirtualCheckoutRecord>,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("source");
            let target = directory.path().join("target");
            fs::create_dir_all(source.join("ov")).unwrap();
            fs::create_dir(&target).unwrap();
            fs::write(source.join("ov/over.toml"), "mode = 'checkout'\n").unwrap();
            let content = (0..30)
                .map(|line| format!("line {line}\n"))
                .collect::<String>();
            for name in ["file", "deleted"] {
                fs::write(source.join("ov").join(name), &content).unwrap();
                fs::write(target.join(name), &content).unwrap();
            }
            fs::write(source.join("unrelated"), "original\n").unwrap();
            let repo = Repository::init(&source).unwrap();
            repo.config()
                .unwrap()
                .set_bool("core.autocrlf", false)
                .unwrap();
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let signature = git2::Signature::now("Test", "test@example.com").unwrap();
            let oid = repo
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    "initial",
                    &repo.find_tree(tree_id).unwrap(),
                    &[],
                )
                .unwrap();
            let overlays = Overlays::new(source);
            let overlay = overlays.get("ov").unwrap();
            let ctx = Context::builder()
                .root(target.clone())
                .repository(overlays)
                .build();
            let tree = DesiredTree::build_own(&ctx, &overlay).unwrap();
            let records = tree
                .entries()
                .iter()
                .filter(|entry| entry.intent.is_virtual_checkout())
                .map(|entry| {
                    let Provenance::Overlay { source, .. } = &entry.provenance else {
                        unreachable!()
                    };
                    let (_, managed) = git::discover_source(source).unwrap();
                    (
                        entry.target.clone(),
                        state::VirtualCheckoutRecord {
                            overlay: "ov".into(),
                            managed_path: managed,
                            base_oid: oid.to_string(),
                            scope: entry.intent.checkout_scope(),
                            source_repository: repo.workdir().map(Path::to_path_buf),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            Self {
                _directory: directory,
                ctx,
                overlay,
                repo,
                target,
                records,
            }
        }

        fn run(&self, select: impl FnMut(&Unit, bool) -> Result<Decision>) -> Result<()> {
            self.paths(std::slice::from_ref(&self.target), select)
        }

        fn paths(
            &self,
            paths: &[PathBuf],
            select: impl FnMut(&Unit, bool) -> Result<Decision>,
        ) -> Result<()> {
            run_with(&self.ctx, &self.overlay, paths, select, &|target| {
                Ok(self.records.get(target).cloned())
            })
        }

        fn indexed(&self, path: &str) -> Option<Vec<u8>> {
            let mut index = self.repo.index().unwrap();
            index.read(true).unwrap();
            index
                .get_path(Path::new(path), 0)
                .map(|entry| self.repo.find_blob(entry.id).unwrap().content().to_vec())
        }

        fn index_bytes(&self) -> Vec<u8> {
            fs::read(self.repo.index().unwrap().path().unwrap()).unwrap()
        }

        fn stage(&self, path: &str, bytes: &[u8]) {
            let mut index = self.repo.index().unwrap();
            index.read(true).unwrap();
            let entry = index.get_path(Path::new(path), 0).unwrap();
            index.add_frombuffer(&entry, bytes).unwrap();
            index.write().unwrap();
        }

        fn configure(&mut self, config: &str) {
            fs::write(self.repo.workdir().unwrap().join("ov/over.toml"), config).unwrap();
            self.overlay = self.ctx.repository.get("ov").unwrap();
            let tree = DesiredTree::build_own(&self.ctx, &self.overlay).unwrap();
            for entry in tree
                .entries()
                .iter()
                .filter(|entry| entry.intent.is_virtual_checkout())
            {
                self.records.get_mut(&entry.target).unwrap().scope = entry.intent.checkout_scope();
            }
        }
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn full_staging_rejects_symlink_ancestors_without_recording_external_bytes(
        #[case] dry_run: bool,
    ) {
        let mut fixture = Fixture::new();
        let source = fixture.repo.workdir().unwrap().join("ov/nested");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("secret"), b"tracked content\n").unwrap();
        let mut index = fixture.repo.index().unwrap();
        index.add_path(Path::new("ov/nested/secret")).unwrap();
        index.write().unwrap();
        let tree = fixture.repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        let head = fixture.repo.head().unwrap().peel_to_commit().unwrap();
        let oid = fixture
            .repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "nested file",
                &tree,
                &[&head],
            )
            .unwrap();
        for record in fixture.records.values_mut() {
            record.base_oid = oid.to_string();
        }
        let outside = fixture._directory.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let secret = b"external bytes must never enter the source object database\n";
        fs::write(outside.join("secret"), secret).unwrap();
        std::os::unix::fs::symlink(&outside, fixture.target.join("nested")).unwrap();
        let original_index = fixture.index_bytes();
        let secret_oid = git2::Oid::hash_object(git2::ObjectType::Blob, secret).unwrap();
        assert!(!fixture.repo.odb().unwrap().exists(secret_oid));
        let tree = DesiredTree::build_own(&fixture.ctx, &fixture.overlay).unwrap();
        let result = super::super::stage_entries_with(
            tree.entries(),
            &[fixture.target.join("nested/secret")],
            dry_run,
            &fixture.overlay,
            false,
            &|target| Ok(fixture.records.get(target).cloned()),
            &git::source_entry,
        );
        assert!(result.unwrap_err().to_string().contains("symlink"));
        assert_eq!(fixture.index_bytes(), original_index);
        assert!(!fixture.repo.odb().unwrap().exists(secret_oid));
        assert_eq!(fs::read(outside.join("secret")).unwrap(), secret);
    }

    #[test]
    fn selected_hunks_preserve_previous_staging_and_leave_skipped_changes_unstaged() {
        let fixture = Fixture::new();
        let base = fs::read_to_string(fixture.target.join("file")).unwrap();
        let staged = base.replace("line 0\n", "previously staged\n");
        fixture.stage("ov/file", staged.as_bytes());
        fixture.stage("unrelated", b"unrelated staged\n");
        let target = staged
            .replace("line 12\n", "accepted middle\n")
            .replace("line 29\n", "skipped end\n");
        fs::write(fixture.target.join("file"), &target).unwrap();
        let mut count = 0;
        fixture
            .run(|_, _| {
                count += 1;
                Ok(if count == 1 {
                    Decision::Accept
                } else {
                    Decision::Skip
                })
            })
            .unwrap();
        assert_eq!(count, 2);
        let expected = staged.replace("line 12\n", "accepted middle\n");
        assert_eq!(fixture.indexed("ov/file").unwrap(), expected.as_bytes());
        assert_eq!(fixture.indexed("unrelated").unwrap(), b"unrelated staged\n");
        assert_eq!(
            fs::read(fixture.target.join("file")).unwrap(),
            target.as_bytes()
        );
        // Git independently reports staged content; target-vs-index assertions
        // above establish that the skipped end remains an unstaged difference.
        let diff = std::process::Command::new("git")
            .current_dir(fixture.repo.workdir().unwrap())
            .args(["diff", "--cached", "--no-ext-diff", "--", "ov/file"])
            .output()
            .unwrap();
        assert!(diff.status.success());
        let text = String::from_utf8(diff.stdout).unwrap();
        assert!(text.contains("+previously staged"));
        assert!(text.contains("+accepted middle"));
        assert!(!text.contains("skipped end"));
        fixture.run(|_, _| Ok(Decision::Accept)).unwrap();
        assert_eq!(fixture.indexed("ov/file").unwrap(), target.as_bytes());
    }

    #[rstest::rstest]
    #[case("cancel")]
    #[case("error")]
    #[case("dry_run")]
    fn abandoned_or_previewed_sessions_preserve_the_exact_index(#[case] outcome: &str) {
        let mut fixture = Fixture::new();
        fs::write(fixture.target.join("file"), "changed\n").unwrap();
        fs::remove_file(fixture.target.join("deleted")).unwrap();
        if outcome == "dry_run" {
            fixture.ctx = Context::builder()
                .root(fixture.target.clone())
                .repository(fixture.ctx.repository.clone())
                .dry_run(true)
                .build();
        }
        let original = fixture.index_bytes();
        let mut calls = 0;
        let result = fixture.run(|_, _| {
            calls += 1;
            if calls == 1 || outcome == "dry_run" {
                return Ok(Decision::Accept);
            }
            if outcome == "error" {
                anyhow::bail!("prompt failed");
            }
            Ok(Decision::Cancel)
        });
        assert_eq!(result.is_err(), outcome == "error");
        assert_eq!(fixture.index_bytes(), original);
    }

    #[test]
    fn dry_run_with_filtered_scope_creates_no_source_objects_or_locks() {
        let mut fixture = Fixture::new();
        fixture.configure("mode = 'checkout'\nexclude = ['deleted']\n");
        fixture.ctx = Context::builder()
            .root(fixture.target.clone())
            .repository(fixture.ctx.repository.clone())
            .dry_run(true)
            .build();
        fs::write(fixture.target.join("file"), "new candidate blob\n").unwrap();
        let inventory = || {
            walkdir::WalkDir::new(fixture.repo.path())
                .into_iter()
                .map(|entry| entry.unwrap())
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| (entry.path().to_path_buf(), fs::read(entry.path()).unwrap()))
                .collect::<BTreeMap<_, _>>()
        };
        let original = inventory();
        fixture
            .run(|_, preview| {
                assert!(preview);
                Ok(Decision::Accept)
            })
            .unwrap();
        assert_eq!(inventory(), original);
    }

    #[test]
    fn finishing_applies_only_choices_accepted_before_finishing() {
        let fixture = Fixture::new();
        fs::write(fixture.target.join("file"), "changed\n").unwrap();
        fs::remove_file(fixture.target.join("deleted")).unwrap();
        let mut calls = 0;
        fixture
            .run(|_, _| {
                calls += 1;
                Ok(if calls == 1 {
                    Decision::Accept
                } else {
                    Decision::Finish
                })
            })
            .unwrap();
        assert!(fixture.indexed("ov/deleted").is_none());
        assert_ne!(fixture.indexed("ov/file").unwrap(), b"changed\n");
        assert!(!fixture.target.join("deleted").exists());
    }

    #[test]
    fn atomic_additions_and_deletions_can_be_accepted_without_changing_targets() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.target.join("deleted")).unwrap();
        fs::write(fixture.target.join("binary"), b"binary\0bytes").unwrap();
        fs::write(fixture.target.join("empty"), b"").unwrap();
        fixture.run(|_, _| Ok(Decision::Accept)).unwrap();
        assert!(fixture.indexed("ov/deleted").is_none());
        assert_eq!(fixture.indexed("ov/binary").unwrap(), b"binary\0bytes");
        assert_eq!(fixture.indexed("ov/empty").unwrap(), b"");
        assert_eq!(
            fs::read(fixture.target.join("binary")).unwrap(),
            b"binary\0bytes"
        );
        assert!(!fixture.target.join("deleted").exists());
    }

    #[test]
    fn concurrent_index_changes_are_preserved_and_owned_locks_are_removed() {
        let fixture = Fixture::new();
        fs::write(fixture.target.join("file"), "changed\n").unwrap();
        let result = fixture.run(|_, _| {
            fixture.stage("unrelated", b"concurrent\n");
            Ok(Decision::Accept)
        });
        assert!(result.unwrap_err().to_string().contains("index changed"));
        assert_eq!(fixture.indexed("unrelated").unwrap(), b"concurrent\n");
        assert_ne!(fixture.indexed("ov/file").unwrap(), b"changed\n");
        assert!(!fixture.repo.path().join("index.lock").exists());
    }

    #[test]
    fn an_existing_lock_is_never_removed_by_patch_staging() {
        let fixture = Fixture::new();
        fs::write(fixture.target.join("file"), "changed\n").unwrap();
        let lock = fixture.repo.path().join("index.lock");
        fs::write(&lock, "another writer").unwrap();
        let original = fixture.index_bytes();
        assert!(
            fixture
                .run(|_, _| Ok(Decision::Accept))
                .unwrap_err()
                .to_string()
                .contains("cannot lock")
        );
        assert_eq!(fs::read(&lock).unwrap(), b"another writer");
        assert_eq!(fixture.index_bytes(), original);
    }

    #[test]
    fn out_of_scope_selections_fail_before_any_choices_are_presented() {
        let fixture = Fixture::new();
        let outside = fixture.repo.workdir().unwrap().join("unrelated");
        assert!(
            fixture
                .paths(&[outside], |_, _| panic!("must validate before prompting"))
                .is_err()
        );
        assert!(
            fixture
                .paths(&[fixture.target.join("over.toml")], |_, _| panic!(
                    "must reject control metadata"
                ))
                .is_err()
        );
        assert!(
            fixture
                .paths(&[fixture.target.join("missing")], |_, _| panic!(
                    "cannot offer an unknown missing path"
                ))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn clean_filters_are_used_for_both_source_validation_and_staged_content() {
        let fixture = Fixture::new();
        let source = fixture.repo.workdir().unwrap();
        fixture
            .repo
            .config()
            .unwrap()
            .set_str("filter.lower.clean", "tr '[:upper:]' '[:lower:]'")
            .unwrap();
        fs::write(source.join("ov/.gitattributes"), "file filter=lower\n").unwrap();
        let base = fs::read_to_string(source.join("ov/file")).unwrap();
        fs::write(source.join("ov/file"), base.to_uppercase()).unwrap();
        let target = base.to_uppercase().replace("LINE 0\n", "FILTERED CHANGE\n");
        fs::write(fixture.target.join("file"), &target).unwrap();
        fixture.run(|_, _| Ok(Decision::Accept)).unwrap();
        assert_eq!(
            fixture.indexed("ov/file").unwrap(),
            target.to_lowercase().as_bytes()
        );
        assert_eq!(
            fs::read_to_string(fixture.target.join("file")).unwrap(),
            target
        );
        assert_eq!(
            fs::read_to_string(source.join("ov/file")).unwrap(),
            base.to_uppercase()
        );
    }

    #[test]
    fn intent_to_add_is_presented_as_a_new_file_without_staging_an_empty_blob() {
        let fixture = Fixture::new();
        fs::write(fixture.repo.workdir().unwrap().join("ov/new"), "").unwrap();
        let result = std::process::Command::new("git")
            .current_dir(fixture.repo.workdir().unwrap())
            .args(["add", "-N", "ov/new"])
            .output()
            .unwrap();
        assert!(result.status.success());
        fs::write(fixture.target.join("new"), "new content\n").unwrap();
        fixture
            .run(|unit, _| {
                assert!(unit.description.contains("Add file"));
                Ok(Decision::Accept)
            })
            .unwrap();
        assert_eq!(fixture.indexed("ov/new").unwrap(), b"new content\n");
    }

    #[test]
    fn ignored_and_nested_repository_paths_are_never_offered_by_directory_selection() {
        let mut fixture = Fixture::new();
        fixture.configure("mode = 'checkout'\nexclude = ['secret']\n[git.nested]\nurl = 'https://example.com/repository.git'\n");
        fs::create_dir(fixture.target.join("nested")).unwrap();
        fs::write(fixture.target.join("nested/foreign"), "foreign\n").unwrap();
        fs::write(fixture.target.join("secret"), "private\n").unwrap();
        fs::write(fixture.target.join("over.toml"), "metadata\n").unwrap();
        fs::write(fixture.target.join("file"), "owned\n").unwrap();
        fixture
            .run(|unit, _| {
                assert_eq!(unit.path, Path::new("ov/file"));
                Ok(Decision::Accept)
            })
            .unwrap();
        assert_eq!(fixture.indexed("ov/file").unwrap(), b"owned\n");
        assert!(fixture.indexed("ov/secret").is_none());
        assert!(fixture.indexed("ov/nested/foreign").is_none());
        assert_eq!(
            fs::read(fixture.target.join("nested/foreign")).unwrap(),
            b"foreign\n"
        );
        assert!(
            fixture
                .paths(&[fixture.target.join("nested/foreign")], |_, _| panic!(
                    "foreign path cannot be offered"
                ))
                .is_err()
        );
    }

    #[test]
    fn independent_source_edits_are_rechecked_after_the_prompt() {
        let fixture = Fixture::new();
        fs::write(fixture.target.join("file"), "target\n").unwrap();
        let original = fixture.index_bytes();
        let result = fixture.run(|_, _| {
            fs::write(
                fixture.repo.workdir().unwrap().join("ov/file"),
                "source changed during prompt\n",
            )?;
            Ok(Decision::Accept)
        });
        assert!(result.unwrap_err().to_string().contains("unstaged edits"));
        assert_eq!(fixture.index_bytes(), original);
        assert!(!fixture.repo.path().join("index.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn ignored_executable_bits_do_not_invent_changes_or_block_source_validation() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        fixture
            .repo
            .config()
            .unwrap()
            .set_bool("core.filemode", false)
            .unwrap();
        fs::set_permissions(
            fixture.repo.workdir().unwrap().join("ov/file"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::set_permissions(
            fixture.target.join("file"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(fixture.target.join("file"), "changed\n").unwrap();
        fixture
            .run(|unit, _| {
                assert!(!unit.description.contains("Mode"));
                Ok(Decision::Accept)
            })
            .unwrap();
        assert_eq!(
            fixture
                .repo
                .index()
                .unwrap()
                .get_path(Path::new("ov/file"), 0)
                .unwrap()
                .mode,
            0o100644
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unselected_symlink_does_not_block_a_selected_regular_file() {
        let fixture = Fixture::new();
        let link = fixture.target.join("link");
        std::os::unix::fs::symlink(fixture.repo.workdir().unwrap().join("unrelated"), &link)
            .unwrap();
        fs::write(fixture.target.join("file"), "changed\n").unwrap();
        fixture
            .paths(&[fixture.target.join("file")], |_, _| Ok(Decision::Accept))
            .unwrap();
        assert_eq!(fixture.indexed("ov/file").unwrap(), b"changed\n");
        assert!(
            fixture
                .paths(&[link], |_, _| panic!("cannot stage a link"))
                .is_err()
        );
    }
}
