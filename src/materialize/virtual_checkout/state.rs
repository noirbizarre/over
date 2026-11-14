//! Persisted association for virtual checkouts (#141): operational metadata
//! under `$XDG_STATE_HOME/over`, not a backup of target content.
//!
//! Safety checks combine this association with source git objects and target
//! content (`super::git`, `crate::status::virtual_checkout`). Losing it leaves
//! target files intact but loses their recorded base and ownership scope.
//! Reapplying can initialize an existing directory against current source HEAD,
//! resolving collisions, but cannot reconstruct its previous base. Restore
//! matching state from backup when recovering that historical association.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::xdg::XdgDirs;
use crate::xdg::state::{StateFile, VersionedState};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VirtualCheckoutState {
    /// Legacy target keys coexist with source-unit keys for shared destinations.
    pub checkouts: HashMap<String, VirtualCheckoutRecord>,
}

impl VersionedState for VirtualCheckoutState {
    const VERSION: u32 = 4;

    fn migrate(version: u32, raw: toml::Value) -> Result<Self> {
        anyhow::ensure!(
            (1..=3).contains(&version),
            "cannot migrate virtual checkout state from version {version}"
        );
        // Legacy records owned their complete subtree. Never infer new holes
        // from today's config: that could hide edits in a deployed old layout.
        raw.try_into().map_err(Into::into)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VirtualCheckoutRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<super::generated::GeneratedState>,
    /// An interrupted first apply reserves its leaves but must still initialize on retry.
    #[serde(default)]
    pub initializing: bool,
    pub overlay: String,
    /// Path of the managed subtree relative to the source repository's
    /// working directory root (empty for a whole-repository virtual
    /// checkout).
    pub managed_path: PathBuf,
    /// Source comparison base at initialization or the most recent commit/sync.
    /// Skipped or absorbed initial collisions may already differ from this base.
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
    for (key, record) in &state.checkouts {
        let target = target_for_key(key)?;
        if key_for(&target) == desired_key || &record.overlay != overlay {
            continue;
        }
        match target.symlink_metadata() {
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
            "checkout association at '{}' has a different target '{}'; unapply with the old configuration before changing mappings",
            target.display(),
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
/// `.`/`..`) so that two
/// `over` invocations naming the *same* directory via different relative
/// paths (or a different current directory) always map to the same
/// record. Without this, e.g. two different overlays both applied with a
/// relative `--target .` from different working directories would collide
/// on the literal string `"."`, silently reading/overwriting each
/// other's association — observed directly, not just theoretical.
///
/// Missing destinations retain the canonical spelling of their existing ancestor,
/// including Windows verbatim prefixes used by legacy records. Fall back to an
/// absolute path if ancestors cannot be resolved; operational guards report the error.
pub(crate) fn key_for(target: &Path) -> String {
    // Reservations precede directory creation; resolve existing ancestors too so
    // initialization and final publication cannot acquire different alias keys.
    let resolved = canonical_target(target).unwrap_or_else(|_| {
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

fn canonical_target(target: &Path) -> Result<PathBuf> {
    let resolved = crate::overlays::mapping::physical(target)?;
    for ancestor in resolved.ancestors() {
        match ancestor.canonicalize() {
            Ok(canonical) => {
                let suffix = resolved.strip_prefix(ancestor)?;
                // Keep std's Windows canonical prefix, and avoid adding a trailing
                // separator to existing destinations when the suffix is empty.
                return Ok(if suffix.as_os_str().is_empty() {
                    canonical
                } else {
                    canonical.join(suffix)
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("target '{}' has no canonical ancestor", target.display())
}

/// Legacy keys remain paths; projection keys encode a tuple without delimiter ambiguity.
pub(crate) fn target_for_key(key: &str) -> Result<PathBuf> {
    if let Some(encoded) = key.strip_prefix("projection:") {
        let (target, _, _, _): (PathBuf, Option<PathBuf>, String, PathBuf) =
            serde_json::from_str(encoded)?;
        Ok(target)
    } else {
        Ok(PathBuf::from(key))
    }
}

pub(crate) fn association_key(target: &Path, record: &VirtualCheckoutRecord) -> Result<String> {
    let target = key_for(target);
    if record.scope.projection.is_none() {
        return Ok(target);
    }
    Ok(format!(
        "projection:{}",
        serde_json::to_string(&(
            target,
            &record.source_repository,
            &record.overlay,
            &record.managed_path,
        ))?
    ))
}

fn unique_record(
    state: &VirtualCheckoutState,
    target: &Path,
) -> Result<Option<VirtualCheckoutRecord>> {
    // Target-only queries are boundary guards, not permission to mutate an owner.
    for (key, record) in &state.checkouts {
        if key_for(&target_for_key(key)?) == key_for(target) {
            return Ok(Some(record.clone()));
        }
    }
    Ok(None)
}

pub(crate) fn record_in_snapshot(
    state: &VirtualCheckoutState,
    entry: &crate::desired::DesiredEntry,
) -> Result<Option<VirtualCheckoutRecord>> {
    let crate::desired::Provenance::Overlay { overlay, source } = &entry.provenance else {
        return Ok(None);
    };
    let source_identity = super::git::discover_source(source)
        .ok()
        .map(|(repo, managed)| (repo.workdir().map(Path::to_path_buf), managed));
    for (key, record) in &state.checkouts {
        if key_for(&target_for_key(key)?) != key_for(&entry.target) {
            continue;
        }
        if record.scope.projection.is_none() {
            // Return legacy ownership even for a different source so verification rejects it.
            return Ok(Some(record.clone()));
        }
        if &record.overlay == overlay
            && source_identity.as_ref().is_some_and(|(root, managed)| {
                root == &record.source_repository && managed == &record.managed_path
            })
        {
            return Ok(Some(record.clone()));
        }
    }
    Ok(None)
}

pub(crate) fn record_for_entry(
    entry: &crate::desired::DesiredEntry,
) -> Result<Option<VirtualCheckoutRecord>> {
    record_in_snapshot(&state_file()?.inspect_blocking()?, entry)
}

/// Lock order is ownership mutation, source index, then short state publication locks.
/// Read-only commands never create this sentinel or acquire publication locks.
pub(crate) fn mutation_guard() -> Result<std::fs::File> {
    let guard = raw_mutation_guard()?;
    let snapshot = state_file()?.inspect_blocking()?;
    let sources: std::collections::BTreeSet<_> = snapshot
        .checkouts
        .values()
        .filter(|record| record.generated.is_some())
        .filter_map(|record| record.source_repository.as_ref())
        .collect();
    for source in sources {
        match git2::Repository::open(source) {
            Ok(repo) => super::generated_migration::ensure_ready(&repo)?,
            // An absent unrelated source cannot currently publish a migration in this state directory.
            Err(error) if error.code() == git2::ErrorCode::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(guard)
}

/// Lifecycle recovery must acquire ownership before inspecting or completing its persistent fence.
pub(crate) fn raw_mutation_guard() -> Result<std::fs::File> {
    let directory = XdgDirs::new()?.state_dir().to_path_buf();
    std::fs::create_dir_all(&directory)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("virtual_checkout.mutation.lock"))?;
    file.lock()?;
    Ok(file)
}

pub(crate) fn validate_claims(
    state: &VirtualCheckoutState,
    entry: &crate::desired::DesiredEntry,
) -> Result<()> {
    let own_key = record_in_snapshot(state, entry)?
        .map(|record| association_key(&entry.target, &record))
        .transpose()?;
    for (key, record) in &state.checkouts {
        let target = target_for_key(key)?;
        let source = record
            .source_repository
            .as_ref()
            .map(|root| root.join(&record.managed_path));
        if own_key.as_ref() == Some(key) {
            continue;
        }
        let Some(source) = source else {
            // Legacy records without source identity still reserve their entire namespace.
            if entry.target.starts_with(&target) || target.starts_with(&entry.target) {
                anyhow::bail!(
                    "target '{}' overlaps legacy checkout '{}' owned by {}; unapply its old layout first",
                    entry.target.display(),
                    target.display(),
                    record.overlay
                );
            }
            continue;
        };
        let mut other = crate::desired::DesiredEntry {
            target,
            provenance: crate::desired::Provenance::Overlay {
                overlay: record.overlay.clone(),
                source,
            },
            intent: crate::desired::MaterializationIntent::ScopedVirtualCheckout(
                record.scope.clone(),
            ),
            permissions: None,
        };
        resolve_projection_with_record(&mut other, false, Some(record.clone()))?;
        // Resolve aliases at the concrete deployment roots as well as lexical paths.
        let mut candidate = entry.clone();
        candidate.target = crate::overlays::mapping::location(&candidate.target)?;
        other.target = crate::overlays::mapping::location(&other.target)?;
        if let Some((path, reason)) = crate::desired::ownership::conflict(&candidate, &other)? {
            anyhow::bail!(
                "conflicting target ownership at '{}': selected checkout conflicts with {} ({reason})",
                path.display(),
                record.overlay
            );
        }
    }
    Ok(())
}

pub(crate) fn reserve(entry: &crate::desired::DesiredEntry) -> Result<()> {
    state_file()?.update_blocking(|state| {
        validate_claims(state, entry)?;
        let mut record = record_in_snapshot(state, entry)?
            .ok_or_else(|| anyhow::anyhow!("run over apply before acquiring checkout files"))?;
        verify(entry, &record)?;
        if let (Some(paths), Some(admitted)) = (
            &mut record.scope.projection,
            entry.intent.checkout_scope().projection,
        ) {
            paths.extend(*admitted);
        }
        state
            .checkouts
            .insert(association_key(&entry.target, &record)?, record);
        Ok(())
    })
}

pub(crate) fn reserve_initial(target: &Path, mut record: VirtualCheckoutRecord) -> Result<()> {
    let source = record
        .source_repository
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("projected checkout lacks source identity"))?
        .join(&record.managed_path);
    let entry = crate::desired::DesiredEntry {
        target: target.to_path_buf(),
        provenance: crate::desired::Provenance::Overlay {
            overlay: record.overlay.clone(),
            source,
        },
        intent: crate::desired::MaterializationIntent::ScopedVirtualCheckout(record.scope.clone()),
        permissions: None,
    };
    record.initializing = true;
    state_file()?.update_blocking(|state| {
        validate_claims(state, &entry)?;
        state
            .checkouts
            .insert(association_key(target, &record)?, record);
        Ok(())
    })
}

/// Build positive claims without reading any destination content or publishing Git objects.
pub(crate) fn resolve_projection(
    entry: &mut crate::desired::DesiredEntry,
    walk_source: bool,
) -> Result<()> {
    if !entry.intent.is_virtual_checkout() {
        return Ok(());
    }
    let existing = record_for_entry(entry)?;
    resolve_projection_with_record(entry, walk_source, existing)
}

fn resolve_projection_with_record(
    entry: &mut crate::desired::DesiredEntry,
    walk_source: bool,
    existing: Option<VirtualCheckoutRecord>,
) -> Result<()> {
    let crate::desired::Provenance::Overlay { source, .. } = &entry.provenance else {
        return Ok(());
    };
    if !entry.intent.is_virtual_checkout() {
        return Ok(());
    }
    let mut scope = entry.intent.checkout_scope();
    if existing
        .as_ref()
        .is_some_and(|record| record.scope.projection.is_none())
    {
        return Ok(());
    }
    let mut paths = existing
        .as_ref()
        .and_then(|record| record.scope.projection.clone())
        .unwrap_or_default();
    if let Some(generated) = existing
        .as_ref()
        .and_then(|record| record.generated.as_ref())
    {
        // Snapshot leaves use destination names; source/index names would reintroduce template suffixes.
        paths.extend(generated.sources.keys().cloned());
        if let Some(rendered) = &scope.rendered {
            paths.extend(rendered.keys().cloned());
        }
        scope.projection = Some(paths);
        entry.intent = crate::desired::MaterializationIntent::ScopedVirtualCheckout(scope);
        return Ok(());
    }
    if walk_source && source.is_dir() {
        for candidate in walkdir::WalkDir::new(source)
            .min_depth(1)
            .into_iter()
            .filter_entry(|item| {
                let relative = item.path().strip_prefix(source).unwrap_or(Path::new(""));
                scope.owns_source_kind(relative, item.file_type().is_dir())
            })
        {
            let candidate = candidate?;
            let relative = candidate.path().strip_prefix(source)?;
            if candidate.file_type().is_file() && !super::git::is_overlay_metadata(relative) {
                paths.insert(relative.to_path_buf());
            }
        }
    }
    if let Ok((repo, managed)) = super::git::discover_source(source) {
        for oid in [
            None,
            existing.as_ref().map(|record| record.base_oid.as_str()),
        ] {
            if let Ok(tree) = super::git::base_tree(&repo, oid) {
                paths.extend(
                    super::git::scoped_entries(&repo, &tree, &managed, &scope)?.into_keys(),
                );
            }
        }
        for indexed in repo.index()?.iter() {
            if let Some(relative) = super::git::relative_index_path(&indexed.path, &managed)?
                && scope.owns_source(&relative)
                && !super::git::is_overlay_metadata(&relative)
            {
                paths.insert(relative);
            }
        }
    }
    scope.projection = Some(paths);
    entry.intent = crate::desired::MaterializationIntent::ScopedVirtualCheckout(scope);
    Ok(())
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
    unique_record(&state, target)
}

#[cfg(test)]
pub(crate) async fn record_for(target: &Path) -> Result<Option<VirtualCheckoutRecord>> {
    record_for_in(&state_file()?, target).await
}

#[cfg(test)]
pub(crate) async fn record_for_in(
    state_file: &StateFile<VirtualCheckoutState>,
    target: &Path,
) -> Result<Option<VirtualCheckoutRecord>> {
    let state = state_file.inspect().await?;
    unique_record(&state, target)
}

pub(crate) async fn persist(target: &Path, record: VirtualCheckoutRecord) -> Result<()> {
    persist_in(&state_file()?, target, record).await
}

pub(crate) async fn persist_in(
    state_file: &StateFile<VirtualCheckoutState>,
    target: &Path,
    mut record: VirtualCheckoutRecord,
) -> Result<()> {
    let key = association_key(target, &record)?;
    let target = target.to_path_buf();
    state_file
        .update(move |s| {
            if let Some(paths) = &mut record.scope.projection {
                if let Some(previous) = s
                    .checkouts
                    .get(&key)
                    .and_then(|old| old.scope.projection.as_ref())
                {
                    // Checkpoint publication must not erase an explicit acquisition after reset.
                    paths.extend(previous.iter().cloned());
                }
                let root = record.source_repository.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("projected checkout lacks source repository identity")
                })?;
                let entry = crate::desired::DesiredEntry {
                    target,
                    provenance: crate::desired::Provenance::Overlay {
                        overlay: record.overlay.clone(),
                        source: root.join(&record.managed_path),
                    },
                    intent: crate::desired::MaterializationIntent::ScopedVirtualCheckout(
                        record.scope.clone(),
                    ),
                    permissions: None,
                };
                validate_claims(s, &entry)?;
            }
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

    #[test]
    fn creating_a_reserved_destination_preserves_its_canonical_key() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("missing").join("target");
        let reserved = key_for(&target);
        std::fs::create_dir_all(&target).unwrap();
        assert_eq!(reserved, key_for(&target));
        assert_eq!(reserved, target.canonicalize().unwrap().to_string_lossy());
    }

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

    #[rstest::rstest]
    #[case(1)]
    #[case(2)]
    fn legacy_records_migrate_without_inventing_mapping_ownership(#[case] version: u32) {
        let raw: toml::Value = toml::from_str("[checkouts.'/target']\noverlay='ov'\nmanaged_path='ov'\nbase_oid='0123456789'\ncreated_at=0\n").unwrap();
        let migrated = VirtualCheckoutState::migrate(version, raw).unwrap();
        assert_eq!(
            migrated.checkouts["/target"].scope,
            crate::desired::CheckoutScope::default()
        );
        assert!(migrated.checkouts["/target"].source_repository.is_none());
        assert!(migrated.checkouts["/target"].scope.projection.is_none());
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
