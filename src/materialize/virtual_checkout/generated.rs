//! Generated desired snapshots and pure base/local/desired reconciliation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use git2::{IndexEntryExtendedFlag, ObjectType, Oid, Repository};
use serde::{Deserialize, Serialize};

use crate::desired::content::{RenderedFile, RenderedInventory};
use crate::desired::{DesiredEntry, Provenance};
use crate::materialize::disposal::PathGuard;
use crate::plan::{ActualState, Operation, actual};

use super::{generated_snapshot, git, snapshot, state};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedState {
    pub reference: String,
    pub base: String,
    /// Historical provenance also protects destinations whose source disappeared.
    pub sources: BTreeMap<PathBuf, PathBuf>,
    /// A conflict retains its original base even after the latest desired ref advances.
    pub conflicts: BTreeMap<PathBuf, String>,
    #[serde(default)]
    pub pending: Option<String>,
    /// Separate desired provenance from the historical guards retained until publication succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_sources: Option<BTreeMap<PathBuf, PathBuf>>,
    #[serde(default)]
    pub applied: BTreeMap<PathBuf, String>,
    #[serde(default)]
    pub observed: BTreeMap<PathBuf, String>,
    #[serde(default)]
    pub permissions: BTreeMap<PathBuf, u32>,
    #[serde(default)]
    pub source_revision: String,
    /// Partial reverse commits must retain the source base of unselected files.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_revisions: BTreeMap<PathBuf, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse_pending: Option<ReversePublication>,
    /// A reverse commit consumes local edits without applying independent source edits to the target.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reverse_observed: BTreeMap<PathBuf, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReversePublication {
    pub parent: String,
    pub source_commit: String,
    pub generated_commit: String,
    pub destinations: Vec<PathBuf>,
    pub observed: BTreeMap<PathBuf, String>,
    pub index_before: String,
    pub sources_before: BTreeMap<PathBuf, String>,
}

pub(crate) fn active(entry: &DesiredEntry) -> Result<bool> {
    Ok(entry.intent.checkout_scope().rendered.is_some()
        || state::record_for_entry(entry)?.is_some_and(|record| record.generated.is_some()))
}

pub(crate) fn generated_paths(entry: &DesiredEntry) -> Result<BTreeSet<PathBuf>> {
    let mut paths: BTreeSet<_> = entry
        .intent
        .checkout_scope()
        .rendered
        .iter()
        .flat_map(|inventory| {
            inventory
                .iter()
                .filter(|(_, file)| file.generated)
                .map(|(path, _)| path.clone())
        })
        .collect();
    if let Some(record) = state::record_for_entry(entry)?
        && let Some(generated) = record.generated
    {
        paths.extend(generated.sources.into_keys());
    }
    Ok(paths)
}

pub(crate) fn classify(entry: &DesiredEntry) -> Result<Operation> {
    state::verify_live_target(entry)?;
    match actual::inspect_with(&entry.target, &entry.access.fs)? {
        ActualState::Missing => Ok(Operation::Create),
        ActualState::Directory => {
            if let Some(record) = state::record_for_entry(entry)? {
                state::verify(entry, &record)?;
                Ok(if record.initializing {
                    Operation::Create
                } else {
                    Operation::Noop
                })
            } else {
                // Initialization uses the same conservative three-way engine; foreign leaves conflict.
                Ok(Operation::Create)
            }
        }
        current => Ok(Operation::Conflict { current }),
    }
}

fn source(entry: &DesiredEntry) -> Result<(&str, &Path)> {
    let Provenance::Overlay { overlay, source } = &entry.provenance else {
        anyhow::bail!("generated checkout lacks an overlay source")
    };
    Ok((overlay, source))
}

fn oid(file: &RenderedFile) -> Result<(Oid, i32)> {
    Ok((Oid::hash_object(ObjectType::Blob, &file.bytes)?, file.mode))
}

fn fingerprint(file: Option<&RenderedFile>) -> Result<String> {
    file.map(|file| Ok(format!("{}:{}", oid(file)?.0, file.mode)))
        .unwrap_or_else(|| Ok("missing".into()))
}

fn ref_target(repo: &Repository, reference: &str) -> Result<Option<Oid>> {
    match repo.find_reference(reference) {
        Ok(reference) => {
            Ok(Some(reference.target().context(
                "generated snapshot ref must be a direct local ref",
            )?))
        }
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

const SNAPSHOT_REF_LOCK_RETRIES: usize = 20;

fn publish_snapshot_ref(
    repo: &Repository,
    reference: &str,
    commit: Oid,
    expected: Option<Oid>,
    mut wait: impl FnMut(),
) -> Result<()> {
    for attempt in 0..=SNAPSHOT_REF_LOCK_RETRIES {
        // Retain the original expectation across retries so contention cannot hide another writer.
        let result = if let Some(expected) = expected {
            repo.reference_matching(reference, commit, true, expected, "over generated snapshot")
        } else {
            repo.reference(reference, commit, false, "over generated snapshot")
        };
        match result {
            Ok(_) => return Ok(()),
            Err(error)
                if error.code() == git2::ErrorCode::Locked
                    && attempt < SNAPSHOT_REF_LOCK_RETRIES =>
            {
                // Only ref publication is safe to retry; target writes and their journal stay intact.
                wait();
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to publish generated snapshot ref '{reference}'")
                });
            }
        }
    }
    unreachable!("the last publication attempt returns without retrying")
}

fn same(left: Option<&RenderedFile>, right: Option<&RenderedFile>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.bytes == right.bytes && left.mode == right.mode,
        _ => false,
    }
}

pub(crate) fn tree_inventory(
    repo: &Repository,
    commit: &str,
    record: &state::VirtualCheckoutRecord,
) -> Result<RenderedInventory> {
    if let Some(inventory) = generated_snapshot::read(repo, Oid::from_str(commit)?)? {
        return Ok(inventory);
    }
    let commit = repo.find_commit(Oid::from_str(commit)?)?;
    let manifest = super::generated_migration::legacy_plain_manifest(repo, &commit, record)?;
    generated_snapshot::inventory(repo, &commit, manifest)
}

fn desired(entry: &DesiredEntry, repo: &Repository, managed: &Path) -> Result<RenderedInventory> {
    if let Some(inventory) = entry.intent.checkout_scope().rendered {
        return Ok(inventory.inventory.clone());
    }
    // A removed template rule must still reconcile the previous generated destinations.
    let head = git::base_tree(repo, None)?;
    let (_, root) = source(entry)?;
    git::scoped_entries(repo, &head, managed, &entry.intent.checkout_scope())?
        .into_iter()
        .map(|(path, (id, mode))| {
            let protection =
                crate::crypt::protection::Protection::for_path(repo, &managed.join(&path))?;
            let file = RenderedFile {
                protection,
                source_revision: Some(repo.head()?.peel_to_commit()?.id().to_string()),
                source: root.join(&path),
                bytes: protection
                    .decode(repo, repo.find_blob(id)?.content())?
                    .to_vec()
                    .into(),
                mode,
                identity: id.to_string(),
                generated: false,
                permissions: None,
            };
            Ok((path, file))
        })
        .collect()
}

pub(crate) fn leaf_access(entry: &DesiredEntry, path: &Path) -> Result<crate::target_fs::Access> {
    let (_, root) = source(entry)?;
    if let Some(file) = entry
        .intent
        .checkout_scope()
        .rendered
        .as_ref()
        .and_then(|files| files.get(path))
    {
        return Ok(entry.access.child(file.source.strip_prefix(root)?, false));
    }
    if let Some(record) = state::record_for_entry(entry)?
        && let Some(source) = record
            .generated
            .as_ref()
            .and_then(|generated| generated.sources.get(path))
    {
        return Ok(entry.access.child(source.strip_prefix(root)?, false));
    }
    Ok(entry.access.child(path, false))
}

fn local(entry: &DesiredEntry, path: &Path) -> Result<Option<RenderedFile>> {
    let root = &entry.target;
    let target = root.join(path);
    crate::materialize::disposal::validate_relative_ancestors_with(root, &target, &entry.access)?;
    let access = leaf_access(entry, path)?;
    let metadata = match access.fs.metadata(&target, false) {
        Ok(metadata) => metadata,
        Err(error) if crate::target_fs::is_not_found(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    ensure!(
        metadata.is_file() || metadata.is_symlink(),
        "checkout destination '{}' is obstructed",
        target.display()
    );
    let bytes = if metadata.is_symlink() {
        access
            .fs
            .read_link(&target)?
            .to_str()
            .context("symlink is not valid UTF-8")?
            .as_bytes()
            .to_vec()
    } else {
        access.fs.read(&target)?
    };
    let mode = if metadata.is_symlink() {
        0o120000
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                0o100644
            } else {
                0o100755
            }
        }
        #[cfg(not(unix))]
        {
            0o100644
        }
    };
    Ok(Some(RenderedFile {
        protection: Default::default(),
        source_revision: None,
        source: target,
        bytes: bytes.into(),
        identity: String::new(),
        mode,
        generated: false,
        permissions: None,
    }))
}

pub(crate) struct Analysis {
    pub base: RenderedInventory,
    pub desired: RenderedInventory,
    pub local: RenderedInventory,
    pub updates: BTreeMap<PathBuf, Option<RenderedFile>>,
    pub conflicts: BTreeMap<PathBuf, String>,
}

impl Analysis {
    fn template_source<'a>(
        &'a self,
        generated: Option<&'a GeneratedState>,
        path: &Path,
    ) -> Option<&'a Path> {
        self.desired
            .get(path)
            .or_else(|| self.base.get(path))
            .filter(|file| !file.source.as_os_str().is_empty())
            .map(|file| file.source.as_path())
            .or_else(|| {
                generated
                    .and_then(|generated| generated.sources.get(path))
                    .map(PathBuf::as_path)
            })
    }
}

pub(crate) fn analyze(
    entry: &DesiredEntry,
    record: Option<&state::VirtualCheckoutRecord>,
) -> Result<Analysis> {
    let (_, root) = source(entry)?;
    let (repo, managed) = git::discover_source(root)?;
    let generated = record.and_then(|record| record.generated.as_ref());
    if let Some(generated) = generated {
        let current = ref_target(&repo, &generated.reference)?;
        let base = Oid::from_str(&generated.base)?;
        let pending = generated
            .pending
            .as_deref()
            .map(Oid::from_str)
            .transpose()?;
        ensure!(
            current == Some(base)
                || (pending.is_some() && current == pending)
                || (current.is_none() && record.is_some_and(|record| record.initializing)),
            "generated snapshot ref '{}' differs from its checkout association; files preserved",
            generated.reference
        );
    }
    let desired = if let Some(generated) = generated
        && let Some(pending) = &generated.pending
    {
        // Finish the exact journaled operation before considering a newer context/render.
        let mut inventory = tree_inventory(
            &repo,
            pending,
            record.context("missing pending snapshot association")?,
        )?;
        for (path, permissions) in &generated.permissions {
            if let Some(file) = inventory.get_mut(path) {
                file.permissions = Some(crate::overlays::FileMode::from_bits(*permissions));
            }
        }
        inventory
    } else {
        desired(entry, &repo, &managed)?
    };
    let mut base = if let Some(generated) = generated.filter(|generated| !generated.base.is_empty())
    {
        tree_inventory(
            &repo,
            &generated.base,
            record.context("missing generated snapshot association")?,
        )?
    } else if let Some(record) =
        record.filter(|record| !record.initializing && !record.base_oid.is_empty())
    {
        // Legacy baselines are source trees, never today's generated output.
        let tree = git::base_tree(&repo, Some(&record.base_oid))?;
        git::scoped_entries(&repo, &tree, &managed, &record.scope)?
            .into_iter()
            .map(|(path, (id, mode))| {
                let protection =
                    crate::crypt::protection::Protection::for_path(&repo, &managed.join(&path))?;
                Ok((
                    path.clone(),
                    RenderedFile {
                        protection,
                        source_revision: Some(record.base_oid.clone()),
                        source: root.join(path),
                        bytes: protection
                            .decode(&repo, repo.find_blob(id)?.content())?
                            .to_vec()
                            .into(),
                        identity: id.to_string(),
                        mode,
                        generated: false,
                        permissions: None,
                    },
                ))
            })
            .collect::<Result<_>>()?
    } else {
        BTreeMap::new()
    };
    if let Some(generated) = generated {
        for (path, commit) in &generated.reverse_observed {
            if let Some(observed) = tree_inventory(
                &repo,
                commit,
                record.context("missing consumed snapshot association")?,
            )?
            .remove(path)
            {
                base.insert(path.clone(), observed);
            }
        }
    }
    let mut analysis = Analysis {
        base,
        desired,
        local: BTreeMap::new(),
        updates: BTreeMap::new(),
        conflicts: BTreeMap::new(),
    };
    let paths: BTreeSet<_> = analysis
        .base
        .keys()
        .chain(analysis.desired.keys())
        // Deleted destinations can disappear from both trees while their local conflict survives.
        .chain(
            generated
                .into_iter()
                .flat_map(|generated| generated.conflicts.keys()),
        )
        .cloned()
        .collect();
    super::safety::SourceProtection::new(&repo)?.validate_paths(
        &entry.target,
        &paths,
        &entry.access,
    )?;
    for path in paths {
        let current = local(entry, &path)?;
        if let Some(current) = &current {
            analysis.local.insert(path.clone(), current.clone());
        }
        let mut base = analysis.base.get(&path).cloned();
        let wanted = analysis.desired.get(&path);
        if same(current.as_ref(), wanted) {
            #[cfg(unix)]
            if let Some(wanted) = wanted
                && let Some(permissions) = wanted.permissions
            {
                use std::os::unix::fs::PermissionsExt;
                if leaf_access(entry, &path)?
                    .fs
                    .metadata(&entry.target.join(&path), false)?
                    .permissions()
                    .mode()
                    & 0o777
                    != permissions.bits()
                {
                    analysis.updates.insert(path.clone(), Some(wanted.clone()));
                }
            }
            continue;
        }
        let previous_conflict = generated.and_then(|generated| generated.conflicts.get(&path));
        if let Some(commit) = previous_conflict.filter(|commit| !commit.is_empty()) {
            base = tree_inventory(
                &repo,
                commit,
                record.context("missing conflict snapshot association")?,
            )?
            .remove(&path);
        }
        if let Some(generated) = generated
            && generated.pending.is_some()
        {
            let observed = fingerprint(current.as_ref())?;
            if generated.applied.get(&path) == Some(&observed) {
                // An interrupted publication already wrote this result; never apply its patch twice.
                continue;
            }
            if generated
                .observed
                .get(&path)
                .is_some_and(|before| *before != observed)
            {
                // Editors do not honor Over's lock; preserve unexpected post-interruption edits.
                analysis.conflicts.insert(path, generated.base.clone());
                continue;
            }
        }
        let result = if same(current.as_ref(), base.as_ref()) {
            Some(wanted.cloned())
        } else if same(wanted, base.as_ref()) && previous_conflict.is_none() {
            None
        } else if let (Some(base), Some(current), Some(wanted)) =
            (base.as_ref(), current.as_ref(), wanted)
        {
            merge(base, current, wanted)?.map(Some)
        } else {
            None
        };
        if let Some(result) = result {
            analysis.updates.insert(path, result);
        } else if !same(wanted, base.as_ref()) || previous_conflict.is_some() {
            let original = previous_conflict
                .cloned()
                .or_else(|| generated.map(|generated| generated.base.clone()))
                .unwrap_or_default();
            analysis.conflicts.insert(path, original);
        }
    }
    Ok(analysis)
}

fn merge(
    base: &RenderedFile,
    local: &RenderedFile,
    desired: &RenderedFile,
) -> Result<Option<RenderedFile>> {
    if [base, local, desired].iter().any(|file| {
        file.mode == 0o120000
            || file.bytes.contains(&0)
            || std::str::from_utf8(&file.bytes).is_err()
    }) {
        return Ok(None);
    }
    if local.mode != base.mode && desired.mode != base.mode && local.mode != desired.mode {
        return Ok(None);
    }
    let mut ancestor = git2::MergeFileInput::new();
    ancestor.content(&base.bytes);
    let mut ours = git2::MergeFileInput::new();
    ours.content(&local.bytes);
    let mut theirs = git2::MergeFileInput::new();
    theirs.content(&desired.bytes);
    let result = git2::merge_file(&ancestor, &ours, &theirs, None)?;
    if !result.is_automergeable() {
        return Ok(None);
    }
    let mut file = desired.clone();
    file.bytes = result.content().to_vec().into();
    if desired.mode == base.mode {
        file.mode = local.mode;
    }
    Ok(Some(file))
}

pub(crate) fn persist(entry: &DesiredEntry, record: &state::VirtualCheckoutRecord) -> Result<()> {
    let key = state::association_key(&entry.target, record)?;
    state::state_file()?.update_blocking(|snapshot| {
        state::validate_claims(snapshot, entry)?;
        snapshot.checkouts.insert(key, record.clone());
        Ok(())
    })
}

pub(crate) struct ReconcileOutcome {
    conflicts: Vec<ReconcileConflict>,
}

struct ReconcileConflict {
    target: PathBuf,
    source: Option<PathBuf>,
}

impl ReconcileOutcome {
    pub(crate) fn ensure_clean(self) -> Result<()> {
        if !self.conflicts.is_empty() {
            let paths = self
                .conflicts
                .iter()
                .map(|conflict| {
                    format!(
                        "'{}' (source template '{}')",
                        conflict.target.display(),
                        conflict
                            .source
                            .as_ref()
                            .map(|source| source.display().to_string())
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "generated checkout conflicts; local files preserved: {paths}; edit the source template or restore the desired output"
            );
        }
        Ok(())
    }
}

pub(crate) fn reconcile(entry: &DesiredEntry, dry_run: bool) -> Result<ReconcileOutcome> {
    state::verify_live_target(entry)?;
    let previous = state::record_for_entry(entry)?;
    ensure!(
        previous
            .as_ref()
            .and_then(|record| record.generated.as_ref())
            .is_none_or(|generated| generated.reverse_pending.is_none()),
        "reverse commit metadata publication was interrupted; rerun over commit first"
    );
    if let Some(record) = &previous {
        state::verify(entry, record)?;
    }
    let analysis = analyze(entry, previous.as_ref())?;
    // Preview and execution must carry the same attention decision; logging a
    // conflict is insufficient for callers deciding whether sync may continue.
    let outcome = ReconcileOutcome {
        conflicts: analysis
            .conflicts
            .keys()
            .map(|path| ReconcileConflict {
                target: entry.target.join(path),
                source: analysis
                    .template_source(
                        previous
                            .as_ref()
                            .and_then(|record| record.generated.as_ref()),
                        path,
                    )
                    .map(Path::to_path_buf),
            })
            .collect(),
    };
    let local_paths: BTreeSet<_> = analysis.base.keys().chain(analysis.local.keys()).collect();
    for path in local_paths {
        if !same(analysis.local.get(path), analysis.base.get(path)) {
            let template = analysis
                .desired
                .get(path)
                .or_else(|| analysis.base.get(path));
            if !template.is_some_and(|file| file.generated) {
                continue;
            }
            crate::ui::info(format!(
                "local edit: '{}' (source template '{}'); apply the edit to its source template",
                entry.target.join(path).display(),
                template
                    .map(|file| file.source.display().to_string())
                    .unwrap_or_default()
            ))?;
        }
    }
    // Check every changed leaf before publishing any update: rename and unlink
    // must not bypass read-only protection merely because a sibling is rendered.
    for path in analysis.updates.keys() {
        let target = entry.target.join(path);
        crate::materialize::disposal::validate_relative_ancestors_with(
            &entry.target,
            &target,
            &entry.access,
        )?;
        match leaf_access(entry, path)?.fs.metadata(&target, false) {
            Ok(metadata) if metadata.is_file() => ensure!(
                !metadata.permissions().readonly(),
                "file '{}' is read-only",
                target.display()
            ),
            Ok(_) => {}
            Err(error) if crate::target_fs::is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
    }
    if dry_run {
        // Use the executable update inventory so clean changes and metadata-only
        // repairs are visible without rendering contents into diagnostic output.
        for (path, wanted) in &analysis.updates {
            let local = analysis.local.get(path);
            let action = if wanted.is_none() {
                "remove"
            } else if local.is_none() {
                "create"
            } else if same(local, wanted.as_ref()) {
                "repair permissions"
            } else {
                "replace"
            };
            crate::ui::info(format!(
                "would {action}: {}",
                entry.target.join(path).display()
            ))?;
        }
        for path in analysis.conflicts.keys() {
            let template = analysis.template_source(
                previous
                    .as_ref()
                    .and_then(|record| record.generated.as_ref()),
                path,
            );
            crate::ui::info(format!(
                "would conflict: '{}' (source template '{}'); local file would be preserved",
                entry.target.join(path).display(),
                template
                    .map(|source| source.display().to_string())
                    .unwrap_or_default()
            ))?;
        }
        return Ok(outcome);
    }
    let (overlay, root) = source(entry)?;
    let (repo, managed) = git::discover_source(root)?;
    super::safety::SourceProtection::new(&repo)?.validate_paths(
        &entry.target,
        analysis
            .base
            .keys()
            .chain(analysis.desired.keys())
            .chain(analysis.updates.keys()),
        &entry.access,
    )?;
    let (tree, manifest) = generated_snapshot::write(&repo, &analysis.desired)?;
    let mut record = previous.unwrap_or_else(|| state::VirtualCheckoutRecord {
        overlay: overlay.to_owned(),
        managed_path: managed,
        created_at: super::now(),
        source_repository: repo.workdir().map(Path::to_path_buf),
        scope: entry.intent.checkout_scope(),
        ..Default::default()
    });
    record.scope = entry.intent.checkout_scope();
    let old = record.generated.clone();
    let identity = format!(
        "{}:{}:{}",
        repo.path().display(),
        root.display(),
        state::key_for(&entry.target)
    );
    let reference = old
        .as_ref()
        .map(|old| old.reference.clone())
        .unwrap_or(format!(
            "refs/over/generated/{}",
            Oid::hash_object(ObjectType::Blob, identity.as_bytes())?
        ));
    let parent = if let Some(old) = old.as_ref().filter(|old| !old.base.is_empty()) {
        repo.find_commit(Oid::from_str(&old.base)?)?
    } else {
        // Legacy source commits need a destination-relative baseline; initial checkouts use an empty one.
        let (baseline, baseline_manifest) = generated_snapshot::write(&repo, &analysis.base)?;
        let signature = git2::Signature::now("Over", "over@localhost")?;
        let id = repo.commit(
            None,
            &signature,
            &signature,
            &baseline_manifest.message("Generated checkout migration base")?,
            &baseline,
            &[],
        )?;
        repo.find_commit(id)?
    };
    let commit = if let Some(pending) = old.as_ref().and_then(|old| old.pending.as_ref()) {
        let pending = repo.find_commit(Oid::from_str(pending)?)?;
        let pending_manifest = match generated_snapshot::Manifest::read(&pending)? {
            Some(manifest) => manifest,
            None => super::generated_migration::legacy_plain_manifest(&repo, &pending, &record)?,
        };
        ensure!(
            pending.tree_id() == tree.id() && pending_manifest == manifest,
            "pending generated snapshot does not match its journal"
        );
        pending.id()
    } else if parent.tree_id() == tree.id()
        && generated_snapshot::Manifest::read(&parent)?.as_ref() == Some(&manifest)
    {
        parent.id()
    } else {
        let signature = git2::Signature::now("Over", "over@localhost")?;
        repo.commit(
            None,
            &signature,
            &signature,
            &manifest.message("Generated checkout desired state")?,
            &tree,
            &[&parent],
        )?
    };
    let mut generated = old.clone().unwrap_or_default();
    if generated.pending.is_none() {
        // Failed destinations still compare against the source used by their retained base.
        for path in analysis.conflicts.keys() {
            generated
                .source_revisions
                .entry(path.clone())
                .or_insert_with(|| generated.source_revision.clone());
        }
        generated
            .source_revisions
            .retain(|path, _| analysis.conflicts.contains_key(path));
        generated.source_revision = entry
            .intent
            .checkout_scope()
            .rendered
            .map(|rendered| rendered.source_revision.clone())
            .unwrap_or(repo.head()?.peel_to_commit()?.id().to_string());
    }
    let before_ref = ref_target(&repo, &reference)?;
    ensure!(
        before_ref == old.as_ref().and_then(|old| Oid::from_str(&old.base).ok())
            || (old
                .as_ref()
                .and_then(|old| old.pending.as_ref())
                .is_some_and(
                    |pending| before_ref.map(|id| id.to_string()).as_ref() == Some(pending)
                ))
            || (before_ref.is_none() && record.initializing),
        "generated snapshot ref differs from its recorded association; files preserved"
    );
    generated.reference = reference.clone();
    generated.base = parent.id().to_string();
    generated.pending = Some(commit.to_string());
    generated.permissions = analysis
        .desired
        .iter()
        .filter_map(|(path, file)| file.permissions.map(|mode| (path.clone(), mode.bits())))
        .collect();
    generated.pending_sources = Some(
        analysis
            .desired
            .iter()
            .filter(|(_, file)| file.generated)
            .map(|(path, file)| (path.clone(), file.source.clone()))
            .collect(),
    );
    // Preserve old provenance until the renamed/deleted physical leaves have been reconciled.
    generated
        .sources
        .extend(generated.pending_sources.iter().flat_map(|sources| {
            sources
                .iter()
                .map(|(path, source)| (path.clone(), source.clone()))
        }));
    generated.conflicts = analysis
        .conflicts
        .iter()
        .map(|(path, base)| {
            (
                path.clone(),
                if base.is_empty() {
                    parent.id().to_string()
                } else {
                    base.clone()
                },
            )
        })
        .collect();
    for (path, file) in &analysis.updates {
        generated
            .applied
            .insert(path.clone(), fingerprint(file.as_ref())?);
        generated
            .observed
            .insert(path.clone(), fingerprint(analysis.local.get(path))?);
    }
    record.generated = Some(generated.clone());
    record.initializing |= old.is_none();
    persist(entry, &record)?;
    ensure!(
        matches!(
            actual::inspect_with(&entry.target, &entry.access.fs)?,
            ActualState::Missing | ActualState::Directory
        ),
        "checkout root changed kind"
    );
    entry.access.fs.create_dir_all(&entry.target)?;
    let root_guard = PathGuard::capture_with(&entry.target, &entry.access.fs)?;
    for (path, replacement) in &analysis.updates {
        root_guard.verify()?;
        ensure!(
            same(local(entry, path)?.as_ref(), analysis.local.get(path)),
            "checkout file '{}' changed during sync; rerun",
            entry.target.join(path).display()
        );
        let target = entry.target.join(path);
        crate::materialize::disposal::validate_relative_ancestors_with(
            &entry.target,
            &target,
            &entry.access,
        )?;
        let access = leaf_access(entry, path)?;
        if let Some(file) = replacement {
            if file.generated {
                crate::desired::content::validate_destination(file, &target, &access.fs)?;
            }
            if file.mode == 0o120000 {
                if access.fs.metadata(&target, false).is_ok() {
                    access.fs.remove_file(&target)?;
                }
                if let Some(parent) = target.parent() {
                    access.fs.create_dir_all(parent)?;
                }
                access
                    .fs
                    .symlink(Path::new(std::str::from_utf8(&file.bytes)?), &target)?;
            } else {
                let existing = match access.fs.metadata(&target, false) {
                    Ok(metadata) => Some(metadata),
                    Err(error) if crate::target_fs::is_not_found(&error) => None,
                    Err(error) => return Err(error),
                };
                let default_mode = if file.generated {
                    0o600 | (file.mode as u32 & 0o111)
                } else {
                    file.mode as u32
                };
                if existing.as_ref().is_some_and(|metadata| metadata.is_file()) {
                    // Physical checkout leaves carry user metadata, unlike
                    // reconstructible artifacts: retain it and detach hard links safely.
                    #[cfg(unix)]
                    let mode = {
                        use std::os::unix::fs::PermissionsExt;
                        Some(file.permissions.unwrap_or_else(|| {
                            crate::overlays::FileMode::from_bits(
                                git::executable_permissions(
                                    existing
                                        .as_ref()
                                        .expect("existing regular file")
                                        .permissions(),
                                    file.mode,
                                )
                                .mode(),
                            )
                        }))
                    };
                    #[cfg(not(unix))]
                    let mode = file.permissions;
                    access.fs.write(&target, &file.bytes, mode)?;
                } else {
                    if existing.is_some() {
                        access.fs.remove_file(&target)?;
                    }
                    if let Some(parent) = target.parent() {
                        access.fs.create_dir_all(parent)?;
                    }
                    access.fs.write(
                        &target,
                        &file.bytes,
                        Some(
                            file.permissions
                                .unwrap_or(crate::overlays::FileMode::from_bits(default_mode)),
                        ),
                    )?;
                }
            }
        } else if access.fs.metadata(&target, false).is_ok() {
            access.fs.remove_file(&target)?;
        }
    }
    let expected = ref_target(&repo, &reference)?;
    ensure!(
        expected == before_ref || expected == Some(commit),
        "generated snapshot ref changed concurrently"
    );
    publish_snapshot_ref(&repo, &reference, commit, expected, || {
        std::thread::sleep(std::time::Duration::from_millis(50));
    })?;
    generated.base = commit.to_string();
    generated.pending = None;
    generated.pending_sources = None;
    generated.applied.clear();
    generated.observed.clear();
    generated
        .reverse_observed
        .retain(|path, _| analysis.conflicts.contains_key(path));
    // Once a conversion has published safely, ordinary files must regain their source workflows.
    generated.sources.retain(|path, _| {
        generated.conflicts.contains_key(path)
            || analysis.desired.get(path).is_none_or(|file| file.generated)
    });
    record.base_oid = generated.source_revision.clone();
    record.generated = Some(generated);
    record.initializing = false;
    persist(entry, &record)?;
    if analysis.conflicts.is_empty() {
        super::metadata::apply(entry)?;
    }
    Ok(outcome)
}

pub(crate) fn inspection(
    entry: &DesiredEntry,
    record: &state::VirtualCheckoutRecord,
    tracked: bool,
) -> Result<crate::status::virtual_checkout::Inspection> {
    let analysis = analyze(entry, Some(record))?;
    let hashes = |inventory: &RenderedInventory| {
        inventory
            .iter()
            .map(|(path, file)| Ok((path.clone(), oid(file)?)))
            .collect::<Result<BTreeMap<_, _>>>()
    };
    let mut base = hashes(&analysis.base)?;
    let mut desired = hashes(&analysis.desired)?;
    let mut local = hashes(&analysis.local)?;
    let (_, root) = source(entry)?;
    let (repo, managed) = git::discover_source(root)?;
    let scope = entry.intent.checkout_scope();
    let staged = git::staged_changes(&repo, &managed, &scope)?;
    let index = repo.index()?;
    let generated_paths = generated_paths(entry)?;
    let mut ordinary_paths: BTreeSet<_> = analysis
        .base
        .iter()
        .chain(analysis.desired.iter())
        .filter(|(_, file)| !file.generated)
        // A conversion still has a generated side: retain its physical comparison domain.
        .filter(|(path, _)| {
            !analysis.base.get(*path).is_some_and(|file| file.generated)
                && !analysis
                    .desired
                    .get(*path)
                    .is_some_and(|file| file.generated)
        })
        .map(|(path, _)| path.clone())
        .collect();
    // Index additions are inspection inputs only: reconciliation must not adopt
    // staged files as desired output or discover unrelated target siblings.
    for item in index.iter() {
        if let Some(path) = git::relative_index_path(&item.path, &managed)?
            && scope.owns_source(&path)
            && scope.owns_target(&path)
            && !git::is_overlay_metadata(&path)
            && !generated_paths.contains(&path)
            && git::is_supported_mode(item.mode)
        {
            ordinary_paths.insert(path);
        }
    }
    super::safety::SourceProtection::new(&repo)?.validate_paths(
        &entry.target,
        &ordinary_paths,
        &entry.access,
    )?;
    let scratch = crate::actions::git::diff::Scratch::new(&repo)?;
    let head = git::base_tree(&repo, None)?;
    let mut indexed = base.clone();
    for path in &ordinary_paths {
        let source_path = managed.join(path);
        if let Some(item) = index.get_path(&source_path, 0).filter(|item| {
            // Intent-to-add reserves a path, but its empty blob is not staged content.
            !IndexEntryExtendedFlag::from_bits_truncate(item.flags_extended).is_intent_to_add()
        }) {
            indexed.insert(path.clone(), (item.id, item.mode as i32));
        } else {
            indexed.remove(path);
        }
        for (inventory, entries) in [
            (&analysis.base, &mut base),
            (&analysis.desired, &mut desired),
        ] {
            if let Some(file) = inventory.get(path) {
                // Snapshot bytes are materialized blobs, not worktree inputs: a known
                // raw blob must bypass non-idempotent clean filters. Protected plaintext
                // still passes through the shared authenticated canonicalization path.
                let canonical = snapshot::canonical_entry(
                    &repo,
                    &scratch,
                    &source_path,
                    &file.bytes,
                    file.mode,
                    &[Some(oid(file)?)],
                )?;
                entries.insert(path.clone(), canonical);
            }
        }
        match self::local(entry, path)? {
            Some(file) => {
                // Ordinary checkout readers reject links: normalizing their mode
                // could hide a file-to-link replacement with identical blob bytes.
                ensure!(
                    file.mode != 0o120000,
                    "ordinary checkout destination '{}' is a symlink",
                    entry.target.join(path).display()
                );
                let head = head
                    .get_path(&source_path)
                    .ok()
                    .map(|item| (item.id(), item.filemode()));
                let canonical = snapshot::canonical_entry(
                    &repo,
                    &scratch,
                    &source_path,
                    &file.bytes,
                    file.mode,
                    &[indexed.get(path).copied(), base.get(path).copied(), head],
                )?;
                local.insert(path.clone(), canonical);
            }
            None => {
                local.remove(path);
            }
        }
    }
    let unstaged = git::entry_changes(&indexed, &local);
    // Reconciliation/journals use physical bytes. Only inspection may dismiss
    // an ordinary conflict or update made equivalent by Git's filters/filemode.
    let conflicts: Vec<_> = analysis
        .conflicts
        .keys()
        .filter(|path| {
            !ordinary_paths.contains(*path)
                || (local.get(*path) != base.get(*path) && local.get(*path) != desired.get(*path))
        })
        .cloned()
        .collect();
    let generated_updates = analysis
        .updates
        .keys()
        .any(|path| !ordinary_paths.contains(path));
    let status = if !conflicts.is_empty() {
        crate::status::Status::Conflict
    } else if base != desired
        || generated_updates
        || (!tracked && (!unstaged.is_empty() || !staged.is_empty()))
    {
        crate::status::Status::Modified
    } else {
        crate::status::Status::Applied
    };
    Ok(crate::status::virtual_checkout::Inspection {
        status,
        staged,
        unstaged,
        unmerged: conflicts,
    })
}

pub(crate) fn restore(entry: &DesiredEntry, paths: &[PathBuf], dry_run: bool) -> Result<()> {
    let mut record =
        state::record_for_entry(entry)?.context("generated checkout has no recorded snapshot")?;
    state::verify(entry, &record)?;
    let mut generated = record
        .generated
        .clone()
        .context("checkout has no generated snapshot")?;
    ensure!(
        generated.pending.is_none() && generated.reverse_pending.is_none(),
        "checkout publication was interrupted; finish sync or reverse commit before restoring"
    );
    let (_, source) = source(entry)?;
    let (repo, _) = git::discover_source(source)?;
    let inventory = tree_inventory(&repo, &generated.base, &record)?;
    let mut prepared = Vec::new();
    for path in paths {
        ensure!(
            inventory.contains_key(path) || generated.sources.contains_key(path),
            "'{}' is not a recorded generated checkout destination",
            entry.target.join(path).display()
        );
        let current = local(entry, path)?;
        let guard =
            PathGuard::capture_with(&entry.target.join(path), &leaf_access(entry, path)?.fs)?;
        prepared.push((path, current, guard));
    }
    if dry_run {
        return Ok(());
    }
    for (path, before, guard) in prepared {
        guard.verify()?;
        ensure!(
            same(local(entry, path)?.as_ref(), before.as_ref()),
            "restore target changed while preparing"
        );
        let target = entry.target.join(path);
        if let Some(file) = inventory.get(path) {
            if file.generated {
                crate::desired::content::validate_destination(
                    file,
                    &target,
                    &leaf_access(entry, path)?.fs,
                )?;
            }
            ensure!(
                file.mode != 0o120000,
                "restore generated checkout symlinks through their source workflow"
            );
            leaf_access(entry, path)?.fs.write(
                &target,
                &file.bytes,
                Some(crate::overlays::FileMode::from_bits(
                    generated
                        .permissions
                        .get(path)
                        .copied()
                        .unwrap_or(if file.generated {
                            0o600 | (file.mode as u32 & 0o111)
                        } else {
                            file.mode as u32
                        }),
                )),
            )?;
        } else if before.is_some() {
            leaf_access(entry, path)?.fs.remove_file(&target)?;
        }
        generated.conflicts.remove(path);
        generated.reverse_observed.remove(path);
        if entry
            .intent
            .checkout_scope()
            .rendered
            .as_ref()
            .and_then(|inventory| inventory.get(path))
            .is_some_and(|file| !file.generated)
        {
            generated.sources.remove(path);
        }
    }
    record.generated = Some(generated);
    persist(entry, &record)
}

pub(crate) fn remove(entry: &DesiredEntry) -> Result<()> {
    let record =
        state::record_for_entry(entry)?.context("generated checkout association disappeared")?;
    state::verify(entry, &record)?;
    let generated = record
        .generated
        .as_ref()
        .context("checkout has no generated snapshot")?;
    ensure!(
        generated.pending.is_none()
            && generated.reverse_pending.is_none()
            && generated.conflicts.is_empty(),
        "generated checkout has an unfinished sync, reverse commit, or unresolved conflicts"
    );
    let analysis = analyze(entry, Some(&record))?;
    ensure!(
        analysis.base.len() == analysis.local.len()
            && analysis
                .base
                .iter()
                .all(|(path, base)| same(Some(base), analysis.local.get(path))),
        "generated checkout has local edits; files preserved"
    );
    for (path, base) in &analysis.base {
        ensure!(
            same(local(entry, path)?.as_ref(), Some(base)),
            "checkout changed during removal"
        );
        leaf_access(entry, path)?
            .fs
            .remove_file(&entry.target.join(path))?;
    }
    let (_, source) = source(entry)?;
    let (repo, _) = git::discover_source(source)?;
    let mut reference = repo.find_reference(&generated.reference)?;
    ensure!(
        reference.target().map(|oid| oid.to_string()).as_deref() == Some(&generated.base),
        "generated ref changed during removal"
    );
    reference.delete()?;
    super::generated_migration::release(&repo, &record)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use git2::{ErrorCode, Repository};
    use rstest::rstest;
    use tempfile::TempDir;

    use super::{SNAPSHOT_REF_LOCK_RETRIES, publish_snapshot_ref, ref_target};

    const REFERENCE: &str = "refs/over/generated/test";

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn snapshot_publication_succeeds_when_a_temporary_lock_is_released(#[case] existing: bool) {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        let before = repo.blob(b"before").unwrap();
        let after = repo.blob(b"after").unwrap();
        let expected = existing.then_some(before);
        if existing {
            repo.reference(REFERENCE, before, false, "test").unwrap();
        }
        let lock = repo.path().join(format!("{REFERENCE}.lock"));
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        fs::write(&lock, b"test-owned lock").unwrap();
        let mut waits = 0;
        publish_snapshot_ref(&repo, REFERENCE, after, expected, || {
            waits += 1;
            // Release only after publication has actually encountered the lock, without a timing race.
            fs::remove_file(&lock).unwrap();
        })
        .unwrap();
        assert_eq!(waits, 1);
        assert_eq!(ref_target(&repo, REFERENCE).unwrap(), Some(after));
    }

    #[test]
    fn snapshot_publication_leaves_a_persistent_lock_and_reference_untouched() {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        let before = repo.blob(b"before").unwrap();
        let after = repo.blob(b"after").unwrap();
        repo.reference(REFERENCE, before, false, "test").unwrap();
        let lock = repo.path().join(format!("{REFERENCE}.lock"));
        fs::write(&lock, b"test-owned lock").unwrap();
        let mut waits = 0;
        let error =
            publish_snapshot_ref(&repo, REFERENCE, after, Some(before), || waits += 1).unwrap_err();
        assert_eq!(waits, SNAPSHOT_REF_LOCK_RETRIES);
        assert_eq!(
            error.downcast_ref::<git2::Error>().unwrap().code(),
            ErrorCode::Locked
        );
        assert!(error.to_string().contains(REFERENCE));
        assert_eq!(fs::read(lock).unwrap(), b"test-owned lock");
        assert_eq!(ref_target(&repo, REFERENCE).unwrap(), Some(before));
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn snapshot_publication_does_not_overwrite_a_concurrent_writer_after_waiting(
        #[case] existing: bool,
    ) {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        let before = repo.blob(b"before").unwrap();
        let after = repo.blob(b"after").unwrap();
        let concurrent = repo.blob(b"concurrent").unwrap();
        let expected = existing.then_some(before);
        if existing {
            repo.reference(REFERENCE, before, false, "test").unwrap();
        }
        let lock = repo.path().join(format!("{REFERENCE}.lock"));
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        fs::write(&lock, b"test-owned lock").unwrap();
        let mut waits = 0;
        let error = publish_snapshot_ref(&repo, REFERENCE, after, expected, || {
            waits += 1;
            fs::remove_file(&lock).unwrap();
            repo.reference(REFERENCE, concurrent, true, "concurrent writer")
                .unwrap();
        })
        .unwrap_err();
        assert_eq!(waits, 1);
        assert_eq!(
            error.downcast_ref::<git2::Error>().unwrap().code(),
            if existing {
                ErrorCode::Modified
            } else {
                ErrorCode::Exists
            }
        );
        assert_eq!(ref_target(&repo, REFERENCE).unwrap(), Some(concurrent));
    }

    #[test]
    fn snapshot_publication_returns_non_lock_errors_without_waiting() {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        let commit = repo.blob(b"snapshot").unwrap();
        let error = publish_snapshot_ref(&repo, "invalid ref", commit, None, || {
            panic!("non-lock errors must not be retried");
        })
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<git2::Error>().unwrap().code(),
            ErrorCode::InvalidSpec
        );
    }
}
