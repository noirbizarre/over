//! Generated desired snapshots and pure base/local/desired reconciliation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use git2::{ObjectType, Oid, Repository};
use serde::{Deserialize, Serialize};

use crate::desired::content::{RenderedFile, RenderedInventory};
use crate::desired::{DesiredEntry, Provenance};
use crate::materialize::disposal::{PathGuard, validate_relative_ancestors};
use crate::plan::{ActualState, Operation, actual};

use super::{git, state};

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
    match actual::inspect(&entry.target)? {
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

fn same(left: Option<&RenderedFile>, right: Option<&RenderedFile>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.bytes == right.bytes && left.mode == right.mode,
        _ => false,
    }
}

fn tree_inventory(
    repo: &Repository,
    commit: &str,
    sources: &BTreeMap<PathBuf, PathBuf>,
) -> Result<RenderedInventory> {
    let tree = repo.find_commit(Oid::from_str(commit)?)?.tree()?;
    git::tracked_entries(&tree)?
        .into_iter()
        .map(|(path, (id, mode))| {
            let bytes = repo.find_blob(id)?.content().to_vec().into();
            let file = RenderedFile {
                source: sources.get(&path).cloned().unwrap_or_default(),
                bytes,
                mode,
                identity: id.to_string(),
                generated: sources.contains_key(&path),
                permissions: None,
            };
            Ok((path, file))
        })
        .collect()
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
            let file = RenderedFile {
                source: root.join(&path),
                bytes: repo.find_blob(id)?.content().to_vec().into(),
                mode,
                identity: id.to_string(),
                generated: false,
                permissions: None,
            };
            Ok((path, file))
        })
        .collect()
}

fn local(root: &Path, path: &Path) -> Result<Option<RenderedFile>> {
    let target = root.join(path);
    validate_relative_ancestors(root, &target)?;
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() || metadata.is_symlink(),
        "checkout destination '{}' is obstructed",
        target.display()
    );
    let bytes = if metadata.is_symlink() {
        fs::read_link(&target)?
            .to_str()
            .context("symlink is not valid UTF-8")?
            .as_bytes()
            .to_vec()
    } else {
        fs::read(&target)?
    };
    let mode = if metadata.is_symlink() {
        0o120000
    } else {
        i32::from(git::on_disk_filemode(&target)?)
    };
    Ok(Some(RenderedFile {
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
            generated
                .pending_sources
                .as_ref()
                .unwrap_or(&generated.sources),
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
        tree_inventory(&repo, &generated.base, &generated.sources)?
    } else if let Some(record) =
        record.filter(|record| !record.initializing && !record.base_oid.is_empty())
    {
        // Legacy baselines are source trees, never today's generated output.
        let tree = git::base_tree(&repo, Some(&record.base_oid))?;
        git::scoped_entries(&repo, &tree, &managed, &record.scope)?
            .into_iter()
            .map(|(path, (id, mode))| {
                Ok((
                    path.clone(),
                    RenderedFile {
                        source: root.join(path),
                        bytes: repo.find_blob(id)?.content().to_vec().into(),
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
            if let Some(observed) = tree_inventory(&repo, commit, &generated.sources)?.remove(path)
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
    for path in paths {
        let current = local(&entry.target, &path)?;
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
                if fs::metadata(entry.target.join(&path))?.permissions().mode() & 0o777
                    != permissions.bits()
                {
                    analysis.updates.insert(path.clone(), Some(wanted.clone()));
                }
            }
            continue;
        }
        let previous_conflict = generated.and_then(|generated| generated.conflicts.get(&path));
        if let Some(commit) = previous_conflict.filter(|commit| !commit.is_empty()) {
            base = tree_inventory(&repo, commit, &BTreeMap::new())?.remove(&path);
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

pub(crate) fn reconcile(entry: &DesiredEntry, dry_run: bool) -> Result<()> {
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
    if dry_run {
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
        return Ok(());
    }
    let (overlay, root) = source(entry)?;
    let (repo, managed) = git::discover_source(root)?;
    let entries = analysis
        .desired
        .iter()
        .map(|(path, file)| Ok((path.clone(), (repo.blob(&file.bytes)?, file.mode))))
        .collect::<Result<_>>()?;
    let tree = git::build_content_tree(&repo, &entries)?;
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
        let baseline_entries = analysis
            .base
            .iter()
            .map(|(path, file)| Ok((path.clone(), (repo.blob(&file.bytes)?, file.mode))))
            .collect::<Result<_>>()?;
        let baseline = git::build_content_tree(&repo, &baseline_entries)?;
        let signature = git2::Signature::now("Over", "over@localhost")?;
        let id = repo.commit(
            None,
            &signature,
            &signature,
            "Generated checkout migration base",
            &baseline,
            &[],
        )?;
        repo.find_commit(id)?
    };
    let commit = if let Some(pending) = old.as_ref().and_then(|old| old.pending.as_ref()) {
        let pending = repo.find_commit(Oid::from_str(pending)?)?;
        ensure!(
            pending.tree_id() == tree.id(),
            "pending generated snapshot does not match its journal"
        );
        pending.id()
    } else if parent.tree_id() == tree.id() {
        parent.id()
    } else {
        let signature = git2::Signature::now("Over", "over@localhost")?;
        repo.commit(
            None,
            &signature,
            &signature,
            "Generated checkout desired state",
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
            actual::inspect(&entry.target)?,
            ActualState::Missing | ActualState::Directory
        ),
        "checkout root changed kind"
    );
    fs::create_dir_all(&entry.target)?;
    let root_guard = PathGuard::capture(&entry.target)?;
    for (path, replacement) in &analysis.updates {
        root_guard.verify()?;
        ensure!(
            same(
                local(&entry.target, path)?.as_ref(),
                analysis.local.get(path)
            ),
            "checkout file '{}' changed during sync; rerun",
            entry.target.join(path).display()
        );
        let target = entry.target.join(path);
        validate_relative_ancestors(&entry.target, &target)?;
        if let Some(file) = replacement {
            if file.generated {
                crate::desired::content::validate_destination(file, &target)?;
            }
            if file.mode == 0o120000 {
                if target.symlink_metadata().is_ok() {
                    fs::remove_file(&target)?;
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(std::str::from_utf8(&file.bytes)?, &target)?;
                #[cfg(windows)]
                std::os::windows::fs::symlink_file(std::str::from_utf8(&file.bytes)?, &target)?;
            } else {
                // Generated output may contain private values; preserve existing modes on local merges.
                let permissions = fs::symlink_metadata(&target)
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .map(|metadata| metadata.permissions());
                let default_mode = if file.generated {
                    0o600 | (file.mode as u32 & 0o111)
                } else {
                    file.mode as u32
                };
                crate::materialize::rendered::atomic_write(
                    &target,
                    &file.bytes,
                    file.permissions
                        .map_or(default_mode, |permissions| permissions.bits()),
                )?;
                if file.permissions.is_none()
                    && let Some(permissions) = permissions
                {
                    #[cfg(unix)]
                    let permissions = {
                        use std::os::unix::fs::PermissionsExt;
                        let mut permissions = permissions;
                        permissions
                            .set_mode((permissions.mode() & !0o111) | (file.mode as u32 & 0o111));
                        permissions
                    };
                    fs::set_permissions(&target, permissions)?;
                }
            }
        } else if target.symlink_metadata().is_ok() {
            fs::remove_file(&target)?;
        }
    }
    let expected = ref_target(&repo, &reference)?;
    ensure!(
        expected == before_ref || expected == Some(commit),
        "generated snapshot ref changed concurrently"
    );
    if let Some(expected) = expected {
        repo.reference_matching(
            &reference,
            commit,
            true,
            expected,
            "over generated snapshot",
        )?;
    } else {
        repo.reference(&reference, commit, false, "over generated snapshot")?;
    }
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
    if !analysis.conflicts.is_empty() {
        let paths = analysis
            .conflicts
            .keys()
            .map(|path| {
                let template = analysis.template_source(record.generated.as_ref(), path);
                format!(
                    "'{}' (source template '{}')",
                    entry.target.join(path).display(),
                    template
                        .map(|source| source.display().to_string())
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "generated checkout conflicts; local files preserved: {paths}; edit the source template or restore the desired output"
        )
    }
    Ok(())
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
    let base = hashes(&analysis.base)?;
    let desired = hashes(&analysis.desired)?;
    let local = hashes(&analysis.local)?;
    let (_, root) = source(entry)?;
    let (repo, managed) = git::discover_source(root)?;
    let scope = entry.intent.checkout_scope();
    let staged = git::staged_changes(&repo, &managed, &scope)?;
    let mut indexed = base.clone();
    let index = repo.index()?;
    for (path, _) in analysis
        .base
        .iter()
        .chain(analysis.desired.iter())
        .filter(|(_, file)| !file.generated)
    {
        let source_path = managed.join(path);
        if let Some(item) = index.get_path(&source_path, 0) {
            let bytes = crate::crypt::policy::materialized_blob(&repo, &source_path, item.id)?;
            indexed.insert(
                path.clone(),
                (
                    Oid::hash_object(ObjectType::Blob, &bytes)?,
                    item.mode as i32,
                ),
            );
        } else {
            indexed.remove(path);
        }
    }
    let unstaged = git::entry_changes(&indexed, &local);
    let status = if !analysis.conflicts.is_empty() {
        crate::status::Status::Conflict
    } else if base != desired
        || !analysis.updates.is_empty()
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
        unmerged: analysis.conflicts.into_keys().collect(),
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
    let inventory = tree_inventory(&repo, &generated.base, &generated.sources)?;
    let mut prepared = Vec::new();
    for path in paths {
        ensure!(
            inventory.contains_key(path) || generated.sources.contains_key(path),
            "'{}' is not a recorded generated checkout destination",
            entry.target.join(path).display()
        );
        let current = local(&entry.target, path)?;
        let guard = PathGuard::capture(&entry.target.join(path))?;
        prepared.push((path, current, guard));
    }
    if dry_run {
        return Ok(());
    }
    for (path, before, guard) in prepared {
        guard.verify()?;
        ensure!(
            same(local(&entry.target, path)?.as_ref(), before.as_ref()),
            "restore target changed while preparing"
        );
        let target = entry.target.join(path);
        if let Some(file) = inventory.get(path) {
            if file.generated {
                crate::desired::content::validate_destination(file, &target)?;
            }
            ensure!(
                file.mode != 0o120000,
                "restore generated checkout symlinks through their source workflow"
            );
            crate::materialize::rendered::atomic_write(
                &target,
                &file.bytes,
                generated
                    .permissions
                    .get(path)
                    .copied()
                    .unwrap_or(if file.generated {
                        0o600 | (file.mode as u32 & 0o111)
                    } else {
                        file.mode as u32
                    }),
            )?;
        } else if before.is_some() {
            fs::remove_file(target)?;
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
            same(local(&entry.target, path)?.as_ref(), Some(base)),
            "checkout changed during removal"
        );
        fs::remove_file(entry.target.join(path))?;
    }
    let (_, source) = source(entry)?;
    let (repo, _) = git::discover_source(source)?;
    let mut reference = repo.find_reference(&generated.reference)?;
    ensure!(
        reference.target().map(|oid| oid.to_string()).as_deref() == Some(&generated.base),
        "generated ref changed during removal"
    );
    reference.delete()?;
    Ok(())
}
