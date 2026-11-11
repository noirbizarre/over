//! Ordered, declaration-relative configuration policy shared by runtime and lint.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};

use super::overlay::Overlay;
use super::permissions::FileMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MaterializationKind {
    #[default]
    Symlink,
    SymlinkDirectory,
    /// Editable files backed by the overlay repository, not a declared Git clone.
    Checkout,
}

impl fmt::Display for MaterializationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Symlink => "symlink",
            Self::SymlinkDirectory => "symlink-directory",
            Self::Checkout => "checkout",
        })
    }
}

/// Both properties share selectors and ordering, but assignments are independent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// A present-but-null assignment does not satisfy runtime rule validation.
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields, extend(
    "anyOf" = [
        {"required": ["mode"], "properties": {"mode": {"not": {"type": "null"}}}},
        {"required": ["permissions"], "properties": {"permissions": {"not": {"type": "null"}}}}
    ],
    "if" = {"properties": {"path": {"pattern": "[*?\\[{]"}}, "required": ["path"]},
    "then" = {"properties": {"mode": {"const": "symlink-directory"}}, "required": ["mode"]}
)))]
pub struct Rule {
    #[cfg_attr(feature = "schema", schemars(length(min = 1), extend("pattern" = r"\S")))]
    pub path: String,
    pub mode: Option<MaterializationKind>,
    pub permissions: Option<FileMode>,
}

impl Rule {
    pub fn is_glob(&self) -> bool {
        self.path.contains(['*', '?', '[', '{'])
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.mode.is_some() || self.permissions.is_some(),
            "rule '{}' must set mode or permissions",
            self.path
        );
        ensure!(!self.path.trim().is_empty(), "rule path must not be empty");
        if self.is_glob() {
            ensure!(
                self.mode == Some(MaterializationKind::SymlinkDirectory),
                "glob rule '{}' requires mode: symlink-directory; other rules use literal path prefixes",
                self.path
            );
            GlobBuilder::new(&self.path)
                .literal_separator(true)
                .build()?;
        }
        Ok(())
    }

    pub fn matches(&self, relative: &Path, is_directory: bool) -> bool {
        if self.is_glob() {
            is_directory
                && GlobBuilder::new(&self.path)
                    .literal_separator(true)
                    .build()
                    .is_ok_and(|glob| glob.compile_matcher().is_match(relative))
        } else {
            relative.starts_with(Path::new(&self.path)) || self.path == "."
        }
    }
}

/// Kept before config merging, which otherwise replaces ancestor rule arrays.
#[derive(Debug, Clone, Default)]
pub struct PolicyScope {
    pub root: PathBuf,
    pub descriptor: PathBuf,
    pub mode: Option<MaterializationKind>,
    pub permissions: Option<FileMode>,
    pub rules: Vec<Rule>,
    pub exclude: Option<Vec<String>>,
}

impl PolicyScope {
    pub fn matches(&self, rule: &Rule, source: &Path, is_directory: bool) -> bool {
        if rule.is_glob() {
            let relative = if Path::new(&rule.path).is_absolute() {
                Some(source)
            } else {
                source.strip_prefix(&self.root).ok()
            };
            relative.is_some_and(|path| rule.matches(path, is_directory))
        } else {
            source.starts_with(super::mapping::lexical(&self.root.join(&rule.path)))
        }
    }
}

/// Keep the declaring scope so diagnostics can explain inherited winners;
/// `None` identifies its baseline, while `Some` is a zero-based rule index.
pub(crate) type AssignmentOrigin<'a> = (&'a PolicyScope, Option<usize>);

pub(crate) struct ResolvedProperties<'a> {
    pub mode: MaterializationKind,
    pub permissions: Option<FileMode>,
    pub mode_origin: Option<AssignmentOrigin<'a>>,
    pub permissions_origin: Option<AssignmentOrigin<'a>>,
}

pub(crate) fn resolve_with_origins<'a>(
    overlay: &'a Overlay,
    relative: &Path,
    is_directory: bool,
) -> ResolvedProperties<'a> {
    let mut resolved = ResolvedProperties {
        mode: MaterializationKind::Symlink,
        permissions: None,
        mode_origin: None,
        permissions_origin: None,
    };
    let source = overlay
        .scopes
        .last()
        .map(|scope| &scope.root)
        .unwrap_or(&overlay.root)
        .join(relative);
    for scope in &overlay.scopes {
        if let Some(value) = scope.mode {
            resolved.mode = value;
            resolved.mode_origin = Some((scope, None));
        }
        if let Some(value) = scope.permissions {
            resolved.permissions = Some(value);
            resolved.permissions_origin = Some((scope, None));
        }
        for (index, rule) in scope.rules.iter().enumerate() {
            if scope.matches(rule, &source, is_directory) {
                if let Some(value) = rule.mode {
                    resolved.mode = value;
                    resolved.mode_origin = Some((scope, Some(index)));
                }
                if let Some(value) = rule.permissions {
                    resolved.permissions = Some(value);
                    resolved.permissions_origin = Some((scope, Some(index)));
                }
            }
        }
    }
    resolved
}

pub(super) fn resolve_properties(
    overlay: &Overlay,
    relative: &Path,
    is_directory: bool,
) -> (MaterializationKind, Option<FileMode>) {
    let resolved = resolve_with_origins(overlay, relative, is_directory);
    (resolved.mode, resolved.permissions)
}

pub(super) fn resolve(overlay: &Overlay, relative: &Path) -> MaterializationKind {
    resolve_properties(overlay, relative, true).0
}

/// Return the final assignment so lint can name the boundary runtime actually uses.
pub(crate) fn mode_origin<'a>(
    overlay: &'a Overlay,
    relative: &Path,
) -> Option<(&'a PolicyScope, Option<usize>)> {
    resolve_with_origins(overlay, relative, true).mode_origin
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlays::Repository;
    use assert_fs::prelude::*;

    #[test]
    fn inherited_rules_keep_their_scope_and_later_broad_rules_win() {
        let root = assert_fs::TempDir::new().unwrap();
        root.child("over.yaml")
            .write_str("mode: checkout\nrules:\n  - {path: ov/config, mode: symlink-directory}\n")
            .unwrap();
        root.child("ov/over.yaml").write_str("rules:\n  - {path: config/editor, mode: checkout}\n  - {path: config, mode: symlink}\n").unwrap();
        let overlay = Repository::new(root.path().to_owned()).get("ov").unwrap();
        assert_eq!(
            resolve(&overlay, Path::new("other")),
            MaterializationKind::Checkout
        );
        assert_eq!(
            resolve(&overlay, Path::new("config/editor")),
            MaterializationKind::Symlink
        );
    }

    #[test]
    fn a_scope_baseline_resets_earlier_rules_without_resetting_permissions() {
        let root = assert_fs::TempDir::new().unwrap();
        root.child("over.yaml")
            .write_str("permissions: '600'\nrules:\n  - {path: ov/config, mode: checkout}\n")
            .unwrap();
        root.child("ov/over.yaml")
            .write_str("mode: symlink\nrules:\n  - {path: config, permissions: '644'}\n")
            .unwrap();
        let overlay = Repository::new(root.path().to_owned()).get("ov").unwrap();
        assert_eq!(
            resolve_properties(&overlay, Path::new("config/editor"), false),
            (
                MaterializationKind::Symlink,
                Some(FileMode::parse("644").unwrap())
            )
        );
        assert_eq!(
            overlay.permission_for(Path::new("other")),
            Some(FileMode::parse("600").unwrap())
        );
    }

    #[test]
    fn literal_prefixes_respect_component_boundaries() {
        let rule = Rule {
            path: "foo".into(),
            mode: Some(MaterializationKind::Symlink),
            permissions: None,
        };
        assert!(rule.matches(Path::new("foo/bar"), true));
        assert!(!rule.matches(Path::new("foobar"), true));
    }

    #[test]
    fn only_directory_symlink_rules_allow_globs_and_select_directory_boundaries() {
        let mut rule = Rule {
            path: "config/*".into(),
            mode: Some(MaterializationKind::SymlinkDirectory),
            permissions: None,
        };
        assert!(rule.validate().is_ok());
        assert!(rule.matches(Path::new("config/editor"), true));
        assert!(!rule.matches(Path::new("config/editor"), false));
        assert!(!rule.matches(Path::new("config/editor/nested"), true));
        rule.mode = Some(MaterializationKind::Checkout);
        assert!(rule.validate().is_err());
    }
}
