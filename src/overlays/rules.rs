//! Materialization rules: a default materialization plus path/subtree
//! overrides, resolved into a [`MaterializationKind`] for each directory
//! encountered while walking an overlay's tree (#126, part of #113).
//!
//! This module only decides *whether* a directory should be recursed into
//! (and its files symlinked individually) or symlinked as a single unit —
//! it has no notion of `source`/`link_type`/`Checkout`, which live on
//! [`crate::desired::MaterializationIntent`] once [`crate::desired::tree`]
//! has resolved concrete filesystem paths. `checkout` is deliberately not a
//! valid `materialization` value here: that intent stays driven by
//! `overlay.git` (see ADR-017). Assigning an arbitrary subtree to a
//! checkout, or migrating an already-symlinked path to one, is rule-change
//! *migration* territory, tracked separately (#129).

use std::fmt;
use std::path::Path;

use globset::GlobBuilder;
use serde::{Deserialize, Serialize};

use super::overlay::Overlay;
use super::permissions::FileMode;

/// How a directory-shaped entry should be materialized once a `rules`/
/// `defaults` entry has been resolved for it (or none matched).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MaterializationKind {
    /// Recurse into the directory and symlink each file individually —
    /// today's implicit default.
    #[default]
    Symlink,
    /// Symlink the directory as a single unit; its contents are not
    /// separately enumerated (equivalent to a `link_dirs` match).
    SymlinkDirectory,
}

impl fmt::Display for MaterializationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MaterializationKind::Symlink => write!(f, "symlink"),
            MaterializationKind::SymlinkDirectory => write!(f, "symlink-directory"),
        }
    }
}

/// Repository- or overlay-level default materialization (and, since #65,
/// default permission mode), applied when no more specific override
/// matches a given path. Inherited through the same cascading descriptor
/// chain as every other overlay field (ADR-002): a root `over.toml` sets a
/// repository-wide baseline, and any ancestor down to the overlay's own
/// directory can override it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Defaults {
    #[serde(default)]
    pub materialization: MaterializationKind,
    /// Default permission mode for entries `over` writes content for
    /// directly (`PartialFile`, #65) with no more specific `permissions`
    /// rule — see [`super::permissions::resolve`]. `None` (the default)
    /// leaves permissions entirely unmanaged, matching pre-#65 behavior.
    #[serde(default)]
    pub mode: Option<FileMode>,
}

/// A single path/subtree materialization override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializationRule {
    /// Glob pattern (or literal path), relative to the overlay root.
    pub path: String,
    pub materialization: MaterializationKind,
}

impl MaterializationRule {
    /// Whether `rel_path` matches this rule's `path` glob. A malformed
    /// glob never matches — `over lint` is where invalid patterns are
    /// reported, not a panic/error here (mirrors `Overlay::is_excluded`).
    fn matches(&self, rel_path: &Path) -> bool {
        GlobBuilder::new(&self.path)
            .literal_separator(true)
            .build()
            .ok()
            .is_some_and(|glob| glob.compile_matcher().is_match(rel_path))
    }

    /// Rough specificity ranking used to break ties when more than one
    /// rule matches the same path: a literal pattern (no glob
    /// metacharacters) always outranks a wildcard one, and within the
    /// same tier a longer pattern is considered more specific. Not a
    /// perfect partial order for every possible pair of globs, but enough
    /// to resolve the common "a specific path overrides a broader glob"
    /// case #113 asks for.
    fn specificity(&self) -> (bool, usize) {
        let is_literal = !self.path.contains(['*', '?', '[', '{']);
        (is_literal, self.path.len())
    }
}

/// `overlay.rules` plus one equivalent [`MaterializationRule`] per
/// `link_dirs` glob (translated to `SymlinkDirectory`), so both resolve
/// through [`resolve`] — one code path, no behavior change for existing
/// `link_dirs` configs (ADR-017).
///
/// `link_dirs`-derived entries are listed first so an explicit `rules`
/// entry wins ties against a legacy `link_dirs` pattern for the same path
/// (`Iterator::max_by_key`, which [`resolve`] uses, returns the *last*
/// maximum on ties).
pub(super) fn effective_rules(overlay: &Overlay) -> Vec<MaterializationRule> {
    let mut rules: Vec<MaterializationRule> = overlay
        .link_dirs
        .iter()
        .flatten()
        .map(|pattern| MaterializationRule {
            path: pattern.clone(),
            materialization: MaterializationKind::SymlinkDirectory,
        })
        .collect();
    rules.extend(overlay.rules.iter().flatten().cloned());
    rules
}

/// Resolve the effective [`MaterializationKind`] for a directory at
/// `rel_path` within `overlay`, in precedence order:
///
/// 1. the most specific matching `rules` entry (explicit config, or a
///    `link_dirs` pattern translated into an equivalent rule);
/// 2. `defaults.materialization`;
/// 3. the hardcoded fallback (`Symlink` — today's implicit default).
///
/// Only meaningful for directories: files are always symlinked
/// individually regardless of `rules`/`defaults` — there is no
/// "file-level checkout" or "whole-file directory" distinction to make
/// here.
pub(super) fn resolve(overlay: &Overlay, rel_path: &Path) -> MaterializationKind {
    let rules = effective_rules(overlay);
    let best = rules
        .iter()
        .filter(|rule| rule.matches(rel_path))
        .max_by_key(|rule| rule.specificity());
    match best {
        Some(rule) => rule.materialization,
        None => overlay
            .defaults
            .map(|d| d.materialization)
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    fn setup_overlay(toml_content: &str) -> Overlay {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        let ov = td.child("ov");
        ov.create_dir_all().unwrap();
        ov.child("over.toml").write_str(toml_content).unwrap();
        let overlay = repo.get("ov").unwrap();
        // Keep `td` alive for the overlay's lifetime (test-only leak).
        Box::leak(Box::new(td));
        overlay
    }

    #[rstest]
    #[case(MaterializationKind::Symlink, "symlink")]
    #[case(MaterializationKind::SymlinkDirectory, "symlink-directory")]
    fn display_matches_the_kebab_case_config_value(
        #[case] kind: MaterializationKind,
        #[case] expected: &str,
    ) {
        assert_eq!(kind.to_string(), expected);
    }

    #[rstest]
    fn no_defaults_no_rules_falls_back_to_symlink() {
        let overlay = setup_overlay("target = \"~\"");
        assert_eq!(
            resolve(&overlay, Path::new("anything")),
            MaterializationKind::Symlink
        );
    }

    #[rstest]
    fn defaults_materialization_applies_to_every_unmatched_path() {
        let overlay =
            setup_overlay("target = \"~\"\n[defaults]\nmaterialization = \"symlink-directory\"");
        assert_eq!(
            resolve(&overlay, Path::new("any/dir")),
            MaterializationKind::SymlinkDirectory
        );
    }

    #[rstest]
    fn a_rule_overrides_the_default_for_its_own_subtree_only() {
        let overlay = setup_overlay(
            r#"
target = "~"
[defaults]
materialization = "symlink"

[[rules]]
path = "special"
materialization = "symlink-directory"
"#,
        );
        assert_eq!(
            resolve(&overlay, Path::new("special")),
            MaterializationKind::SymlinkDirectory
        );
        assert_eq!(
            resolve(&overlay, Path::new("other")),
            MaterializationKind::Symlink
        );
    }

    #[rstest]
    fn a_literal_rule_wins_over_an_overlapping_glob_rule() {
        let overlay = setup_overlay(
            r#"
target = "~"
[[rules]]
path = "some/*"
materialization = "symlink"

[[rules]]
path = "some/dir"
materialization = "symlink-directory"
"#,
        );
        // Both rules match "some/dir"; the literal, more specific one wins
        // regardless of declaration order.
        assert_eq!(
            resolve(&overlay, Path::new("some/dir")),
            MaterializationKind::SymlinkDirectory
        );
        // The glob-only rule still applies to paths the literal one
        // doesn't cover.
        assert_eq!(
            resolve(&overlay, Path::new("some/other")),
            MaterializationKind::Symlink
        );
    }

    #[rstest]
    fn link_dirs_resolves_through_the_same_mechanism_as_an_explicit_rule() {
        let overlay = setup_overlay("target = \"~\"\nlink_dirs = [\".config/nvim\"]");
        assert_eq!(
            resolve(&overlay, Path::new(".config/nvim")),
            MaterializationKind::SymlinkDirectory
        );
        assert_eq!(
            resolve(&overlay, Path::new(".config/other")),
            MaterializationKind::Symlink
        );
    }

    #[rstest]
    fn an_explicit_rule_wins_a_tie_against_a_link_dirs_pattern() {
        // Same exact path via both mechanisms, disagreeing on the result:
        // the explicit `rules` entry (added after `link_dirs`-derived
        // entries in `effective_rules`) must win.
        let overlay = setup_overlay(
            r#"
target = "~"
link_dirs = ["dual"]

[[rules]]
path = "dual"
materialization = "symlink"
"#,
        );
        assert_eq!(
            resolve(&overlay, Path::new("dual")),
            MaterializationKind::Symlink
        );
    }
}
