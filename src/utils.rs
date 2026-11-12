use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use dirs::home_dir;

/// Resolve the over home directory.
///
/// Uses `explicit` if provided, otherwise falls back to `~/.dotfiles`.
pub fn resolve_home(explicit: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(home) = explicit {
        // Invocation-relative CLI paths must become stable roots before configuration is resolved.
        crate::overlays::mapping::absolute(home)
    } else {
        let default = home_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?
            .join(".dotfiles");
        Ok(default)
    }
}

// Shorten a path by replacing the home directory with ~
pub fn short_path(path: &str) -> String {
    short_path_in(path, home_dir().as_deref())
}

fn short_path_in(path: &str, home: Option<&Path>) -> String {
    let Some(home_path) = home else {
        return path.to_string();
    };
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

/// Abbreviate path tokens in human diagnostics, never stored values or literal content.
/// Use [`short_path`] for complete paths: whitespace can also belong to a sibling name.
pub fn short_paths(text: &str) -> String {
    short_paths_in(text, home_dir().as_deref())
}

fn short_paths_in(text: &str, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return text.to_owned();
    };
    // A trailing separator in HOME must not consume the separator of a displayed child.
    let display = home.to_string_lossy();
    let plain = if home.parent().is_some() {
        display.trim_end_matches(std::path::is_separator)
    } else {
        &display
    };
    let mut result = abbreviate_tokens(text, plain);
    // Debug fields escape Windows separators and quotes, unlike Path::display().
    let debug = format!("{plain:?}");
    let escaped = &debug[1..debug.len() - 1];
    if escaped != plain {
        result = abbreviate_tokens(&result, escaped);
    }
    result
}

fn abbreviate_tokens(text: &str, home: &str) -> String {
    if home.is_empty() {
        return text.to_owned();
    }
    let mut result = String::with_capacity(text.len());
    let mut copied = 0;
    for (start, _) in text.match_indices(home) {
        let end = start + home.len();
        let mut before = &text[..start];
        // Styling must not hide the token boundary, or colored paths stay absolute.
        while before.ends_with('m') {
            let Some(escape) = before.rfind("\x1b[") else {
                break;
            };
            if !before[escape + 2..before.len() - 1]
                .chars()
                .all(|c| c.is_ascii_digit() || c == ';')
            {
                break;
            }
            before = &before[..escape];
        }
        let leading = before.chars().next_back().is_none_or(|c| {
            c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '(' | '[' | '{' | '=')
        });
        let mut suffix = text[end..].chars();
        let trailing = suffix.next().is_none_or(|c| {
            c == '/' || (cfg!(windows) && c == '\\')
                // A space may continue a sibling's name, rather than end the home path.
                || (c.is_whitespace() && (c == '\n' || c == '\r' || suffix.as_str().trim().is_empty()))
                || (matches!(c, '\'' | '"' | '`' | ')' | ']' | '}' | ',' | ';' | ':' | '\x1b')
                    // Punctuation inside a sibling name is not a path-component boundary.
                    && (c == '\x1b' || suffix.next().is_none_or(|next| {
                        next.is_whitespace() || matches!(next, '\'' | '"' | '`' | ')' | ']' | '}' | ',' | ';' | ':' | '\x1b')
                    })))
        });
        // An absolute-looking query parameter is still part of a remote URL.
        let in_url = before
            .split_whitespace()
            .next_back()
            .is_some_and(|token| token.contains("://"));
        // Both boundaries matter: neither /backup/home/user nor /home/user-other is home.
        if leading && trailing && !in_url {
            result.push_str(&text[copied..start]);
            result.push('~');
            copied = end;
        }
    }
    result.push_str(&text[copied..]);
    result
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

/// Distro ID (e.g. `"ubuntu"`, `"arch"`) for `machine.distro_id` templates.
/// Memoized because machine facts must remain consistent within an invocation.
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
    #[case("/home/alice", "~")]
    #[case("/home/alice/.config", "~/.config")]
    #[case("/home/alice-other/file", "/home/alice-other/file")]
    #[case("/backup/home/alice/file", "/backup/home/alice/file")]
    #[case("/tmp/file", "/tmp/file")]
    #[case("relative/file", "relative/file")]
    #[case("~/file", "~/file")]
    #[case("", "")]
    fn only_paths_with_the_home_component_prefix_are_abbreviated(
        #[case] input: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(
            short_path_in(input, Some(Path::new("/home/alice"))),
            expected
        );
    }

    #[rstest]
    #[case(
        "source '/home/alice/a' (referent: /home/alice/b)",
        "source '~/a' (referent: ~/b)"
    )]
    #[case("`/home/alice`\n  /home/alice/文 件", "`~`\n  ~/文 件")]
    #[case("\x1b[34m/home/alice/a\x1b[0m", "\x1b[34m~/a\x1b[0m")]
    #[case("path=\"/home/alice/a\"", "path=\"~/a\"")]
    #[case("path: /home/alice backup/file", "path: /home/alice backup/file")]
    #[case("path: '/home/alice backup/file'", "path: '/home/alice backup/file'")]
    #[case("/home/alice backup", "/home/alice backup")]
    #[case("path: /home/alice\n", "path: ~\n")]
    #[case("at /home/alice: missing", "at ~: missing")]
    #[case(
        "/home/alice,backup/a /home/alice:backup/a",
        "/home/alice,backup/a /home/alice:backup/a"
    )]
    #[case(
        "https://example.com/?path=/home/alice/a",
        "https://example.com/?path=/home/alice/a"
    )]
    #[case(
        "/home/alice-other /backup/home/alice/a",
        "/home/alice-other /backup/home/alice/a"
    )]
    #[case(
        "https://example.com/home/alice/a file:///home/alice/a host:/home/alice/a",
        "https://example.com/home/alice/a file:///home/alice/a host:/home/alice/a"
    )]
    fn diagnostic_paths_are_abbreviated_without_rewriting_other_tokens(
        #[case] input: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(
            short_paths_in(input, Some(Path::new("/home/alice"))),
            expected
        );
    }

    #[test]
    fn an_unknown_home_preserves_paths_and_diagnostics() {
        assert_eq!(short_path_in("/home/alice/a", None), "/home/alice/a");
        assert_eq!(short_paths_in("at /home/alice/a", None), "at /home/alice/a");
    }

    #[test]
    fn home_names_with_spaces_and_unicode_are_matched_as_a_whole() {
        assert_eq!(
            short_paths_in("at '/home/文 件/a'", Some(Path::new("/home/文 件"))),
            "at '~/a'"
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_and_debug_escaped_windows_paths_are_abbreviated() {
        let home = Path::new(r"C:\Users\alice");
        assert_eq!(
            short_paths_in(r"at C:\Users\alice\a", Some(home)),
            r"at ~\a"
        );
        assert_eq!(
            short_paths_in(r#"path="C:\\Users\\alice\\a""#, Some(home)),
            r#"path="~\\a""#
        );
    }

    #[test]
    fn a_home_with_a_trailing_separator_keeps_the_child_separator() {
        assert_eq!(
            short_paths_in("at /home/alice/a", Some(Path::new("/home/alice/"))),
            "at ~/a"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_backslash_in_a_unix_sibling_name_is_not_a_separator() {
        assert_eq!(
            short_paths_in(r"/home/alice\other/a", Some(Path::new("/home/alice"))),
            r"/home/alice\other/a"
        );
    }

    #[rstest]
    #[case::home_dir(&home(), "~")]
    #[case::home_dir_with_path(&format!("{}/.config", home()), "~/.config")]
    #[case::outside_home("/tmp", "/tmp")]
    fn short_path_replaces_home_directory_with_tilde(#[case] path: &str, #[case] expected: &str) {
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
    fn longest_common_suffix_finds_shared_trailing_path_segments(
        #[case] a: &str,
        #[case] b: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(longest_common_suffix(a, b), expected);
    }

    #[test]
    fn short_path_shortens_deeply_nested_home_paths() {
        let h = home();
        let path = format!("{}/a/b/c/d/e/f.txt", h);
        let result = short_path(&path);
        assert_eq!(result, "~/a/b/c/d/e/f.txt");
    }

    #[test]
    fn short_path_of_empty_string_returns_empty_string() {
        let result = short_path("");
        assert_eq!(result, "");
    }

    #[test]
    fn resolve_home_with_explicit_path() {
        let explicit = std::env::temp_dir().join("custom-over");
        let result = resolve_home(Some(&explicit)).unwrap();
        assert_eq!(result, explicit);
    }

    #[test]
    fn resolve_home_defaults_to_dotfiles() {
        let result = resolve_home(None).unwrap();
        let expected = home_dir().unwrap().join(".dotfiles");
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
