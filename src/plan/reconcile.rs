use std::fmt;
use std::sync::LazyLock;

use anyhow::{Context as AnyhowContext, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::actions::fs::EnsureDirLink;
use crate::actions::{EnsureDir, EnsureLink, EnsureSymlink};
use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent, Provenance};
use crate::exec::{Action, Ctx};
use crate::ui::style;

use super::actual::{self, ActualState};
use super::step::{Operation, PlanStep};

static SPINNER_STYLE: LazyLock<ProgressStyle> = LazyLock::new(|| {
    ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
        .expect("static progress template must be valid")
        .tick_chars(style::TICK_CHARS_BRAILLE_4_6_DOWN.as_str())
});

/// The reconciliation plan for a [`DesiredTree`]: one [`PlanStep`] per
/// [`DesiredEntry`], classified against actual filesystem state.
///
/// Building a plan never touches the filesystem beyond read-only inspection
/// (see [`actual::inspect`]); only [`Plan::execute`] mutates anything, and it
/// still honors `ctx.dry_run` exactly like every `Action` already does.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    steps: Vec<PlanStep>,
}

impl Plan {
    /// Classify every entry in `desired` against current filesystem state.
    pub fn build(desired: &DesiredTree) -> Result<Self> {
        let mut steps = Vec::with_capacity(desired.len());
        for entry in desired.entries() {
            let operation = classify(entry)?;
            steps.push(PlanStep {
                entry: entry.clone(),
                operation,
            });
        }
        Ok(Self { steps })
    }

    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn has_conflicts(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s.operation, Operation::Conflict { .. }))
    }

    /// Execute every actionable step (`Create`/`Conflict`) in the plan's
    /// deterministic order (`DesiredTree` sorts entries by target path, so
    /// a directory's entry always precedes anything nested under it).
    /// `Noop`/`Deferred` steps are skipped — the former because there's
    /// nothing to do, the latter because [`MaterializationIntent::Checkout`]
    /// isn't materializable yet (#108/#110).
    pub async fn execute(&self, ctx: Ctx) -> Result<()> {
        let has_actionable = self
            .steps
            .iter()
            .any(|s| matches!(s.operation, Operation::Create | Operation::Conflict { .. }));
        if !has_actionable {
            return Ok(());
        }

        let progress = ProgressBar::new_spinner()
            .with_style(SPINNER_STYLE.clone())
            .with_message("");

        for step in &self.steps {
            if matches!(step.operation, Operation::Noop | Operation::Deferred) {
                continue;
            }

            if ctx.verbose || ctx.dry_run {
                progress.println(format!("{step}"));
            }
            progress.set_message(format!("{step}"));

            let action = build_action(ctx.clone(), &step.entry);
            action.execute(ctx.clone()).await.with_context(|| {
                format!("failed to reconcile '{}'", step.entry.target.display())
            })?;
        }

        progress.finish_and_clear();
        Ok(())
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let create = self
            .steps
            .iter()
            .filter(|s| matches!(s.operation, Operation::Create))
            .count();
        let noop = self
            .steps
            .iter()
            .filter(|s| matches!(s.operation, Operation::Noop))
            .count();
        let conflicts = self
            .steps
            .iter()
            .filter(|s| matches!(s.operation, Operation::Conflict { .. }))
            .count();
        let deferred = self
            .steps
            .iter()
            .filter(|s| matches!(s.operation, Operation::Deferred))
            .count();

        writeln!(
            f,
            "{} {} to create, {} unchanged, {} conflict(s), {} deferred",
            style::white_b("Plan:"),
            create,
            noop,
            conflicts,
            deferred,
        )?;
        for step in self
            .steps
            .iter()
            .filter(|s| !matches!(s.operation, Operation::Noop))
        {
            writeln!(f, "  {step}")?;
        }
        Ok(())
    }
}

/// Classify a single entry against current filesystem state. Read-only.
fn classify(entry: &DesiredEntry) -> Result<Operation> {
    match &entry.intent {
        MaterializationIntent::Directory => {
            let actual = actual::inspect(&entry.target)?;
            Ok(match actual {
                ActualState::Missing => Operation::Create,
                ActualState::Directory => Operation::Noop,
                other => Operation::Conflict { current: other },
            })
        }
        MaterializationIntent::SymlinkFile { source, .. }
        | MaterializationIntent::SymlinkDirectory { source, .. } => {
            let actual = actual::inspect(&entry.target)?;
            Ok(match actual {
                ActualState::Missing => Operation::Create,
                ActualState::Symlink { ref points_to } if points_to == source => Operation::Noop,
                other => Operation::Conflict { current: other },
            })
        }
        MaterializationIntent::Checkout => Ok(Operation::Deferred),
    }
}

/// Translate an entry into the existing, tested `Action` that materializes
/// it — the single place that knows about concrete `actions::fs`/
/// `actions::symlink` types (see [`super::step::Operation`]'s doc comment:
/// #108 replaces this with a registered `Materializer` lookup).
///
/// Dispatches on `provenance`, not just `intent`, because two different
/// existing actions handle symlinks with different conflict semantics:
/// regular per-file overlay entries go through `EnsureLink`/`EnsureDirLink`
/// (interactive skip/overwrite/absorb/diff resolution, ADR-006), while
/// `.link.*` sidecar entries go through `EnsureSymlink` (unconditional
/// overwrite, and the only one of the two that supports hard links).
fn build_action(ctx: Ctx, entry: &DesiredEntry) -> Box<dyn Action> {
    let is_sidecar = matches!(entry.provenance, Provenance::SymlinkSidecar { .. });

    match &entry.intent {
        MaterializationIntent::Directory => Box::new(EnsureDir::new(entry.target.clone())),
        MaterializationIntent::SymlinkFile { source, link_type } => {
            if is_sidecar {
                Box::new(EnsureSymlink::new(
                    source.clone(),
                    entry.target.clone(),
                    link_type.clone(),
                    false,
                ))
            } else {
                Box::new(EnsureLink::new(ctx, source.clone(), entry.target.clone()))
            }
        }
        MaterializationIntent::SymlinkDirectory { source, link_type } => {
            if is_sidecar {
                Box::new(EnsureSymlink::new(
                    source.clone(),
                    entry.target.clone(),
                    link_type.clone(),
                    true,
                ))
            } else {
                Box::new(EnsureDirLink::new(
                    ctx,
                    source.clone(),
                    entry.target.clone(),
                ))
            }
        }
        MaterializationIntent::Checkout => {
            unreachable!("Checkout entries are always Operation::Deferred and never executed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::symlink::LinkType;
    use crate::exec::Context;
    use crate::overlays::{Overlay, Repository};
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;
    use std::fs;
    use std::path::PathBuf;

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    fn ctx(root: PathBuf, repo: Repository, overlay: Option<Overlay>) -> Ctx {
        let mut builder = Context::builder().root(root).repository(repo).verbose(true);
        if let Some(o) = overlay {
            builder = builder.overlay(o);
        }
        builder.build()
    }

    #[rstest]
    fn empty_desired_tree_produces_empty_plan() {
        let plan = Plan::build(&DesiredTree::default()).unwrap();
        assert!(plan.is_empty());
        assert!(!plan.has_conflicts());
    }

    #[rstest]
    fn missing_directory_classifies_as_create() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~/sub\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let plan = Plan::build(&desired).unwrap();

        assert_eq!(plan.len(), 1);
        assert!(matches!(plan.steps()[0].operation, Operation::Create));
    }

    #[rstest]
    fn existing_directory_classifies_as_noop() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let plan = Plan::build(&desired).unwrap();

        assert_eq!(plan.len(), 1);
        assert!(matches!(plan.steps()[0].operation, Operation::Noop));
        assert!(!plan.has_conflicts());
    }

    #[rstest]
    fn file_where_directory_expected_is_a_conflict() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~/blocked\"")
            .unwrap();
        // A file already sits where the overlay's target directory should go.
        fs::write(td.path().join("blocked"), "in the way").unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let plan = Plan::build(&desired).unwrap();

        assert!(plan.has_conflicts());
    }

    #[tokio::test]
    async fn create_step_creates_directory_and_symlink() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let plan = Plan::build(&desired).unwrap();
        plan.execute(c.clone()).await.unwrap();

        let target = td.path().join("file.txt");
        assert!(target.is_symlink());
        assert_eq!(
            fs::read_link(&target).unwrap(),
            overlay.root.join("file.txt")
        );
    }

    #[tokio::test]
    async fn second_execution_is_all_noop_and_idempotent() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(c.clone())
            .await
            .unwrap();

        // Rebuild the plan against post-apply state.
        let desired2 = DesiredTree::build(&c, &overlay).unwrap();
        let plan2 = Plan::build(&desired2).unwrap();
        assert!(
            plan2
                .steps()
                .iter()
                .all(|s| matches!(s.operation, Operation::Noop)),
            "second plan should find nothing left to do"
        );
        plan2.execute(c.clone()).await.unwrap();

        let target = td.path().join("file.txt");
        assert!(target.is_symlink());
    }

    #[tokio::test]
    async fn dry_run_execute_does_not_touch_filesystem() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~/newdir\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let dry_ctx = Context::builder()
            .dry_run(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let plan = Plan::build(&desired).unwrap();
        assert!(!plan.is_empty());
        plan.execute(dry_ctx).await.unwrap();

        assert!(!td.path().join("newdir").exists());
    }

    #[tokio::test]
    async fn conflict_with_force_overwrites_target() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("overlay").unwrap();
        fs::write(td.path().join("file.txt"), "existing").unwrap();

        let overlay = repo.get("ov").unwrap();
        let read_ctx = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let desired = DesiredTree::build(&read_ctx, &overlay).unwrap();
        let plan = Plan::build(&desired).unwrap();
        assert!(plan.has_conflicts());

        let force_ctx = Context::builder()
            .force(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();
        plan.execute(force_ctx).await.unwrap();

        let target = td.path().join("file.txt");
        assert!(target.is_symlink());
        assert_eq!(
            fs::read_link(&target).unwrap(),
            overlay.root.join("file.txt")
        );
    }

    #[tokio::test]
    async fn conflict_with_no_prompt_errors() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("overlay").unwrap();
        fs::write(td.path().join("file.txt"), "existing").unwrap();

        let overlay = repo.get("ov").unwrap();
        let read_ctx = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let desired = DesiredTree::build(&read_ctx, &overlay).unwrap();
        let plan = Plan::build(&desired).unwrap();

        let no_prompt_ctx = Context::builder()
            .no_prompt(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();
        let result = plan.execute(no_prompt_ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn sidecar_hard_link_is_dispatched_to_ensure_symlink() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let hard_source = td.child("hard_source.txt");
        hard_source.write_str("hard").unwrap();
        // Escape backslashes so Windows paths don't get misparsed as TOML
        // unicode escape sequences (see `Overlay::resolve_target` tests).
        let target_toml = hard_source.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("hardlink.link.toml")
            .write_str(&format!("target = \"{}\"\ntype = \"hard\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let desired = DesiredTree::build(&c, &overlay).unwrap();
        let plan = Plan::build(&desired).unwrap();
        plan.execute(c.clone()).await.unwrap();

        let target = td.path().join("hardlink");
        // Hard links are real files, not symlinks.
        assert!(!target.is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "hard");
    }

    #[test]
    fn build_action_directory() {
        let entry = DesiredEntry {
            target: PathBuf::from("/tmp/does-not-matter"),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/repo/ov"),
            },
            intent: MaterializationIntent::Directory,
        };
        let ctx = Context::builder().build();
        let action = build_action(ctx, &entry);
        assert!(format!("{action}").contains("create directory:"));
    }

    #[test]
    fn build_action_symlink_sidecar_uses_ensure_symlink() {
        let entry = DesiredEntry {
            target: PathBuf::from("/tmp/link"),
            provenance: Provenance::SymlinkSidecar {
                overlay: "ov".to_string(),
                config: PathBuf::from("/repo/ov/x.link.toml"),
                template: "/opt/x".to_string(),
                resolved: PathBuf::from("/opt/x"),
            },
            intent: MaterializationIntent::SymlinkFile {
                source: PathBuf::from("/opt/x"),
                link_type: LinkType::Soft,
            },
        };
        let ctx = Context::builder().build();
        let action = build_action(ctx, &entry);
        assert!(format!("{action}").contains("symlink"));
    }
}
