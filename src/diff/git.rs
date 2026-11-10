//! Resolve owned Git comparisons independently of reconciliation health.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use git2::{IndexEntryExtendedFlag, Oid, Repository};

use crate::actions::git::diff::{Comparison, Scratch, View};
use crate::desired::{CheckoutScope, DesiredEntry, DesiredTree, MaterializationIntent, Provenance};
use crate::materialize::virtual_checkout::snapshot::{honors_filemode, target_content};
use crate::materialize::virtual_checkout::{git as vc, state};
use crate::materialize::{Materializer, PartialFileMaterializer, SymlinkMaterializer};
use crate::overlays::MaterializationKind;
use crate::overlays::mapping;
use crate::plan::actual::{self, ActualState};

type Entries = BTreeMap<PathBuf, (Oid, i32)>;

#[derive(Default)]
pub(crate) struct Report {
    pub comparisons: Vec<Comparison>,
    pub diagnostics: Vec<String>,
}

impl Report {
    pub fn build(desired: &DesiredTree, views: &[View]) -> Result<Self> {
        let mut report = Self::default();
        let mut linked: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
        for selection in &desired.sources {
            let overlay = &selection.overlay;
            let repo = match Repository::discover(&overlay.root) {
                Ok(repo) => repo,
                Err(error) if error.code() == git2::ErrorCode::NotFound => {
                    report.diagnostics.push(format!(
                        "{}: no Git repository for source {}",
                        overlay.name,
                        overlay.root.display()
                    ));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let workdir = repo.workdir().context("diff source repository is bare")?;
            let root = mapping::physical(&overlay.root)?;
            let prefix = root.strip_prefix(mapping::physical(workdir)?)?;
            let index = repo.index()?;
            let mut candidates: BTreeSet<PathBuf> = index
                .iter()
                .map(|entry| git_path(&entry.path))
                .collect::<Result<_>>()?;
            match repo.head() {
                Ok(head) => {
                    collect_tree_paths(&head.peel_to_tree()?, Path::new(""), &mut candidates)?
                }
                Err(error)
                    if matches!(
                        error.code(),
                        git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
                    ) => {}
                Err(error) => return Err(error.into()),
            }
            for path in candidates {
                let Ok(relative) = path.strip_prefix(prefix) else {
                    continue;
                };
                let source = overlay.root.join(relative);
                if !selection.policy.includes(&source, false) {
                    continue;
                }
                // A terminal directory mode owns all descendants even if a child
                // rule would otherwise select checkout materialization.
                let boundary = selection
                    .routing
                    .directories
                    .keys()
                    .find(|path| relative.starts_with(path))
                    .map(PathBuf::as_path)
                    .unwrap_or(Path::new(""));
                let terminal = relative
                    .ancestors()
                    .skip(1)
                    .filter(|path| path.starts_with(boundary))
                    .filter_map(|path| {
                        let mode = overlay.materialization_for(path);
                        // Only checkout materializes the overlay root itself;
                        // directory-symlink defaults start at child directories.
                        (mode != MaterializationKind::Symlink
                            && (!path.as_os_str().is_empty()
                                || mode == MaterializationKind::Checkout))
                            .then_some((path, mode))
                    })
                    .last();
                // Mode assignments apply to directory units, not individual files.
                let mode = terminal
                    .map(|(_, mode)| mode)
                    .unwrap_or(MaterializationKind::Symlink);
                if mode == MaterializationKind::Checkout {
                    continue;
                }
                let target = selection.routing.target_for(relative);
                // Historical residual paths cannot claim a destination now
                // owned by an independently routed mapping.
                if selection.routing.logical_path_for(&target)? != relative {
                    continue;
                }
                if !source.exists() {
                    let logical = terminal.map(|(path, _)| path).unwrap_or(relative);
                    report.link_diagnostic(
                        &selection.routing.target_for(logical),
                        &overlay.root.join(logical),
                    )?;
                }
                linked
                    .entry(workdir.to_path_buf())
                    .or_default()
                    .insert(path);
            }
        }

        let checkout_state = if desired
            .entries()
            .iter()
            .any(|entry| entry.intent.is_virtual_checkout())
        {
            Some(state::state_file()?.inspect_blocking()?)
        } else {
            None
        };
        for entry in desired.entries() {
            match &entry.intent {
                MaterializationIntent::SymlinkFile { source, .. }
                | MaterializationIntent::SymlinkDirectory { source, .. } => {
                    report.link_diagnostic(&entry.target, source)?;
                    // Sidecars can refer to another repository, outside the overlay tree.
                    if matches!(entry.provenance, Provenance::SymlinkSidecar { .. }) {
                        match vc::discover_source(source) {
                            Ok((repo, path)) => {
                                // Git rejects an empty pathspec; a directory link
                                // to the worktree root explicitly owns that root.
                                let path = if path.as_os_str().is_empty() {
                                    PathBuf::from(".")
                                } else {
                                    path
                                };
                                linked
                                    .entry(
                                        repo.workdir()
                                            .context("source has no worktree")?
                                            .to_path_buf(),
                                    )
                                    .or_default()
                                    .insert(path);
                            }
                            Err(error) => report
                                .diagnostics
                                .push(format!("{}: {error}", entry.target.display())),
                        }
                    }
                }
                intent if intent.is_virtual_checkout() => {
                    let state = checkout_state.as_ref().context("missing checkout state")?;
                    state::verify_live_target_snapshot(state, entry)?;
                    let Some(record) = state.checkouts.get(&state::key_for(&entry.target)) else {
                        report.diagnostics.push(format!(
                            "{}: virtual checkout has no recorded base; apply it first",
                            entry.target.display()
                        ));
                        continue;
                    };
                    state::verify(entry, record)?;
                    report
                        .comparisons
                        .extend(virtual_comparisons(entry, record, views)?);
                }
                MaterializationIntent::PartialFile { .. } => {
                    // Partial ownership is a managed block, never a whole-file Git snapshot.
                    report.entry_diagnostic(
                        entry,
                        super::classify_fs(entry, &PartialFileMaterializer.classify(entry)?)?,
                    );
                }
                MaterializationIntent::Directory => report.entry_diagnostic(
                    entry,
                    super::classify_fs(entry, &SymlinkMaterializer.classify(entry)?)?,
                ),
                MaterializationIntent::Checkout => {
                    report.entry_diagnostic(entry, super::classify_checkout(entry)?)
                }
                _ => {}
            }
        }
        for (root, scope) in linked {
            let repo = Repository::open(&root)?;
            for &view in views {
                report.comparisons.push(Comparison {
                    repository: root.clone(),
                    scope: scope.iter().cloned().collect(),
                    view,
                    trees: None,
                    baseline: None,
                    scratch: Scratch::new(&repo)?,
                });
            }
        }
        report.diagnostics.sort();
        report.diagnostics.dedup();
        Ok(report)
    }

    fn entry_diagnostic(&mut self, entry: &DesiredEntry, change: super::Change) {
        let diagnostic = super::DiffEntry {
            entry: entry.clone(),
            change,
        };
        if diagnostic.needs_attention() {
            self.diagnostics.push(diagnostic.to_string());
        }
    }

    fn link_diagnostic(&mut self, target: &Path, source: &Path) -> Result<()> {
        let problem = match actual::inspect(target)? {
            ActualState::Symlink { points_to } => {
                let resolved = if points_to.is_absolute() {
                    points_to
                } else {
                    target
                        .parent()
                        .context("link has no parent")?
                        .join(points_to)
                };
                if mapping::absolute(&resolved)? != mapping::absolute(source)?
                    && mapping::physical(&resolved)? != mapping::physical(source)?
                {
                    Some(format!(
                        "misdirected symlink (expected {})",
                        source.display()
                    ))
                } else if !resolved.exists() {
                    Some("broken symlink".into())
                } else {
                    None
                }
            }
            ActualState::Missing => Some("missing symlink".into()),
            actual => Some(format!("expected symlink, found {actual}")),
        };
        if let Some(problem) = problem {
            self.diagnostics
                .push(format!("{}: {problem}", target.display()));
        }
        Ok(())
    }
}

fn git_path(bytes: &[u8]) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        Ok(PathBuf::from(std::str::from_utf8(bytes)?))
    }
}

fn collect_tree_paths(
    tree: &git2::Tree<'_>,
    prefix: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let mut failure = None;
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() != Some(git2::ObjectType::Tree) {
            match git_path(entry.name_bytes()) {
                Ok(path) => {
                    paths.insert(prefix.join(root).join(path));
                }
                Err(error) => {
                    failure = Some(error);
                    return git2::TreeWalkResult::Abort;
                }
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}

fn virtual_comparisons(
    entry: &DesiredEntry,
    record: &state::VirtualCheckoutRecord,
    views: &[View],
) -> Result<Vec<Comparison>> {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        anyhow::bail!("virtual checkout has no source");
    };
    let (repo, managed) = vc::discover_source(source)?;
    let scope = entry.intent.checkout_scope();
    let base = vc::base_tree(&repo, Some(&record.base_oid))?;
    let filemode = honors_filemode(&repo)?;
    let mut base_entries = vc::tracked_entries(&base)?;
    let owns = |path: &Path| -> bool {
        path.strip_prefix(&managed)
            .is_ok_and(|relative| scope.owns_source(relative) && !vc::is_overlay_metadata(relative))
    };
    base_entries.retain(|path, _| owns(path));
    let index = repo.index()?;
    let mut index_entries = Entries::new();
    for item in index.iter() {
        let path = git_path(&item.path)?;
        if owns(&path) {
            ensure!(
                item.flags & 0x3000 == 0,
                "owned path '{}' has unresolved index conflicts",
                path.display()
            );
            // Intent-to-add reserves an index path, not a staged empty blob.
            // Its target content must remain an unstaged new-file addition.
            if IndexEntryExtendedFlag::from_bits_truncate(item.flags_extended).is_intent_to_add() {
                continue;
            }
            if vc::is_supported_mode(item.mode) {
                index_entries.insert(path, (item.id, item.mode as i32));
            } else {
                ensure!(
                    base.get_path(&path)
                        .ok()
                        .map(|old| (old.id(), old.filemode() as u32))
                        == Some((item.id, item.mode)),
                    "unsupported staged virtual checkout entry at '{}'",
                    path.display()
                );
            }
        }
    }
    for path in base_entries.keys().chain(index_entries.keys()) {
        ensure!(
            scope.owns_target(path.strip_prefix(&managed)?),
            "source path '{}' collides with a protected target namespace",
            path.display()
        );
    }
    let paths: BTreeSet<_> = base_entries
        .keys()
        .chain(index_entries.keys())
        .cloned()
        .collect();
    let mut comparisons = Vec::new();
    for &view in views {
        let scratch = Scratch::new(&repo)?;
        let left_entries = if view == View::Staged {
            &base_entries
        } else {
            &index_entries
        };
        let left = vc::build_content_tree(&scratch.repo, left_entries)?.id();
        let mut right_entries = Entries::new();
        if view == View::Staged {
            right_entries.clone_from(&index_entries);
        } else {
            ensure!(
                matches!(
                    actual::inspect(&entry.target)?,
                    ActualState::Directory | ActualState::Missing
                ),
                "virtual checkout '{}' is not an ordinary directory",
                entry.target.display()
            );
            let mut candidates = paths.clone();
            candidates.extend(
                target_files(&entry.target, &scope)?
                    .into_iter()
                    .map(|path| managed.join(path)),
            );
            for path in candidates {
                let relative = path.strip_prefix(&managed)?;
                if !owns(&path) || !scope.owns_target(relative) {
                    continue;
                }
                if let Some((content, mut mode)) = target_content(&entry.target, relative)? {
                    vc::ensure_supported_path(&base, &index, &path)
                        .context("cannot compare unsupported virtual checkout content")?;
                    let oid = scratch.target_blob(&repo, &path, &content)?;
                    if !filemode {
                        // A platform/configuration ignoring executable bits must
                        // not invent a mode change against a tracked executable.
                        mode = index_entries
                            .get(&path)
                            .map(|(_, mode)| *mode)
                            .unwrap_or(0o100644);
                    }
                    right_entries.insert(path, (oid, mode));
                }
            }
        }
        let right = vc::build_content_tree(&scratch.repo, &right_entries)?.id();
        let scope: Vec<_> = left_entries
            .keys()
            .chain(right_entries.keys())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        comparisons.push(Comparison {
            repository: repo
                .workdir()
                .context("source has no worktree")?
                .to_path_buf(),
            scope,
            view,
            trees: Some((left, right)),
            baseline: Some(record.base_oid.clone()),
            scratch,
        });
    }
    Ok(comparisons)
}

fn target_files(root: &Path, scope: &CheckoutScope) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            entry.path().strip_prefix(root).is_ok_and(|path| {
                scope.owns_source_kind(path, entry.file_type().is_dir())
                    && scope.owns_target_kind(path, entry.file_type().is_dir())
                    && !vc::is_overlay_metadata(path)
            })
        })
    {
        let entry = entry?;
        // Following a newly added link would include content outside ownership;
        // silently dropping it would disguise an unsupported target change.
        ensure!(
            entry.file_type().is_file() || entry.file_type().is_dir(),
            "unsupported virtual checkout target entry '{}'",
            entry.path().display()
        );
        if entry.file_type().is_file() {
            files.push(entry.path().strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(files)
}
