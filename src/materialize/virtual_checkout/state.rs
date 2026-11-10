//! Persisted association for virtual checkouts (#141): operational metadata
//! under `$XDG_STATE_HOME/over`, not a backup of target content.
//!
//! Safety checks combine this association with source git objects and target
//! content (`super::git`, `crate::status::virtual_checkout`). Losing it leaves
//! target files intact but loses their recorded base and ownership scope.
//! `VirtualCheckoutMaterializer::classify` refuses populated ordinary-file
//! directories without an association; reapplying alone cannot recover it.
//! Restore matching state from backup when available, preserving target files
//! and source history before attempting recovery.

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
    const VERSION: u32 = 2;

    fn migrate(version: u32, raw: toml::Value) -> Result<Self> {
        anyhow::ensure!(
            version == 1,
            "cannot migrate virtual checkout state from version {version}"
        );
        // Legacy records owned their complete subtree. Never infer new holes
        // from today's config: that could hide edits in a deployed old layout.
        raw.try_into().map_err(Into::into)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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
    #[serde(default)]
    pub scope: crate::desired::CheckoutScope,
    #[serde(default)]
    pub source_repository: Option<PathBuf>,
}

/// A changed configuration cannot silently reinterpret a deployed checkout.
pub(crate) fn verify(
    entry: &crate::desired::DesiredEntry,
    record: &VirtualCheckoutRecord,
) -> Result<()> {
    let crate::desired::Provenance::Overlay { overlay, source } = &entry.provenance else {
        anyhow::bail!("virtual checkout has no overlay source");
    };
    let (repo, managed_path) = super::git::discover_source(source)?;
    anyhow::ensure!(
        &record.overlay == overlay
            && record.managed_path == managed_path
            && record.scope == entry.intent.checkout_scope()
            && record
                .source_repository
                .as_ref()
                .is_none_or(|root| Some(root.as_path()) == repo.workdir()),
        "checkout association at '{}' differs from its source or mapping scope; unapply with the old configuration before changing mappings",
        entry.target.display()
    );
    Ok(())
}

/// Look up the source unit as well as the desired target: moving a mapping
/// otherwise looks like a fresh checkout and strands the old editable files.
pub(crate) fn verify_live_target(entry: &crate::desired::DesiredEntry) -> Result<()> {
    verify_live_target_in(&state_file()?, entry)
}

pub(crate) fn verify_live_target_in(
    state_file: &StateFile<VirtualCheckoutState>,
    entry: &crate::desired::DesiredEntry,
) -> Result<()> {
    let state = state_file.inspect_blocking()?;
    verify_live_target_snapshot(&state, entry)
}

pub(crate) fn verify_live_target_snapshot(
    state: &VirtualCheckoutState,
    entry: &crate::desired::DesiredEntry,
) -> Result<()> {
    let crate::desired::Provenance::Overlay { overlay, source } = &entry.provenance else {
        anyhow::bail!("virtual checkout has no overlay source");
    };
    let desired_key = key_for(&entry.target);
    for (target, record) in &state.checkouts {
        if target == &desired_key || &record.overlay != overlay {
            continue;
        }
        match Path::new(target).symlink_metadata() {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        let (repo, managed_path) = super::git::discover_source(source)?;
        // Old records lack repository identity; preserve their existing
        // overlay/path association rather than silently redeploying them.
        let same_repository = match (&record.source_repository, repo.workdir()) {
            (Some(recorded), Some(current)) => {
                crate::overlays::mapping::physical(recorded)?
                    == crate::overlays::mapping::physical(current)?
            }
            (None, _) => true,
            _ => false,
        };
        anyhow::ensure!(
            record.managed_path != managed_path || !same_repository,
            "checkout association at '{target}' has a different target '{}'; unapply with the old configuration before changing mappings",
            entry.target.display()
        );
    }
    Ok(())
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

/// The state map's key for `target` — canonicalized (resolves symlinks/
/// `.`/`..`, matching `super::git::checkout_subtree`'s own
/// canonicalization before the actual git2 checkout call) so that two
/// `over` invocations naming the *same* directory via different relative
/// paths (or a different current directory) always map to the same
/// record. Without this, e.g. two different overlays both applied with a
/// relative `--target .` from different working directories would collide
/// on the literal string `"."`, silently reading/overwriting each
/// other's association — observed directly, not just theoretical.
///
/// Falls back to an absolute (but not symlink-resolved) path if
/// canonicalization fails — every call site here only ever looks this up
/// for a target that has just been materialized (so it exists), but
/// there's no reason to make this key computation itself fallible for a
/// clearly non-fatal, best-effort concern.
pub(crate) fn key_for(target: &Path) -> String {
    let resolved = target.canonicalize().unwrap_or_else(|_| {
        if target.is_absolute() {
            target.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(target))
                .unwrap_or_else(|_| target.to_path_buf())
        }
    });
    resolved.to_string_lossy().to_string()
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
    let state = state_file.inspect_blocking()?;
    Ok(state.checkouts.get(&key_for(target)).cloned())
}

pub(crate) async fn record_for(target: &Path) -> Result<Option<VirtualCheckoutRecord>> {
    record_for_in(&state_file()?, target).await
}

pub(crate) async fn record_for_in(
    state_file: &StateFile<VirtualCheckoutState>,
    target: &Path,
) -> Result<Option<VirtualCheckoutRecord>> {
    let state = state_file.inspect().await?;
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

/// Use the pre-removal key: a replacement symlink resolves to a different unit.
pub(crate) async fn remove_key(key: String) -> Result<()> {
    remove_key_in(&state_file()?, key).await
}

#[cfg(test)]
pub(crate) async fn remove_in(
    state_file: &StateFile<VirtualCheckoutState>,
    target: &Path,
) -> Result<()> {
    let key = key_for(target);
    remove_key_in(state_file, key).await
}

async fn remove_key_in(state_file: &StateFile<VirtualCheckoutState>, key: String) -> Result<()> {
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

    #[tokio::test]
    async fn live_source_associations_distinguish_repositories_and_ignore_removed_targets() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let other_source = temp.path().join("other-source");
        git2::Repository::init(&source).unwrap();
        git2::Repository::init(&other_source).unwrap();
        let old_target = temp.path().join("old-target");
        std::fs::create_dir(&old_target).unwrap();
        let state_file = StateFile::new(temp.path().join("state.toml"));
        let entry = crate::desired::DesiredEntry {
            target: temp.path().join("new-target"),
            provenance: crate::desired::Provenance::Overlay {
                overlay: "ov".to_string(),
                source: source.clone(),
            },
            intent: crate::desired::MaterializationIntent::VirtualCheckout,
            permissions: None,
        };
        let mut association = record("", "deadbeef");
        association.source_repository = Some(other_source);
        persist_in(&state_file, &old_target, association.clone())
            .await
            .unwrap();
        assert!(verify_live_target_in(&state_file, &entry).is_ok());

        association.source_repository = Some(source);
        persist_in(&state_file, &old_target, association)
            .await
            .unwrap();
        assert!(verify_live_target_in(&state_file, &entry).is_err());
        std::fs::remove_dir(&old_target).unwrap();
        assert!(verify_live_target_in(&state_file, &entry).is_ok());
        assert!(!entry.target.exists());
    }

    #[test]
    fn legacy_records_migrate_without_inventing_mapping_ownership() {
        let raw: toml::Value = toml::from_str("[checkouts.'/target']\noverlay='ov'\nmanaged_path='ov'\nbase_oid='0123456789'\ncreated_at=0\n").unwrap();
        let migrated = VirtualCheckoutState::migrate(1, raw).unwrap();
        assert_eq!(
            migrated.checkouts["/target"].scope,
            crate::desired::CheckoutScope::default()
        );
        assert!(migrated.checkouts["/target"].source_repository.is_none());
    }

    fn record(managed_path: &str, oid: &str) -> VirtualCheckoutRecord {
        VirtualCheckoutRecord {
            overlay: "ov".to_string(),
            managed_path: PathBuf::from(managed_path),
            base_oid: oid.to_string(),
            created_at: 0,
            last_commit_at: None,
            ..Default::default()
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

    #[test]
    fn key_for_resolves_relative_paths_against_the_current_directory() {
        // Two different real directories that happen to share the exact
        // same relative path string ("target") must never collide —
        // this is the scenario `over` hit directly: the same relative
        // `--target` value used from two different working directories.
        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp_a.path().join("target")).unwrap();
        std::fs::create_dir_all(tmp_b.path().join("target")).unwrap();

        let original_cwd = std::env::current_dir().unwrap();

        std::env::set_current_dir(tmp_a.path()).unwrap();
        let key_a = key_for(Path::new("target"));

        std::env::set_current_dir(tmp_b.path()).unwrap();
        let key_b = key_for(Path::new("target"));

        std::env::set_current_dir(original_cwd).unwrap();

        assert_ne!(key_a, key_b);
        assert_eq!(
            key_a,
            tmp_a
                .path()
                .canonicalize()
                .unwrap()
                .join("target")
                .to_string_lossy()
        );
        assert_eq!(
            key_b,
            tmp_b
                .path()
                .canonicalize()
                .unwrap()
                .join("target")
                .to_string_lossy()
        );
    }
}
