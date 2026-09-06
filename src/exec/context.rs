use std::collections::HashMap;
use std::env::consts::{ARCH, OS};
use std::{path::PathBuf, sync::Arc};

use gethostname::gethostname;
use indicatif::{MultiProgress, ProgressBar};
use serde::Serialize;

use crate::overlays::{Overlay, Repository};
use crate::utils;

/// Machine facts exposed to templates as `{{ machine.* }}` (OS, arch,
/// hostname, username, Linux distro), enabling conditional overlay behavior
/// per machine.
#[derive(Debug, Clone, Serialize)]
pub struct MachineInfo {
    pub os: String,
    pub arch: String,
    pub hostname: String,
    pub username: String,
    pub distro: Option<String>,
    pub distro_id: Option<String>,
}

impl MachineInfo {
    /// Detect facts about the machine `over` is currently running on.
    pub fn detect() -> Self {
        Self {
            os: OS.to_string(),
            arch: ARCH.to_string(),
            hostname: gethostname().to_string_lossy().to_string(),
            username: detect_username(),
            distro: utils::detect_linux_distro_name(),
            distro_id: utils::detect_linux_distro_id(),
        }
    }
}

impl Default for MachineInfo {
    // `Context`/`ContextBuilder` derive `Default`, which requires every field
    // to implement it; there's no meaningful "empty" machine, so default
    // means "the real, detected machine".
    fn default() -> Self {
        Self::detect()
    }
}

/// Resolve current username from `USER` (Unix) / `USERNAME` (Windows),
/// falling back to "unknown" in sandboxes/containers where neither is set.
fn detect_username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

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

    /// Facts about the current machine, available to all templates.
    pub machine: MachineInfo,

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
    machine: MachineInfo,
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

    pub fn machine(mut self, machine: MachineInfo) -> Self {
        self.machine = machine;
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
            machine: self.machine,
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
            machine: self.machine.clone(),
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
            machine: self.machine.clone(),
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
            machine: self.machine.clone(),
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
            machine: self.machine.clone(),
            progress: self.progress.clone(),
            resolved_overlays: self.resolved_overlays.clone(),
        }
    }
}

/// Shared handle to a [`Context`], cheap to clone (an `Arc` bump) and safe to
/// pass across `.await` points.
///
/// Parameter-shape convention across the codebase (all three are
/// intentional, not interchangeable by accident):
/// - `ctx: Ctx` (owned) — trait methods (`Action::execute`,
///   `Materializer::materialize`) and anything that must move the context
///   into a spawned/boxed future or store it past the call. Cheap to
///   produce at the call site via `ctx.clone()`.
/// - `ctx: &Ctx` — thin, non-owning wrappers that may need to clone once to
///   delegate into an owning callee (e.g. `Overlay::add_file` borrowing here
///   and cloning once before calling `actions::fs::add_file`).
/// - `ctx: &Context` — pure readers that only inspect fields (flags, `root`,
///   …) and never need `Arc` semantics; `&Ctx` derefs to `&Context` at call
///   sites, so this is the most permissive signature for such helpers.
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
        // Default `machine` is the real detected machine, not an empty value.
        assert_eq!(ctx.machine.os, std::env::consts::OS);
        assert_eq!(ctx.machine.arch, std::env::consts::ARCH);
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
        assert_eq!(new_ctx.machine.os, ctx.machine.os);
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

    #[test]
    fn machine_info_detect_populates_os_and_arch() {
        let machine = MachineInfo::detect();
        assert_eq!(machine.os, std::env::consts::OS);
        assert_eq!(machine.arch, std::env::consts::ARCH);
    }

    #[test]
    fn builder_with_machine_override() {
        let machine = MachineInfo {
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            hostname: "test-host".to_string(),
            username: "tester".to_string(),
            distro: None,
            distro_id: None,
        };
        let ctx = Context::builder()
            .repository(dummy_repo())
            .machine(machine)
            .build();
        assert_eq!(ctx.machine.os, "macos");
        assert_eq!(ctx.machine.hostname, "test-host");
    }
}
