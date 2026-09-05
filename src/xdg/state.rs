//! Versioned, atomic, locked persistence boundary for files under
//! `$XDG_STATE_HOME/over`.
//!
//! This module only provides the generic mechanics — schema-less consumers
//! (repository/worktree associations, sync checkpoints, status caches, …)
//! are added by later features on top of [`StateFile`]. Rules enforced here,
//! per the XDG state contract:
//!
//! - every document is versioned, so a mismatched on-disk schema is either
//!   migrated or rejected loudly, never silently misread;
//! - writes are atomic (temp file + rename), so a crash mid-write can never
//!   leave a half-written document behind;
//! - reads and writes are serialized through an advisory file lock, so two
//!   concurrent `over`/`git-over` invocations can't corrupt the same file.

use std::fs::OpenOptions;
use std::path::PathBuf;
// `std::fs::File::lock`/`lock_shared`/`unlock` (stable since Rust 1.89) give
// us advisory `flock`/`LockFileEx` locking natively, so no extra dependency
// (e.g. `fs4`) is needed here.

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::task::spawn_blocking;

/// A document that can be persisted under `$XDG_STATE_HOME/over`.
///
/// `Default` is required because a missing state file is a normal, expected
/// condition (first run, or the user deleted it): [`StateFile::load`]
/// returns `T::default()` rather than an error.
pub trait VersionedState: Serialize + DeserializeOwned + Default {
    /// Current on-disk schema version for this document.
    const VERSION: u32;

    /// Upgrades an older, raw on-disk document to the current shape.
    ///
    /// The default implementation refuses every version, so implementors
    /// only need to add a match arm the day they actually introduce a new
    /// schema; until then, an unreadable file fails loudly instead of the
    /// migration boundary silently doing nothing.
    fn migrate(version: u32, _raw: toml::Value) -> Result<Self> {
        anyhow::bail!("cannot migrate state from unknown schema version {version}")
    }
}

/// On-disk envelope: `{ version = N, [state] ... }`.
///
/// Kept separate from `T` so a version mismatch can be detected (and
/// migrated) *before* attempting to deserialize the body as the current
/// shape.
#[derive(serde::Deserialize)]
struct RawEnvelope {
    version: u32,
    state: toml::Value,
}

#[derive(Serialize)]
struct Envelope<'a, T> {
    version: u32,
    state: &'a T,
}

/// A single versioned, atomically-written, lock-protected state document.
pub struct StateFile<T> {
    path: PathBuf,
    _marker: std::marker::PhantomData<T>,
}

impl<T> StateFile<T>
where
    T: VersionedState + Send + 'static,
{
    /// Points at `path` (not required to exist yet).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Advisory lock sentinel next to `path`.
    ///
    /// A dedicated file (rather than locking `path` itself) so the state
    /// file can always be replaced with `rename` without disturbing an
    /// in-progress lock on the old inode.
    fn lock_path(&self) -> PathBuf {
        let mut lock_path = self.path.clone().into_os_string();
        lock_path.push(".lock");
        lock_path.into()
    }

    /// Returns `T::default()` if the file doesn't exist yet.
    pub async fn load(&self) -> Result<T> {
        let path = self.path.clone();
        let lock_path = self.lock_path();
        spawn_blocking(move || {
            // A shared lock is defense-in-depth beyond the atomic-rename
            // guarantee (helps on filesystems without atomic rename, e.g.
            // some network filesystems) and serializes with concurrent
            // `update()`/`save()` calls.
            let lock_file = Self::open_lock_file(&lock_path)?;
            lock_file
                .lock_shared()
                .with_context(|| format!("failed to lock {}", lock_path.display()))?;
            let state = Self::read(&path)?;
            lock_file.unlock().ok();
            Ok(state)
        })
        .await?
    }

    /// Whole-document overwrite, written atomically.
    pub async fn save(&self, value: T) -> Result<()> {
        let path = self.path.clone();
        let lock_path = self.lock_path();
        spawn_blocking(move || {
            let lock_file = Self::open_lock_file(&lock_path)?;
            lock_file
                .lock()
                .with_context(|| format!("failed to lock {}", lock_path.display()))?;
            Self::write_atomic(&path, &value)?;
            lock_file.unlock().ok();
            Ok(())
        })
        .await?
    }

    /// Locked read-modify-write: loads the current document (or its
    /// default), lets `mutate` update it in place, then saves it — all
    /// inside a single critical section, so concurrent callers can't
    /// interleave and lose an update.
    pub async fn update(
        &self,
        mutate: impl FnOnce(&mut T) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let path = self.path.clone();
        let lock_path = self.lock_path();
        spawn_blocking(move || {
            // `lock`/`lock_shared` are blocking syscalls; running the whole
            // read-modify-write sequence inside this one `spawn_blocking`
            // closure keeps the lock held across the mutation without ever
            // holding it across an `.await` point.
            let lock_file = Self::open_lock_file(&lock_path)?;
            lock_file
                .lock()
                .with_context(|| format!("failed to lock {}", lock_path.display()))?;

            let mut state = Self::read(&path)?;
            mutate(&mut state)?;
            Self::write_atomic(&path, &state)?;

            // Locks are released on drop too; unlocking explicitly makes
            // the end of the critical section visible in the code.
            lock_file.unlock().ok();
            Ok(())
        })
        .await?
    }

    /// Opens (creating if needed) the lock sentinel file, creating its
    /// parent directory first since XDG state dirs aren't guaranteed to
    /// pre-exist.
    fn open_lock_file(lock_path: &std::path::Path) -> Result<std::fs::File> {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)
            .with_context(|| format!("failed to open lock file {}", lock_path.display()))
    }

    /// Reads and parses the document, applying [`VersionedState::migrate`]
    /// if it was written by an older schema version. Missing file → default.
    fn read(path: &std::path::Path) -> Result<T> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let raw: RawEnvelope = toml::from_str(&contents)
            .with_context(|| format!("failed to parse state file {}", path.display()))?;

        match raw.version.cmp(&T::VERSION) {
            std::cmp::Ordering::Equal => raw
                .state
                .try_into()
                .with_context(|| format!("failed to decode state file {}", path.display())),
            std::cmp::Ordering::Less => T::migrate(raw.version, raw.state).with_context(|| {
                format!(
                    "failed to migrate state file {} from schema v{} to v{}",
                    path.display(),
                    raw.version,
                    T::VERSION
                )
            }),
            std::cmp::Ordering::Greater => anyhow::bail!(
                "state file {} was written by a newer version of over (schema v{}, expected v{}); refusing to read it",
                path.display(),
                raw.version,
                T::VERSION
            ),
        }
    }

    /// Serializes `value` and atomically replaces `path`: write to a sibling
    /// temp file in the same directory (so `rename` stays on one
    /// filesystem), then rename over the target.
    fn write_atomic(path: &std::path::Path, value: &T) -> Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        let envelope = Envelope {
            version: T::VERSION,
            state: value,
        };
        let contents = toml::to_string_pretty(&envelope)
            .context("failed to serialize state document to TOML")?;

        let tmp_path = parent.join(format!(
            ".{}.tmp-{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
            std::process::id()
        ));
        std::fs::write(&tmp_path, contents)
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "failed to atomically replace {} with {}",
                path.display(),
                tmp_path.display()
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
    struct Counter {
        count: u32,
    }

    impl VersionedState for Counter {
        const VERSION: u32 = 1;
    }

    #[tokio::test]
    async fn load_returns_default_when_file_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let state_file: StateFile<Counter> = StateFile::new(tmp.path().join("state.toml"));

        let value = state_file.load().await.unwrap();

        assert_eq!(value, Counter::default());
    }

    #[tokio::test]
    async fn save_then_load_round_trips_the_value() {
        let tmp = tempfile::tempdir().unwrap();
        let state_file: StateFile<Counter> = StateFile::new(tmp.path().join("state.toml"));

        state_file.save(Counter { count: 42 }).await.unwrap();
        let value = state_file.load().await.unwrap();

        assert_eq!(value, Counter { count: 42 });
    }

    #[derive(Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
    struct Migrated {
        count: u32,
        // Introduced in v2; migrated-from-v1 documents get the default.
        label: String,
    }

    impl VersionedState for Migrated {
        const VERSION: u32 = 2;

        fn migrate(version: u32, raw: toml::Value) -> Result<Self> {
            if version != 1 {
                anyhow::bail!("cannot migrate state from unknown schema version {version}");
            }
            let old: Counter = raw.try_into()?;
            Ok(Migrated {
                count: old.count,
                label: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn load_migrates_an_older_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.toml");
        std::fs::write(&path, "version = 1\n\n[state]\ncount = 7\n").unwrap();
        let state_file: StateFile<Migrated> = StateFile::new(path);

        let value = state_file.load().await.unwrap();

        assert_eq!(
            value,
            Migrated {
                count: 7,
                label: String::new()
            }
        );
    }

    #[tokio::test]
    async fn load_rejects_a_newer_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.toml");
        std::fs::write(&path, "version = 99\n\n[state]\ncount = 1\n").unwrap();
        let state_file: StateFile<Counter> = StateFile::new(path);

        let err = state_file.load().await.unwrap_err();

        assert!(err.to_string().contains("newer version"));
    }

    #[tokio::test]
    async fn concurrent_updates_are_serialized_and_none_are_lost() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.toml");
        let a: StateFile<Counter> = StateFile::new(path.clone());
        let b: StateFile<Counter> = StateFile::new(path);

        let (r1, r2) = tokio::join!(
            a.update(|s| {
                s.count += 1;
                Ok(())
            }),
            b.update(|s| {
                s.count += 1;
                Ok(())
            })
        );
        r1.unwrap();
        r2.unwrap();

        let value = a.load().await.unwrap();
        assert_eq!(value.count, 2);
    }
}
