//! XDG Base Directory layout for `over`.
//!
//! Resolves the three directories `over` is allowed to touch outside of the
//! overlay repository itself (`~/.over`, resolved separately by
//! [`crate::utils::resolve_home`]):
//!
//! - `config_dir` (`$XDG_CONFIG_HOME/over`) — reserved for future user
//!   configuration; nothing reads or writes it yet.
//! - `state_dir` (`$XDG_STATE_HOME/over`) — persistent operational metadata
//!   (see [`state`]). Deleting it should only lose recoverable checkpoints,
//!   never authoritative data.
//! - `cache_dir` (`$XDG_CACHE_HOME/over`) — disposable derived data. Nothing
//!   may depend on its presence: deleting it must always be harmless.
//!
//! `over` deliberately follows the XDG Base Directory Specification on every
//! platform (unlike the `dirs`/`directories` crates, which map to
//! platform-native locations on macOS/Windows) so behavior stays predictable
//! across environments.

pub mod state;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use etcetera::app_strategy::{self, AppStrategy, AppStrategyArgs};

/// Resolved XDG directories for `over`.
pub struct XdgDirs {
    inner: app_strategy::Xdg,
}

impl XdgDirs {
    /// Resolves `over`'s XDG directories from the environment.
    pub fn new() -> Result<Self> {
        let inner = app_strategy::Xdg::new(AppStrategyArgs {
            // Xdg ignores qualifier/author (they only affect the Apple/Windows
            // strategies), but `AppStrategyArgs` still requires them.
            top_level_domain: String::new(),
            author: String::new(),
            app_name: "over".to_string(),
        })
        .context("could not determine home directory")?;
        Ok(Self { inner })
    }

    /// `$XDG_CONFIG_HOME/over`, defaulting to `~/.config/over`.
    ///
    /// Reserved for future user configuration; not read or written yet.
    pub fn config_dir(&self) -> PathBuf {
        self.inner.config_dir()
    }

    /// `$XDG_STATE_HOME/over`, defaulting to `~/.local/state/over`.
    pub fn state_dir(&self) -> PathBuf {
        self.inner
            .state_dir()
            // The Xdg strategy always resolves a state dir (only the Apple/
            // Windows strategies return `None` here), so this never fires.
            .expect("Xdg strategy always provides a state_dir")
    }

    /// `$XDG_CACHE_HOME/over`, defaulting to `~/.cache/over`.
    ///
    /// Nothing depends on this directory existing or its contents surviving:
    /// deleting it must always be harmless.
    pub fn cache_dir(&self) -> PathBuf {
        self.inner.cache_dir()
    }

    /// Creates `path` (and its parents) if it doesn't exist yet.
    ///
    /// XDG directories are never guaranteed to pre-exist (the spec requires
    /// applications to create them on demand), so every writer must call
    /// this before touching a file inside one.
    pub fn ensure_dir(path: &Path) -> Result<()> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create directory {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clears every XDG env var so tests observe the documented defaults.
    ///
    /// # Safety
    /// `env::remove_var` is only unsound when other threads read/write the
    /// process environment concurrently; `cargo nextest` runs each test in
    /// its own process, so this is safe in practice for this test suite.
    fn clear_xdg_env() {
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("XDG_STATE_HOME");
            std::env::remove_var("XDG_CACHE_HOME");
        }
    }

    /// # Safety
    /// See [`clear_xdg_env`].
    unsafe fn set_env(key: &str, value: &Path) {
        unsafe {
            std::env::set_var(key, value);
        }
    }

    #[test]
    fn defaults_follow_the_xdg_spec_under_home() {
        clear_xdg_env();
        let home = etcetera::home_dir().unwrap();
        let dirs = XdgDirs::new().unwrap();

        assert_eq!(dirs.config_dir(), home.join(".config/over"));
        assert_eq!(dirs.state_dir(), home.join(".local/state/over"));
        assert_eq!(dirs.cache_dir(), home.join(".cache/over"));
    }

    #[test]
    fn config_dir_honors_xdg_config_home() {
        clear_xdg_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { set_env("XDG_CONFIG_HOME", tmp.path()) };

        let dirs = XdgDirs::new().unwrap();

        assert_eq!(dirs.config_dir(), tmp.path().join("over"));
    }

    #[test]
    fn state_dir_honors_xdg_state_home() {
        clear_xdg_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { set_env("XDG_STATE_HOME", tmp.path()) };

        let dirs = XdgDirs::new().unwrap();

        assert_eq!(dirs.state_dir(), tmp.path().join("over"));
    }

    #[test]
    fn cache_dir_honors_xdg_cache_home() {
        clear_xdg_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { set_env("XDG_CACHE_HOME", tmp.path()) };

        let dirs = XdgDirs::new().unwrap();

        assert_eq!(dirs.cache_dir(), tmp.path().join("over"));
    }

    #[test]
    fn ensure_dir_creates_missing_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b/c");

        XdgDirs::ensure_dir(&nested).unwrap();

        assert!(nested.is_dir());
    }
}
