//! Recoverable generated-history migration; retiring refs is not object erasure.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use anyhow::{Context as _, Result, ensure};
use git2::{Oid, Repository};
use serde::{Deserialize, Serialize};

use crate::crypt::{access, atomic_write, format, policy, protection::Protection};

use super::generated_snapshot::{Entry, Manifest};
use super::{git, state};

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    version: u32,
    /// Retain IDs after their refs disappear: unreachable plaintext is still available by OID.
    leaked: BTreeSet<String>,
    #[serde(default)]
    audited: BTreeSet<String>,
    #[serde(default)]
    cleanup: BTreeSet<String>,
    pending: Option<Publication>,
}

#[derive(Serialize, Deserialize)]
struct Publication {
    administrative: std::path::PathBuf,
    refs: BTreeMap<String, (String, String)>,
    records: BTreeMap<String, (state::VirtualCheckoutRecord, state::VirtualCheckoutRecord)>,
    replacements: BTreeSet<String>,
}

fn ledger_path(repo: &Repository) -> std::path::PathBuf {
    access::private_dir(repo).join("generated-snapshots.json")
}

fn load(repo: &Repository) -> Result<Ledger> {
    let path = ledger_path(repo);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => ensure!(
            metadata.is_file() && !metadata.is_symlink(),
            "unsafe generated snapshot ledger"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Ledger {
                version: 1,
                ..Default::default()
            });
        }
        Err(error) => return Err(error.into()),
    }
    let ledger: Ledger = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        ledger.version == 1,
        "unsupported generated snapshot ledger version"
    );
    Ok(ledger)
}

fn save(repo: &Repository, ledger: &Ledger) -> Result<()> {
    atomic_write(&ledger_path(repo), &serde_json::to_vec(ledger)?)
}

pub(crate) fn pending(repo: &Repository) -> Result<bool> {
    Ok(load(repo)?.pending.is_some())
}

pub(crate) fn ensure_ready(repo: &Repository) -> Result<()> {
    ensure!(
        !pending(repo)?,
        "generated snapshot migration interrupted; rerun over unlock or over crypt recover before modifying checkouts"
    );
    Ok(())
}

fn state_directory_obstructed() -> Result<bool> {
    let directory = crate::xdg::XdgDirs::new()?.state_dir();
    // Windows and Unix report different errors beneath a file; inspect parents before descending.
    for ancestor in directory.ancestors().collect::<Vec<_>>().into_iter().rev() {
        match fs::metadata(ancestor) {
            Ok(metadata) if !metadata.is_dir() => return Ok(true),
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn associations(repo: &Repository) -> Result<BTreeMap<String, state::VirtualCheckoutRecord>> {
    if state_directory_obstructed()? {
        return Ok(BTreeMap::new());
    }
    let path = crate::xdg::XdgDirs::new()?
        .state_dir()
        .join("virtual_checkout.toml");
    match fs::symlink_metadata(path) {
        Ok(_) => (),
        // Crypto-only repositories must not require a usable XDG checkout state directory.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(BTreeMap::new());
        }
        Err(error) => return Err(error.into()),
    }
    let administrative = fs::canonicalize(repo.commondir())?;
    state::state_file()?
        .inspect_blocking()?
        .checkouts
        .into_iter()
        .filter_map(|(key, record)| {
            record.generated.as_ref()?;
            let source = record.source_repository.as_ref()?;
            match Repository::open(source) {
                Ok(source) => Some((|| {
                    Ok((fs::canonicalize(source.commondir())? == administrative)
                        .then_some((key, record)))
                })()),
                // A missing unrelated association is not authority for any ref in this repository.
                Err(error) if error.code() == git2::ErrorCode::NotFound => None,
                Err(error) => Some(Err(error.into())),
            }
        })
        .collect::<Result<Vec<_>>>()
        .map(|records| records.into_iter().flatten().collect())
}

pub(crate) fn ownership_guard(repo: &Repository) -> Result<Option<fs::File>> {
    let has_refs = repo
        .references_glob("refs/over/generated/*")?
        .next()
        .transpose()?
        .is_some();
    let relevant = has_refs || ledger_path(repo).try_exists()? || !associations(repo)?.is_empty();
    if !relevant && state_directory_obstructed()? {
        // No association can exist here, so crypto-only worktrees need no checkout mutation lock.
        return Ok(None);
    }
    match state::raw_mutation_guard() {
        Ok(guard) => Ok(Some(guard)),
        // Such a directory cannot hold or publish a checkout association in the first place.
        Err(error)
            if !relevant
                && error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotADirectory) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn pointers(record: &state::VirtualCheckoutRecord) -> Vec<&str> {
    let Some(generated) = &record.generated else {
        return Vec::new();
    };
    let mut roots = vec![generated.base.as_str()];
    roots.extend(generated.pending.as_deref());
    roots.extend(generated.conflicts.values().map(String::as_str));
    roots.extend(generated.reverse_observed.values().map(String::as_str));
    if let Some(pending) = &generated.reverse_pending {
        roots.push(&pending.generated_commit);
        roots.extend(pending.observed.values().map(String::as_str));
    }
    roots.into_iter().filter(|root| !root.is_empty()).collect()
}

fn retained_prefix(record: &state::VirtualCheckoutRecord) -> Result<String> {
    let generated = record
        .generated
        .as_ref()
        .context("missing generated association")?;
    let suffix = generated
        .reference
        .strip_prefix("refs/over/generated/")
        .context("invalid generated association ref")?;
    ensure!(!suffix.is_empty(), "invalid generated association ref");
    Ok(format!("refs/over/generated-retained/{suffix}/"))
}

fn pin_record(repo: &Repository, record: &state::VirtualCheckoutRecord) -> Result<()> {
    let prefix = retained_prefix(record)?;
    let wanted: BTreeSet<_> = pointers(record).into_iter().collect();
    // XDG-only pending/conflict roots must survive the explicit GC needed for legacy cleanup.
    for id in &wanted {
        repo.reference(
            &format!("{prefix}{id}"),
            Oid::from_str(id)?,
            true,
            "over retain operational generated snapshot",
        )?;
    }
    Ok(())
}

fn retain_record(repo: &Repository, record: &state::VirtualCheckoutRecord) -> Result<()> {
    pin_record(repo, record)?;
    let prefix = retained_prefix(record)?;
    let wanted: BTreeSet<_> = pointers(record).into_iter().collect();
    for reference in repo.references_glob(&format!("{prefix}*"))? {
        let mut reference = reference?;
        let name = reference.name().context("invalid retained generated ref")?;
        if !wanted.contains(
            name.strip_prefix(&prefix)
                .context("invalid retained generated ref")?,
        ) {
            reference.delete()?;
        }
    }
    Ok(())
}

pub(crate) fn release(repo: &Repository, record: &state::VirtualCheckoutRecord) -> Result<()> {
    for reference in repo.references_glob(&format!("{}*", retained_prefix(record)?))? {
        reference?.delete()?;
    }
    Ok(())
}

fn roots(
    repo: &Repository,
    records: &BTreeMap<String, state::VirtualCheckoutRecord>,
) -> Result<(BTreeMap<String, Oid>, BTreeSet<Oid>)> {
    let mut refs = BTreeMap::new();
    for reference in repo.references()? {
        let reference = reference?;
        if !reference.name_bytes().starts_with(b"refs/over/generated/")
            && !reference
                .name_bytes()
                .starts_with(b"refs/over/generated-retained/")
        {
            continue;
        }
        refs.insert(
            reference
                .name()
                .context("generated ref is not UTF-8")?
                .to_owned(),
            reference.target().context("generated ref must be direct")?,
        );
    }
    let mut roots: BTreeSet<_> = refs.values().copied().collect();
    for record in records.values() {
        for pointer in pointers(record) {
            roots.insert(Oid::from_str(pointer)?);
        }
    }
    Ok((refs, roots))
}

fn history(repo: &Repository, roots: &BTreeSet<Oid>) -> Result<Vec<Oid>> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    let mut walk = repo.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
    for root in roots {
        walk.push(*root)?;
    }
    walk.map(|id| id.map_err(Into::into)).collect()
}

fn audit_orphans(repo: &Repository, reachable: &BTreeSet<Oid>, ledger: &Ledger) -> Result<()> {
    let odb = repo.odb()?;
    let mut commits = Vec::new();
    let mut failure = None;
    // Failed publication can leave commits without refs or XDG pointers. They still expose blobs.
    odb.foreach(&mut |id: &Oid| {
        match odb.read_header(*id) {
            Ok((_, git2::ObjectType::Commit)) => commits.push(*id),
            Ok(_) => (),
            Err(error) => {
                failure = Some(error);
                return false;
            }
        }
        true
    })?;
    if let Some(error) = failure {
        return Err(error.into());
    }
    for id in commits {
        if reachable.contains(&id) || ledger.audited.contains(&id.to_string()) {
            continue;
        }
        let commit = repo.find_commit(id)?;
        let over = commit
            .committer()
            .email()
            .is_ok_and(|email| email == "over@localhost");
        if !over {
            continue;
        }
        let title = commit.summary()?.unwrap_or_default();
        if !(title.starts_with("Generated checkout") || title.starts_with("Reverse-patch")) {
            continue;
        }
        match Manifest::read(&commit)? {
            Some(manifest) => {
                let tree = commit.tree()?;
                for (path, entry) in manifest.entries {
                    if entry.protection == Protection::Encrypted {
                        format::inspect(repo.find_blob(tree.get_path(&path)?.id())?.content())?;
                    }
                }
            }
            None => ensure!(
                git::tracked_entries(&commit.tree()?)?.is_empty(),
                "orphan legacy generated snapshot {id} remains in the object database without trustworthy provenance; restore its association/history or explicitly clean up its Git objects before retrying"
            ),
        }
    }
    Ok(())
}

fn remap(record: &mut state::VirtualCheckoutRecord, replacements: &BTreeMap<String, String>) {
    fn replace(value: &mut String, replacements: &BTreeMap<String, String>) {
        if let Some(new) = replacements.get(value) {
            *value = new.clone();
        }
    }
    if let Some(generated) = &mut record.generated {
        replace(&mut generated.base, replacements);
        if let Some(pending) = &mut generated.pending {
            replace(pending, replacements);
        }
        for value in generated
            .conflicts
            .values_mut()
            .chain(generated.reverse_observed.values_mut())
        {
            replace(value, replacements);
        }
        if let Some(pending) = &mut generated.reverse_pending {
            replace(&mut pending.generated_commit, replacements);
            for value in pending.observed.values_mut() {
                replace(value, replacements);
            }
        }
    }
}

/// Called with checkout mutation ownership and crypto-exclusive ownership, before other transitions.
pub(crate) fn recover(repo: &Repository) -> Result<bool> {
    let mut ledger = load(repo)?;
    let Some(pending) = ledger.pending.as_ref() else {
        let cleaning = !ledger.cleanup.is_empty();
        if cleaning {
            cleanup_completed(repo, &mut ledger)?;
        }
        return Ok(cleaning);
    };
    ensure!(
        pending.administrative == fs::canonicalize(repo.commondir())?,
        "generated migration belongs to another repository"
    );
    // Validate both durability domains before completing a partially published migration.
    for (name, (old, new)) in &pending.refs {
        let current = repo.refname_to_id(name)?.to_string();
        ensure!(
            &current == old || &current == new,
            "generated ref changed during migration; preserve the ledger and repair before retrying"
        );
    }
    state::state_file()?.update_blocking(|snapshot| {
        for (key, (old, new)) in &pending.records {
            let current = snapshot.checkouts.get(key);
            ensure!(current == Some(old) || current == Some(new), "checkout association changed during generated migration; preserve the ledger and repair before retrying");
        }
        // A durable intent makes either publication order recoverable; never overwrite a third value.
        for (name, (old, new)) in &pending.refs {
            let current = repo.refname_to_id(name)?.to_string();
            ensure!(&current == old || &current == new, "generated ref changed during migration publication; preserve the ledger and repair before retrying");
            if current == *old {
                repo.reference_matching(name, Oid::from_str(new)?, true, Oid::from_str(old)?, "over protect generated history")?;
            }
        }
        for (key, (_, new)) in &pending.records { snapshot.checkouts.insert(key.clone(), new.clone()); }
        Ok(())
    })?;
    for (_, new) in pending.records.values() {
        pin_record(repo, new)?;
    }
    // Retained-ref names in the journal must survive until its completion is durable.
    let completed = ledger
        .pending
        .take()
        .context("missing generated migration intent")?;
    ledger.cleanup.extend(completed.replacements);
    save(repo, &ledger)?;
    cleanup_completed(repo, &mut ledger)?;
    Ok(true)
}

fn cleanup_completed(repo: &Repository, ledger: &mut Ledger) -> Result<()> {
    // Once publication is durable, cleanup follows current associations rather than stale journal names.
    for record in associations(repo)?.values() {
        retain_record(repo, record)?;
    }
    for id in &ledger.cleanup {
        Oid::from_str(id)?;
        let name = format!("refs/over/crypt-migration/{id}");
        match repo.find_reference(&name) {
            Ok(mut reference) => reference.delete()?,
            Err(error) if error.code() == git2::ErrorCode::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    ledger.cleanup.clear();
    save(repo, ledger)
}

fn owning_repository(
    repo: &Repository,
    record: &state::VirtualCheckoutRecord,
) -> Result<Repository> {
    let owner = Repository::open(record.source_repository.as_deref().unwrap_or(repo.path()))?;
    ensure!(
        fs::canonicalize(owner.commondir())? == fs::canonicalize(repo.commondir())?,
        "legacy association belongs to another repository"
    );
    Ok(owner)
}

fn source_path(
    repo: &Repository,
    record: &state::VirtualCheckoutRecord,
    path: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let generated = record
        .generated
        .as_ref()
        .context("missing generated provenance")?;
    let source = match generated.sources.get(path) {
        Some(source) => {
            let root = crate::overlays::mapping::physical(
                repo.workdir().context("source has no worktree")?,
            )?;
            crate::overlays::mapping::location(source)?
                .strip_prefix(root)
                .context("legacy source is outside its owning worktree")?
                .to_path_buf()
        }
        None => record.managed_path.join(path),
    };
    ensure!(
        super::generated_snapshot::relative(&source),
        "unsafe legacy source provenance"
    );
    Ok(source)
}

fn source_history(repo: &Repository, record: &state::VirtualCheckoutRecord) -> Result<Vec<Oid>> {
    let generated = record
        .generated
        .as_ref()
        .context("missing generated provenance")?;
    let mut roots = BTreeSet::new();
    roots.insert(repo.head()?.peel_to_commit()?.id());
    for revision in
        std::iter::once(&generated.source_revision).chain(generated.source_revisions.values())
    {
        if !revision.is_empty() {
            roots.insert(Oid::from_str(revision)?);
        }
    }
    history(repo, &roots)
}

fn protected_source_paths(
    repo: &Repository,
    record: &state::VirtualCheckoutRecord,
) -> Result<BTreeSet<std::path::PathBuf>> {
    let mut protected = BTreeSet::new();
    for revision in source_history(repo, record)? {
        let tree = repo.find_commit(revision)?.tree()?;
        let leaves = git::tracked_entries(&tree)?;
        let paths = leaves
            .keys()
            .filter(|path| path.starts_with(&record.managed_path))
            .cloned()
            .collect::<Vec<_>>();
        let attributes = policy::attribute_map(repo, &paths, Some(&revision.to_string()))?;
        for path in paths {
            if attributes.get(&path) == Some(&true)
                || format::recognized(repo.find_blob(leaves[&path].0)?.content())
            {
                protected.insert(path);
            }
        }
    }
    Ok(protected)
}

/// Key-independent comparisons require an unprotected source scope, but do not certify old history.
pub(crate) fn legacy_plain_manifest(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    record: &state::VirtualCheckoutRecord,
) -> Result<Manifest> {
    let owner = owning_repository(repo, record)?;
    ensure!(
        protected_source_paths(&owner, record)?.is_empty(),
        "legacy protected or ambiguous generated snapshots need provenance migration; run over unlock before reconciliation"
    );
    let generated = record
        .generated
        .as_ref()
        .context("missing generated provenance")?;
    let entries = git::tracked_entries(&commit.tree()?)?.into_keys().map(|path| {
        let source = source_path(&owner, record, &path)?;
        ensure!(!policy::protected(&owner, &source)?, "legacy protected generated snapshot needs migration; run over unlock before reconciliation");
        let entry = Entry {
            source,
            source_revision: generated.source_revisions.get(&path).or(Some(&generated.source_revision)).filter(|revision| !revision.is_empty()).cloned(),
            protection: Protection::Plain,
            generated: generated.sources.contains_key(&path),
            permissions: generated.permissions.get(&path).copied(),
        };
        Ok((path, entry))
    }).collect::<Result<_>>()?;
    Ok(Manifest::new(entries))
}

/// Legacy source maps are incomplete; require historical evidence instead of today's attributes.
fn legacy_manifest(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    record: &state::VirtualCheckoutRecord,
    historical_protected: &BTreeSet<std::path::PathBuf>,
) -> Result<Manifest> {
    let owner = owning_repository(repo, record)?;
    let generated = record
        .generated
        .as_ref()
        .context("missing generated provenance")?;
    let leaves = git::tracked_entries(&commit.tree()?)?;
    let mut entries = BTreeMap::new();
    for (path, (blob, _)) in leaves {
        let source = source_path(&owner, record, &path)?;
        let transformed = generated.sources.contains_key(&path);
        let bytes = repo.find_blob(blob)?;
        ensure!(
            !generated.source_revision.is_empty(),
            "legacy snapshot has no historical source revision"
        );
        let mut found = false;
        let protected = historical_protected.contains(&source);
        let mut public_source_bytes = false;
        for revision in source_history(&owner, record)? {
            let tree = repo.find_commit(revision)?.tree()?;
            let entry = match tree.get_path(&source) {
                Ok(entry) => entry,
                Err(error) if error.code() == git2::ErrorCode::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let source_blob = repo.find_blob(entry.id())?;
            found = true;
            // Lost source ancestry cannot prove a rendering public. Only already-public source bytes can.
            public_source_bytes |= !protected
                && !format::recognized(source_blob.content())
                && source_blob.content() == bytes.content();
        }
        ensure!(
            found,
            "cannot establish historical provenance for legacy generated path '{}'; retain its objects and restore its association/source history before retrying over unlock",
            path.display()
        );
        entries.insert(
            path,
            Entry {
                source,
                // A legacy association identifies an owning source hint, not an immutable historical revision.
                source_revision: None,
                protection: if protected || !public_source_bytes {
                    Protection::Encrypted
                } else {
                    Protection::Plain
                },
                generated: transformed,
                permissions: None,
            },
        );
    }
    Ok(Manifest::new(entries))
}

pub(crate) fn migrate(repo: &Repository, key: &format::RepositoryKey) -> Result<()> {
    recover(repo)?;
    let records = associations(repo)?;
    let (refs, roots) = roots(repo, &records)?;
    let history = history(repo, &roots)?;
    let mut ledger = load(repo)?;
    audit_orphans(repo, &history.iter().copied().collect(), &ledger)?;
    let mut owners = BTreeMap::new();
    for (association, record) in &records {
        let roots = pointers(record)
            .into_iter()
            .map(Oid::from_str)
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;
        for id in self::history(repo, &roots)? {
            owners.entry(id).or_insert(association);
        }
    }
    let mut replacements = BTreeMap::<String, String>::new();
    let mut protected_histories = BTreeMap::new();
    for id in history {
        let commit = repo.find_commit(id)?;
        let existing = Manifest::read(&commit)?;
        let legacy = existing.is_none();
        let manifest = match existing {
            Some(manifest) => manifest,
            None => {
                ensure!(
                    commit
                        .summary()?
                        .is_some_and(|title| title.starts_with("Generated checkout")
                            || title.starts_with("Reverse-patch")
                            || title.starts_with("Reverse-patched")),
                    "unknown generated history at {id}; restore provenance before migration"
                );
                if git::tracked_entries(&commit.tree()?)?.is_empty() {
                    Manifest::new(BTreeMap::new())
                } else {
                    let owner = owners.get(&id).context("orphan legacy generated snapshot has no trustworthy association; restore XDG state/source provenance before over unlock")?;
                    if !protected_histories.contains_key(*owner) {
                        let repository = owning_repository(repo, &records[*owner])?;
                        protected_histories.insert(
                            (*owner).clone(),
                            protected_source_paths(&repository, &records[*owner])?,
                        );
                    }
                    legacy_manifest(
                        repo,
                        &commit,
                        &records[*owner],
                        &protected_histories[*owner],
                    )?
                }
            }
        };
        let tree = commit.tree()?;
        let mut entries = git::tracked_entries(&tree)?;
        for (path, provenance) in &manifest.entries {
            if provenance.protection == Protection::Encrypted {
                let (blob, _) = entries
                    .get_mut(path)
                    .context("snapshot manifest entry is missing")?;
                let stored = repo.find_blob(*blob)?;
                if legacy && !format::recognized(stored.content()) {
                    // Record exposure before abandoning any old reference, even if preparation later fails.
                    ledger.leaked.insert(blob.to_string());
                    save(repo, &ledger)?;
                    *blob = repo.blob(
                        &provenance
                            .protection
                            .encode_with_key(stored.content(), key)?,
                    )?;
                } else {
                    provenance
                        .protection
                        .decode_with_key(stored.content(), key)?;
                }
            }
        }
        let parents = commit
            .parent_ids()
            .map(|id| {
                let mapped = replacements
                    .get(&id.to_string())
                    .cloned()
                    .unwrap_or_else(|| id.to_string());
                repo.find_commit(Oid::from_str(&mapped)?)
            })
            .collect::<std::result::Result<Vec<_>, git2::Error>>()?;
        if legacy
            || parents
                .iter()
                .map(|parent| parent.id())
                .ne(commit.parent_ids())
        {
            let tree = git::build_content_tree(repo, &entries)?;
            let rewritten = repo.commit(
                None,
                &commit.author(),
                &commit.committer(),
                &manifest.message(
                    commit
                        .summary()?
                        .unwrap_or("Generated checkout migrated snapshot"),
                )?,
                &tree,
                &parents.iter().collect::<Vec<_>>(),
            )?;
            replacements.insert(id.to_string(), rewritten.to_string());
        }
        if legacy {
            // Remember classified originals before refs move, so later ODB audits retain their ledger.
            ledger.audited.insert(id.to_string());
            save(repo, &ledger)?;
        }
    }
    if !replacements.is_empty() {
        let changed_refs = refs
            .into_iter()
            .filter_map(|(name, old)| {
                replacements
                    .get(&old.to_string())
                    .map(|new| (name, (old.to_string(), new.clone())))
            })
            .collect();
        let changed_records = records
            .into_iter()
            .filter_map(|(name, old)| {
                let mut new = old.clone();
                remap(&mut new, &replacements);
                (new != old).then_some((name, (old, new)))
            })
            .collect();
        let pins: BTreeSet<_> = replacements.values().cloned().collect();
        // Pin before recording the intent so concurrent maintenance cannot prune a replacement.
        for id in &pins {
            repo.reference(
                &format!("refs/over/crypt-migration/{id}"),
                Oid::from_str(id)?,
                true,
                "over generated migration preparation",
            )?;
        }
        ledger.pending = Some(Publication {
            administrative: fs::canonicalize(repo.commondir())?,
            refs: changed_refs,
            records: changed_records,
            replacements: pins,
        });
        save(repo, &ledger)?;
        recover(repo)?;
    }
    for record in associations(repo)?.values() {
        retain_record(repo, record)?;
    }
    verify_exposure(repo)
}

pub(crate) fn verify_exposure(repo: &Repository) -> Result<()> {
    let ledger = load(repo)?;
    ensure!(
        ledger.pending.is_none(),
        "generated snapshot migration interrupted; rerun over unlock or over crypt recover"
    );
    ensure!(
        ledger.cleanup.is_empty(),
        "generated snapshot migration cleanup remains; rerun over unlock or over crypt recover"
    );
    let odb = repo.odb()?;
    let remaining = ledger
        .leaked
        .iter()
        .map(|id| Ok(odb.exists(Oid::from_str(id)?)))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|exists| *exists)
        .count();
    ensure!(
        remaining == 0,
        "generated snapshots were migrated, but {remaining} known protected plaintext Git objects remain (including unreachable/packed/alternate objects). Preserve generated-snapshots.json, inspect refs/reflogs and alternate object stores, perform deliberate Git history/reflog/object cleanup with repository writers stopped, then rerun over unlock or over lock; changing refs alone does not erase objects"
    );
    Ok(())
}

pub(crate) fn inspect(repo: &Repository, key: Option<&format::RepositoryKey>) -> Result<()> {
    verify_exposure(repo)?;
    let records = associations(repo)?;
    let (_, roots) = roots(repo, &records)?;
    let history = history(repo, &roots)?;
    audit_orphans(repo, &history.iter().copied().collect(), &load(repo)?)?;
    for id in history {
        let commit = repo.find_commit(id)?;
        let manifest = Manifest::read(&commit)?.context("legacy generated snapshots need provenance migration; run over unlock (old plaintext objects also require explicit Git cleanup)")?;
        let tree = commit.tree()?;
        for (path, entry) in manifest.entries {
            if entry.protection == Protection::Encrypted {
                let blob = repo.find_blob(tree.get_path(&path)?.id())?;
                format::inspect(blob.content())?;
                if let Some(key) = key {
                    entry.protection.decode_with_key(blob.content(), key)?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn rotation_preflight(repo: &Repository) -> Result<()> {
    verify_exposure(repo)?;
    let records = associations(repo)?;
    let (_, roots) = roots(repo, &records)?;
    let history = history(repo, &roots)?;
    audit_orphans(repo, &history.iter().copied().collect(), &load(repo)?)?;
    for id in history {
        let commit = repo.find_commit(id)?;
        let manifest = Manifest::read(&commit)?
            .context("migrate legacy generated snapshots with over unlock before rotating keys")?;
        ensure!(
            !manifest
                .entries
                .values()
                .any(|entry| entry.protection == Protection::Encrypted),
            "key rotation would strand protected generated snapshots; resolve local edits and unapply their generated checkouts before rotating, then reapply with the new key"
        );
    }
    Ok(())
}
