use std::env::current_dir;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use dirs::home_dir;

use crate::exec;
use crate::overlays::Overlay;

/// Authorize one removal without granting permission to bypass its domain safeguards.
pub(crate) fn confirm_removal(message: &str, yes: bool, no_prompt: bool) -> Result<()> {
    confirm_removal_with(
        yes,
        !no_prompt && std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        || {
            let _pause = crate::ui::progress::pause()?;
            choisir::Confirm::with_theme(&crate::ui::style::DialogTheme::default())
                .with_prompt(message)
                .default(false)
                .interact()
                .context("could not read removal confirmation")
        },
    )
}

fn confirm_removal_with(
    yes: bool,
    can_prompt: bool,
    prompt: impl FnOnce() -> Result<bool>,
) -> Result<()> {
    // Piped input and --no-prompt must never accidentally authorize loss.
    ensure!(
        yes || can_prompt,
        "confirmation required; use --yes deliberately"
    );
    ensure!(
        yes || prompt()?,
        "removal cancelled; use --yes to explicitly approve"
    );
    Ok(())
}

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
#[cfg(test)]
pub fn resolve_inputs(inputs: &[String]) -> Result<Vec<PathBuf>> {
    resolve_inputs_with_missing(inputs, false)
}

/// Checkout staging accepts missing literal paths to record tracked deletions.
pub(crate) fn resolve_inputs_with_missing(
    inputs: &[String],
    allow_missing: bool,
) -> Result<Vec<PathBuf>> {
    let cwd = current_dir()?;
    let mut resolved = Vec::new();

    for input in inputs {
        let expanded = expand_tilde(input);
        let pattern_str = expanded.to_string_lossy();

        if is_glob_pattern(&pattern_str) {
            let matches: Vec<_> = glob::glob(&pattern_str)
                .map_err(|e| anyhow!("invalid glob pattern '{}': {}", input, e))?
                .filter_map(|entry| entry.ok())
                .collect();

            if matches.is_empty() {
                bail!("no files matched pattern '{}'", input);
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
            if !allow_missing && !abs.exists() {
                bail!("{} does not exist", abs.display());
            }
            resolved.push(abs);
        }
    }

    Ok(resolved)
}

/// Resolve single-overlay commands, asking for deliberate selection when necessary.
pub fn select_overlay(ctx: &exec::Context, name: Option<&str>) -> Result<Overlay> {
    super::selection::select(ctx, name, &[], false)
}

/// Re-run a cwd-sensitive test alone, including when libtest runs tests in parallel.
#[cfg(test)]
pub(crate) fn run_in_isolated_process() -> bool {
    const CHILD: &str = "OVER_CLI_ISOLATED_TEST";
    let thread = std::thread::current();
    let name = thread.name().expect("test thread has a name");
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let temp = assert_fs::TempDir::new().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, name)
        // Set process-global inputs before any child runtime threads start.
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{name}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    #[rstest]
    #[case(false, false, false, false, false)]
    #[case(false, true, false, true, false)]
    #[case(false, true, true, true, true)]
    #[case(true, false, false, false, true)]
    #[case(true, true, false, false, true)]
    fn only_explicit_consent_authorizes_removal(
        #[case] yes: bool,
        #[case] can_prompt: bool,
        #[case] answer: bool,
        #[case] should_prompt: bool,
        #[case] authorized: bool,
    ) {
        let mut prompted = false;
        let result = confirm_removal_with(yes, can_prompt, || {
            prompted = true;
            Ok(answer)
        });
        assert_eq!(result.is_ok(), authorized);
        assert_eq!(prompted, should_prompt);
    }

    #[test]
    fn a_failed_confirmation_never_authorizes_removal() {
        let error = confirm_removal_with(false, true, || bail!("terminal closed")).unwrap_err();
        assert!(error.to_string().contains("terminal closed"));
    }

    #[rstest]
    #[case(None, None, false)]
    #[case(Some("ov"), None, true)]
    #[case(None, Some("ov"), false)]
    #[case(None, Some("missing"), false)]
    fn noninteractive_selection_requires_explicit_or_active_context(
        #[case] name: Option<&str>,
        #[case] default: Option<&str>,
        #[case] succeeds: bool,
    ) {
        if run_in_isolated_process() {
            return;
        }
        let temp = TempDir::new().unwrap();
        temp.child("ov/over.toml")
            .write_str("target = '~'")
            .unwrap();
        if let Some(default) = default {
            temp.child("over.toml")
                .write_str(&format!("host_overlay = '{default}'"))
                .unwrap();
        }
        let repo = Repository::new(temp.path().to_path_buf());
        let ctx = exec::Context::builder()
            .repository(repo.clone())
            .no_prompt(true)
            .build();
        let result = select_overlay(&ctx, name);
        assert_eq!(result.is_ok(), succeeds);
        if name.is_none() && default.is_none() {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("cannot infer an overlay")
            );
        }
    }

    #[rstest]
    #[case("*.txt", true)]
    #[case("src/**/*.rs", true)]
    #[case("file?.log", true)]
    #[case("[abc].txt", true)]
    #[case("{a,b}.txt", true)]
    #[case("plain.txt", false)]
    #[case("path/to/file.rs", false)]
    #[case("no-special-chars", false)]
    fn is_glob_pattern_detects_glob_metacharacters(#[case] input: &str, #[case] expected: bool) {
        assert_eq!(is_glob_pattern(input), expected);
    }

    #[rstest]
    #[case("~/Documents", true)]
    #[case("~", true)]
    #[case("/absolute/path", false)]
    #[case("relative/path", false)]
    #[case("~user/path", false)] // only ~/... is expanded, not ~user/
    fn tilde_prefixed_paths_expand_to_home_others_unchanged(
        #[case] input: &str,
        #[case] should_expand: bool,
    ) {
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
    fn bare_tilde_expands_to_home_dir_exactly() {
        let result = expand_tilde("~");
        if let Some(home) = home_dir() {
            assert_eq!(result, home);
        }
    }

    #[test]
    fn resolve_inputs_returns_absolute_path_for_existing_file() {
        let td = TempDir::new().unwrap();
        let file = td.child("hello.txt");
        file.touch().unwrap();

        let path_str = file.path().to_string_lossy().to_string();
        let resolved = resolve_inputs(&[path_str]).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], file.path());
    }

    #[test]
    fn resolve_inputs_errors_on_nonexistent_path() {
        let result = resolve_inputs(&["/nonexistent/path/file.txt".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_inputs_expands_glob_to_matching_files() {
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
    fn resolve_inputs_errors_when_glob_matches_nothing() {
        let td = TempDir::new().unwrap();
        let pattern = format!("{}/*.nonexistent", td.path().display());
        let result = resolve_inputs(&[pattern]);
        assert!(result.is_err());
    }

    #[test]
    fn select_overlay_explicit_name_wins_over_default() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("over.toml"),
            "host_overlay = \"default-one\"",
        )
        .unwrap();
        for name in ["default-one", "explicit-one"] {
            let ov = tmp.path().join(name);
            std::fs::create_dir_all(&ov).unwrap();
            std::fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        }

        let repo = Repository::new(tmp.path().to_path_buf());
        let ctx = exec::Context::builder().repository(repo.clone()).build();
        let overlay = select_overlay(&ctx, Some("explicit-one")).unwrap();
        assert_eq!(overlay.name, "explicit-one");
    }

    #[test]
    fn select_overlay_does_not_use_the_host_when_context_is_absent() {
        if run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("over.toml"),
            "host_overlay = \"myoverlay\"\napply.default_to_host = true",
        )
        .unwrap();
        let ov = tmp.path().join("myoverlay");
        std::fs::create_dir_all(&ov).unwrap();
        std::fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let repo = Repository::new(tmp.path().to_path_buf());
        let ctx = exec::Context::builder().repository(repo.clone()).build();
        assert!(select_overlay(&ctx, None).is_err());
    }

    #[test]
    fn select_overlay_no_default_and_empty_repo_errors() {
        if run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let ctx = exec::Context::builder().repository(repo.clone()).build();
        let result = select_overlay(&ctx, None);
        assert!(result.is_err());
    }

    #[test]
    fn select_overlay_misconfigured_default_errors_without_prompting() {
        if run_in_isolated_process() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("over.toml"),
            "host_overlay = \"does-not-exist\"",
        )
        .unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let ctx = exec::Context::builder().repository(repo.clone()).build();
        // Must error, not silently fall through to an interactive prompt
        // that would hang the test suite waiting on stdin.
        let result = select_overlay(&ctx, None);
        assert!(result.is_err());
    }
}
