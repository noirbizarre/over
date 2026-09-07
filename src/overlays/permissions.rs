//! File permission rules (#65): a default permission mode plus
//! path/subtree overrides, resolved into a [`FileMode`] for entries `over`
//! writes real content for directly.
//!
//! Mirrors [`super::rules`] structurally (same `defaults`/`rules`-shaped
//! config, same specificity precedence), but resolves against a
//! [`crate::desired::MaterializationIntent::PartialFile`] entry's own
//! sidecar-relative path (its stem), not a path walked from the overlay's
//! own file tree — symlinked entries share their source's inode and have
//! no independent permission of their own to manage this way (see
//! ADR-020).

use std::fmt;
use std::num::ParseIntError;
use std::path::Path;

use globset::GlobBuilder;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::overlay::Overlay;

/// A desired unix permission mode: the low 12 bits (`rwxrwxrwx` plus
/// setuid/setgid/sticky), declared in config as an octal string (`"644"`,
/// `"0755"`, `"4755"`) and enforced only for entries `over` writes real
/// content for (currently `MaterializationIntent::PartialFile`, #65).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMode(u32);

impl FileMode {
    /// Parse an octal permission string, masked to the low 12 bits so a
    /// stray leading digit (or an accidentally-decimal value) can never
    /// escape into filetype bits `std::fs::Permissions` doesn't expose
    /// anyway.
    pub fn parse(s: &str) -> Result<Self, ParseIntError> {
        let trimmed = s.trim_start_matches("0o");
        let value = u32::from_str_radix(trimmed, 8)?;
        Ok(Self(value & 0o7777))
    }

    /// Build a `FileMode` from raw mode bits (e.g. `Permissions::mode()`),
    /// masking to the same low 12 bits `parse` does.
    pub fn from_bits(bits: u32) -> Self {
        Self(bits & 0o7777)
    }

    pub fn bits(&self) -> u32 {
        self.0
    }
}

impl fmt::Display for FileMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:o}", self.0)
    }
}

impl Serialize for FileMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for FileMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        FileMode::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// A single path/subtree permission override. `path` matches a
/// [`MaterializationIntent::PartialFile`](crate::desired::MaterializationIntent::PartialFile)
/// entry's own sidecar stem (e.g. `ssh/agent` for `ssh/agent.partial.toml`),
/// not a real file in the overlay's walked tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub path: String,
    pub mode: FileMode,
}

impl PermissionRule {
    /// Whether `rel_path` matches this rule's `path` glob. A malformed
    /// glob never matches — mirrors `MaterializationRule::matches`.
    fn matches(&self, rel_path: &Path) -> bool {
        GlobBuilder::new(&self.path)
            .literal_separator(true)
            .build()
            .ok()
            .is_some_and(|glob| glob.compile_matcher().is_match(rel_path))
    }

    /// Same specificity ranking as `MaterializationRule::specificity`: a
    /// literal pattern always outranks a glob, and within the same tier a
    /// longer pattern is more specific.
    fn specificity(&self) -> (bool, usize) {
        let is_literal = !self.path.contains(['*', '?', '[', '{']);
        (is_literal, self.path.len())
    }
}

/// Resolve the effective [`FileMode`] for `rel_path`, in precedence order:
///
/// 1. the most specific matching `permissions` entry;
/// 2. `defaults.mode`;
/// 3. `None` — no declared permission, left entirely unmanaged (today's
///    behavior for anyone who hasn't opted in).
pub(super) fn resolve(overlay: &Overlay, rel_path: &Path) -> Option<FileMode> {
    let rules = overlay.permissions.iter().flatten();
    let best = rules
        .filter(|rule| rule.matches(rel_path))
        .max_by_key(|rule| rule.specificity());
    match best {
        Some(rule) => Some(rule.mode),
        None => overlay.defaults.and_then(|d| d.mode),
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
    #[case("644")]
    #[case("0644")]
    fn file_mode_parses_octal_strings_with_and_without_a_leading_zero(#[case] input: &str) {
        assert_eq!(FileMode::parse(input).unwrap().bits(), 0o644);
    }

    #[test]
    fn file_mode_display_renders_octal_without_leading_zero() {
        assert_eq!(FileMode::parse("755").unwrap().to_string(), "755");
    }

    #[test]
    fn file_mode_masks_extraneous_high_bits() {
        // A raw `st_mode` value carries file-type bits above the
        // permission bits (e.g. `0o100644` for a regular file) — only the
        // low 12 bits are ever meaningful here.
        assert_eq!(FileMode::from_bits(0o100644).bits(), 0o644);
    }

    #[test]
    fn file_mode_invalid_octal_string_is_an_error() {
        assert!(FileMode::parse("not-octal").is_err());
        assert!(FileMode::parse("999").is_err());
    }

    #[rstest]
    fn no_defaults_no_rules_resolves_to_none() {
        let overlay = setup_overlay("target = \"~\"");
        assert_eq!(resolve(&overlay, Path::new("anything")), None);
    }

    #[rstest]
    fn defaults_mode_applies_to_every_unmatched_path() {
        let overlay = setup_overlay("target = \"~\"\n[defaults]\nmode = \"600\"");
        assert_eq!(
            resolve(&overlay, Path::new("any/path")),
            Some(FileMode::parse("600").unwrap())
        );
    }

    #[rstest]
    fn a_rule_overrides_the_default_for_its_own_path_only() {
        let overlay = setup_overlay(
            r#"
target = "~"
[defaults]
mode = "644"

[[permissions]]
path = "secrets"
mode = "600"
"#,
        );
        assert_eq!(
            resolve(&overlay, Path::new("secrets")),
            Some(FileMode::parse("600").unwrap())
        );
        assert_eq!(
            resolve(&overlay, Path::new("other")),
            Some(FileMode::parse("644").unwrap())
        );
    }

    #[rstest]
    fn a_literal_rule_wins_over_an_overlapping_glob_rule() {
        let overlay = setup_overlay(
            r#"
target = "~"
[[permissions]]
path = "ssh/*"
mode = "644"

[[permissions]]
path = "ssh/agent"
mode = "600"
"#,
        );
        assert_eq!(
            resolve(&overlay, Path::new("ssh/agent")),
            Some(FileMode::parse("600").unwrap())
        );
        assert_eq!(
            resolve(&overlay, Path::new("ssh/config")),
            Some(FileMode::parse("644").unwrap())
        );
    }

    #[rstest]
    fn no_matching_rule_and_no_defaults_is_none_even_with_other_rules_present() {
        let overlay = setup_overlay(
            r#"
target = "~"
[[permissions]]
path = "secrets"
mode = "600"
"#,
        );
        assert_eq!(resolve(&overlay, Path::new("unrelated")), None);
    }
}
