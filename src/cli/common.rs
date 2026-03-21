use std::env::current_dir;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use dirs::home_dir;

/// Check whether a string contains glob metacharacters.
pub fn is_glob_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[') || s.contains('{')
}

/// Expand a tilde prefix in a path string to the home directory.
pub fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    } else if s == "~"
        && let Some(home) = home_dir()
    {
        return home;
    }
    PathBuf::from(s)
}

/// Resolve a list of input strings (which may be globs, tildes, relative paths,
/// directories, or plain files) into a flat list of absolute paths.
pub fn resolve_inputs(inputs: &[String]) -> Result<Vec<PathBuf>> {
    let cwd = current_dir()?;
    let mut resolved = Vec::new();

    for input in inputs {
        let expanded = expand_tilde(input);
        let pattern_str = expanded.to_string_lossy();

        if is_glob_pattern(&pattern_str) {
            let matches: Vec<_> = glob::glob(&pattern_str)
                .map_err(|e| anyhow!("Invalid glob pattern '{}': {}", input, e))?
                .filter_map(|entry| entry.ok())
                .collect();

            if matches.is_empty() {
                return Err(anyhow!("No files matched pattern '{}'", input));
            }

            for path in matches {
                let abs = if path.is_relative() {
                    cwd.join(&path)
                } else {
                    path
                };
                resolved.push(abs);
            }
        } else {
            let abs = if expanded.is_relative() {
                cwd.join(&expanded)
            } else {
                expanded
            };
            if !abs.exists() {
                return Err(anyhow!("{} does not exist", abs.display()));
            }
            resolved.push(abs);
        }
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    #[rstest]
    #[case("*.txt", true)]
    #[case("src/**/*.rs", true)]
    #[case("file?.log", true)]
    #[case("[abc].txt", true)]
    #[case("{a,b}.txt", true)]
    #[case("plain.txt", false)]
    #[case("path/to/file.rs", false)]
    #[case("no-special-chars", false)]
    fn test_is_glob_pattern(#[case] input: &str, #[case] expected: bool) {
        assert_eq!(is_glob_pattern(input), expected);
    }

    #[rstest]
    #[case("~/Documents", true)]
    #[case("~", true)]
    #[case("/absolute/path", false)]
    #[case("relative/path", false)]
    #[case("~user/path", false)] // only ~/... is expanded, not ~user/
    fn test_expand_tilde(#[case] input: &str, #[case] should_expand: bool) {
        let result = expand_tilde(input);
        if should_expand {
            if let Some(home) = home_dir() {
                assert!(
                    result.starts_with(&home),
                    "expected {:?} to start with home dir {:?}",
                    result,
                    home
                );
            }
        } else {
            // Should be returned as-is
            assert_eq!(result, PathBuf::from(input));
        }
    }

    #[test]
    fn test_expand_tilde_bare() {
        let result = expand_tilde("~");
        if let Some(home) = home_dir() {
            assert_eq!(result, home);
        }
    }

    #[test]
    fn test_resolve_inputs_absolute_file() {
        let td = TempDir::new().unwrap();
        let file = td.child("hello.txt");
        file.touch().unwrap();

        let path_str = file.path().to_string_lossy().to_string();
        let resolved = resolve_inputs(&[path_str]).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], file.path());
    }

    #[test]
    fn test_resolve_inputs_nonexistent_errors() {
        let result = resolve_inputs(&["/nonexistent/path/file.txt".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_inputs_glob_expands() {
        let td = TempDir::new().unwrap();
        td.child("a.txt").touch().unwrap();
        td.child("b.txt").touch().unwrap();
        td.child("c.rs").touch().unwrap();

        let pattern = format!("{}/*.txt", td.path().display());
        let resolved = resolve_inputs(&[pattern]).unwrap();
        assert_eq!(resolved.len(), 2);
        assert!(resolved.iter().all(|p| p.extension().unwrap() == "txt"));
    }

    #[test]
    fn test_resolve_inputs_glob_no_match_errors() {
        let td = TempDir::new().unwrap();
        let pattern = format!("{}/*.nonexistent", td.path().display());
        let result = resolve_inputs(&[pattern]);
        assert!(result.is_err());
    }
}
