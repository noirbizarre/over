use std::collections::HashMap;
use std::env::consts::{ARCH, OS};
use std::{path::PathBuf, sync::Arc};

use gethostname::gethostname;
use indicatif::{MultiProgress, ProgressBar};
use serde::Serialize;

use crate::overlays::{DataMap, Overlay, Repository};
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

#[derive(Debug, Clone)]
pub(crate) struct PathInputs {
    // Keep capture errors so a later render cannot silently adopt a different cwd.
    cwd: Result<PathBuf, String>,
    pub home: Option<PathBuf>,
}

impl Default for PathInputs {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().map_err(|error| error.to_string()),
            home: dirs::home_dir(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Context {
    #[serde(skip)]
    pub crypto: Arc<crate::secrets::CryptoService>,
    #[serde(skip)]
    pub(crate) path_inputs: PathInputs,

    #[serde(skip)]
    pub(crate) current_target: Option<PathBuf>,

    /// Run without applying changes
    pub dry_run: bool,

    /// Toggle debug traces,
    pub debug: bool,

    /// Toggle verbose output
    pub verbose: bool,

    /// Overwrite ordinary filesystem conflicts, not protected checkouts.
    pub force: bool,

    /// Explicitly authorize loss of an owned real checkout's local data/history.
    pub discard_checkout: bool,

    /// Disable interactive prompts (fail on conflict)
    pub no_prompt: bool,

    /// Skip operational overlay composition, retaining dependency metadata.
    pub no_uses: bool,

    /// Run install hooks after reconciliation, before post-apply hooks.
    pub install: bool,

    /// Target root (~)
    pub root: PathBuf,

    pub repository: Repository,

    pub overlay: Option<Overlay>,

    /// Facts about the current machine, available to all templates.
    pub machine: MachineInfo,

    /// Resolved consumer metadata for the selected execution root.
    pub data: DataMap,

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
    path_inputs: PathInputs,
    dry_run: bool,
    debug: bool,
    verbose: bool,
    force: bool,
    discard_checkout: bool,
    no_prompt: bool,
    no_uses: bool,
    install: bool,
    root: PathBuf,
    repository: Repository,
    overlay: Option<Overlay>,
    machine: MachineInfo,
    data: DataMap,
    progress: Option<Progress>,
    resolved_overlays: Arc<HashMap<String, String>>,
}

impl ContextBuilder {
    pub fn install(mut self, install: bool) -> Self {
        self.install = install;
        self
    }

    pub fn data(mut self, data: DataMap) -> Self {
        self.data = data;
        self
    }

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

    pub fn discard_checkout(mut self, v: bool) -> Self {
        self.discard_checkout = v;
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
            crypto: Arc::new(crate::secrets::CryptoService::default()),
            path_inputs: self.path_inputs,
            current_target: None,
            dry_run: self.dry_run,
            debug: self.debug,
            verbose: self.verbose,
            force: self.force,
            discard_checkout: self.discard_checkout,
            no_prompt: self.no_prompt,
            no_uses: self.no_uses,
            install: self.install,
            root: self.root,
            repository: self.repository,
            overlay: self.overlay,
            machine: self.machine,
            data: self.data,
            progress: self.progress,
            resolved_overlays: self.resolved_overlays,
        })
    }
}

impl Context {
    /// Anchor once to invocation cwd, never the cwd of a later template or hook.
    pub(crate) fn absolute_path(&self, path: &std::path::Path) -> anyhow::Result<PathBuf> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.path_inputs
                .cwd
                .as_ref()
                .map_err(|error| {
                    anyhow::anyhow!("could not capture invocation directory: {error}")
                })?
                .join(path)
        };
        crate::overlays::mapping::absolute(&absolute)
    }

    pub(crate) fn without_target(&self) -> Arc<Self> {
        Arc::new(Self {
            current_target: None,
            ..self.clone()
        })
    }

    pub(crate) fn with_target(&self, target: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            current_target: Some(target),
            ..self.clone()
        })
    }

    pub(crate) fn prepare_overlay(&self, overlay: &Overlay) -> anyhow::Result<Ctx> {
        let ctx = self.with_overlay(overlay.clone());
        let target = overlay.resolve_target(&ctx)?;
        Ok(ctx.with_target(target))
    }

    pub(crate) fn with_path_inputs(&self, original: &Self) -> Arc<Self> {
        Arc::new(Self {
            path_inputs: original.path_inputs.clone(),
            ..self.clone()
        })
    }

    /// Prepare metadata before rendering paths; dependency context copies keep
    /// this snapshot so they cannot discard the command target's overrides.
    pub fn resolve_data(&self) -> anyhow::Result<Ctx> {
        Ok(Arc::new(Self {
            // Metadata may change the rendered target, invalidating an earlier binding.
            current_target: None,
            data: self.repository.resolve_data(self, self.overlay.as_ref())?,
            ..self.clone()
        }))
    }

    /// Create a new [`ContextBuilder`] with default values.
    pub fn builder() -> ContextBuilder {
        ContextBuilder::default()
    }

    pub fn with_overlay(&self, overlay: Overlay) -> Arc<Self> {
        Arc::new(Self {
            // Dependencies must not inherit their consumer's destination.
            current_target: None,
            overlay: Some(overlay),
            ..self.clone()
        })
    }

    pub fn with_progress(&self, progress: ProgressBar) -> Arc<Self> {
        Arc::new(Self {
            progress: Some(Progress::Progress(progress)),
            ..self.clone()
        })
    }

    pub fn with_multiprogress(&self, progress: MultiProgress) -> Arc<Self> {
        Arc::new(Self {
            progress: Some(Progress::MultiProgress(progress)),
            ..self.clone()
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
            ..self.clone()
        })
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
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use indicatif::{MultiProgress, ProgressBar};
    use rstest::rstest;

    use super::*;
    use crate::exec::templates::{TemplateContext, render_string};

    #[test]
    fn shared_paths_serialize_as_strings_and_home_is_independent_of_the_execution_root() {
        let td = assert_fs::TempDir::new().unwrap();
        let mut ctx = Context::builder()
            .repository(Repository::new(td.path().join("repository")))
            .root(td.path().join("deployment"))
            .build()
            .as_ref()
            .clone();
        ctx.path_inputs.home = Some(td.path().join("actual home ü"));
        let ctx = ctx.with_target(td.path().join("effective target"));
        let state = TemplateContext::new(&ctx).unwrap();
        let json = serde_json::to_value(&state).unwrap();
        for (field, expected) in [
            ("home", td.path().join("actual home ü")),
            ("over", td.path().join("repository")),
            ("target", td.path().join("effective target")),
        ] {
            assert_eq!(json["paths"][field], expected.to_str().unwrap());
            assert_eq!(
                render_string(&format!("{{{{ paths.{field} }}}}"), &state).unwrap(),
                expected.to_str().unwrap()
            );
        }
        for alias in ["home", "over_home", "target"] {
            assert!(json.get(alias).is_none());
        }
        assert_eq!(json["machine"]["os"], ctx.machine.os);
        assert_eq!(json["machine"]["arch"], ctx.machine.arch);
        assert_eq!(json["machine"]["hostname"], ctx.machine.hostname);
    }

    #[rstest]
    #[case("home")]
    #[case("over")]
    #[case("target")]
    fn unavailable_paths_are_undefined_and_unguarded_rendering_fails(#[case] field: &str) {
        let mut ctx = Context::default();
        ctx.path_inputs.home = None;
        let state = TemplateContext::new(&ctx).unwrap();
        assert_eq!(
            render_string(&format!("{{{{ paths.{field} is defined }}}}"), &state).unwrap(),
            "False"
        );
        assert!(render_string(&format!("{{{{ paths.{field} }}}}"), &state).is_err());
        assert_eq!(
            render_string(
                &format!("{{{{ paths.{field} | default('unavailable') }}}}"),
                &state
            )
            .unwrap(),
            "unavailable"
        );
    }

    #[test]
    fn bound_targets_survive_progress_copies_but_never_follow_an_overlay_switch() {
        let (td, overlay) = make_overlay();
        let ctx = Context::builder()
            .root(td.path().join("deployment"))
            .overlay(overlay.clone())
            .build();
        let prepared = ctx.prepare_overlay(&overlay).unwrap();
        let target = prepared.current_target.clone();
        assert_eq!(
            prepared.with_progress(ProgressBar::hidden()).current_target,
            target
        );
        assert_eq!(
            prepared
                .with_multiprogress(MultiProgress::new())
                .current_target,
            target
        );
        assert_eq!(
            prepared
                .with_resolved_overlay("other".into(), "/other".into())
                .current_target,
            target
        );
        assert!(prepared.with_overlay(overlay).current_target.is_none());
        assert!(prepared.without_target().current_target.is_none());
    }

    #[test]
    fn a_target_cannot_reference_itself_or_inherit_another_overlays_target() {
        let (td, mut overlay) = make_overlay();
        let ctx = Context::builder()
            .root(td.path().join("deployment"))
            .build()
            .with_target(td.path().join("stale"));
        overlay.target = "{{ paths.target }}".into();
        assert!(overlay.resolve_target(&ctx).is_err());
        overlay.target =
            "{{ paths.target if paths.target is defined else paths.over }}/destination".into();
        let ctx = Context {
            repository: Repository::new(td.path().to_path_buf()),
            ..ctx.as_ref().clone()
        };
        assert_eq!(
            overlay.resolve_target(&ctx).unwrap(),
            td.path().join("destination")
        );
    }

    #[test]
    fn captured_directory_failures_are_reported_instead_of_retried() {
        let mut ctx = Context::default();
        ctx.path_inputs.cwd = Err("injected capture failure".into());
        ctx.repository = Repository::new(PathBuf::from("relative"));
        assert!(
            TemplateContext::new(&ctx)
                .err()
                .unwrap()
                .to_string()
                .contains("injected capture failure")
        );
    }

    #[test]
    fn default_selection_shares_base_paths_without_resolving_a_target_or_recursing_into_data() {
        let (td, overlay) = make_overlay();
        td.child("over.toml").write_str(
            "default_overlay = \"{{ 'test-overlay' if paths.over is defined and paths.home is defined and paths.target is not defined else 'missing' }}\"\n"
        ).unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        let ctx = Context::builder()
            .repository(repo.clone())
            .root(td.path().join("deployment"))
            .build()
            .with_target(td.path().join("stale"));
        assert_eq!(
            repo.default_overlay(&ctx).unwrap().unwrap().name,
            overlay.name
        );
        assert!(ctx.resolve_data().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_fail_instead_of_identifying_a_different_destination() {
        use std::os::unix::ffi::OsStringExt;
        let mut ctx = Context::default();
        ctx.path_inputs.home = Some(PathBuf::from(std::ffi::OsString::from_vec(
            b"/home/\xff".to_vec(),
        )));
        assert!(TemplateContext::new(&ctx).is_err());
    }

    #[test]
    fn prepared_paths_remain_stable_after_the_process_changes_directory() {
        const CHILD: &str = "OVER_PATH_CONTEXT_CWD_TEST";
        if std::env::var_os(CHILD).is_some() {
            let invocation = std::env::current_dir().unwrap();
            std::fs::create_dir_all("repository/overlay").unwrap();
            std::fs::create_dir("elsewhere").unwrap();
            std::fs::write("repository/overlay/over.toml", "target = '~/dest'").unwrap();
            let repo = Repository::new(PathBuf::from("repository"));
            let overlay = Overlay::new(&repo, std::path::Path::new("repository/overlay")).unwrap();
            let ctx = Context::builder()
                .repository(repo)
                .root(PathBuf::from("deployment"))
                .overlay(overlay.clone())
                .build();
            // This branch runs alone in a child process, avoiding parallel-test cwd races.
            std::env::set_current_dir("elsewhere").unwrap();
            let rendered = crate::actions::symlink::render_symlink_target(
                "{{ paths.over }}|{{ paths.target }}",
                &ctx,
            )
            .unwrap();
            assert_eq!(
                rendered,
                format!(
                    "{}|{}",
                    invocation.join("repository").display(),
                    invocation.join("deployment").join("dest").display()
                )
            );
            let routing = overlay.resolve_mappings(&ctx).unwrap();
            assert_eq!(routing.base, invocation.join("deployment").join("dest"));
            return;
        }
        let td = assert_fs::TempDir::new().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "exec::context::tests::prepared_paths_remain_stable_after_the_process_changes_directory", "--nocapture"])
            .env(CHILD, "1").current_dir(td.path()).output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn checkout_disposal_authorization_survives_context_copies() {
        let ctx = super::Context::builder().discard_checkout(true).build();
        assert!(
            ctx.with_progress(indicatif::ProgressBar::hidden())
                .discard_checkout
        );
        assert!(
            ctx.with_multiprogress(indicatif::MultiProgress::new())
                .discard_checkout
        );
        assert!(
            ctx.with_resolved_overlay("ov".into(), "/target".into())
                .discard_checkout
        );
    }
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

    /// Every flag `Context::builder()` sets ends up exactly as requested,
    /// independent of which other flags are also set — whether all five are
    /// requested (mirrors the former `builder_sets_flags`) or only some
    /// (mirrors the former `builder_partial_flags`); unset flags must stay
    /// `false` rather than leaking a previous case's value.
    #[rstest]
    #[case(true, true, true, true, true)]
    #[case(true, false, true, false, false)]
    fn builder_sets_requested_flags(
        #[case] dry_run: bool,
        #[case] debug: bool,
        #[case] verbose: bool,
        #[case] force: bool,
        #[case] no_prompt: bool,
    ) {
        let ctx = Context::builder()
            .dry_run(dry_run)
            .debug(debug)
            .verbose(verbose)
            .force(force)
            .no_prompt(no_prompt)
            .build();

        assert_eq!(ctx.dry_run, dry_run);
        assert_eq!(ctx.debug, debug);
        assert_eq!(ctx.verbose, verbose);
        assert_eq!(ctx.force, force);
        assert_eq!(ctx.no_prompt, no_prompt);
    }

    #[test]
    fn builder_sets_root_and_repository() {
        #[cfg(unix)]
        let root_path = PathBuf::from("/home/test");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\home\\test");

        let ctx = Context::builder()
            .root(root_path.clone())
            .repository(dummy_repo())
            .build();

        assert_eq!(ctx.root, root_path);
        assert_eq!(ctx.repository.root, dummy_repo().root);
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
    fn cloning_a_context_preserves_its_execution_root() {
        #[cfg(unix)]
        let root_path = PathBuf::from("/tmp");
        #[cfg(windows)]
        let root_path = PathBuf::from("C:\\tmp");

        let ctx = Context::builder()
            .dry_run(true)
            .root(root_path.clone())
            .repository(dummy_repo())
            .build();

        let cloned = ctx.as_ref().clone();
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
