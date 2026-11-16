//! Deployment authority stops at the backing source, even inside a shared target.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, ensure};
use git2::Repository;

use crate::desired::{CheckoutScope, DesiredEntry, Provenance};
use crate::overlays::mapping;
use crate::target_fs::Access;

use super::{git, state::VirtualCheckoutRecord};

pub(crate) struct SourceProtection {
    roots: BTreeSet<PathBuf>,
}

impl SourceProtection {
    pub(crate) fn new(repo: &Repository) -> Result<Self> {
        let mut roots = BTreeSet::new();
        for path in [Some(repo.path()), Some(repo.commondir()), repo.workdir()]
            .into_iter()
            .flatten()
        {
            roots.insert(mapping::physical(path)?);
        }
        if let Some(workdir) = repo.workdir() {
            // A gitfile can live outside the administrative directory it names.
            roots.insert(mapping::location(&workdir.join(".git"))?);
        }
        Ok(Self { roots })
    }

    pub(crate) fn validate_paths<'a>(
        &self,
        target: &Path,
        paths: impl IntoIterator<Item = &'a PathBuf>,
        access: &Access,
    ) -> Result<()> {
        for path in paths {
            ensure!(
                !path.as_os_str().is_empty()
                    && path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                "invalid virtual checkout destination '{}'",
                path.display()
            );
            self.validate_path(&target.join(path), &access.child(path, false), true)?;
        }
        Ok(())
    }

    pub(crate) fn validate_container(&self, target: &Path, access: &Access) -> Result<()> {
        self.validate_path(target, access, false)
    }

    fn validate_path(&self, path: &Path, access: &Access, destructive: bool) -> Result<()> {
        // Check the entry location as well as its referent: replacing an alias
        // located inside the source is unsafe even when it points elsewhere.
        let location = resolve(path, access, false)?;
        let physical = resolve(path, access, true)?;
        let hint = if location != physical {
            "; for deployed symlinks, unapply the old layout first"
        } else {
            ""
        };
        for resolved in [mapping::absolute(path)?, location, physical] {
            for root in &self.roots {
                ensure!(
                    !resolved.starts_with(root) && (!destructive || !root.starts_with(&resolved)),
                    "virtual checkout destination '{}' overlaps protected source storage '{}'{}",
                    path.display(),
                    root.display(),
                    hint
                );
            }
        }
        Ok(())
    }
}

fn resolve(path: &Path, access: &Access, follow_leaf: bool) -> Result<PathBuf> {
    if !follow_leaf {
        let absolute = mapping::absolute(path)?;
        if let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) {
            return Ok(resolve(parent, access, true)?.join(name));
        }
    }
    let mut ancestor = path.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        let result = access.fs.physical(&ancestor);
        match result {
            Ok(mut resolved) => {
                for component in suffix.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            // A planned file-to-directory transition has no physical children
            // yet; validate its existing file ancestor and append the suffix.
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotADirectory) =>
            {
                let Some(name) = ancestor.file_name() else {
                    return Err(error);
                };
                suffix.push(name.to_os_string());
                ancestor.pop();
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn validate_entry(entry: &DesiredEntry) -> Result<()> {
    if !entry.intent.is_virtual_checkout() {
        return Ok(());
    }
    let record = super::state::record_for_entry(entry)?;
    validate_entry_with_record(entry, record.as_ref())
}

pub(crate) fn validate_entry_with_record(
    entry: &DesiredEntry,
    record: Option<&VirtualCheckoutRecord>,
) -> Result<()> {
    let Provenance::Overlay { source, .. } = &entry.provenance else {
        anyhow::bail!("virtual checkout lacks an overlay source");
    };
    let (repo, managed) = match git::discover_source(source) {
        Ok(source) => source,
        // Desired ownership lint also works before repository initialization.
        // Materialization still requires discovery before any target mutation.
        Err(error)
            if error
                .downcast_ref::<git2::Error>()
                .is_some_and(|error| error.code() == git2::ErrorCode::NotFound) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let protection = SourceProtection::new(&repo)?;
    let scope = entry.intent.checkout_scope();
    protection.validate_container(&entry.target, &scope.access)?;
    let mut paths = BTreeSet::new();
    let generated = record.and_then(|record| record.generated.as_ref());
    if scope.rendered.is_some() || generated.is_some() {
        if let Some(rendered) = &scope.rendered {
            paths.extend(rendered.keys().cloned());
        }
        if let Some(record) = record {
            paths.extend(
                record
                    .scope
                    .projection
                    .iter()
                    .flat_map(|paths| paths.iter())
                    .cloned(),
            );
        }
        if let Some(generated) = generated {
            // Persisted inventories retain deleted and interrupted destinations;
            // reporting projections may additionally contain source-only names.
            paths.extend(generated.sources.keys().cloned());
            paths.extend(
                generated
                    .pending_sources
                    .iter()
                    .flat_map(|paths| paths.keys())
                    .cloned(),
            );
            paths.extend(generated.conflicts.keys().cloned());
        }
    } else {
        collect_ordinary_paths(&repo, &managed, &scope, record, &mut paths)?;
    }
    protection.validate_paths(&entry.target, &paths, &scope.access)
}

fn collect_ordinary_paths(
    repo: &Repository,
    managed: &Path,
    scope: &CheckoutScope,
    record: Option<&VirtualCheckoutRecord>,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    if let Some(projection) = &scope.projection {
        paths.extend(
            projection
                .iter()
                .filter(|path| scope.owns_target(path))
                .cloned(),
        );
        // Singleton acquisition claims deliberately omit the unit's other
        // source leaves; rescanning HEAD would reject those as mapping holes.
        return Ok(());
    }
    for oid in [
        None,
        record
            .filter(|record| !record.base_oid.is_empty())
            .map(|record| record.base_oid.as_str()),
    ] {
        let tree = git::base_tree(repo, oid)?;
        paths.extend(git::scoped_entries(repo, &tree, managed, scope)?.into_keys());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use git2::Repository;

    use crate::target_fs::Access;

    use super::SourceProtection;

    #[rstest::rstest]
    #[case("home", false)]
    #[case("home/.dotfiles", false)]
    #[case("home/.dotfiles/untracked", false)]
    #[case("home/.dotfiles/.git/config", false)]
    #[case("home/.config/app/config", true)]
    fn destructive_paths_cannot_replace_source_containing_ancestors(
        #[case] relative: &str,
        #[case] allowed: bool,
    ) {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(temp.path().join("home/.dotfiles")).unwrap();
        let protection = SourceProtection::new(&repo).unwrap();
        assert_eq!(
            protection
                .validate_paths(temp.path(), [&PathBuf::from(relative)], &Access::default())
                .is_ok(),
            allowed,
        );
        // The same home is valid as a shared, non-destructive container.
        protection
            .validate_container(&temp.path().join("home"), &Access::default())
            .unwrap();
    }

    #[test]
    fn file_to_directory_transitions_are_checked_without_requiring_existing_children() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo = Repository::init(temp.path().join("home/.dotfiles")).unwrap();
        std::fs::write(temp.path().join("home/config"), "old leaf").unwrap();
        let protection = SourceProtection::new(&repo).unwrap();
        protection
            .validate_paths(
                &temp.path().join("home"),
                [&PathBuf::from("config/new/leaf")],
                &Access::default(),
            )
            .unwrap();
    }
}
