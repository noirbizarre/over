use std::{path::PathBuf, sync::Arc};

use indicatif::{MultiProgress, ProgressBar};
use serde::Serialize;

use crate::overlays::{Overlay, Repository};

#[derive(Debug, Default, Serialize)]
pub struct Context {
    /// Run without applying changes
    pub dry_run: bool,

    /// Toggle debug traces,
    pub debug: bool,

    /// Toggle verbose output
    pub verbose: bool,

    /// Run overwriting everything without prompt
    pub force: bool,

    /// Disable interactive prompts (fail on conflict)
    pub no_prompt: bool,

    /// Target root (~)
    pub root: PathBuf,

    pub repository: Repository,

    pub overlay: Option<Overlay>,

    #[serde(skip)]
    pub progress: Option<Progress>,
}

// Store the current progress bar
#[derive(Debug, Clone)]
pub enum Progress {
    Progress(ProgressBar),
    MultiProgress(MultiProgress),
}

impl Progress {
    pub fn try_progress(&self) -> Option<&ProgressBar> {
        match self {
            Progress::Progress(p) => Some(p),
            _ => None,
        }
    }

    pub fn try_multiprogress(&self) -> Option<&MultiProgress> {
        match self {
            Progress::MultiProgress(p) => Some(p),
            _ => None,
        }
    }
}

/// Builder for [`Context`], avoiding long positional argument lists.
///
/// All boolean flags default to `false`, other fields to their `Default`.
/// Use the setter methods to configure, then call [`build()`](ContextBuilder::build)
/// to get an `Arc<Context>`.
#[derive(Default)]
pub struct ContextBuilder {
    dry_run: bool,
    debug: bool,
    verbose: bool,
    force: bool,
    no_prompt: bool,
    root: PathBuf,
    repository: Repository,
    overlay: Option<Overlay>,
    progress: Option<Progress>,
}

impl ContextBuilder {
    pub fn dry_run(mut self, v: bool) -> Self {
        self.dry_run = v;
        self
    }

    pub fn debug(mut self, v: bool) -> Self {
        self.debug = v;
        self
    }

    pub fn verbose(mut self, v: bool) -> Self {
        self.verbose = v;
        self
    }

    pub fn force(mut self, v: bool) -> Self {
        self.force = v;
        self
    }

    pub fn no_prompt(mut self, v: bool) -> Self {
        self.no_prompt = v;
        self
    }

    pub fn root(mut self, root: PathBuf) -> Self {
        self.root = root;
        self
    }

    pub fn repository(mut self, repository: Repository) -> Self {
        self.repository = repository;
        self
    }

    pub fn overlay(mut self, overlay: Overlay) -> Self {
        self.overlay = Some(overlay);
        self
    }

    pub fn build(self) -> Arc<Context> {
        Arc::new(Context {
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
            no_prompt: self.no_prompt,
            root: self.root,
            repository: self.repository,
            overlay: self.overlay,
            progress: self.progress,
        })
    }
}

impl Context {
    /// Create a new [`ContextBuilder`] with default values.
    pub fn builder() -> ContextBuilder {
        ContextBuilder::default()
    }

    pub fn with_overlay(&self, overlay: Overlay) -> Arc<Self> {
        Arc::new(Self {
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
            no_prompt: self.no_prompt,
            root: self.root.clone(),
            repository: self.repository.clone(),
            overlay: Some(overlay),
            progress: self.progress.clone(),
        })
    }

    pub fn with_progress(&self, progress: ProgressBar) -> Arc<Self> {
        Arc::new(Self {
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
            no_prompt: self.no_prompt,
            root: self.root.clone(),
            repository: self.repository.clone(),
            overlay: self.overlay.clone(),
            progress: Some(Progress::Progress(progress)),
        })
    }

    pub fn with_multiprogress(&self, progress: MultiProgress) -> Arc<Self> {
        Arc::new(Self {
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
            no_prompt: self.no_prompt,
            root: self.root.clone(),
            repository: self.repository.clone(),
            overlay: self.overlay.clone(),
            progress: Some(Progress::MultiProgress(progress)),
        })
    }

    pub fn try_progress(&self) -> Option<&ProgressBar> {
        self.progress.as_ref().and_then(|p| p.try_progress())
    }

    pub fn try_multiprogress(&self) -> Option<&MultiProgress> {
        self.progress.as_ref().and_then(|p| p.try_multiprogress())
    }
}

pub type Ctx = Arc<Context>;

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use indicatif::{MultiProgress, ProgressBar};

    fn dummy_repo() -> Repository {
        Repository::new(PathBuf::from("/tmp/over-test-repo"))
    }

    fn make_overlay() -> (TempDir, Overlay) {
        let td = TempDir::new().unwrap();
        let ov_dir = td.child("test-overlay");
        ov_dir.create_dir_all().unwrap();
        ov_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        let overlay = Overlay::new(&repo, ov_dir.path()).unwrap();
        (td, overlay)
    }

    #[test]
    fn builder_defaults_all_false() {
        let ctx = Context::builder().build();
        assert!(!ctx.dry_run);
        assert!(!ctx.debug);
        assert!(!ctx.verbose);
        assert!(!ctx.force);
        assert!(!ctx.no_prompt);
        assert_eq!(ctx.root, PathBuf::default());
        assert!(ctx.overlay.is_none());
        assert!(ctx.progress.is_none());
    }

    #[test]
    fn builder_sets_flags() {
        let ctx = Context::builder()
            .dry_run(true)
            .debug(true)
            .verbose(true)
            .force(true)
            .no_prompt(true)
            .root(PathBuf::from("/home/test"))
            .repository(dummy_repo())
            .build();

        assert!(ctx.dry_run);
        assert!(ctx.debug);
        assert!(ctx.verbose);
        assert!(ctx.force);
        assert!(ctx.no_prompt);
        assert_eq!(ctx.root, PathBuf::from("/home/test"));
        assert_eq!(ctx.repository.root, PathBuf::from("/tmp/over-test-repo"));
    }

    #[test]
    fn builder_partial_flags() {
        let ctx = Context::builder()
            .dry_run(true)
            .verbose(true)
            .root(PathBuf::from("/tmp"))
            .build();

        assert!(ctx.dry_run);
        assert!(!ctx.debug);
        assert!(ctx.verbose);
        assert!(!ctx.force);
        assert!(!ctx.no_prompt);
    }

    #[test]
    fn with_overlay_preserves_other_fields() {
        let ctx = Context::builder()
            .dry_run(true)
            .verbose(true)
            .root(PathBuf::from("/home/test"))
            .repository(dummy_repo())
            .build();

        let (_td, overlay) = make_overlay();
        let new_ctx = ctx.with_overlay(overlay);

        assert!(new_ctx.dry_run);
        assert!(new_ctx.verbose);
        assert_eq!(new_ctx.root, PathBuf::from("/home/test"));
        assert!(new_ctx.overlay.is_some());
        // Original should be unchanged
        assert!(ctx.overlay.is_none());
    }

    #[test]
    fn with_progress_sets_progress_bar() {
        let ctx = Context::builder()
            .root(PathBuf::from("/tmp"))
            .repository(dummy_repo())
            .build();
        let pb = ProgressBar::hidden();
        let new_ctx = ctx.with_progress(pb.clone());
        // Original context should remain without progress
        assert!(ctx.try_progress().is_none());
        // New context should have the progress bar
        let stored = new_ctx.try_progress().expect("progress bar present");
        assert_eq!(stored.position(), pb.position());
    }

    #[test]
    fn with_multiprogress_sets_multi() {
        let ctx = Context::builder()
            .root(PathBuf::from("/tmp"))
            .repository(dummy_repo())
            .build();
        let mp = MultiProgress::new();
        let new_ctx = ctx.with_multiprogress(mp.clone());
        assert!(ctx.try_multiprogress().is_none());
        assert!(new_ctx.try_multiprogress().is_some());
    }
}
