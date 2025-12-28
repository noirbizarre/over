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

    /// Run overwriting eveything without prompt
    pub force: bool,

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

impl Context {
    pub fn new(
        dry_run: bool,
        debug: bool,
        verbose: bool,
        force: bool,
        root: PathBuf,
        repository: Repository,
        overlay: Option<Overlay>,
    ) -> Arc<Self> {
        Arc::new(Self {
            dry_run,
            debug,
            verbose,
            force,
            root,
            repository,
            overlay,
            progress: None,
        })
    }

    pub fn with_overlay(&self, overlay: Overlay) -> Arc<Self> {
        Arc::new(Self {
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
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
    use indicatif::{MultiProgress, ProgressBar};

    fn dummy_repo() -> Repository {
        Repository::new(PathBuf::from("/tmp/over-test-repo"))
    }

    #[test]
    fn with_progress_sets_progress_bar() {
        let ctx = Context::new(
            false,
            false,
            false,
            false,
            PathBuf::from("/tmp"),
            dummy_repo(),
            None,
        );
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
        let ctx = Context::new(
            false,
            false,
            false,
            false,
            PathBuf::from("/tmp"),
            dummy_repo(),
            None,
        );
        let mp = MultiProgress::new();
        let new_ctx = ctx.with_multiprogress(mp.clone());
        assert!(ctx.try_multiprogress().is_none());
        assert!(new_ctx.try_multiprogress().is_some());
    }
}
