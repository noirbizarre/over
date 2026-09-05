use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use dirs::home_dir;

/// Resolve the over home directory.
///
/// Uses `explicit` if provided, otherwise falls back to `~/.over`.
pub fn resolve_home(explicit: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(home) = explicit {
        Ok(home.clone())
    } else {
        let default = home_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?
            .join(".over");
        Ok(default)
    }
}

// Shorten a path by replacing the home directory with ~
pub fn short_path(path: &str) -> String {
    let Some(home) = home_dir() else {
        return path.to_string();
    };
    let home_path = home.as_path();
    let p = Path::new(path);
    if let Ok(suffix) = p.strip_prefix(home_path) {
        if suffix.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", suffix.display())
        }
    } else {
        path.to_string()
    }
}

/// Parse a single `KEY=value` line from `/etc/os-release`-formatted content,
/// stripping surrounding quotes (values are often quoted, e.g. `NAME="Ubuntu"`).
fn parse_os_release_field(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    content
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .map(|value| value.trim_matches('"').to_string())
}

/// Distro ID (e.g. `"ubuntu"`, `"arch"`) from `/etc/os-release`. Used both for
/// package-manager resolution (`actions::install`) and the `machine.distro_id`
/// template variable. Memoized: the file never changes mid-process.
pub fn detect_linux_distro_id() -> Option<String> {
    static DISTRO_ID: OnceLock<Option<String>> = OnceLock::new();
    DISTRO_ID
        .get_or_init(|| {
            let content = fs::read_to_string("/etc/os-release").ok()?;
            parse_os_release_field(&content, "ID")
        })
        .clone()
}

/// Human-readable distro name (e.g. `"Ubuntu"`, `"Arch Linux"`) from
/// `/etc/os-release`, exposed to templates as `machine.distro`.
pub fn detect_linux_distro_name() -> Option<String> {
    static DISTRO_NAME: OnceLock<Option<String>> = OnceLock::new();
    DISTRO_NAME
        .get_or_init(|| {
            let content = fs::read_to_string("/etc/os-release").ok()?;
            parse_os_release_field(&content, "NAME")
        })
        .clone()
}

// Find the longest common suffix between two strings
// Returns an empty string if no common suffix is found
#[allow(dead_code)]
pub fn longest_common_suffix<'a>(a: &'a str, b: &'a str) -> String {
    let reversed = a
        .chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a)
        .collect::<String>();
    reversed.chars().rev().collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::*;

    fn home() -> String {
        let h = home_dir().unwrap();
        h.to_str().unwrap().to_string()
    }

    #[rstest]
    #[case::home_dir(&home(), "~")]
    #[case::home_dir_with_path(&format!("{}/.config", home()), "~/.config")]
    #[case::outside_home("/tmp", "/tmp")]
    fn test_short_path(#[case] path: &str, #[case] expected: &str) {
        assert_eq!(short_path(path), expected.to_string());
    }

    #[rstest]
    #[case::empty("", "", "")]
    #[case::no_common_suffix("hello", "world", "")]
    #[case::common_suffix(
        "~/.dotfiles/path/to/overlay",
        "somewhere/path/to/overlay",
        "/path/to/overlay"
    )]
    #[case::common_unicode_suffix(
        "~/.dötfiles/päth/tö/ovërlay",
        "sömewhere/päth/tö/ovërlay",
        "/päth/tö/ovërlay"
    )]
    fn test_longest_common_suffix(#[case] a: &str, #[case] b: &str, #[case] expected: &str) {
        assert_eq!(longest_common_suffix(a, b), expected);
    }

    #[test]
    fn test_short_path_deeply_nested() {
        let h = home();
        let path = format!("{}/a/b/c/d/e/f.txt", h);
        let result = short_path(&path);
        assert_eq!(result, "~/a/b/c/d/e/f.txt");
    }

    #[test]
    fn test_short_path_empty_string() {
        let result = short_path("");
        assert_eq!(result, "");
    }

    #[test]
    fn resolve_home_with_explicit_path() {
        let explicit = PathBuf::from("/custom/over");
        let result = resolve_home(Some(&explicit)).unwrap();
        assert_eq!(result, PathBuf::from("/custom/over"));
    }

    #[test]
    fn resolve_home_defaults_to_dot_over() {
        let result = resolve_home(None).unwrap();
        let expected = home_dir().unwrap().join(".over");
        assert_eq!(result, expected);
    }

    #[test]
    fn parse_os_release_field_extracts_quoted_and_unquoted_values() {
        let content = "ID=ubuntu\nNAME=\"Ubuntu\"\nVERSION_ID=\"22.04\"\n";
        assert_eq!(
            parse_os_release_field(content, "ID"),
            Some("ubuntu".to_string())
        );
        assert_eq!(
            parse_os_release_field(content, "NAME"),
            Some("Ubuntu".to_string())
        );
    }

    #[test]
    fn parse_os_release_field_missing_key_returns_none() {
        let content = "ID=arch\n";
        assert_eq!(parse_os_release_field(content, "NAME"), None);
    }
}
