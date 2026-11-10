use std::fmt;
use std::sync::LazyLock;

use anyhow::{Context as _, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::desired::{DesiredEntry, DesiredTree, MaterializationIntent};
use crate::exec::Ctx;
use crate::materialize::MaterializerRegistry;
use crate::ui::style;

use super::step::{Operation, PlanStep};

/// Whether a step needs [`Materializer::materialize`](crate::materialize::Materializer::materialize)
/// called for it: `Create`/`Conflict` always did; an unblocked `Migrate`
/// does too (#129) — a blocked one doesn't, exactly like `Noop`/`Deferred`.
fn is_actionable(operation: &Operation) -> bool {
    match operation {
        Operation::Create | Operation::Conflict { .. } | Operation::Repair { .. } => true,
        Operation::Migrate { blocked, .. } => blocked.is_none(),
        Operation::Noop | Operation::Deferred => false,
    }
}

static SPINNER_STYLE: LazyLock<ProgressStyle> = LazyLock::new(|| {
    ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
        .expect("static progress template must be valid")
        .tick_chars(style::TICK_CHARS_BRAILLE_4_6_DOWN.as_str())
});

fn classify(
    desired: &DesiredTree,
    entry: &DesiredEntry,
    registry: &MaterializerRegistry,
) -> Result<Operation> {
    let mut operation = match registry.find(&entry.intent) {
        Some(materializer) => materializer.classify(entry)?,
        None => Operation::Deferred,
    };
    if let crate::desired::Provenance::Git {
        overlay, repo_key, ..
    } = &entry.provenance
        && matches!(operation, Operation::Migrate { .. })
    {
        // Only the composed tree knows the declaring overlay source;
        // arbitrary symlinks are not proof of a legacy installation.
        let source = desired
            .sources
            .iter()
            .find(|selection| selection.overlay.name == *overlay)
            .map(|selection| (selection.overlay.root.join(repo_key), &selection.policy));
        let owned = match source {
            Some((source, policy)) => {
                let matches = if entry.target.is_symlink() {
                    std::fs::read_link(&entry.target)? == source
                } else {
                    crate::materialize::symlink::directory_is_symlink_mirror(
                        &entry.target,
                        &source,
                    )?
                };
                if matches {
                    policy.validate_directory_link(&entry.target, &source)?;
                }
                matches
            }
            None => false,
        };
        if !owned {
            operation = Operation::Conflict {
                current: super::actual::inspect(&entry.target)?,
            };
        }
    }
    if matches!(entry.intent, MaterializationIntent::Directory)
        && !desired.entries().iter().any(|other| {
            other.target == entry.target && matches!(other.intent, MaterializationIntent::Checkout)
        })
        && !entry.target.is_symlink()
        && ((entry.target.join(".git").exists() && git2::Repository::open(&entry.target).ok()
            .and_then(|repo| repo.config().ok())
            .and_then(|config| config.get_string("over.overlay").ok())
            .is_some_and(|name| matches!(&entry.provenance, crate::desired::Provenance::Overlay { overlay, .. } if *overlay == name)))
            || crate::materialize::virtual_checkout::state::record_for_blocking(&entry.target)?.is_some())
    {
        // A parent checkout is indivisible: never force-overwrite its
        // individual editable files after a recursive-rule change.
        operation = Operation::Migrate {
            from: MaterializationIntent::VirtualCheckout,
            to: entry.intent.clone(),
            blocked: Some("checkout to recursive links requires unapplying its old layout first".into()),
        };
    }
    Ok(operation)
}

/// The reconciliation plan for a [`DesiredTree`]: one [`PlanStep`] per
/// [`DesiredEntry`], classified against actual state by whichever
/// [`crate::materialize::Materializer`] owns its intent.
///
/// Building a plan never touches the filesystem beyond read-only inspection
/// (see [`Materializer::classify`](crate::materialize::Materializer::classify));
/// only [`Plan::execute`] mutates anything, and it still honors `ctx.dry_run`
/// exactly like every `Action` already does.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    steps: Vec<PlanStep>,
    guards: Vec<Option<crate::materialize::disposal::PathGuard>>,
    // Children behind a replaceable scaffold have no inspectable actual state yet.
    blocked_by: Vec<Option<std::path::PathBuf>>,
    desired: DesiredTree,
}

impl Plan {
    /// Classify every entry in `desired` against current state, asking the
    /// registered [`Materializer`](crate::materialize::Materializer) that
    /// owns each entry's intent. Since #110 every intent has a registered
    /// backend. Children blocked by a replaceable scaffold are deferred until
    /// execution can classify their actual state without following that parent.
    pub fn build(desired: &DesiredTree) -> Result<Self> {
        let registry = MaterializerRegistry::default();
        let mut steps = Vec::with_capacity(desired.len());
        let mut blocked_by = Vec::with_capacity(desired.len());
        for entry in desired.entries() {
            let blocker = desired.blocking_scaffold(&entry.target)?;
            blocked_by.push(blocker.clone());
            if blocker.is_some() {
                // Do not read through a scaffold symlink or pretend ENOTDIR means missing.
                steps.push(PlanStep {
                    entry: entry.clone(),
                    operation: Operation::Deferred,
                });
                continue;
            }
            let operation = classify(desired, entry, &registry)?;
            steps.push(PlanStep {
                entry: entry.clone(),
                operation,
            });
        }
        let guards = steps
            .iter()
            .zip(&blocked_by)
            .map(|(step, blocker)| {
                if blocker.is_some() {
                    Ok(None)
                } else {
                    crate::materialize::disposal::PathGuard::capture(&step.entry.target).map(Some)
                }
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            steps,
            guards,
            blocked_by,
            desired: desired.clone(),
        })
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

    /// Provisioning must wait until child destinations can be inspected safely.
    pub(crate) fn has_blocked_children(&self) -> bool {
        self.blocked_by.iter().any(Option::is_some)
    }

    /// Reconcile scaffolds before parallel repository provisioning needs them.
    pub(crate) async fn prepare_scaffolds(&self, ctx: Ctx) -> Result<()> {
        self.validate_boundaries()?;
        let mut scaffolds = Self {
            desired: self.desired.clone(),
            ..Self::default()
        };
        for (index, step) in self.steps.iter().enumerate() {
            if matches!(step.entry.intent, MaterializationIntent::Directory) {
                // Keep clone orchestration in its normal progress-aware dispatcher;
                // only the parent authorization must happen before provisioning.
                scaffolds.steps.push(step.clone());
                scaffolds.guards.push(self.guards[index].clone());
                scaffolds.blocked_by.push(self.blocked_by[index].clone());
            }
        }
        scaffolds.execute(ctx).await
    }

    /// Execute every actionable step (`Create`/`Conflict`/an unblocked
    /// `Migrate`) in the plan's deterministic order (`DesiredTree` sorts
    /// entries by target path, so a directory's entry always precedes
    /// anything nested under it). Blocked children are reclassified after their
    /// scaffold is replaced; remaining `Noop`/`Deferred` steps are skipped. A
    /// blocked `Migrate` fails preflight: silently skipping it could let
    /// descendants write through an unresolved parent and run success hooks.
    pub async fn execute(&self, ctx: Ctx) -> Result<()> {
        self.validate_boundaries()?;
        let has_actionable = self.steps.iter().any(|s| is_actionable(&s.operation));
        if !has_actionable {
            return Ok(());
        }

        let registry = MaterializerRegistry::default();
        let mut guards = self.guards.clone();
        for guard in guards.iter().flatten() {
            guard.verify()?;
        }
        let progress = ProgressBar::new_spinner()
            .with_style(SPINNER_STYLE.clone())
            .with_message("");

        for (index, step) in self.steps.iter().enumerate() {
            let refreshed;
            let step = if let Some(parent) = &self.blocked_by[index] {
                if ctx.dry_run {
                    progress.println(format!(
                        "inspect '{}' after replacing scaffold '{}'",
                        step.entry.target.display(),
                        parent.display()
                    ));
                    continue;
                }
                anyhow::ensure!(
                    self.desired
                        .blocking_scaffold(&step.entry.target)?
                        .is_none(),
                    "scaffold '{}' was not replaced; refusing child writes",
                    parent.display()
                );
                // Parent replacement changes both location and actual state. Reuse the
                // full classifier so checkout ownership/migration boundaries still apply.
                refreshed = PlanStep {
                    entry: step.entry.clone(),
                    operation: classify(&self.desired, &step.entry, &registry)?,
                };
                validate_boundary(&refreshed)?;
                &refreshed
            } else {
                step
            };
            if let Some(Some(guard)) = guards.get(index) {
                guard.verify()?;
            }
            if !is_actionable(&step.operation) {
                continue;
            }

            if ctx.verbose || ctx.dry_run {
                progress.println(format!("{step}"));
                if matches!(step.operation, Operation::Conflict { .. }) {
                    progress.println(format!("  {} '{}': {}", if ctx.dry_run { "would reconcile" } else { "reconcile" }, step.entry.target.display(),
                        if ctx.force { "overwrite authorized by --force; directories may be recursively removed" }
                        else if ctx.no_prompt { "unresolved conflict will fail without changes" }
                        else { "requires interactive skip/overwrite/absorb choice before destructive action" }));
                }
            }
            progress.set_message(format!("{step}"));

            let materializer = registry.find(&step.entry.intent).with_context(|| {
                format!(
                    "no materializer registered for '{}'",
                    step.entry.target.display()
                )
            })?;
            materializer
                .materialize(ctx.clone(), step)
                .await
                .with_context(|| {
                    format!("failed to reconcile '{}'", step.entry.target.display())
                })?;
            if !ctx.dry_run
                && step.entry.target.is_dir()
                && !step.entry.target.is_symlink()
                && matches!(
                    step.operation,
                    Operation::Migrate { .. } | Operation::Conflict { .. }
                )
            {
                // A completed parent-link migration intentionally changes the
                // physical locations of its children. Refresh only that scope.
                for (child, guard) in self.steps.iter().zip(&mut guards).skip(index + 1) {
                    if child.entry.target.starts_with(&step.entry.target)
                        && self
                            .desired
                            .blocking_scaffold(&child.entry.target)?
                            .is_none()
                    {
                        *guard = Some(crate::materialize::disposal::PathGuard::capture(
                            &child.entry.target,
                        )?);
                    }
                }
            }
        }

        progress.finish_and_clear();
        Ok(())
    }

    /// Refuse before any child can execute through an unresolved parent.
    pub(crate) fn validate_boundaries(&self) -> Result<()> {
        for step in &self.steps {
            validate_boundary(step)?;
        }
        Ok(())
    }
}

fn validate_boundary(step: &PlanStep) -> Result<()> {
    if matches!(step.entry.intent, MaterializationIntent::Checkout)
        && matches!(step.operation, Operation::Conflict { .. })
    {
        anyhow::bail!(
            "refusing checkout replacement at '{}': legacy source ownership is not established; move it aside or unapply its old layout",
            step.entry.target.display()
        );
    }
    if let Operation::Migrate {
        blocked: Some(reason),
        ..
    } = &step.operation
    {
        anyhow::bail!(
            "migration blocked at '{}': {reason}",
            step.entry.target.display()
        );
    }
    Ok(())
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
        let migrate = self
            .steps
            .iter()
            .filter(|s| matches!(s.operation, Operation::Migrate { blocked: None, .. }))
            .count();
        let blocked = self
            .steps
            .iter()
            .filter(|s| {
                matches!(
                    s.operation,
                    Operation::Migrate {
                        blocked: Some(_),
                        ..
                    }
                )
            })
            .count();
        let deferred = self
            .steps
            .iter()
            .filter(|s| matches!(s.operation, Operation::Deferred))
            .count();
        let repair = self
            .steps
            .iter()
            .filter(|s| matches!(s.operation, Operation::Repair { .. }))
            .count();

        writeln!(
            f,
            "{} {} to create, {} unchanged, {} conflict(s), {} to migrate, \
             {} migration(s) blocked, {} to repair, {} deferred",
            style::white_b("Plan:"),
            create,
            noop,
            conflicts,
            migrate,
            blocked,
            repair,
            deferred,
        )?;
        for (index, step) in self
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| !matches!(s.operation, Operation::Noop))
        {
            if let Some(parent) = &self.blocked_by[index] {
                writeln!(
                    f,
                    "  inspect '{}' after replacing scaffold '{}'",
                    step.entry.target.display(),
                    parent.display()
                )?;
            } else {
                writeln!(f, "  {step}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::symlink::LinkType;
    use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
    use crate::exec::Context;
    use crate::overlays::{Overlay, Repository};

    #[cfg(unix)]
    #[tokio::test]
    async fn an_ancestor_retargeted_after_planning_cannot_redirect_writes() {
        let td = tempfile::tempdir().unwrap();
        let first = td.path().join("first");
        let second = td.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let alias = td.path().join("alias");
        std::os::unix::fs::symlink(&first, &alias).unwrap();
        let source = td.path().join("source");
        std::fs::write(&source, "overlay").unwrap();
        let desired = DesiredTree::from_entries(vec![DesiredEntry {
            target: alias.join("file"),
            provenance: Provenance::Overlay {
                overlay: "ov".into(),
                source: source.clone(),
            },
            intent: MaterializationIntent::SymlinkFile {
                source,
                link_type: LinkType::Soft,
            },
            permissions: None,
        }]);
        let plan = Plan::build(&desired).unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&second, &alias).unwrap();
        assert!(
            plan.execute(Context::builder().force(true).build())
                .await
                .is_err()
        );
        assert!(!first.join("file").exists());
        assert!(!second.join("file").exists());
    }
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

    #[rstest]
    #[case(false, false)]
    #[case(true, false)]
    #[case(true, true)]
    #[tokio::test]
    async fn a_file_scaffold_requires_authorization_before_nested_content_is_materialized(
        #[case] force: bool,
        #[case] dry_run: bool,
    ) {
        let (td, repo) = repo_and_root();
        td.child("ov/over.yaml")
            .write_str("target: '~/blocked'")
            .unwrap();
        td.child("ov/nested/deeper/file")
            .write_str("managed")
            .unwrap();
        td.child("home/blocked")
            .write_str("preserve until authorized")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let context = Context::builder()
            .repository(repo)
            .root(td.path().join("home"))
            .no_prompt(true)
            .force(force)
            .dry_run(dry_run)
            .build();
        let desired = DesiredTree::build(&context, &overlay).unwrap();
        desired.validate_ownership().unwrap();
        let plan = Plan::build(&desired).unwrap();
        assert!(plan.has_conflicts());
        let result = overlay.apply(&context).await;
        if force && !dry_run {
            result.unwrap();
            assert_eq!(
                fs::read_to_string(td.path().join("home/blocked/nested/deeper/file")).unwrap(),
                "managed"
            );
            assert!(
                Plan::build(&desired)
                    .unwrap()
                    .steps()
                    .iter()
                    .all(|step| matches!(step.operation, Operation::Noop))
            );
        } else {
            assert_eq!(result.is_err(), !dry_run);
            assert_eq!(
                fs::read_to_string(td.path().join("home/blocked")).unwrap(),
                "preserve until authorized"
            );
            assert_eq!(fs::read_dir(td.path().join("home")).unwrap().count(), 1);
        }
    }

    fn ctx(root: PathBuf, repo: Repository, overlay: Option<Overlay>) -> Ctx {
        let mut builder = Context::builder().root(root).repository(repo).verbose(true);
        if let Some(o) = overlay {
            builder = builder.overlay(o);
        }
        builder.build()
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    #[tokio::test]
    async fn replacing_a_foreign_scaffold_link_never_writes_through_its_old_referent(
        #[case] force: bool,
    ) {
        let (td, repo) = repo_and_root();
        td.child("ov/over.yaml")
            .write_str("target: '~/blocked'")
            .unwrap();
        td.child("ov/nested/file").write_str("managed").unwrap();
        td.child("foreign/nested/file")
            .write_str("foreign content")
            .unwrap();
        td.child("home").create_dir_all().unwrap();
        symlink::symlink_dir(td.path().join("foreign"), td.path().join("home/blocked")).unwrap();
        let overlay = repo.get("ov").unwrap();
        let context = Context::builder()
            .repository(repo)
            .root(td.path().join("home"))
            .no_prompt(true)
            .force(force)
            .build();
        let desired = DesiredTree::build(&context, &overlay).unwrap();
        let plan = Plan::build(&desired).unwrap();
        assert_eq!(
            plan.blocked_by
                .iter()
                .filter(|parent| parent.is_some())
                .count(),
            2
        );
        assert!(format!("{plan}").contains("after replacing scaffold"));
        let result = plan.execute(context).await;
        assert_eq!(
            fs::read_to_string(td.path().join("foreign/nested/file")).unwrap(),
            "foreign content"
        );
        assert!(!td.path().join("foreign/nested/file").is_symlink());
        if force {
            result.unwrap();
            assert!(!td.path().join("home/blocked").is_symlink());
            assert_eq!(
                fs::read_to_string(td.path().join("home/blocked/nested/file")).unwrap(),
                "managed"
            );
        } else {
            assert!(result.is_err());
            assert!(td.path().join("home/blocked").is_symlink());
        }
    }

    #[rstest]
    #[case("file")]
    #[case("nested/file")]
    fn an_unmanaged_non_directory_ancestor_remains_a_planning_error(#[case] suffix: &str) {
        let td = TempDir::new().unwrap();
        td.child("blocker").write_str("unmanaged").unwrap();
        let desired = DesiredTree::from_entries(vec![DesiredEntry {
            target: td.path().join("blocker").join(suffix),
            provenance: Provenance::Overlay {
                overlay: "ov".into(),
                source: td.path().join("source"),
            },
            intent: MaterializationIntent::SymlinkFile {
                source: td.path().join("source"),
                link_type: LinkType::Soft,
            },
            permissions: None,
        }]);
        assert!(desired.validate_ownership().is_err());
        assert!(Plan::build(&desired).is_err());
        assert_eq!(
            fs::read_to_string(td.path().join("blocker")).unwrap(),
            "unmanaged"
        );
    }

    #[rstest]
    #[case(false, false)]
    #[case(true, false)]
    #[case(true, true)]
    #[tokio::test]
    async fn clone_provisioning_waits_for_scaffold_replacement_authorization(
        #[case] force: bool,
        #[case] dry_run: bool,
    ) {
        let (td, repo) = repo_and_root();
        let upstream = git2::Repository::init(td.path().join("upstream")).unwrap();
        let tree_id = upstream.index().unwrap().write_tree().unwrap();
        let tree = upstream.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.com").unwrap();
        upstream
            .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        td.child("ov/over.yaml")
            .write_str(&format!(
                "target: '~/blocked'\ngit:\n  checkout: '{}'",
                td.path().join("upstream").display()
            ))
            .unwrap();
        td.child("ov/checkout/config").write_str("overlay").unwrap();
        td.child("home/blocked").write_str("preserve").unwrap();
        let overlay = repo.get("ov").unwrap();
        let context = Context::builder()
            .repository(repo)
            .root(td.path().join("home"))
            .no_prompt(true)
            .force(force)
            .dry_run(dry_run)
            .build();
        let result = overlay.apply(&context).await;
        if force && !dry_run {
            result.unwrap();
            assert!(td.path().join("home/blocked/checkout/.git").is_dir());
            assert_eq!(
                fs::read_to_string(td.path().join("home/blocked/checkout/config")).unwrap(),
                "overlay"
            );
        } else {
            assert_eq!(result.is_err(), !dry_run);
            assert_eq!(
                fs::read_to_string(td.path().join("home/blocked")).unwrap(),
                "preserve"
            );
        }
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

    #[test]
    fn unblocked_migrate_is_actionable() {
        let op = Operation::Migrate {
            from: MaterializationIntent::Checkout,
            to: MaterializationIntent::Directory,
            blocked: None,
        };
        assert!(is_actionable(&op));
    }

    #[test]
    fn repair_is_actionable() {
        let op = Operation::Repair {
            current: crate::overlays::FileMode::parse("644").unwrap(),
            desired: crate::overlays::FileMode::parse("600").unwrap(),
        };
        assert!(is_actionable(&op));
    }

    #[test]
    fn blocked_migrate_is_not_actionable() {
        let op = Operation::Migrate {
            from: MaterializationIntent::Checkout,
            to: MaterializationIntent::Directory,
            blocked: Some("has uncommitted changes".to_string()),
        };
        assert!(!is_actionable(&op));
    }

    #[tokio::test]
    async fn blocked_migration_reports_failure_without_executing() {
        let (td, repo) = repo_and_root();
        let c = ctx(td.path().to_path_buf(), repo, None);
        let target = td.path().join("some_target");
        let entry = DesiredEntry {
            target: target.clone(),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/src"),
            },
            intent: MaterializationIntent::SymlinkDirectory {
                source: PathBuf::from("/src"),
                link_type: LinkType::Soft,
            },
            permissions: None,
        };
        let plan = Plan {
            guards: Vec::new(),
            blocked_by: vec![None],
            desired: DesiredTree::default(),
            steps: vec![PlanStep {
                entry,
                operation: Operation::Migrate {
                    from: MaterializationIntent::Checkout,
                    to: MaterializationIntent::SymlinkDirectory {
                        source: PathBuf::from("/src"),
                        link_type: LinkType::Soft,
                    },
                    blocked: Some("has uncommitted changes".to_string()),
                },
            }],
        };
        // No materializer would even be looked up for this step since it's
        // skipped — if it weren't, this would fail trying to remove/link a
        // nonexistent `/src`.
        assert!(plan.execute(c).await.is_err());
        assert!(!target.exists());
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

    // ── #129: end-to-end rule-change migrations ─────────────────────────
    //
    // These use a `.link.toml` sidecar (not `link_dirs`/`rules`) for the
    // symlink side of each scenario specifically so `conf` doesn't *also*
    // exist as a real directory in the overlay's own source tree —
    // `overlay.git` and `walk_overlay_tree` are two independent,
    // uncoordinated `DesiredEntry` sources (a pre-existing gap, not #129's
    // to fix), and having both resolve the same target would produce two
    // competing entries instead of the single one these tests need.

    fn init_committed_repo(path: &std::path::Path) {
        let repo = git2::Repository::init(path).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@test.com").unwrap();
        drop(cfg);
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        fs::write(path.join("README.md"), "# Test").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }

    #[tokio::test]
    async fn root_level_checkout_coexists_with_its_own_base_directory_entry() {
        // `git = "<url>"` (the `ROOT_PATH` shorthand) makes the overlay's
        // entire target root a checkout — `collect_own_entries` still
        // unconditionally emits a plain `Directory` entry for that exact
        // same root too (every overlay needs a place to live). A second
        // `apply` (or a `--force` one, as here) must never treat the
        // already-cloned checkout as a pending `Directory` migration and
        // delete it — a real regression #129 introduced and fixed before
        // landing.
        //
        // `target`/`root` must be a *separate* directory from the
        // repository/overlay descriptor tree here (unlike most other tests
        // in this file, which reuse the same `td` for both): the clone
        // target is the root itself, and `libgit2` refuses to clone into a
        // non-empty directory — which the repository root always is, since
        // it holds the overlay's own `ov/` descriptor.
        let (home, repo) = repo_and_root();
        let overlay_dir = home.child("ov");
        overlay_dir.create_dir_all().unwrap();
        let source_repo = home.child("source_repo");
        source_repo.create_dir_all().unwrap();
        init_committed_repo(source_repo.path());
        let repo_url = source_repo.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"~\"\ngit = \"{repo_url}\""))
            .unwrap();
        let root = TempDir::new().unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(
            root.path().to_path_buf(),
            repo.clone(),
            Some(overlay.clone()),
        );
        let clone_ctx = c
            .clone()
            .with_multiprogress(indicatif::MultiProgress::new());
        // Mirrors `Overlay::apply_inner`: `clone_repositories` runs before
        // `Plan::build`/`execute` (ADR-014) — by the time the plan sees the
        // `Checkout` entry, the repository already exists and classifies
        // as `Noop`, exactly like a real `apply`.
        crate::actions::git::clone_repositories(clone_ctx.clone(), &overlay, root.path())
            .await
            .unwrap();
        assert!(root.path().join(".git").exists());

        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(clone_ctx.clone())
            .await
            .unwrap();
        assert!(root.path().join(".git").exists(), "checkout must survive");

        // Re-apply (mirroring `over apply --force` run twice): the plan
        // must find nothing to migrate, and the checkout must survive.
        let desired2 = DesiredTree::build(&c, &overlay).unwrap();
        let plan2 = Plan::build(&desired2).unwrap();
        assert!(
            plan2
                .steps()
                .iter()
                .all(|s| matches!(s.operation, Operation::Noop)),
            "second plan should find nothing to do, got: {plan2}"
        );
        plan2.execute(clone_ctx).await.unwrap();
        assert!(root.path().join(".git").exists(), "checkout must survive");
    }

    #[tokio::test]
    async fn rule_change_to_checkout_preserves_a_foreign_sidecar_link() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        // Phase 1: a `.link.toml` sidecar symlinks `conf` to some other
        // real directory — apply creates the symlink.
        let elsewhere = td.child("elsewhere");
        elsewhere.create_dir_all().unwrap();
        let elsewhere_toml = elsewhere.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("conf.link.toml")
            .write_str(&format!("target = \"{elsewhere_toml}\""))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(c.clone())
            .await
            .unwrap();
        let conf_target = td.path().join("conf");
        assert!(conf_target.is_symlink(), "sidecar should create a symlink");

        // Phase 2: drop the sidecar, add `conf` to `overlay.git` instead.
        fs::remove_file(overlay_dir.path().join("conf.link.toml")).unwrap();
        let source_repo = td.child("source_repo");
        source_repo.create_dir_all().unwrap();
        init_committed_repo(source_repo.path());
        let repo_url = source_repo.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"~\"\n[git]\nconf = \"{repo_url}\""))
            .unwrap();

        let overlay2 = repo.get("ov").unwrap();
        let desired2 = DesiredTree::build(&c, &overlay2).unwrap();
        let plan2 = Plan::build(&desired2).unwrap();
        let step = plan2
            .steps()
            .iter()
            .find(|s| s.entry.target == conf_target)
            .unwrap();
        assert!(matches!(step.operation, Operation::Conflict { .. }));

        let clone_ctx = c
            .clone()
            .with_multiprogress(indicatif::MultiProgress::new());
        assert!(plan2.execute(clone_ctx).await.is_err());
        assert_eq!(fs::read_link(&conf_target).unwrap(), elsewhere.path());
        assert!(!conf_target.join(".git").exists());
    }

    #[tokio::test]
    async fn clean_checkout_migrates_back_to_symlink_when_git_entry_removed() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();

        // Phase 1: `conf` is a git checkout.
        let source_repo = td.child("source_repo");
        source_repo.create_dir_all().unwrap();
        init_committed_repo(source_repo.path());
        let repo_url = source_repo.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"~\"\n[git]\nconf = \"{repo_url}\""))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let clone_ctx = c
            .clone()
            .with_multiprogress(indicatif::MultiProgress::new());
        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(clone_ctx)
            .await
            .unwrap();
        let conf_target = td.path().join("conf");
        assert!(conf_target.join(".git").exists());

        // Phase 2: `overlay.git` is dropped, a `.link.toml` sidecar takes
        // over the same path instead.
        let elsewhere = td.child("elsewhere");
        elsewhere.create_dir_all().unwrap();
        let elsewhere_toml = elsewhere.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("conf.link.toml")
            .write_str(&format!("target = \"{elsewhere_toml}\""))
            .unwrap();

        let overlay2 = repo.get("ov").unwrap();
        let desired2 = DesiredTree::build(&c, &overlay2).unwrap();
        let plan2 = Plan::build(&desired2).unwrap();
        let step = plan2
            .steps()
            .iter()
            .find(|s| s.entry.target == conf_target)
            .unwrap();
        match &step.operation {
            Operation::Migrate {
                from,
                blocked: None,
                ..
            } => assert!(matches!(from, MaterializationIntent::Checkout)),
            other => panic!("expected a safe Migrate from Checkout, got {other:?}"),
        }

        let discard = Context::builder()
            .discard_checkout(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .build();
        plan2.execute(discard).await.unwrap();
        assert!(
            !conf_target.join(".git").exists(),
            "checkout should be gone"
        );
        assert!(conf_target.is_symlink(), "now a symlink instead");
        assert_eq!(fs::read_link(&conf_target).unwrap(), elsewhere.path());
    }

    #[tokio::test]
    async fn dirty_checkout_blocks_migration_back_to_symlink() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();

        let source_repo = td.child("source_repo");
        source_repo.create_dir_all().unwrap();
        init_committed_repo(source_repo.path());
        let repo_url = source_repo.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"~\"\n[git]\nconf = \"{repo_url}\""))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let clone_ctx = c
            .clone()
            .with_multiprogress(indicatif::MultiProgress::new());
        let desired = DesiredTree::build(&c, &overlay).unwrap();
        Plan::build(&desired)
            .unwrap()
            .execute(clone_ctx)
            .await
            .unwrap();
        let conf_target = td.path().join("conf");

        // Make an uncommitted change inside the checkout.
        fs::write(conf_target.join("README.md"), "local edit").unwrap();

        let elsewhere = td.child("elsewhere");
        elsewhere.create_dir_all().unwrap();
        let elsewhere_toml = elsewhere.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("conf.link.toml")
            .write_str(&format!("target = \"{elsewhere_toml}\""))
            .unwrap();

        let overlay2 = repo.get("ov").unwrap();
        let desired2 = DesiredTree::build(&c, &overlay2).unwrap();
        let plan2 = Plan::build(&desired2).unwrap();
        let step = plan2
            .steps()
            .iter()
            .find(|s| s.entry.target == conf_target)
            .unwrap();
        match &step.operation {
            Operation::Migrate { blocked: None, .. } => {}
            other => panic!("expected consent-required checkout disposal, got {other:?}"),
        }

        // Even with `--force`, the dirty checkout must stay untouched.
        let force_ctx = Context::builder()
            .force(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay2.clone())
            .build();
        assert!(plan2.execute(force_ctx).await.is_err());
        assert!(conf_target.join(".git").exists(), "checkout must remain");
        assert_eq!(
            fs::read_to_string(conf_target.join("README.md")).unwrap(),
            "local edit",
            "uncommitted change must remain"
        );
    }
}
