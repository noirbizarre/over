use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::utils::short_path;

/// What actually exists at a [`super::PlanStep`]'s target path right now,
/// independent of what [`crate::desired::DesiredEntry`] says should be
/// there. [`inspect`] is read-only — the "actual filesystem state" half of
/// reconciliation that #107 deliberately left for this issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActualState {
    /// Nothing exists at this path (not even a broken symlink).
    Missing,
    /// A real (non-symlink) directory.
    Directory,
    /// A real (non-symlink) file.
    File,
    /// A symlink, resolved to where it currently points (which may not
    /// exist, or may not match what's desired — that's for the caller to
    /// compare).
    Symlink { points_to: PathBuf },
}

/// Human-readable description of what currently occupies a path — shared
/// by [`super::step::PlanStep`]'s conflict diagnostics and `crate::diff`'s
/// `Unexpected` reporting, so both describe the same [`ActualState`] the
/// same way instead of each re-deriving the wording.
impl fmt::Display for ActualState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActualState::Missing => write!(f, "nothing"),
            ActualState::Directory => write!(f, "a directory"),
            ActualState::File => write!(f, "a file"),
            ActualState::Symlink { points_to } => {
                write!(
                    f,
                    "a symlink to {}",
                    short_path(&points_to.to_string_lossy())
                )
            }
        }
    }
}

/// Inspect what currently exists at `path`, without mutating anything.
///
/// Uses `symlink_metadata` (not `metadata`) so a symlink is reported as
/// `Symlink`, never silently followed to whatever it points at — the same
/// distinction `EnsureLink`/`EnsureDirLink::execute` and
/// `actions::symlink::resolve_conflict` already make inline, made reusable
/// and testable here instead.
pub fn inspect(path: &Path) -> Result<ActualState> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ActualState::Missing),
        Err(e) => return Err(e.into()),
    };

    if metadata.file_type().is_symlink() {
        let points_to = fs::read_link(path)?;
        return Ok(ActualState::Symlink { points_to });
    }

    Ok(if metadata.is_dir() {
        ActualState::Directory
    } else {
        ActualState::File
    })
}

/// Whether `path` is, recursively, nothing but symlinks (or doesn't exist
/// at all) — no real file/directory content anywhere underneath.
///
/// A symlink never holds unique data of its own — removing one is always
/// safe (`CheckoutMaterializer::classify`'s stale-symlink branch, #129,
/// already relies on exactly this reasoning for a single symlink). A
/// directory built entirely of symlinks is safe to discard for the exact
/// same reason, generalized: nothing is lost that `over` didn't put there
/// in the first place. This is the primitive that recognizes a legacy
/// `over` symlink-only installation with zero prior XDG state (#130) as
/// safe to remove and replace with a checkout — a single real file
/// anywhere underneath means that can't be proven, and must never be
/// silently discarded.
pub fn is_symlink_only(path: &Path) -> Result<bool> {
    match inspect(path)? {
        ActualState::Missing | ActualState::Symlink { .. } => Ok(true),
        ActualState::File => Ok(false),
        ActualState::Directory => {
            for entry in fs::read_dir(path)? {
                if !is_symlink_only(&entry?.path())? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;

    #[test]
    fn missing_path_reports_missing() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("does-not-exist");
        assert_eq!(inspect(&path).unwrap(), ActualState::Missing);
    }

    #[test]
    fn real_directory_reports_directory() {
        let td = TempDir::new().unwrap();
        let dir = td.child("adir");
        dir.create_dir_all().unwrap();
        assert_eq!(inspect(dir.path()).unwrap(), ActualState::Directory);
    }

    #[test]
    fn real_file_reports_file() {
        let td = TempDir::new().unwrap();
        let file = td.child("afile.txt");
        file.write_str("content").unwrap();
        assert_eq!(inspect(file.path()).unwrap(), ActualState::File);
    }

    #[test]
    fn file_symlink_reports_symlink_with_target() {
        let td = TempDir::new().unwrap();
        let source = td.child("source.txt");
        source.write_str("content").unwrap();
        let link = td.path().join("link.txt");
        symlink::symlink_file(source.path(), &link).unwrap();

        match inspect(&link).unwrap() {
            ActualState::Symlink { points_to } => assert_eq!(points_to, source.path()),
            other => panic!("expected Symlink, got {other:?}"),
        }
    }

    #[test]
    fn dir_symlink_reports_symlink_with_target_not_directory() {
        let td = TempDir::new().unwrap();
        let source = td.child("source_dir");
        source.create_dir_all().unwrap();
        let link = td.path().join("link_dir");
        symlink::symlink_dir(source.path(), &link).unwrap();

        // A directory symlink must be reported as `Symlink`, not `Directory`
        // — following it would hide the distinction between "already
        // correctly linked" and "a real directory sits here".
        match inspect(&link).unwrap() {
            ActualState::Symlink { points_to } => assert_eq!(points_to, source.path()),
            other => panic!("expected Symlink, got {other:?}"),
        }
    }

    #[test]
    fn broken_symlink_is_still_reported_as_symlink() {
        let td = TempDir::new().unwrap();
        let source = td.path().join("gone.txt");
        let link = td.path().join("broken_link.txt");
        symlink::symlink_file(&source, &link).unwrap();

        match inspect(&link).unwrap() {
            ActualState::Symlink { points_to } => assert_eq!(points_to, source),
            other => panic!("expected Symlink, got {other:?}"),
        }
    }

    #[test]
    fn missing_path_is_symlink_only() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("does-not-exist");
        assert!(is_symlink_only(&path).unwrap());
    }

    #[test]
    fn a_single_symlink_is_symlink_only() {
        let td = TempDir::new().unwrap();
        let source = td.child("source.txt");
        source.write_str("content").unwrap();
        let link = td.path().join("link.txt");
        symlink::symlink_file(source.path(), &link).unwrap();
        assert!(is_symlink_only(&link).unwrap());
    }

    #[test]
    fn a_real_file_is_not_symlink_only() {
        let td = TempDir::new().unwrap();
        let file = td.child("afile.txt");
        file.write_str("content").unwrap();
        assert!(!is_symlink_only(file.path()).unwrap());
    }

    #[test]
    fn a_directory_of_only_symlinks_is_symlink_only() {
        let td = TempDir::new().unwrap();
        let source = td.child("source_dir");
        source.create_dir_all().unwrap();
        source.child("a.txt").write_str("a").unwrap();
        source.child("sub").create_dir_all().unwrap();
        source.child("sub/b.txt").write_str("b").unwrap();

        let target = td.child("target_dir");
        target.create_dir_all().unwrap();
        fs::create_dir_all(target.path().join("sub")).unwrap();
        symlink::symlink_file(source.path().join("a.txt"), target.path().join("a.txt")).unwrap();
        symlink::symlink_file(
            source.path().join("sub/b.txt"),
            target.path().join("sub/b.txt"),
        )
        .unwrap();

        assert!(is_symlink_only(target.path()).unwrap());
    }

    #[test]
    fn a_directory_with_one_real_file_is_not_symlink_only() {
        let td = TempDir::new().unwrap();
        let source = td.child("source.txt");
        source.write_str("content").unwrap();

        let target = td.child("target_dir");
        target.create_dir_all().unwrap();
        symlink::symlink_file(source.path(), target.path().join("linked.txt")).unwrap();
        fs::write(target.path().join("foreign.txt"), "not from over").unwrap();

        assert!(!is_symlink_only(target.path()).unwrap());
    }

    #[test]
    fn a_real_file_nested_in_a_subdirectory_is_not_symlink_only() {
        let td = TempDir::new().unwrap();
        let target = td.child("target_dir");
        target.create_dir_all().unwrap();
        target.child("sub").create_dir_all().unwrap();
        fs::write(target.path().join("sub/foreign.txt"), "not from over").unwrap();

        assert!(!is_symlink_only(target.path()).unwrap());
    }
}
