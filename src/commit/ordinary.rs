//! Capture explicit ordinary checkout operands without publishing staging.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use crate::actions::git::diff::Scratch;
use crate::desired::{DesiredEntry, Provenance};
use crate::materialize::virtual_checkout::{git, snapshot, state};

use super::CommitOptions;

pub(crate) struct File {
    pub target: PathBuf,
    pub relative: PathBuf,
    pub path: PathBuf,
    pub update: git::OrdinaryUpdate,
    pub kind: git::FileChangeKind,
}

struct Check {
    entry: DesiredEntry,
    relative: PathBuf,
    content: Option<(zeroize::Zeroizing<Vec<u8>>, i32)>,
}

#[derive(Default)]
pub(crate) struct Prepared {
    pub files: Vec<File>,
    pub selected_sources: BTreeSet<PathBuf>,
    checks: Vec<Check>,
}

impl Prepared {
    pub fn verify(&self) -> Result<()> {
        for check in &self.checks {
            ensure!(
                snapshot::target_content(
                    &check.entry.target,
                    &check.relative,
                    &check.entry.access
                )? == check.content,
                "checkout input '{}' changed while preparing the commit; retry",
                check.entry.target.join(&check.relative).display()
            );
        }
        Ok(())
    }
}

pub(crate) fn prepare(entries: &[DesiredEntry], opts: &CommitOptions) -> Result<Prepared> {
    let mut prepared = Prepared::default();
    if opts.paths.is_empty() {
        return Ok(prepared);
    }
    for entry in entries {
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            continue;
        };
        let Some(record) = state::record_for_entry(entry)? else {
            continue;
        };
        state::verify(entry, &record)?;
        let (repo, managed) = git::discover_source(source)?;
        let scope = entry.intent.checkout_scope();
        let head = git::base_tree(&repo, None)?;
        let tracked = git::scoped_entries(&repo, &head, &managed, &scope)?;
        let index = repo.index()?;
        let mut candidates: BTreeSet<_> = tracked.keys().cloned().collect();
        // HEAD and index together retain deleted paths and staged acquisitions absent on disk.
        for indexed in index.iter() {
            let path = PathBuf::from(std::str::from_utf8(&indexed.path)?);
            if let Ok(relative) = path.strip_prefix(&managed) {
                candidates.insert(relative.to_path_buf());
            }
        }
        if let Some(projection) = &scope.projection {
            candidates.extend(projection.iter().cloned());
        } else {
            // Legacy exclusive checkouts may own new files; walk only named subtrees.
            for prefix in &opts.paths {
                let target = prefix
                    .strip_prefix(source)
                    .map_or_else(|_| prefix.clone(), |relative| entry.target.join(relative));
                let target = if entry.target.starts_with(&target) {
                    entry.target.clone()
                } else {
                    target
                };
                if !target.starts_with(&entry.target) {
                    continue;
                }
                if !target.is_dir() {
                    candidates.insert(target.strip_prefix(&entry.target)?.to_path_buf());
                    continue;
                }
                for item in walkdir::WalkDir::new(&target)
                    .into_iter()
                    .filter_entry(|item| {
                        item.path()
                            .strip_prefix(&entry.target)
                            .is_ok_and(|relative| {
                                scope.owns_source_kind(relative, item.file_type().is_dir())
                                    && scope.owns_target_kind(relative, item.file_type().is_dir())
                            })
                    })
                {
                    let item = item?;
                    if !item.file_type().is_dir() {
                        candidates.insert(item.path().strip_prefix(&entry.target)?.to_path_buf());
                    }
                }
            }
        }
        let scratch = Scratch::new(&repo)?;
        for relative in candidates {
            if !super::selects(entry, &relative, &opts.paths)
                || !scope.owns_source(&relative)
                || !scope.owns_target(&relative)
                || git::is_overlay_metadata(&relative)
                || record.generated.as_ref().is_some_and(|generated| {
                    generated.sources.contains_key(&relative)
                        || generated
                            .sources
                            .values()
                            .any(|path| path == &source.join(&relative))
                })
            {
                continue;
            }
            let selected = source.join(&relative);
            // Even a target reverted to HEAD must suppress its obsolete staged snapshot.
            prepared.selected_sources.insert(selected);
            let path = managed.join(&relative);
            git::ensure_supported_path(&head, &index, &path)?;
            let old = tracked.get(&relative).copied();
            let indexed = index
                .get_path(&path, 0)
                .map(|entry| (entry.id, entry.mode as i32));
            let expected = snapshot::canonical_source_entry(&repo, &path)?;
            // Replacing staging is intentional; overwriting independent source work is not.
            ensure!(
                git::entries_match(expected, old) || git::entries_match(expected, indexed),
                "source file '{}' has unstaged edits",
                source.join(&relative).display()
            );
            crate::desired::content::validate_selection(entry, &[entry.target.join(&relative)])?;
            let content = snapshot::target_content(&entry.target, &relative, &entry.access)?;
            let canonical = content
                .as_ref()
                .map(|(bytes, mode)| -> Result<_> {
                    let (oid, mode) = snapshot::canonical_entry(
                        &repo,
                        &scratch,
                        &path,
                        bytes,
                        *mode,
                        &[indexed, old],
                    )?;
                    Ok((oid, mode, scratch.repo.find_blob(oid)?.content().to_vec()))
                })
                .transpose()?;
            let new = canonical.as_ref().map(|(oid, mode, _)| (*oid, *mode));
            if !git::entries_match(old, new) {
                let kind = match (old, new) {
                    (None, Some(_)) => git::FileChangeKind::Added,
                    (Some(_), None) => git::FileChangeKind::Deleted,
                    _ => git::FileChangeKind::Modified,
                };
                prepared.files.push(File {
                    target: entry.target.clone(),
                    relative: relative.clone(),
                    path,
                    update: git::OrdinaryUpdate {
                        content: canonical.map(|(_, mode, bytes)| (bytes, mode as u32)),
                        expected,
                    },
                    kind,
                });
            }
            prepared.checks.push(Check {
                entry: entry.clone(),
                relative,
                content,
            });
        }
    }
    prepared
        .verify()
        .context("checkout changed during commit preparation")?;
    Ok(prepared)
}
