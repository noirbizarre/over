//! Persisted, informational-only bookkeeping for `over sync` (#110/#111).
//!
//! Deliberately **not** authoritative: nothing that decides whether a
//! checkout can be safely synced, whether a merge is in progress, or how to
//! recover from a conflict ever reads this file — that all comes from
//! git's own `repo.state()`/`MERGE_HEAD`/index (see
//! `actions::git::sync`). This is purely the "overlay source/worktree
//! association and synchronization checkpoint" the issue asks to persist
//! under `$XDG_STATE_HOME/over`, read back only to enrich `over sync`'s own
//! output (e.g. "last synced 3 days ago"). Deleting this file loses none of
//! that correctness, only the informational history.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::xdg::state::VersionedState;

/// One record per synced checkout/worktree path, keyed by its
/// canonicalized target path (string form, so it round-trips through TOML
/// map keys).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncState {
    pub checkouts: HashMap<String, CheckoutRecord>,
}

impl VersionedState for SyncState {
    const VERSION: u32 = 1;
}

/// Informational record for a single checkout/worktree.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CheckoutRecord {
    pub overlay: String,
    /// Always `"."` today — `over sync` only ever operates on an overlay's
    /// root `git` entry (see `crate::sync`'s module doc).
    pub repo_key: String,
    /// `Some(name)` for one worktree of a bare+worktrees repo, `None` for a
    /// plain (non-bare) checkout.
    pub worktree: Option<String>,
    pub last_synced_oid: Option<String>,
    /// Unix timestamp (seconds) of the last sync *attempt* (recorded even
    /// when the outcome was `Blocked`/`Conflict`, so this reflects "last
    /// time `over sync` looked at this checkout", not just the last
    /// success). A plain integer rather than a formatted timestamp so this
    /// module doesn't need a date/time dependency just to record one.
    pub last_synced_at: Option<i64>,
    /// Short human-readable label of the last outcome (e.g. `"merged"`,
    /// `"conflict"`), for display only.
    pub last_outcome: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xdg::state::StateFile;

    #[tokio::test]
    async fn round_trips_through_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        let state_file: StateFile<SyncState> = StateFile::new(tmp.path().join("sync.toml"));

        state_file
            .update(|s| {
                s.checkouts.insert(
                    "/home/user/.config/nvim".to_string(),
                    CheckoutRecord {
                        overlay: "dotfiles".to_string(),
                        repo_key: ".".to_string(),
                        worktree: None,
                        last_synced_oid: Some("abc123".to_string()),
                        last_synced_at: Some(1_700_000_000),
                        last_outcome: Some("merged".to_string()),
                    },
                );
                Ok(())
            })
            .await
            .unwrap();

        let loaded = state_file.load().await.unwrap();
        let record = loaded.checkouts.get("/home/user/.config/nvim").unwrap();
        assert_eq!(record.overlay, "dotfiles");
        assert_eq!(record.last_synced_oid.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn missing_file_loads_as_empty_default() {
        let tmp = tempfile::tempdir().unwrap();
        let state_file: StateFile<SyncState> = StateFile::new(tmp.path().join("sync.toml"));

        let loaded = state_file.load().await.unwrap();

        assert!(loaded.checkouts.is_empty());
    }
}
