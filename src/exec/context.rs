use std::collections::HashMap;
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

    /// Do not process uses (skip overlay composition)
    pub no_uses: bool,

    /// Target root (~)
    pub root: PathBuf,

    pub repository: Repository,

    pub overlay: Option<Overlay>,

    #[serde(skip)]
    pub progress: Option<Progress>,

    #[serde(skip)]
    pub resolved_overlays: Arc<HashMap<String, String>>,
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
    no_uses: bool,
    root: PathBuf,
    repository: Repository,
    overlay: Option<Overlay>,
    progress: Option<Progress>,
    resolved_overlays: Arc<HashMap<String, String>>,
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

    pub fn no_uses(mut self, v: bool) -> Self {
        self.no_uses = v;
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

    pub fn progress(mut self, progress: Progress) -> Self {
        self.progress = Some(progress);
        self
    }

    pub fn resolved_overlays(mut self, v: Arc<HashMap<String, String>>) -> Self {
        self.resolved_overlays = v;
        self
    }

    pub fn build(self) -> Arc<Context> {
        Arc::new(Context {
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
            no_prompt: self.no_prompt,
            no_uses: self.no_uses,
            root: self.root,
            repository: self.repository,
            overlay: self.overlay,
            progress: self.progress,
            resolved_overlays: self.resolved_overlays,
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
            no_uses: self.no_uses,
            root: self.root.clone(),
            repository: self.repository.clone(),
            overlay: Some(overlay),
            progress: self.progress.clone(),
            resolved_overlays: self.resolved_overlays.clone(),
        })
    }

    pub fn with_progress(&self, progress: ProgressBar) -> Arc<Self> {
        Arc::new(Self {
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
            no_prompt: self.no_prompt,
            no_uses: self.no_uses,
            root: self.root.clone(),
            repository: self.repository.clone(),
            overlay: self.overlay.clone(),
            progress: Some(Progress::Progress(progress)),
            resolved_overlays: self.resolved_overlays.clone(),
        })
    }

    pub fn with_multiprogress(&self, progress: MultiProgress) -> Arc<Self> {
        Arc::new(Self {
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
            no_prompt: self.no_prompt,
            no_uses: self.no_uses,
            root: self.root.clone(),
            repository: self.repository.clone(),
            overlay: self.overlay.clone(),
            progress: Some(Progress::MultiProgress(progress)),
            resolved_overlays: self.resolved_overlays.clone(),
        })
    }

    pub fn try_progress(&self) -> Option<&ProgressBar> {
        self.progress.as_ref().and_then(|p| p.try_progress())
    }

    pub fn try_multiprogress(&self) -> Option<&MultiProgress> {
        self.progress.as_ref().and_then(|p| p.try_multiprogress())
    }

    pub fn with_resolved_overlay(&self, name: String, target: String) -> Arc<Self> {
        let mut map = (*self.resolved_overlays).clone();
        map.insert(name, target);
        Arc::new(Self {
            resolved_overlays: Arc::new(map),
            ..self.clone_for_overlay_update()
        })
    }

    fn clone_for_overlay_update(&self) -> Self {
        Self {
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
            no_prompt: self.no_prompt,
            no_uses: self.no_uses,
            root: self.root.clone(),
            repository: self.repository.clone(),
            overlay: self.overlay.clone(),
            progress: self.progress.clone(),
            resolved_overlays: self.resolved_overlays.clone(),
        }
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
        #[cfg(unix)]
        let repo_path = PathBuf::from("/tmp/over-test-repo");
        #[cfg(windows)]
        let repo_path = PathBuf::from("C:\\over-test-repo");

        Repository::new(repo_path)
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
        #[cfg(unix)]
        let root_path = PathBuf::from("/home/test");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\home\\test");

        let ctx = Context::builder()
            .dry_run(true)
            .debug(true)
            .verbose(true)
            .force(true)
            .no_prompt(true)
            .root(root_path.clone())
            .repository(dummy_repo())
            .build();

        assert!(ctx.dry_run);
        assert!(ctx.debug);
        assert!(ctx.verbose);
        assert!(ctx.force);
        assert!(ctx.no_prompt);
        assert_eq!(ctx.root, root_path);
        assert_eq!(ctx.repository.root, dummy_repo().root);
    }

    #[test]
    fn builder_partial_flags() {
        #[cfg(unix)]
        let root_path = PathBuf::from("/tmp");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\tmp");

        let ctx = Context::builder()
            .dry_run(true)
            .verbose(true)
            .root(root_path)
            .build();

        assert!(ctx.dry_run);
        assert!(!ctx.debug);
        assert!(ctx.verbose);
        assert!(!ctx.force);
        assert!(!ctx.no_prompt);
    }

    #[test]
    fn with_overlay_preserves_other_fields() {
        #[cfg(unix)]
        let root_path = PathBuf::from("/home/test");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\home\\test");

        let ctx = Context::builder()
            .dry_run(true)
            .verbose(true)
            .root(root_path.clone())
            .repository(dummy_repo())
            .build();

        let (_td, overlay) = make_overlay();
        let new_ctx = ctx.with_overlay(overlay);

        assert!(new_ctx.dry_run);
        assert!(new_ctx.verbose);
        assert_eq!(new_ctx.root, root_path);
        assert!(new_ctx.overlay.is_some());
        // Original should be unchanged
        assert!(ctx.overlay.is_none());
    }

    #[test]
    fn with_progress_sets_progress_bar() {
        #[cfg(unix)]
        let root_path = PathBuf::from("/tmp");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\tmp");

        let ctx = Context::builder()
            .root(root_path)
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
        #[cfg(unix)]
        let root_path = PathBuf::from("/tmp");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\tmp");

        let ctx = Context::builder()
            .root(root_path)
            .repository(dummy_repo())
            .build();
        let mp = MultiProgress::new();
        let new_ctx = ctx.with_multiprogress(mp.clone());
        assert!(ctx.try_multiprogress().is_none());
        assert!(new_ctx.try_multiprogress().is_some());
    }

    #[test]
    fn with_resolved_overlay_adds_entry() {
        #[cfg(unix)]
        let root_path = PathBuf::from("/tmp");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\tmp");

        let ctx = Context::builder()
            .root(root_path)
            .repository(dummy_repo())
            .build();

        let new_ctx = ctx.with_resolved_overlay("myoverlay".to_string(), "/target".to_string());
        assert_eq!(
            new_ctx.resolved_overlays.get("myoverlay"),
            Some(&"/target".to_string())
        );
        assert!(ctx.resolved_overlays.is_empty());
    }

    #[test]
    fn with_resolved_overlay_preserves_other_fields() {
        #[cfg(unix)]
        let root_path = PathBuf::from("/tmp");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\tmp");

        let ctx = Context::builder()
            .dry_run(true)
            .debug(true)
            .verbose(true)
            .force(true)
            .no_prompt(true)
            .root(root_path.clone())
            .repository(dummy_repo())
            .build();

        let new_ctx = ctx.with_resolved_overlay("ov".to_string(), "/t".to_string());
        assert!(new_ctx.dry_run);
        assert!(new_ctx.debug);
        assert!(new_ctx.verbose);
        assert!(new_ctx.force);
        assert!(new_ctx.no_prompt);
        assert_eq!(new_ctx.root, root_path);
    }

    #[test]
    fn progress_try_progress_returns_some() {
        let pb = ProgressBar::hidden();
        let progress = Progress::Progress(pb.clone());
        assert!(progress.try_progress().is_some());
        assert!(progress.try_multiprogress().is_none());
    }

    #[test]
    fn progress_try_multiprogress_returns_some() {
        let mp = MultiProgress::new();
        let progress = Progress::MultiProgress(mp.clone());
        assert!(progress.try_multiprogress().is_some());
        assert!(progress.try_progress().is_none());
    }

    #[test]
    fn try_progress_without_progress_returns_none() {
        let ctx = Context::builder().build();
        assert!(ctx.try_progress().is_none());
        assert!(ctx.try_multiprogress().is_none());
    }

    #[test]
    fn builder_with_overlay() {
        let (_td, overlay) = make_overlay();
        let ctx = Context::builder()
            .repository(dummy_repo())
            .overlay(overlay.clone())
            .build();
        assert!(ctx.overlay.is_some());
        assert_eq!(ctx.overlay.as_ref().unwrap().name, overlay.name);
    }

    #[test]
    fn builder_with_progress() {
        let pb = ProgressBar::hidden();
        let ctx = Context::builder()
            .repository(dummy_repo())
            .progress(Progress::Progress(pb))
            .build();
        assert!(ctx.progress.is_some());
    }

    #[test]
    fn builder_with_resolved_overlays() {
        let mut map = HashMap::new();
        map.insert("ov".to_string(), "/target".to_string());
        let map = Arc::new(map);
        let ctx = Context::builder()
            .repository(dummy_repo())
            .resolved_overlays(map.clone())
            .build();
        assert_eq!(
            ctx.resolved_overlays.get("ov"),
            Some(&"/target".to_string())
        );
    }

    #[test]
    fn context_clone_for_overlay_update() {
        #[cfg(unix)]
        let root_path = PathBuf::from("/tmp");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\tmp");

        let ctx = Context::builder()
            .dry_run(true)
            .root(root_path.clone())
            .repository(dummy_repo())
            .build();

        let cloned = ctx.clone_for_overlay_update();
        assert!(cloned.dry_run);
        assert_eq!(cloned.root, root_path);
    }
}
