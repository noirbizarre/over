//! Publish isolated index edits without losing concurrent staged changes.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    path: Vec<u8>,
    id: git2::Oid,
    mode: u32,
    flags: u16,
    extended: u16,
}

/// Compare staging, not Git's disposable filesystem/stat caches.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct IndexSnapshot {
    entries: Option<Vec<Entry>>,
    extensions: Vec<([u8; 4], Vec<u8>)>,
}

impl IndexSnapshot {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        Self::from_bytes(read_index(path)?.as_deref())
    }

    pub(crate) fn from_bytes(bytes: Option<&[u8]>) -> Result<Self> {
        let Some(bytes) = bytes else {
            return Ok(Self {
                entries: None,
                extensions: Vec::new(),
            });
        };
        // Decode the captured bytes, not a second read or a cached repository index:
        // either could silently adopt staging that the caller will later overwrite.
        let mut temporary = tempfile::NamedTempFile::new()?;
        temporary.write_all(bytes)?;
        let index = git2::Index::open(temporary.path())?;
        let entries = index
            .iter()
            .map(|entry| Entry {
                path: entry.path,
                id: entry.id,
                mode: entry.mode,
                // Path length and EXTENDED encode representation; stage and VALID
                // affect staging even when write-tree would produce the same tree.
                flags: entry.flags & 0xb000,
                // Intent-to-add and skip-worktree are persisted staging policy.
                extended: entry.flags_extended & 0x6000,
            })
            .collect();
        Ok(Self {
            entries: Some(entries),
            extensions: meaningful_extensions(bytes)?,
        })
    }
}

fn meaningful_extensions(bytes: &[u8]) -> Result<Vec<([u8; 4], Vec<u8>)>> {
    // libgit2 validates the index first. Walk its encoded entries only to retain
    // non-cache extensions (including resolve-undo and unknown optional metadata).
    let invalid = || anyhow::anyhow!("invalid index extension layout");
    let word = |offset: usize| -> Result<u32> {
        Ok(u32::from_be_bytes(
            bytes
                .get(offset..offset + 4)
                .ok_or_else(invalid)?
                .try_into()?,
        ))
    };
    let version = word(4)?;
    ensure!((2..=4).contains(&version), "unsupported index version");
    let end = bytes
        .len()
        .checked_sub(20)
        .context("invalid index checksum")?;
    let mut offset = 12;
    for _ in 0..word(8)? {
        let start = offset;
        let flags = *bytes.get(start + 60).ok_or_else(invalid)?;
        offset += if flags & 0x40 != 0 { 64 } else { 62 };
        if version == 4 {
            // V4 prefix compression encodes the removed prefix as a varint.
            loop {
                let byte = *bytes.get(offset).ok_or_else(invalid)?;
                offset += 1;
                if byte & 0x80 == 0 {
                    break;
                }
            }
        }
        offset += bytes
            .get(offset..end)
            .ok_or_else(invalid)?
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(invalid)?
            + 1;
        if version != 4 {
            offset = start + (offset - start).next_multiple_of(8);
        }
    }
    let mut extensions = Vec::new();
    while offset < end {
        let signature: [u8; 4] = bytes
            .get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()?;
        let size = word(offset + 4)? as usize;
        offset += 8;
        let data = bytes
            .get(offset..offset.checked_add(size).ok_or_else(invalid)?)
            .filter(|_| offset + size <= end)
            .ok_or_else(invalid)?;
        // These extensions are caches or offsets into this particular encoding.
        if !matches!(&signature, b"TREE" | b"UNTR" | b"FSMN" | b"EOIE" | b"IEOT") {
            extensions.push((signature, data.to_vec()));
        }
        offset += size;
    }
    ensure!(offset == end, "invalid index extension boundary");
    extensions.sort();
    Ok(extensions)
}

pub(crate) fn read_index(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("failed to snapshot source index"),
    }
}

pub(crate) struct IndexLock {
    path: PathBuf,
    file: File,
}

impl IndexLock {
    /// Journaled callers can recognize their own lock after a process crash.
    pub(crate) fn mark_ownership(&mut self) -> Result<Vec<u8>> {
        let mut token = vec![0u8; 32];
        getrandom::fill(&mut token)
            .map_err(|error| anyhow::anyhow!("could not create index ownership token: {error}"))?;
        self.file.write_all(&token)?;
        self.file.sync_all()?;
        Ok(token)
    }

    pub(crate) fn acquire(index: &Path) -> Result<Self> {
        let mut path = index.as_os_str().to_os_string();
        path.push(".lock");
        let path = PathBuf::from(path);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("cannot lock index at '{}'", path.display()))?;
        Ok(Self { path, file })
    }

    pub(crate) fn publish(&mut self, index: &Path, bytes: &[u8]) -> Result<()> {
        // A newly created lock must not broaden a private/shared index's access.
        match fs::metadata(index) {
            Ok(metadata) => self.file.set_permissions(metadata.permissions())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.file.write_all(bytes)?;
        self.file.sync_all()?;
        fs::rename(&self.path, index).context("failed to publish selected index changes")?;
        self.path.clear();
        Ok(())
    }

    /// Keep cooperating Git writers excluded while a larger journaled operation finishes.
    pub(crate) fn replace_while_locked(&mut self, index: &Path, bytes: &[u8]) -> Result<()> {
        let parent = index.parent().context("index has no parent")?;
        let mut prepared = tempfile::NamedTempFile::new_in(parent)?;
        match fs::metadata(index) {
            Ok(metadata) => prepared.as_file().set_permissions(metadata.permissions())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        prepared.write_all(bytes)?;
        prepared.as_file().sync_all()?;
        prepared.persist(index).map_err(|error| error.error)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        // Publication clears ownership so cleanup cannot remove another writer's lock.
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, git2::Index) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        fs::write(directory.path().join("file"), b"staged").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file")).unwrap();
        index.write().unwrap();
        (directory, index)
    }

    #[test]
    fn stat_refreshes_and_index_versions_do_not_change_staging() {
        let (directory, mut index) = fixture();
        let path = index.path().unwrap().to_path_buf();
        let before = read_index(&path).unwrap().unwrap();
        let snapshot = IndexSnapshot::from_bytes(Some(&before)).unwrap();
        let mut entry = index.get(0).unwrap();
        entry.ctime = git2::IndexTime::new(1, 2);
        entry.mtime = git2::IndexTime::new(3, 4);
        entry.dev = 5;
        entry.ino = 6;
        entry.uid = 7;
        entry.gid = 8;
        entry.file_size = 9;
        index.add(&entry).unwrap();
        index.write().unwrap();
        assert_ne!(read_index(&path).unwrap().unwrap(), before);
        assert_eq!(IndexSnapshot::read(&path).unwrap(), snapshot);
        index.set_version(4).unwrap();
        index.write().unwrap();
        assert_eq!(IndexSnapshot::read(&path).unwrap(), snapshot);
        // Building the tree adds a TREE cache extension without changing staging.
        let repo = git2::Repository::open(directory.path()).unwrap();
        index.write_tree_to(&repo).unwrap();
        index.write().unwrap();
        assert_eq!(IndexSnapshot::read(&path).unwrap(), snapshot);
    }

    #[rstest::rstest]
    #[case("path")]
    #[case("object")]
    #[case("mode")]
    #[case("stage")]
    #[case("assume-valid")]
    #[case("intent-to-add")]
    #[case("skip-worktree")]
    #[case("remove")]
    fn meaningful_staging_changes_are_never_treated_as_refreshes(#[case] change: &str) {
        let (_directory, mut index) = fixture();
        let path = index.path().unwrap().to_path_buf();
        let snapshot = IndexSnapshot::read(&path).unwrap();
        let mut entry = index.get(0).unwrap();
        index.clear().unwrap();
        match change {
            "path" => entry.path = b"non-utf8-\xff".to_vec(),
            "object" => entry.id = git2::Oid::ZERO_SHA1,
            "mode" => entry.mode = 0o100755,
            "stage" => entry.flags |= 0x1000,
            "assume-valid" => entry.flags |= 0x8000,
            "intent-to-add" => entry.flags_extended |= 0x2000,
            "skip-worktree" => entry.flags_extended |= 0x4000,
            "remove" => (),
            _ => unreachable!(),
        }
        if change != "remove" {
            index.add(&entry).unwrap();
        }
        index.write().unwrap();
        assert_ne!(IndexSnapshot::read(&path).unwrap(), snapshot);
    }

    #[test]
    fn prefix_compression_preserves_raw_paths_and_all_conflict_stages() {
        let (_directory, mut index) = fixture();
        let path = index.path().unwrap().to_path_buf();
        let mut entry = index.get(0).unwrap();
        index.clear().unwrap();
        // Removing this prefix at the next path needs a multi-byte V4 varint.
        entry.path = format!("{}file", "directory/".repeat(30)).into_bytes();
        index.add(&entry).unwrap();
        entry.path = b"z-non-utf8-\xff".to_vec();
        for stage in 1..=3 {
            entry.flags = stage << 12;
            index.add(&entry).unwrap();
        }
        index.set_version(2).unwrap();
        index.write().unwrap();
        let snapshot = IndexSnapshot::read(&path).unwrap();
        assert_eq!(snapshot.entries.as_ref().unwrap().len(), 4);
        index.set_version(4).unwrap();
        index.write().unwrap();
        assert_eq!(IndexSnapshot::read(&path).unwrap(), snapshot);
    }

    #[test]
    fn missing_empty_and_invalid_indexes_are_distinct() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index");
        let missing = IndexSnapshot::read(&path).unwrap();
        git2::Index::open(&path).unwrap().write().unwrap();
        assert_ne!(IndexSnapshot::read(&path).unwrap(), missing);
        assert!(IndexSnapshot::from_bytes(Some(b"broken")).is_err());
    }

    #[test]
    fn only_disposable_extensions_are_ignored() {
        // Exercise extension classification separately from checksum validation:
        // real indexes are validated by libgit2 before this helper is called.
        let mut bytes = b"DIRC\0\0\0\x02\0\0\0\0".to_vec();
        for signature in [
            b"TREE", b"UNTR", b"FSMN", b"EOIE", b"IEOT", b"REUC", b"NEWX",
        ] {
            bytes.extend_from_slice(signature);
            bytes.extend_from_slice(&1u32.to_be_bytes());
            bytes.push(42);
        }
        bytes.extend_from_slice(&[0; 20]);
        assert_eq!(
            meaningful_extensions(&bytes).unwrap(),
            vec![(*b"NEWX", vec![42]), (*b"REUC", vec![42])]
        );
    }

    #[test]
    fn failed_publication_cleans_up_only_its_owned_lock() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("directory");
        fs::create_dir(&destination).unwrap();
        let lock_path;
        {
            let mut lock = IndexLock::acquire(&destination).unwrap();
            lock_path = lock.path.clone();
            assert!(
                lock.publish(&destination, b"cannot replace a directory")
                    .is_err()
            );
        }
        assert!(!lock_path.exists());
        assert!(destination.is_dir());
    }

    #[test]
    fn retained_publication_keeps_other_index_writers_out_until_completion() {
        let directory = tempfile::tempdir().unwrap();
        let index = directory.path().join("index");
        fs::write(&index, b"old").unwrap();
        let mut lock = IndexLock::acquire(&index).unwrap();
        lock.replace_while_locked(&index, b"prepared").unwrap();
        assert_eq!(fs::read(&index).unwrap(), b"prepared");
        assert!(IndexLock::acquire(&index).is_err());
        drop(lock);
        assert!(IndexLock::acquire(&index).is_ok());
    }
}
