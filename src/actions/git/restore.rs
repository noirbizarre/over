//! Snapshot and publish selected restore destinations without Git checkout side effects.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use git2::{IndexEntryExtendedFlag, Oid, Repository};

use super::index::{IndexLock, read_index};
use crate::materialize::virtual_checkout::git::{self, SourceTransaction};
use crate::restore::RestoreOptions;

pub(crate) struct RestorePath {
    pub repository: PathBuf,
    pub source: PathBuf,
    pub root: PathBuf,
    pub relative: PathBuf,
}

struct PreparedIndex {
    repo: Repository,
    path: PathBuf,
    original: Option<Vec<u8>>,
    _directory: tempfile::TempDir,
    index: git2::Index,
    head: Oid,
    tree: Option<Oid>,
    changed: bool,
}

type Content = Option<(Vec<u8>, u32)>;

struct TargetUpdate<'a> {
    request: &'a RestorePath,
    before: Content,
    after: Content,
}

pub(crate) fn restore_paths(
    paths: &[RestorePath],
    options: &RestoreOptions,
    dry_run: bool,
    revalidate: impl Fn() -> Result<()>,
) -> Result<()> {
    let mut indexes = BTreeMap::new();
    let mut targets = Vec::new();
    let mut index_updates = Vec::new();
    for request in paths {
        if !indexes.contains_key(&request.repository) {
            let repo = Repository::open(&request.repository)?;
            let path = repo
                .index()?
                .path()
                .context("source index has no path")?
                .to_path_buf();
            let original = read_index(&path)?;
            let directory = tempfile::tempdir()?;
            let copy = directory.path().join("index");
            if let Some(bytes) = &original {
                fs::write(&copy, bytes)?;
            }
            let index = git2::Index::open(&copy)?;
            let head = repo.head()?.peel_to_tree()?.id();
            let tree = if let Some(revision) = &options.source {
                Some(
                    repo.revparse_single(revision)
                        .with_context(|| format!("invalid restore revision '{revision}'"))?
                        .peel_to_tree()
                        .with_context(|| format!("restore revision '{revision}' is not a tree"))?
                        .id(),
                )
            } else if options.staged {
                Some(head)
            } else {
                None
            };
            indexes.insert(
                request.repository.clone(),
                PreparedIndex {
                    repo,
                    path,
                    original,
                    _directory: directory,
                    index,
                    head,
                    tree,
                    changed: false,
                },
            );
        }
        let prepared = indexes
            .get_mut(&request.repository)
            .context("missing restore repository")?;
        let path = &request.source;
        ensure!(
            (1..=3).all(|stage| prepared.index.get_path(path, stage).is_none()),
            "'{}' has unresolved index conflicts",
            path.display()
        );
        let indexed = prepared.index.get_path(path, 0);
        if let Some(entry) = &indexed {
            ensure!(
                entry.flags_extended & IndexEntryExtendedFlag::INTENT_TO_ADD.bits() == 0,
                "'{}' is intent-to-add; stage or unstage it before restoring",
                path.display()
            );
        }
        let before = indexed.as_ref().map(|entry| (entry.id, entry.mode));
        let head = prepared.repo.find_tree(prepared.head)?;
        let head_entry = tree_entry(&head, path)?;
        let selected = if let Some(oid) = prepared.tree {
            tree_entry(&prepared.repo.find_tree(oid)?, path)?
        } else {
            before
        };
        ensure!(
            if prepared.tree.is_none() {
                before.is_some()
            } else {
                before.is_some() || head_entry.is_some() || selected.is_some()
            },
            "'{}' does not match a tracked restore path",
            path.display()
        );
        for (_, mode) in before.into_iter().chain(head_entry).chain(selected) {
            ensure!(
                git::is_supported_mode(mode),
                "unsupported tracked restore entry '{}'",
                path.display()
            );
        }
        if options.writes_worktree() {
            let target = target_content(&request.root, &request.relative)?;
            // Recovering historical-only content must not overwrite a new local file.
            ensure!(
                before.is_some() || head_entry.is_some() || target.is_none(),
                "refusing to overwrite untracked file '{}'",
                request.root.join(&request.relative).display()
            );
            let after = selected
                .map(|(oid, mode)| -> Result<_> {
                    Ok((prepared.repo.find_blob(oid)?.content().to_vec(), mode))
                })
                .transpose()?;
            if target != after {
                targets.push(TargetUpdate {
                    request,
                    before: target,
                    after,
                });
            }
        }
        if options.staged && before != selected {
            if let Some((oid, mode)) = selected {
                let serialized = path
                    .to_str()
                    .context("source path is not UTF-8")?
                    .to_owned();
                #[cfg(windows)]
                let serialized = serialized.replace('\\', "/");
                let prefix = format!("{serialized}/");
                // Index::add resolves file/directory collisions by dropping other
                // entries. Explicit file restore must never unstage those paths.
                ensure!(
                    !prepared
                        .index
                        .iter()
                        .any(|entry| entry.path.starts_with(prefix.as_bytes()))
                        && !path
                            .ancestors()
                            .skip(1)
                            // git2::Index::get_path panics on the empty root path.
                            .take_while(|ancestor| !ancestor.as_os_str().is_empty())
                            .any(|ancestor| { prepared.index.get_path(ancestor, 0).is_some() }),
                    "'{}' conflicts with other indexed paths",
                    path.display()
                );
                let entry = git2::IndexEntry {
                    ctime: git2::IndexTime::new(0, 0),
                    mtime: git2::IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    uid: 0,
                    gid: 0,
                    mode,
                    file_size: prepared
                        .repo
                        .find_blob(oid)?
                        .size()
                        .try_into()
                        .context("restore file too large")?,
                    id: oid,
                    flags: 0,
                    flags_extended: 0,
                    path: serialized.into_bytes(),
                };
                prepared.index.add(&entry)?;
            } else {
                prepared.index.remove_path(path)?;
            }
            prepared.changed = true;
            index_updates.push((path, selected.is_some()));
        }
    }
    revalidate()?;
    for prepared in indexes.values() {
        ensure!(
            read_index(&prepared.path)? == prepared.original,
            "source index changed; rerun over restore"
        );
    }
    for target in &targets {
        crate::ui::info(format!(
            "{}{} worktree: {}",
            if dry_run { "Would " } else { "" },
            if target.after.is_some() {
                "Restore"
            } else {
                "Remove"
            },
            target.request.root.join(&target.request.relative).display()
        ))?;
    }
    for (path, present) in &index_updates {
        crate::ui::info(format!(
            "{}{} index: {}",
            if dry_run { "Would " } else { "" },
            if *present { "Restore" } else { "Remove" },
            path.display()
        ))?;
    }
    if dry_run || (targets.is_empty() && index_updates.is_empty()) {
        return Ok(());
    }
    // Lock every source snapshot, including worktree-only reads, so index writers
    // cannot change the meaning of default restore during publication.
    let mut locks = indexes
        .values()
        .map(|prepared| IndexLock::acquire(&prepared.path))
        .collect::<Result<Vec<_>>>()?;
    for prepared in indexes.values() {
        ensure!(
            read_index(&prepared.path)? == prepared.original,
            "source index changed; rerun over restore"
        );
    }
    revalidate()?;
    for target in &targets {
        ensure!(
            target_content(&target.request.root, &target.request.relative)? == target.before,
            "target changed while preparing restore; rerun over restore"
        );
    }
    // Serialize every new index before touching targets; serialization failure
    // must not discard any working-tree edits.
    let mut publications = Vec::new();
    for prepared in indexes.values_mut() {
        publications.push(if prepared.changed {
            prepared.index.write()?;
            Some(fs::read(
                prepared
                    .index
                    .path()
                    .context("prepared index has no path")?,
            )?)
        } else {
            None
        });
    }
    let mut transaction = SourceTransaction::default();
    let result = (|| -> Result<()> {
        for target in &targets {
            transaction.prepare(
                target.request.root.join(&target.request.relative),
                target.after.clone(),
            )?;
        }
        transaction.apply()?;
        for ((prepared, lock), bytes) in indexes.values().zip(&mut locks).zip(publications) {
            if let Some(bytes) = bytes {
                lock.publish(&prepared.path, &bytes)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        transaction
            .rollback()
            .with_context(|| format!("{error:#}; target rollback also failed"))?;
        return Err(error);
    }
    transaction.cleanup()
}

fn tree_entry(tree: &git2::Tree<'_>, path: &Path) -> Result<Option<(Oid, u32)>> {
    match tree.get_path(path) {
        Ok(entry) => Ok(Some((entry.id(), entry.filemode() as u32))),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn target_content(root: &Path, relative: &Path) -> Result<Content> {
    let mut path = root.to_path_buf();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            path.push(component);
        }
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect '{}'", path.display()));
            }
        };
        let leaf = path == root.join(relative);
        ensure!(
            !metadata.is_symlink()
                && if leaf {
                    metadata.is_file()
                } else {
                    metadata.is_dir()
                },
            "restore requires a regular file and non-symlink ancestors: '{}'",
            path.display()
        );
        if !leaf && path != root {
            match path.join(".git").symlink_metadata() {
                Ok(_) => anyhow::bail!(
                    "restore cannot cross nested repository '{}'",
                    path.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).context("cannot inspect nested repository boundary");
                }
            }
        }
        if leaf {
            return Ok(Some((
                fs::read(&path)?,
                i32::from(git::on_disk_filemode(&path)?) as u32,
            )));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct Fixture {
        _directory: tempfile::TempDir,
        repo: Repository,
        request: RestorePath,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("source");
            let root = directory.path().join("target");
            fs::create_dir(&root).unwrap();
            let repo = Repository::init(&source).unwrap();
            fs::write(source.join("file"), "HEAD\n").unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("file")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let signature = git2::Signature::now("Test", "test@example.com").unwrap();
            repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
                .unwrap();
            drop(tree);
            fs::write(source.join("file"), "indexed\n").unwrap();
            index.add_path(Path::new("file")).unwrap();
            index.write().unwrap();
            fs::write(root.join("file"), "target edits\n").unwrap();
            let request = RestorePath {
                repository: repo.path().to_path_buf(),
                source: PathBuf::from("file"),
                root,
                relative: PathBuf::from("file"),
            };
            Self {
                _directory: directory,
                repo,
                request,
            }
        }
    }

    #[test]
    fn a_concurrent_index_change_is_preserved_without_touching_targets() {
        let fixture = Fixture::new();
        let index = fixture.repo.index().unwrap().path().unwrap().to_path_buf();
        let result = restore_paths(
            &[fixture.request],
            &RestoreOptions::default(),
            false,
            || {
                fs::write(&index, b"concurrent writer")?;
                Ok(())
            },
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("source index changed")
        );
        assert_eq!(fs::read(index).unwrap(), b"concurrent writer");
        assert_eq!(
            fs::read(fixture._directory.path().join("target/file")).unwrap(),
            b"target edits\n"
        );
    }

    #[test]
    fn a_concurrent_target_edit_is_not_overwritten() {
        let fixture = Fixture::new();
        let target = fixture.request.root.join("file");
        let index = fixture.repo.index().unwrap().path().unwrap().to_path_buf();
        let before = fs::read(&index).unwrap();
        let result = restore_paths(
            &[fixture.request],
            &RestoreOptions::default(),
            false,
            || {
                fs::write(&target, b"concurrent target edits")?;
                Ok(())
            },
        );
        assert!(result.unwrap_err().to_string().contains("target changed"));
        assert_eq!(fs::read(target).unwrap(), b"concurrent target edits");
        assert_eq!(fs::read(index).unwrap(), before);
    }

    #[test]
    fn an_index_publication_failure_rolls_back_target_replacements() {
        let fixture = Fixture::new();
        let target = fixture.request.root.join("file");
        let index = fixture.repo.index().unwrap().path().unwrap().to_path_buf();
        let backup = index.with_extension("backup");
        let calls = Cell::new(0);
        let options = RestoreOptions {
            staged: true,
            worktree: true,
            ..Default::default()
        };
        let result = restore_paths(&[fixture.request], &options, false, || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                // Inject rename failure after index snapshot validation, without
                // relying on permissions (which privileged test runners bypass).
                fs::rename(&index, &backup)?;
                fs::create_dir(&index)?;
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(target).unwrap(), b"target edits\n");
        assert!(!index.with_extension("lock").exists());
        fs::remove_dir(&index).unwrap();
        fs::rename(backup, index).unwrap();
        assert_eq!(
            fs::read_dir(fixture._directory.path().join("target"))
                .unwrap()
                .count(),
            1
        );
    }
}
