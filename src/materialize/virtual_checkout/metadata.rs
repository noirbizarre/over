//! File-wide metadata follows source policy even when a checkout is one reconciliation unit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    let mut assignments = BTreeMap::from([(entry.target.clone(), entry.access.clone())]);
    if let Some(rendered) = &scope.rendered {
        for (relative, file) in rendered.iter() {
            let source_relative = file.source.strip_prefix(source)?;
            add_directories(entry, relative, source_relative, &mut assignments)?;
            let mut access = entry.access.child(source_relative, false);
            access.permissions.mode = file.permissions;
            assignments.insert(entry.target.join(relative), access);
        }
    } else {
        let (repo, managed) = super::git::discover_source(source)?;
        let tree = super::git::base_tree(&repo, None)?;
        for relative in super::git::scoped_entries(&repo, &tree, &managed, &scope)?.keys() {
            add_directories(entry, relative, relative, &mut assignments)?;
            let mut access = entry.access.child(relative, false);
            // Ordinary checkout modes remain Git-owned; owner/group are independent policy.
            access.permissions.mode = None;
            assignments.insert(entry.target.join(relative), access);
        }
    }
    Ok(assignments.into_iter().collect())
}

fn add_directories(
    entry: &DesiredEntry,
    destination: &Path,
    source: &Path,
    assignments: &mut BTreeMap<PathBuf, Access>,
) -> Result<()> {
    // Generated names change paths, not their policy identity. Walk corresponding
    // parents rather than resolving a directory rule against the rendered name.
    let mut parents = source.ancestors().skip(1);
    for destination in destination.ancestors().skip(1) {
        if destination.as_os_str().is_empty() {
            break;
        }
        // Additional generated scaffolds inherit the checkout root's source policy.
        let source = parents.next().unwrap_or_else(|| Path::new(""));
        let access = entry.access.child(source, true);
        let path = entry.target.join(destination);
        if let Some(previous) = assignments.get(&path) {
            anyhow::ensure!(
                previous.permissions == access.permissions
                    && previous.fs.is_privileged() == access.fs.is_privileged(),
                "incompatible source directory policies for '{}'",
                path.display()
            );
        } else {
            assignments.insert(path, access);
        }
    }
    Ok(())
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
            Ok(metadata)
                if metadata.is_dir() && entry.intent.checkout_scope().defer_directory_metadata => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::desired::content::{RenderedCheckout, RenderedFile};
    use crate::desired::{CheckoutScope, MaterializationIntent};
    use crate::exec::Context;
    use crate::overlays::{FileMode, Repository};

    fn rendered_entry(policy: serde_json::Value) -> (tempfile::TempDir, DesiredEntry) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source/ov");
        std::fs::create_dir_all(&source).unwrap();
        git2::Repository::init(temp.path().join("source")).unwrap();
        std::fs::write(source.join("over.yaml"), policy.to_string()).unwrap();
        let overlay = Arc::new(
            Repository::new(temp.path().join("source"))
                .get("ov")
                .unwrap(),
        );
        let ctx = Context::builder().build();
        let access = Access::new(&ctx, overlay, Path::new(""), true);
        let inventory = ["first", "second"]
            .into_iter()
            .map(|name| {
                (
                    PathBuf::from(format!("renamed/deeper/{name}")),
                    RenderedFile {
                        protection: Default::default(),
                        source_revision: None,
                        source: source.join(format!("original/deeper/{name}.tmpl")),
                        bytes: Arc::from(b"content".as_slice()),
                        identity: name.into(),
                        mode: 0o100644,
                        generated: true,
                        permissions: None,
                    },
                )
            })
            .collect();
        let entry = DesiredEntry {
            target: temp.path().join("target"),
            access,
            provenance: Provenance::Overlay {
                overlay: "ov".into(),
                source,
            },
            intent: MaterializationIntent::ScopedVirtualCheckout(CheckoutScope {
                rendered: Some(Arc::new(RenderedCheckout {
                    inventory,
                    source_revision: "fixture".into(),
                })),
                ..Default::default()
            }),
            permissions: None,
        };
        (temp, entry)
    }

    #[test]
    fn intermediate_assignments_are_unique_and_follow_source_directory_policies_after_renames() {
        let (_temp, entry) = rendered_entry(serde_json::json!({"rules": [
            {"path": "original", "permissions": "0700", "sudo": true},
            {"path": "original/deeper", "permissions": "0750", "sudo": false},
            {"path": "original/deeper/*", "permissions": "0600"}
        ]}));
        let assignments = assignments(&entry)
            .unwrap()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(assignments.len(), 5);
        let parent = &assignments[&entry.target.join("renamed")];
        assert_eq!(parent.permissions.mode, Some(FileMode::from_bits(0o700)));
        assert!(parent.fs.is_privileged());
        let child = &assignments[&entry.target.join("renamed/deeper")];
        assert_eq!(child.permissions.mode, Some(FileMode::from_bits(0o750)));
        assert!(!child.fs.is_privileged());
        assert_eq!(
            assignments[&entry.target.join("renamed/deeper/first")]
                .permissions
                .mode,
            None
        );
        assert!(!assignments.contains_key(&entry.target.join("original")));
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[case("owner")]
    #[case("group")]
    fn directory_ownership_drift_is_detected_without_a_leaf_metadata_policy(#[case] field: &str) {
        use std::os::unix::fs::MetadataExt;
        let actual = std::fs::metadata(".").unwrap();
        let id = if field == "owner" {
            actual.uid()
        } else {
            actual.gid()
        };
        let other = if id == 0 { 1 } else { 0 };
        let (_temp, entry) = rendered_entry(serde_json::json!({"rules": [
            {"path": "original/deeper", "permissions": {(field): other}},
            {"path": "original/deeper/*", "permissions": {(field): id}}
        ]}));
        std::fs::create_dir_all(entry.target.join("renamed/deeper")).unwrap();
        // Only this intermediate directory exists with a nonmatching policy.
        assert!(drift(&entry).unwrap().is_some());
    }
}
