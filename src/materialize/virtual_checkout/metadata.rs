//! File-wide metadata follows source policy even when a checkout is one reconciliation unit.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::desired::{DesiredEntry, Provenance};
use crate::overlays::Permissions;
use crate::target_fs::Access;

pub(crate) fn assignments(entry: &DesiredEntry) -> Result<Vec<(PathBuf, Access)>> {
    if !entry.access.has_metadata_policy() {
        return Ok(Vec::new());
    }
    // Metadata follows aliases too; validate before applying even a mode-only
    // update to an otherwise unchanged deployed file. Container updates are
    // nonrecursive, so an ancestor home remains a valid metadata target.
    super::safety::validate_entry(entry)?;
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        return Ok(Vec::new());
    };
    let scope = entry.intent.checkout_scope();
    let mut assignments = vec![(entry.target.clone(), entry.access.clone())];
    if let Some(rendered) = &scope.rendered {
        for (relative, file) in rendered.iter() {
            let mut access = entry.access.child(file.source.strip_prefix(source)?, false);
            access.permissions.mode = file.permissions;
            assignments.push((entry.target.join(relative), access));
        }
    } else {
        let (repo, managed) = super::git::discover_source(source)?;
        let tree = super::git::base_tree(&repo, None)?;
        for relative in super::git::scoped_entries(&repo, &tree, &managed, &scope)?.keys() {
            let mut access = entry.access.child(relative, false);
            // Ordinary checkout modes remain Git-owned; owner/group are independent policy.
            access.permissions.mode = None;
            assignments.push((entry.target.join(relative), access));
        }
    }
    Ok(assignments)
}

pub(crate) fn validate(entry: &DesiredEntry) -> Result<()> {
    for (_, access) in assignments(entry)? {
        access.validate_identities()?;
    }
    Ok(())
}

pub(crate) fn drift(entry: &DesiredEntry) -> Result<Option<(Permissions, Permissions)>> {
    for (path, access) in assignments(entry)? {
        match access.metadata_drift(&path) {
            Ok(Some(drift)) => return Ok(Some(drift)),
            Ok(None) => {}
            Err(error) if crate::target_fs::is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

pub(crate) fn apply(entry: &DesiredEntry) -> Result<()> {
    for (path, access) in assignments(entry)?.into_iter().rev() {
        if !access.has_metadata() {
            continue;
        }
        match access.fs.metadata(&path, false) {
            Ok(_) => access
                .fs
                .set_metadata(&path, &access.permissions)
                .with_context(|| format!("could not enforce metadata on '{}'", path.display()))?,
            Err(error) if crate::target_fs::is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
