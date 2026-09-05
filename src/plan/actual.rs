use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;

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
}
