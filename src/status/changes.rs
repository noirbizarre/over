//! File changes are independent of materialization health.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent, Provenance};
use crate::materialize::virtual_checkout::git::{self, FileChangeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileState {
    Added,
    Modified,
    Deleted,
    TypeChanged,
    Unmerged,
}

impl FileState {
    pub fn symbol(self) -> char {
        match self {
            Self::Added => 'A',
            Self::Modified => 'M',
            Self::Deleted => 'D',
            Self::TypeChanged => 'T',
            Self::Unmerged => 'U',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Change {
    pub worktree: PathBuf,
    pub target: Option<PathBuf>,
    pub path: PathBuf,
    pub staged: Option<FileState>,
    pub unstaged: Option<FileState>,
}

impl Change {
    /// Git paths use forward slashes, but a Unix backslash is a literal filename byte.
    pub fn git_path(&self) -> Result<String> {
        let path = self
            .path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("status requires UTF-8 paths"))?;
        Ok(if cfg!(windows) {
            path.replace('\\', "/")
        } else {
            path.to_owned()
        })
    }

    pub fn xy(&self) -> String {
        [self.staged, self.unstaged]
            .into_iter()
            .map(|state| state.map_or(' ', FileState::symbol))
            .collect()
    }

    pub fn short(&self) -> Result<String> {
        // Lossy path conversion would make two different Git paths indistinguishable to scripts.
        let text = |path: &Path| -> Result<String> {
            Ok(path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("status requires UTF-8 paths"))?
                .to_owned())
        };
        Ok(format!(
            "{}\t{}\t{}\t{}\n",
            self.xy(),
            serde_json::to_string(&text(&self.worktree)?)?,
            serde_json::to_string(&self.git_path()?)?,
            serde_json::to_string(&self.target.as_deref().map(text).transpose()?)?
        ))
    }
}

pub(crate) fn virtual_changes(
    entry: &DesiredEntry,
    inspection: &super::virtual_checkout::Inspection,
) -> Result<Vec<Change>> {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        unreachable!()
    };
    let (repo, managed) = git::discover_source(source)?;
    let worktree = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("source has no worktree"))?
        .to_path_buf();
    let worktree = crate::overlays::mapping::lexical(&worktree);
    let mut changes = BTreeMap::new();
    for (staged, files) in [(true, &inspection.staged), (false, &inspection.unstaged)] {
        for file in files {
            let change = changes.entry(file.path.clone()).or_insert_with(|| Change {
                worktree: worktree.clone(),
                target: Some(entry.target.clone()),
                path: managed.join(&file.path),
                staged: None,
                unstaged: None,
            });
            let state = Some(match file.kind {
                FileChangeKind::Added => FileState::Added,
                FileChangeKind::Modified => FileState::Modified,
                FileChangeKind::Deleted => FileState::Deleted,
            });
            if staged {
                change.staged = state;
            } else {
                change.unstaged = state;
            }
        }
    }
    for path in &inspection.unmerged {
        changes.insert(
            path.clone(),
            Change {
                worktree: worktree.clone(),
                target: Some(entry.target.clone()),
                path: managed.join(path),
                staged: Some(FileState::Unmerged),
                unstaged: Some(FileState::Unmerged),
            },
        );
    }
    Ok(changes.into_values().collect())
}

#[derive(Default)]
pub(crate) struct NativeCache {
    candidates: BTreeMap<PathBuf, Vec<Vec<u8>>>,
    changes: BTreeMap<PathBuf, BTreeMap<PathBuf, Option<Change>>>,
    whole: std::collections::BTreeSet<PathBuf>,
}

pub(crate) fn native_changes(
    desired: &DesiredTree,
    cache: &mut NativeCache,
) -> Result<Vec<Change>> {
    let mut queries = BTreeMap::<PathBuf, std::collections::BTreeSet<PathBuf>>::new();
    let mut full_repositories = std::collections::BTreeSet::new();
    let virtual_sources = desired
        .entries()
        .iter()
        .filter_map(|entry| {
            if entry.intent.is_virtual_checkout()
                && let Provenance::Overlay { source, .. } = &entry.provenance
            {
                Some(
                    crate::overlays::mapping::physical(source)
                        .map(|source| (source, entry.intent.checkout_scope())),
                )
            } else {
                None
            }
        })
        .collect::<Result<Vec<_>>>()?;
    for source in &desired.sources {
        let repo = match git2::Repository::discover(&source.overlay.root) {
            Ok(repo) => repo,
            Err(error) if error.code() == git2::ErrorCode::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let Some(worktree) = repo.workdir() else {
            continue;
        };
        let worktree = crate::overlays::mapping::lexical(worktree);
        // Git resolves source aliases (notably macOS /var); policy retains the declared namespace.
        let physical_worktree = crate::overlays::mapping::physical(&worktree)?;
        let physical_source = crate::overlays::mapping::physical(&source.overlay.root)?;
        let managed = physical_source.strip_prefix(&physical_worktree)?;
        let descriptors = source
            .overlay
            .scopes
            .iter()
            .map(|scope| crate::overlays::mapping::physical(&scope.descriptor))
            .collect::<Result<std::collections::BTreeSet<_>>>()?;
        let paths = queries.entry(worktree.clone()).or_default();
        // Index paths include staged additions; HEAD additionally preserves staged deletions.
        if !cache.candidates.contains_key(&worktree) {
            let mut candidates = repo
                .index()?
                .iter()
                .map(|entry| entry.path)
                .collect::<Vec<_>>();
            match repo.head() {
                Ok(head) => {
                    head.peel_to_tree()?
                        .walk(git2::TreeWalkMode::PreOrder, |root, entry| {
                            if entry.kind() != Some(git2::ObjectType::Tree) {
                                let mut path = root.as_bytes().to_vec();
                                path.extend(entry.name_bytes());
                                candidates.push(path);
                            }
                            git2::TreeWalkResult::Ok
                        })?;
                }
                Err(error)
                    if matches!(
                        error.code(),
                        git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound
                    ) => {}
                Err(error) => return Err(error.into()),
            }
            cache.candidates.insert(worktree.clone(), candidates);
        }
        for bytes in &cache.candidates[&worktree] {
            if std::str::from_utf8(bytes).is_err() {
                // An unrelated non-text filename must not make a scoped status fail.
                git::relative_index_path(bytes, managed)?;
                continue;
            }
            let relative = Path::new(std::str::from_utf8(bytes)?);
            let absolute = physical_worktree.join(relative);
            let in_source = relative.strip_prefix(managed).ok().is_some_and(|tail| {
                !source
                    .policy
                    .is_ignored(&source.overlay.root.join(tail), false)
            });
            let descriptor = descriptors.contains(&absolute);
            // Ancestor ignore inputs affect this overlay even when removed from today's filesystem.
            let ignore_input = absolute
                .file_name()
                .is_some_and(|name| name == ".overignore")
                && absolute
                    .parent()
                    .is_some_and(|parent| physical_source.starts_with(parent));
            if !(in_source || descriptor || ignore_input) {
                continue;
            }
            // Virtual content compares against its target, never against source worktree bytes too.
            let virtual_content = virtual_sources.iter().any(|(source, scope)| {
                absolute
                    .strip_prefix(source)
                    .ok()
                    .is_some_and(|path| scope.owns_source(path))
                    && !crate::overlays::ignore::is_control_file(&absolute)
            });
            if virtual_content {
                continue;
            }
            paths.insert(relative.to_path_buf());
        }
    }
    for entry in desired.entries() {
        if matches!(entry.intent, MaterializationIntent::Checkout)
            && let Provenance::Git {
                repo_key, config, ..
            } = &entry.provenance
            && repo_key == crate::actions::git::config::ROOT_PATH
            && entry.target.exists()
        {
            if desired.blocking_scaffold(&entry.target)?.is_some()
                || !entry.target.symlink_metadata()?.is_dir()
            {
                // A checkout behind a scaffold or foreign link is a structural finding,
                // not permission to run Git or filters in that link's referent.
                continue;
            }
            if config.worktree || config.worktrees.is_some() {
                let repo = git2::Repository::open_bare(entry.target.join(".git"))?;
                for name in repo.worktrees()?.iter() {
                    let name =
                        name?.ok_or_else(|| anyhow::anyhow!("worktree name is not UTF-8"))?;
                    queries.entry(entry.target.join(name)).or_default();
                    full_repositories.insert(entry.target.join(name));
                }
            } else {
                queries.entry(entry.target.clone()).or_default();
                full_repositories.insert(entry.target.clone());
            }
        }
    }
    let mut changes = Vec::new();
    for (worktree, paths) in queries {
        // An empty source selection must not accidentally query the entire enclosing repository.
        let whole = full_repositories.contains(&worktree);
        if paths.is_empty() && !whole {
            continue;
        }
        let repo = git2::Repository::open(&worktree)?;
        let known = cache.changes.entry(worktree.clone()).or_default();
        if whole && cache.whole.contains(&worktree) {
            changes.extend(known.values().flatten().cloned());
            continue;
        }
        let pending = paths
            .iter()
            .filter(|path| !known.contains_key(*path))
            .cloned()
            .collect::<Vec<_>>();
        if !whole && pending.is_empty() {
            changes.extend(
                paths
                    .iter()
                    .filter_map(|path| known.get(path).and_then(Clone::clone)),
            );
            continue;
        }
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(false)
            .include_ignored(false)
            .exclude_submodules(true)
            .renames_head_to_index(false)
            .renames_index_to_workdir(false)
            .disable_pathspec_match(true)
            .update_index(false);
        if !whole {
            for path in &pending {
                opts.pathspec(path);
            }
        }
        for path in &pending {
            known.insert(path.clone(), None);
        }
        let index = repo.index()?;
        for entry in repo.statuses(Some(&mut opts))?.iter() {
            let flags = entry.status();
            ensure!(
                !flags.contains(git2::Status::WT_UNREADABLE),
                "cannot read tracked file"
            );
            let state = |new, modified, deleted, changed| {
                if flags.is_conflicted() {
                    Some(FileState::Unmerged)
                } else if flags.contains(changed) {
                    Some(FileState::TypeChanged)
                } else if flags.contains(new) {
                    Some(FileState::Added)
                } else if flags.contains(deleted) {
                    Some(FileState::Deleted)
                } else if flags.contains(modified) {
                    Some(FileState::Modified)
                } else {
                    None
                }
            };
            let mut change = Change {
                worktree: worktree.clone(),
                target: None,
                path: PathBuf::from(std::str::from_utf8(entry.path_bytes())?),
                staged: state(
                    git2::Status::INDEX_NEW,
                    git2::Status::INDEX_MODIFIED,
                    git2::Status::INDEX_DELETED,
                    git2::Status::INDEX_TYPECHANGE,
                ),
                unstaged: state(
                    git2::Status::WT_NEW,
                    git2::Status::WT_MODIFIED,
                    git2::Status::WT_DELETED,
                    git2::Status::WT_TYPECHANGE,
                ),
            };
            if index.get_path(&change.path, 0).is_some_and(|entry| {
                git2::IndexEntryExtendedFlag::from_bits_truncate(entry.flags_extended)
                    .is_intent_to_add()
            }) {
                // Reserved paths have no staged blob, even when libgit2 exposes INDEX_NEW.
                change.staged = None;
                if change.unstaged != Some(FileState::Deleted) {
                    change.unstaged = Some(FileState::Added);
                }
            }
            known.insert(change.path.clone(), Some(change));
        }
        if whole {
            cache.whole.insert(worktree);
            changes.extend(known.values().flatten().cloned());
        } else {
            changes.extend(
                paths
                    .iter()
                    .filter_map(|path| known.get(path).and_then(Clone::clone)),
            );
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn fixture() -> (tempfile::TempDir, git2::Repository, DesiredTree) {
        let temp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        fs::write(temp.path().join("file"), "original\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file")).unwrap();
        index.write().unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        {
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
                .unwrap();
        }
        let desired = DesiredTree::from_entries(vec![DesiredEntry {
            target: temp.path().to_path_buf(),
            permissions: None,
            intent: MaterializationIntent::Checkout,
            provenance: Provenance::Git {
                overlay: "selected".into(),
                repo_key: ".".into(),
                config: Box::new(
                    serde_json::from_value(serde_json::json!("https://example.com/repo.git"))
                        .unwrap(),
                ),
            },
        }]);
        (temp, repo, desired)
    }

    #[test]
    fn native_renames_are_deletion_and_addition_without_untracked_discovery() {
        let (temp, repo, desired) = fixture();
        fs::rename(temp.path().join("file"), temp.path().join("renamed")).unwrap();
        fs::write(temp.path().join("untracked"), "unrelated\n").unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(Path::new("file")).unwrap();
        index.add_path(Path::new("renamed")).unwrap();
        index.write().unwrap();
        let changes = native_changes(&desired, &mut NativeCache::default()).unwrap();
        assert_eq!(
            changes
                .iter()
                .map(|change| (change.path.as_path(), change.xy()))
                .collect::<Vec<_>>(),
            vec![
                (Path::new("file"), "D ".into()),
                (Path::new("renamed"), "A ".into())
            ]
        );
        assert_eq!(
            super::super::git::inspect_reporting(&desired.entries()[0]).unwrap(),
            super::super::Status::Applied
        );
        // Reporting must not make a staged checkout safe for destructive removal.
        assert_eq!(
            super::super::git::inspect_content(&desired.entries()[0]).unwrap(),
            super::super::Status::Modified
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_type_changes_and_backslashes_retain_their_git_identity() {
        let (temp, _repo, desired) = fixture();
        fs::remove_file(temp.path().join("file")).unwrap();
        std::os::unix::fs::symlink("referent", temp.path().join("file")).unwrap();
        let changes = native_changes(&desired, &mut NativeCache::default()).unwrap();
        assert_eq!(changes[0].xy(), " T");
        let mut change = changes[0].clone();
        change.path = PathBuf::from("tab\tline\nquote\"back\\slash");
        let row = change.short().unwrap();
        let fields = row.trim_end_matches('\n').split('\t').collect::<Vec<_>>();
        assert_eq!(fields.len(), 4);
        assert_eq!(
            serde_json::from_str::<String>(fields[2]).unwrap(),
            change.path.to_str().unwrap()
        );
    }

    #[test]
    fn unresolved_native_index_stages_are_one_unmerged_file() {
        let (_temp, repo, desired) = fixture();
        let mut index = repo.index().unwrap();
        let mut entry = index.get_path(Path::new("file"), 0).unwrap();
        index.remove_path(Path::new("file")).unwrap();
        for stage in 1..=3 {
            entry.flags = (entry.flags & !0x3000) | (stage << 12);
            entry.id = repo.blob(format!("stage {stage}\n").as_bytes()).unwrap();
            index.add(&entry).unwrap();
        }
        index.write().unwrap();
        let changes = native_changes(&desired, &mut NativeCache::default()).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].xy(), "UU");
    }

    #[cfg(unix)]
    #[test]
    fn native_changes_never_inspect_through_checkout_links_or_blocked_scaffolds() {
        let (temp, _repo, desired) = fixture();
        fs::write(temp.path().join("file"), "foreign dirty content\n").unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(temp.path(), &alias).unwrap();
        let mut checkout = desired.entries()[0].clone();
        checkout.target = alias.clone();
        let linked = DesiredTree::from_entries(vec![checkout.clone()]);
        assert!(
            native_changes(&linked, &mut NativeCache::default())
                .unwrap()
                .is_empty()
        );
        checkout.target = alias.join("nested");
        let blocked = DesiredTree::from_entries(vec![
            DesiredEntry {
                target: alias,
                permissions: None,
                intent: MaterializationIntent::Directory,
                provenance: Provenance::Overlay {
                    overlay: "selected".into(),
                    source: temp.path().to_path_buf(),
                },
            },
            checkout,
        ]);
        fs::create_dir(temp.path().join("nested")).unwrap();
        assert!(
            native_changes(&blocked, &mut NativeCache::default())
                .unwrap()
                .is_empty()
        );
    }
}
