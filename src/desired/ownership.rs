//! Desired-versus-desired ownership, independent of deployment state or ordering.

use std::path::{Path, PathBuf};

use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent, Provenance};
use crate::overlays::mapping;

/// Stable identity lets presentation layers deduplicate shared compositions.
#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) struct OwnershipIssue {
    key: Option<ConflictKey>,
    pub message: String,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ClaimIdentity {
    overlay: String,
    source: String,
    target: PathBuf,
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConflictKey {
    claims: [ClaimIdentity; 2],
    reason: String,
}

fn owner(entry: &DesiredEntry) -> (&str, String) {
    match &entry.provenance {
        Provenance::Overlay { overlay, source } => (overlay, source.display().to_string()),
        Provenance::SymlinkSidecar {
            overlay, config, ..
        }
        | Provenance::PartialSidecar { overlay, config } => (overlay, config.display().to_string()),
        Provenance::Git {
            overlay, repo_key, ..
        } => (overlay, format!("git[{repo_key}]")),
    }
}

fn scaffold(entry: &DesiredEntry) -> bool {
    matches!(entry.intent, MaterializationIntent::Directory)
}

fn directory(entry: &DesiredEntry) -> bool {
    matches!(
        entry.intent,
        MaterializationIntent::Directory
            | MaterializationIntent::SymlinkDirectory { .. }
            | MaterializationIntent::Checkout
            | MaterializationIntent::VirtualCheckout
            | MaterializationIntent::ScopedVirtualCheckout(_)
    )
}

fn owns(entry: &DesiredEntry, target: &Path, is_dir: bool) -> bool {
    let Ok(relative) = target.strip_prefix(&entry.target) else {
        return false;
    };
    match &entry.intent {
        MaterializationIntent::ScopedVirtualCheckout(scope) => {
            scope.owns_target_kind(relative, is_dir)
        }
        _ => true,
    }
}

/// Return a concrete shared namespace witness, respecting checkout holes.
fn overlap(a: &DesiredEntry, b: &DesiredEntry) -> anyhow::Result<Option<PathBuf>> {
    let root = if a.target.starts_with(&b.target) {
        &a.target
    } else if b.target.starts_with(&a.target) {
        &b.target
    } else {
        return Ok(None);
    };
    let is_dir = if a.target == *root {
        directory(a)
    } else {
        directory(b)
    };
    if owns(a, root, is_dir) && owns(b, root, is_dir) {
        return Ok(Some(root.clone()));
    }
    if !directory(a) || !directory(b) {
        return Ok(None);
    }

    // A policy-rejected directory point need not exclude every descendant (for
    // example a control-file basename). Inspect sources, never deployed contents.
    for entry in [a, b] {
        if let MaterializationIntent::ScopedVirtualCheckout(scope) = &entry.intent
            && let Some((source, _)) = &scope.source_policy
        {
            for candidate in walkdir::WalkDir::new(source) {
                let candidate = candidate?;
                let target = entry.target.join(candidate.path().strip_prefix(source)?);
                if target.starts_with(root)
                    && owns(a, &target, candidate.file_type().is_dir())
                    && owns(b, &target, candidate.file_type().is_dir())
                {
                    return Ok(Some(target));
                }
            }
        }
    }
    Ok(None)
}

pub(crate) fn check(tree: &DesiredTree) -> Vec<OwnershipIssue> {
    let mut diagnostics = Vec::new();
    // Resolve parent aliases once per entry, retaining managed symlink leaves as
    // deployment locations rather than accidentally claiming their referents.
    let locations: Vec<_> = tree
        .entries()
        .iter()
        .map(|entry| {
            match mapping::location(&entry.target).or_else(|error| {
                // Only an explicitly managed scaffold can authorize a future namespace;
                // an unrelated ENOTDIR must remain an error rather than look absent.
                match tree.blocking_scaffold(&entry.target)? {
                    Some(parent) => {
                        Ok(mapping::location(&parent)?.join(entry.target.strip_prefix(parent)?))
                    }
                    None => Err(error),
                }
            }) {
                Ok(location) => Some(location),
                Err(error) => {
                    diagnostics.push(OwnershipIssue {
                        key: None,
                        message: format!(
                            "cannot resolve ownership location '{}': {error:#}",
                            entry.target.display()
                        ),
                    });
                    None
                }
            }
        })
        .collect();
    for (index, a) in tree.entries().iter().enumerate() {
        for (other_index, b) in tree.entries().iter().enumerate().skip(index + 1) {
            let (a_owner, a_source) = owner(a);
            let (b_owner, b_source) = owner(b);
            let conflict = conflict(a, b).and_then(|lexical| {
                if lexical.is_some() {
                    return Ok(lexical);
                }
                let (Some(a_location), Some(b_location)) =
                    (&locations[index], &locations[other_index])
                else {
                    return Ok(None);
                };
                // Most pairs are unrelated; avoid cloning intents/content unless
                // their resolved namespaces can actually intersect.
                if !a_location.starts_with(b_location) && !b_location.starts_with(a_location) {
                    return Ok(None);
                }
                let mut physical_a = a.clone();
                let mut physical_b = b.clone();
                physical_a.target = a_location.clone();
                physical_b.target = b_location.clone();
                conflict(&physical_a, &physical_b)
            });
            let (witness, reason) = match conflict {
                Ok(Some(conflict)) => conflict,
                Ok(None) => continue,
                Err(error) => {
                    diagnostics.push(OwnershipIssue {
                        key: None,
                        message: format!("cannot inspect ownership sources: {error}"),
                    });
                    continue;
                }
            };
            let mut owners = [
                format!("{a_owner}: {a_source} -> {}", a.target.display()),
                format!("{b_owner}: {b_source} -> {}", b.target.display()),
            ];
            owners.sort();
            let mut claims = [
                ClaimIdentity {
                    overlay: a_owner.to_owned(),
                    source: a_source,
                    target: a.target.clone(),
                },
                ClaimIdentity {
                    overlay: b_owner.to_owned(),
                    source: b_source,
                    target: b.target.clone(),
                },
            ];
            claims.sort();
            let key = ConflictKey {
                claims,
                reason: reason.clone(),
            };
            diagnostics.push(OwnershipIssue {
                key: Some(key),
                message: format!(
                    "conflicting target ownership at '{}': {} conflicts with {} ({reason})",
                    witness.display(),
                    owners[0],
                    owners[1]
                ),
            });
        }
    }
    diagnostics
}

pub(crate) fn conflict(
    a: &DesiredEntry,
    b: &DesiredEntry,
) -> anyhow::Result<Option<(PathBuf, String)>> {
    // A declared Git checkout ensures repository presence; this overlay's own
    // content is intentionally materialized inside it as a separate operation.
    if owner(a).0 == owner(b).0
        && (matches!(a.intent, MaterializationIntent::Checkout)
            || matches!(b.intent, MaterializationIntent::Checkout))
    {
        return Ok(None);
    }
    if scaffold(a) || scaffold(b) {
        let (scaffold, other) = if scaffold(a) { (a, b) } else { (b, a) };
        if directory(other) || !scaffold.target.starts_with(&other.target) {
            return Ok(None);
        }
    }
    let reason = match (&a.intent, &b.intent) {
        (
            MaterializationIntent::PartialFile { marker: am, .. },
            MaterializationIntent::PartialFile { marker: bm, .. },
        ) if a.target == b.target => {
            if am == bm {
                format!("duplicate partial marker '{am}'")
            } else if a.permissions.is_some()
                && b.permissions.is_some()
                && a.permissions != b.permissions
            {
                "incompatible file-wide permissions on partial blocks".to_owned()
            } else {
                return Ok(None);
            }
        }
        _ => "overlapping managed namespaces".to_owned(),
    };
    Ok(overlap(a, b)?.map(|witness| (witness, reason)))
}

#[cfg(test)]
mod tests {
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    use crate::exec::Context;
    use crate::lint::{LintResult, lint_overlay_with_context, lint_repository_with_context};
    use crate::overlays::Repository;

    fn fixture() -> (TempDir, std::sync::Arc<Context>) {
        let temp = TempDir::new().unwrap();
        temp.child("source").create_dir_all().unwrap();
        let context = Context::builder()
            .repository(Repository::new(temp.path().join("source")))
            .root(temp.path().join("target"))
            .build();
        (temp, context)
    }

    fn write(temp: &TempDir, path: &str, content: &str) {
        temp.child(format!("source/{path}"))
            .write_str(content)
            .unwrap();
    }

    fn conflicts(result: &LintResult) -> Vec<&crate::lint::Diagnostic> {
        result
            .diagnostics
            .iter()
            .filter(|diag| diag.message.contains("conflicting target ownership"))
            .collect()
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn composed_collisions_identify_both_sources_and_the_resolved_target(#[case] transitive: bool) {
        let (temp, context) = fixture();
        write(
            &temp,
            "selected/over.yaml",
            if transitive {
                "uses: [middle]"
            } else {
                "uses: [dependency]"
            },
        );
        if transitive {
            write(&temp, "middle/over.yaml", "uses: [dependency]");
        }
        write(&temp, "selected/config/editor", "selected");
        write(&temp, "dependency/over.yaml", "target: '~'");
        write(&temp, "dependency/config/editor", "dependency");
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        let diagnostics = conflicts(&result);
        assert_eq!(diagnostics.len(), 1, "{result:?}");
        let diagnostic = diagnostics[0];
        // Diagnostics retain native paths; assertions must also work on Windows.
        let message = diagnostic.message.replace('\\', "/");
        for expected in ["selected", "dependency", "config/editor", "target"] {
            assert!(message.contains(expected), "{diagnostic:?}");
        }
        assert!(diagnostic.hint.as_ref().unwrap().contains("remap"));
        assert!(
            conflicts(&lint_overlay_with_context(&context, "selected", false).unwrap()).is_empty()
        );
    }

    #[test]
    fn diamonds_and_multiple_selected_roots_report_each_real_conflict_once() {
        let (temp, context) = fixture();
        for (name, config) in [
            ("selected", "uses: [left, right]"),
            ("left", "uses: [shared]"),
            ("right", "uses: [shared]"),
            ("shared", "target: '~'"),
        ] {
            write(&temp, &format!("{name}/over.yaml"), config);
        }
        write(&temp, "shared/config/editor", "shared");
        let clean = lint_repository_with_context(&context);
        assert!(!clean.has_errors(), "{clean:?}");
        write(&temp, "left/config/editor", "left");
        let result = lint_repository_with_context(&context);
        assert_eq!(conflicts(&result).len(), 1, "{result:?}");
    }

    #[rstest]
    #[case("exclude: [config]", false)]
    #[case("target: '~/elsewhere'", false)]
    #[case("target: '~'", true)]
    fn exclusions_and_distinct_targets_limit_ownership(
        #[case] dependency: &str,
        #[case] collides: bool,
    ) {
        let (temp, context) = fixture();
        write(&temp, "selected/over.yaml", "uses: [dependency]");
        write(&temp, "dependency/over.yaml", dependency);
        write(&temp, "selected/config/editor", "selected");
        write(&temp, "dependency/config/editor", "dependency");
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert_eq!(!conflicts(&result).is_empty(), collides, "{result:?}");
    }

    #[test]
    fn unrelated_overlays_are_not_an_implicit_composition() {
        let (temp, context) = fixture();
        for name in ["a", "b"] {
            write(&temp, &format!("{name}/over.yaml"), "target: '~'");
            write(&temp, &format!("{name}/same"), name);
        }
        let result = lint_repository_with_context(&context);
        assert!(!result.has_errors(), "{result:?}");
    }

    #[test]
    fn repository_lint_loads_descriptorless_dependencies_and_normalized_references() {
        let (temp, context) = fixture();
        write(&temp, "selected/over.yaml", "uses: ['./dependency/']");
        write(&temp, "selected/same", "selected");
        write(&temp, "dependency/same", "dependency");
        let result = lint_repository_with_context(&context);
        assert_eq!(conflicts(&result).len(), 1, "{result:?}");
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("non-existent"))
        );
    }

    #[test]
    fn ignored_source_entries_do_not_claim_targets() {
        let (temp, context) = fixture();
        write(&temp, "selected/over.yaml", "uses: [dependency]");
        write(&temp, "selected/config/editor", "selected");
        write(&temp, "dependency/over.yaml", "target: '~'");
        write(&temp, "dependency/.overignore", "config/\n");
        write(&temp, "dependency/config/editor", "ignored");
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert!(!result.has_errors(), "{result:?}");
    }

    #[cfg(unix)]
    #[rstest]
    #[case(false, false)]
    #[case(true, false)]
    #[case(false, true)]
    #[case(true, true)]
    fn directory_aliases_cannot_hide_file_or_partial_ownership(
        #[case] applied: bool,
        #[case] partial: bool,
    ) {
        let (temp, context) = fixture();
        std::fs::create_dir_all(context.root.join("real")).unwrap();
        std::os::unix::fs::symlink(context.root.join("real"), context.root.join("alias")).unwrap();
        write(
            &temp,
            "selected/over.yaml",
            "uses: [dependency]\ntarget: '~/real'",
        );
        write(&temp, "dependency/over.yaml", "target: '~/alias'");
        if partial {
            write(
                &temp,
                "selected/block.partial.yaml",
                "target: '~/real/settings'\nmarker: shared\ncontent: selected",
            );
            write(
                &temp,
                "dependency/block.partial.yaml",
                "target: '~/alias/settings'\nmarker: shared\ncontent: dependency",
            );
            if applied {
                std::fs::write(
                    context.root.join("real/settings"),
                    "existing partial content",
                )
                .unwrap();
            }
        } else {
            write(&temp, "selected/settings", "selected");
            write(&temp, "dependency/settings", "dependency");
            if applied {
                std::os::unix::fs::symlink(
                    context.repository.root.join("selected/settings"),
                    context.root.join("real/settings"),
                )
                .unwrap();
            }
        }
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert_eq!(conflicts(&result).len(), 1, "{result:?}");
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn incompatible_sidecars_and_ordinary_files_conflict_within_one_overlay(#[case] partial: bool) {
        let (temp, context) = fixture();
        write(&temp, "selected/over.yaml", "target: '~'");
        write(&temp, "selected/settings", "ordinary file");
        if partial {
            write(
                &temp,
                "selected/block.partial.yaml",
                "target: '~/settings'\ncontent: managed block",
            );
        } else {
            write(&temp, "selected/settings.link.yaml", "target: '~/referent'");
        }
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert_eq!(conflicts(&result).len(), 1, "{result:?}");
        assert!(conflicts(&result)[0].message.contains("settings"));
    }

    #[test]
    fn whole_directory_links_cannot_expose_excluded_contents() {
        let (temp, context) = fixture();
        write(
            &temp,
            "selected/over.yaml",
            "exclude: [config/private]\nrules:\n  - {path: config, mode: symlink-directory}",
        );
        write(&temp, "selected/config/private", "excluded");
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("would expose ignored content")),
            "{result:?}"
        );
    }

    #[test]
    fn overlapping_mapping_destinations_are_configuration_errors() {
        let (temp, context) = fixture();
        write(
            &temp,
            "selected/over.yaml",
            "map:\n  first: {target: '~/mapped'}\n  second: {target: '~/mapped/nested'}",
        );
        write(&temp, "selected/first/file", "first");
        write(&temp, "selected/second/file", "second");
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert!(result.has_errors(), "{result:?}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("overlap") && diag.hint.is_some()),
            "{result:?}"
        );
    }

    #[rstest]
    #[case("symlink", false)]
    #[case("symlink-directory", true)]
    #[case("checkout", true)]
    fn directory_materialization_controls_namespace_ownership(
        #[case] mode: &str,
        #[case] collides: bool,
    ) {
        let (temp, context) = fixture();
        write(
            &temp,
            "selected/over.yaml",
            &format!("uses: [dependency]\nrules:\n  - {{path: config, mode: {mode}}}"),
        );
        write(&temp, "selected/config/first", "selected");
        write(&temp, "dependency/over.yaml", "target: '~'");
        write(&temp, "dependency/config/second", "dependency");
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert_eq!(!conflicts(&result).is_empty(), collides, "{result:?}");
    }

    #[test]
    fn a_file_conflicts_with_another_owners_required_directory() {
        let (temp, context) = fixture();
        write(&temp, "selected/over.yaml", "uses: [dependency]");
        write(&temp, "selected/config", "file");
        write(&temp, "dependency/over.yaml", "target: '~'");
        write(&temp, "dependency/config/editor", "nested");
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert!(!conflicts(&result).is_empty(), "{result:?}");
    }

    #[test]
    fn directory_mappings_collide_at_their_resolved_destinations() {
        let (temp, context) = fixture();
        write(
            &temp,
            "selected/over.yaml",
            "uses: [dependency]\nmap:\n  config: {target: '~/mapped'}",
        );
        write(&temp, "selected/config/editor", "selected");
        write(&temp, "dependency/over.yaml", "target: '~'");
        write(&temp, "dependency/mapped/editor", "dependency");
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert_eq!(conflicts(&result).len(), 1, "{result:?}");
        assert!(
            conflicts(&result)[0]
                .message
                .replace('\\', "/")
                .contains("mapped/editor")
        );
    }

    #[test]
    fn independently_mapped_checkout_holes_can_be_owned_by_another_overlay() {
        let (temp, context) = fixture();
        write(
            &temp,
            "selected/over.yaml",
            "uses: [dependency]\nmode: checkout\nmap:\n  config: {target: '~/outside'}",
        );
        write(&temp, "selected/config/editor", "mapped");
        write(&temp, "selected/ordinary", "ordinary");
        write(&temp, "dependency/over.yaml", "target: '~'");
        write(&temp, "dependency/config/editor", "dependency");
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert!(!result.has_errors(), "{result:?}");
    }

    #[rstest]
    #[case("one", "two", "600", "600", false)]
    #[case("one", "one", "600", "600", true)]
    #[case("one", "two", "600", "644", true)]
    fn partial_blocks_share_only_compatible_markers_and_file_permissions(
        #[case] first: &str,
        #[case] second: &str,
        #[case] first_mode: &str,
        #[case] second_mode: &str,
        #[case] collides: bool,
    ) {
        let (temp, context) = fixture();
        write(
            &temp,
            "selected/over.yaml",
            &format!("uses: [dependency]\npermissions: '{first_mode}'"),
        );
        write(
            &temp,
            "dependency/over.yaml",
            &format!("permissions: '{second_mode}'"),
        );
        for (name, marker) in [("selected", first), ("dependency", second)] {
            write(
                &temp,
                &format!("{name}/shell.partial.yaml"),
                &format!("target: '~/shell'\nmarker: {marker}\ncontent: managed"),
            );
        }
        let result = lint_overlay_with_context(&context, "selected", true).unwrap();
        assert_eq!(!conflicts(&result).is_empty(), collides, "{result:?}");
    }

    #[test]
    fn unresolved_destinations_do_not_hide_other_composition_conflicts() {
        let (temp, context) = fixture();
        write(&temp, "broken/over.yaml", "target: '~/{{ data.missing }}'");
        write(&temp, "selected/over.yaml", "uses: [dependency]");
        write(&temp, "dependency/over.yaml", "target: '~'");
        write(&temp, "selected/same", "selected");
        write(&temp, "dependency/same", "dependency");
        let result = lint_repository_with_context(&context);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diag| diag.overlay == "broken" && diag.message.contains("undefined")),
            "{result:?}"
        );
        assert_eq!(conflicts(&result).len(), 1);
    }

    #[test]
    fn consumer_metadata_is_resolved_independently_for_each_composition() {
        let (temp, context) = fixture();
        write(
            &temp,
            "shared/over.yaml",
            "target: '~/{{ data.destination | default(\"fallback\") }}'",
        );
        write(&temp, "shared/file", "shared");
        for (name, target) in [("first", "first"), ("second", "second")] {
            write(
                &temp,
                &format!("{name}/over.yaml"),
                &format!("uses: [shared]\ntarget: '~/{target}'\ndata: {{destination: {target}}}"),
            );
            write(&temp, &format!("{name}/file"), name);
        }
        let result = lint_repository_with_context(&context);
        assert_eq!(conflicts(&result).len(), 2, "{result:?}");
        assert!(conflicts(&result).iter().any(|diag| {
            diag.message
                .replace('\\', "/")
                .contains("target/first/file")
        }));
        assert!(conflicts(&result).iter().any(|diag| {
            diag.message
                .replace('\\', "/")
                .contains("target/second/file")
        }));
    }
}
