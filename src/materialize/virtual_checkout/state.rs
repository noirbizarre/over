//! Persisted association for virtual checkouts (#141), mirroring
//! `crate::sync::state`'s own contract exactly: reconstructible,
//! non-authoritative operational metadata under `$XDG_STATE_HOME/over`.
//!
//! Nothing here ever decides whether a materialize/commit/sync operation
//! is *safe* — that's always re-derived from the source repository's own
//! git objects and the target's on-disk content (`super::git`,
//! `crate::status::virtual_checkout`). Losing this file only loses the
//! `base_oid` bookkeeping (an existing virtual checkout would then be
//! re-adopted against the source repo's current `HEAD` the next time it's
//! classified — see `VirtualCheckoutMaterializer::classify`), never
//! corrupts or discards target content.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::xdg::XdgDirs;
use crate::xdg::state::{StateFile, VersionedState};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VirtualCheckoutState {
    /// Keyed by the checkout's target path (stringified), like
    /// `sync::state::SyncState::checkouts`.
    pub checkouts: HashMap<String, VirtualCheckoutRecord>,
}

impl VersionedState for VirtualCheckoutState {
    const VERSION: u32 = 1;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VirtualCheckoutRecord {
    pub overlay: String,
    /// Path of the managed subtree relative to the source repository's
    /// working directory root (empty for a whole-repository virtual
    /// checkout).
    pub managed_path: PathBuf,
    /// The source repository commit this checkout's target content was
    /// last known to fully reflect (either at creation, or after the most
    /// recent `over commit`/`over sync` fast-forward).
    pub base_oid: String,
    pub created_at: i64,
    pub last_commit_at: Option<i64>,
}

/// The real, default state file location under `$XDG_STATE_HOME/over`.
/// Kept separate from the `_in`-suffixed functions below so tests can point
/// at an isolated `StateFile` instead (mirrors `crate::sync`'s own
/// `persist_checkpoint(state_file: &StateFile<SyncState>, ...)` pattern).
pub(crate) fn state_file() -> Result<StateFile<VirtualCheckoutState>> {
    Ok(StateFile::new(
        XdgDirs::new()?.state_dir().join("virtual_checkout.toml"),
    ))
}

fn key_for(target: &Path) -> String {
    target.to_string_lossy().to_string()
}

/// Synchronous read, for use from [`crate::materialize::Materializer::classify`]
/// (which is deliberately sync across every backend) — see
/// `xdg::state::StateFile::load_blocking`'s own doc for why this is safe.
pub(crate) fn record_for_blocking(target: &Path) -> Result<Option<VirtualCheckoutRecord>> {
    record_for_blocking_in(&state_file()?, target)
}

pub(crate) fn record_for_blocking_in(
    state_file: &StateFile<VirtualCheckoutState>,
    target: &Path,
) -> Result<Option<VirtualCheckoutRecord>> {
    let state = state_file.load_blocking()?;
    Ok(state.checkouts.get(&key_for(target)).cloned())
}

pub(crate) async fn record_for(target: &Path) -> Result<Option<VirtualCheckoutRecord>> {
    record_for_in(&state_file()?, target).await
}

pub(crate) async fn record_for_in(
    state_file: &StateFile<VirtualCheckoutState>,
    target: &Path,
) -> Result<Option<VirtualCheckoutRecord>> {
    let state = state_file.load().await?;
    Ok(state.checkouts.get(&key_for(target)).cloned())
}

pub(crate) async fn persist(target: &Path, record: VirtualCheckoutRecord) -> Result<()> {
    persist_in(&state_file()?, target, record).await
}

pub(crate) async fn persist_in(
    state_file: &StateFile<VirtualCheckoutState>,
    target: &Path,
    record: VirtualCheckoutRecord,
) -> Result<()> {
    let key = key_for(target);
    state_file
        .update(move |s| {
            s.checkouts.insert(key, record);
            Ok(())
        })
        .await
}

pub(crate) async fn remove(target: &Path) -> Result<()> {
    remove_in(&state_file()?, target).await
}

pub(crate) async fn remove_in(
    state_file: &StateFile<VirtualCheckoutState>,
    target: &Path,
) -> Result<()> {
    let key = key_for(target);
    state_file
        .update(move |s| {
            s.checkouts.remove(&key);
            Ok(())
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(managed_path: &str, oid: &str) -> VirtualCheckoutRecord {
        VirtualCheckoutRecord {
            overlay: "ov".to_string(),
            managed_path: PathBuf::from(managed_path),
            base_oid: oid.to_string(),
            created_at: 0,
            last_commit_at: None,
        }
    }

    #[tokio::test]
    async fn record_round_trips_through_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let state_file: StateFile<VirtualCheckoutState> =
            StateFile::new(tmp.path().join("virtual_checkout.toml"));

        let target = PathBuf::from("/home/user/dotfiles-checkout");
        assert!(record_for_in(&state_file, &target).await.unwrap().is_none());

        persist_in(&state_file, &target, record(".", "deadbeef"))
            .await
            .unwrap();
        let loaded = record_for_in(&state_file, &target).await.unwrap().unwrap();
        assert_eq!(loaded.base_oid, "deadbeef");
        assert_eq!(
            record_for_blocking_in(&state_file, &target)
                .unwrap()
                .unwrap()
                .base_oid,
            "deadbeef"
        );

        remove_in(&state_file, &target).await.unwrap();
        assert!(record_for_in(&state_file, &target).await.unwrap().is_none());
    }
}
