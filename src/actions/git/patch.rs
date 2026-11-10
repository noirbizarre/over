//! Select changes between immutable snapshots, independently of their destination.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use git2::{ApplyOptions, DiffOptions, Oid, Patch, Repository};

use crate::materialize::virtual_checkout::git::build_content_tree;

pub(crate) type Snapshot = BTreeMap<PathBuf, (Oid, i32)>;

pub(crate) struct Unit {
    pub path: PathBuf,
    pub description: String,
    pub patch: Vec<u8>,
}

struct File {
    path: PathBuf,
    before: Option<(Oid, i32)>,
    after: Option<(Oid, i32)>,
    hunks: Vec<usize>,
    atomic: Option<usize>,
    mode: Option<usize>,
}

pub(crate) struct Session {
    pub units: Vec<Unit>,
    files: Vec<File>,
}

impl Session {
    pub fn prepare(repo: &Repository, before: &Snapshot, after: &Snapshot) -> Result<Self> {
        let mut session = Self {
            units: Vec::new(),
            files: Vec::new(),
        };
        let paths: BTreeSet<_> = before.keys().chain(after.keys()).cloned().collect();
        for path in &paths {
            ensure!(
                !path
                    .ancestors()
                    .skip(1)
                    .any(|parent| paths.contains(parent)),
                "cannot patch a file/directory replacement at '{}'",
                path.display()
            );
        }
        for path in paths {
            let old = before.get(&path).copied();
            let new = after.get(&path).copied();
            if old == new {
                continue;
            }
            let mut file = File {
                path: path.clone(),
                before: old,
                after: new,
                hunks: Vec::new(),
                atomic: None,
                mode: None,
            };
            let left = build_content_tree(
                repo,
                &old.map(|entry| (path.clone(), entry)).into_iter().collect(),
            )?;
            let right = build_content_tree(
                repo,
                &new.map(|entry| (path.clone(), entry)).into_iter().collect(),
            )?;
            let diff = repo.diff_tree_to_tree(
                Some(&left),
                Some(&right),
                Some(DiffOptions::new().context_lines(3)),
            )?;
            let mut patch = Patch::from_diff(&diff, 0)?.context("missing change patch")?;
            let binary = patch.delta().flags().is_binary();
            if old.is_none() || new.is_none() || binary {
                file.atomic = Some(session.units.len());
                session.units.push(Unit {
                    path: path.clone(),
                    description: if binary {
                        "Binary change (whole file)"
                    } else if old.is_none() {
                        "Add file (whole file)"
                    } else {
                        "Delete file (whole file)"
                    }
                    .into(),
                    patch: patch.to_buf()?.to_vec(),
                });
            } else {
                let (_, old_mode) = old.context("modified file has no destination")?;
                let (_, new_mode) = new.context("modified file has no source")?;
                for hunk_index in 0..patch.num_hunks() {
                    let (hunk, count) = patch.hunk(hunk_index)?;
                    let mut bytes = hunk.header().to_vec();
                    for line_index in 0..count {
                        let line = patch.line_in_hunk(hunk_index, line_index)?;
                        if matches!(line.origin(), ' ' | '+' | '-') {
                            bytes.push(line.origin() as u8);
                        }
                        bytes.extend_from_slice(line.content());
                    }
                    file.hunks.push(session.units.len());
                    session.units.push(Unit {
                        path: path.clone(),
                        description: format!("Hunk {}/{}", hunk_index + 1, patch.num_hunks()),
                        patch: bytes,
                    });
                }
                if old.map(|entry| entry.1) != new.map(|entry| entry.1) {
                    file.mode = Some(session.units.len());
                    session.units.push(Unit {
                        path: path.clone(),
                        description: format!(
                            "Mode change {:o} → {:o} (whole change)",
                            old_mode, new_mode
                        ),
                        patch: Vec::new(),
                    });
                }
            }
            session.files.push(file);
        }
        Ok(session)
    }

    /// Return only accepted updates; an absent update never removes a path.
    pub fn apply(
        &self,
        repo: &Repository,
        accepted: &BTreeSet<usize>,
    ) -> Result<BTreeMap<PathBuf, Option<(Oid, i32)>>> {
        ensure!(
            accepted.iter().all(|id| *id < self.units.len()),
            "invalid patch selection"
        );
        let mut updates = BTreeMap::new();
        for file in &self.files {
            if let Some(id) = file.atomic {
                if accepted.contains(&id) {
                    updates.insert(file.path.clone(), file.after);
                }
                continue;
            }
            let content_selected = file.hunks.iter().any(|id| accepted.contains(id));
            let mode_selected = file.mode.is_some_and(|id| accepted.contains(&id));
            if !content_selected && !mode_selected {
                continue;
            }
            let (mut oid, old_mode) = file.before.context("modified file has no destination")?;
            let (desired_oid, desired_mode) = file.after.context("modified file has no source")?;
            if content_selected {
                let left =
                    build_content_tree(repo, &[(file.path.clone(), (oid, old_mode))].into())?;
                // Normalize modes so Git cannot apply metadata that the user skipped.
                let right = build_content_tree(
                    repo,
                    &[(file.path.clone(), (desired_oid, old_mode))].into(),
                )?;
                let diff = repo.diff_tree_to_tree(
                    Some(&left),
                    Some(&right),
                    Some(DiffOptions::new().context_lines(3)),
                )?;
                let mut position = 0;
                let mut options = ApplyOptions::new();
                options.hunk_callback(|_| {
                    let selected = accepted.contains(&file.hunks[position]);
                    position += 1;
                    selected
                });
                let result = repo.apply_to_tree(&left, &diff, Some(&mut options))?;
                oid = result
                    .get_path(&file.path, 0)
                    .context("applied patch lost its destination")?
                    .id;
            }
            let entry = (
                oid,
                if mode_selected {
                    desired_mode
                } else {
                    old_mode
                },
            );
            if Some(entry) != file.before {
                updates.insert(file.path.clone(), Some(entry));
            }
        }
        Ok(updates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case(b"old\r\n", b"new\r\n")]
    #[case(b"old", b"new")]
    #[case(b"old\n", b"new")]
    #[case(b"old", b"new\n")]
    fn applying_text_hunks_preserves_exact_newline_bytes(
        #[case] before: &[u8],
        #[case] after: &[u8],
    ) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(dir.path()).unwrap();
        let snapshot =
            |bytes: &[u8]| [(PathBuf::from("file"), (repo.blob(bytes).unwrap(), 0o100644))].into();
        let session = Session::prepare(&repo, &snapshot(before), &snapshot(after)).unwrap();
        let updates = session.apply(&repo, &[0].into()).unwrap();
        assert_eq!(
            repo.find_blob(updates[&PathBuf::from("file")].unwrap().0)
                .unwrap()
                .content(),
            after
        );
    }

    #[test]
    fn content_and_mode_changes_are_independently_selectable() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(dir.path()).unwrap();
        let old = repo.blob(b"old\n").unwrap();
        let new = repo.blob(b"new\n").unwrap();
        let path = PathBuf::from("file");
        let session = Session::prepare(
            &repo,
            &[(path.clone(), (old, 0o100644))].into(),
            &[(path.clone(), (new, 0o100755))].into(),
        )
        .unwrap();
        assert_eq!(session.units.len(), 2);
        for (choices, expected) in [
            (vec![0], (new, 0o100644)),
            (vec![1], (old, 0o100755)),
            (vec![0, 1], (new, 0o100755)),
        ] {
            assert_eq!(
                session
                    .apply(&repo, &choices.into_iter().collect())
                    .unwrap()[&path],
                Some(expected)
            );
        }
        let mode_only = Session::prepare(
            &repo,
            &[(path.clone(), (old, 0o100644))].into(),
            &[(path.clone(), (old, 0o100755))].into(),
        )
        .unwrap();
        assert_eq!(mode_only.units.len(), 1);
        assert_eq!(
            mode_only.apply(&repo, &[0].into()).unwrap()[&path],
            Some((old, 0o100755))
        );
    }

    #[test]
    fn skipped_length_changes_do_not_shift_later_selected_hunks() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(dir.path()).unwrap();
        let before = "first\n1\n2\n3\n4\n5\n6\n7\n8\n9\nlast\n";
        let after = before
            .replace("first", "first\ninserted")
            .replace("last", "changed");
        let snapshot = |text: &str| {
            [(
                PathBuf::from("file"),
                (repo.blob(text.as_bytes()).unwrap(), 0o100644),
            )]
            .into()
        };
        let session = Session::prepare(&repo, &snapshot(before), &snapshot(&after)).unwrap();
        assert_eq!(session.units.len(), 2);
        let result = session.apply(&repo, &[1].into()).unwrap();
        let blob = repo
            .find_blob(result[&PathBuf::from("file")].unwrap().0)
            .unwrap();
        assert_eq!(blob.content(), before.replace("last", "changed").as_bytes());
        assert!(session.apply(&repo, &BTreeSet::new()).unwrap().is_empty());
        // The same engine can restore a selected edit toward an older snapshot.
        let reverse = Session::prepare(&repo, &snapshot(&after), &snapshot(before)).unwrap();
        let result = reverse.apply(&repo, &[1].into()).unwrap();
        assert_eq!(
            repo.find_blob(result[&PathBuf::from("file")].unwrap().0)
                .unwrap()
                .content(),
            after.replace("changed", "last").as_bytes()
        );
    }

    #[test]
    fn skipped_atomic_changes_never_create_or_remove_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(dir.path()).unwrap();
        let snapshot = [(
            PathBuf::from("file"),
            (repo.blob(b"binary\0data").unwrap(), 0o100644),
        )]
        .into();
        for (before, after) in [(&Snapshot::new(), &snapshot), (&snapshot, &Snapshot::new())] {
            let session = Session::prepare(&repo, before, after).unwrap();
            assert!(session.apply(&repo, &BTreeSet::new()).unwrap().is_empty());
            assert_eq!(
                session
                    .apply(&repo, &[0].into())
                    .unwrap()
                    .get(&PathBuf::from("file")),
                Some(&after.get(&PathBuf::from("file")).copied())
            );
        }
    }
}
