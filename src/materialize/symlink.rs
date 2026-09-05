use anyhow::Result;
use async_trait::async_trait;

use crate::actions::fs::EnsureDirLink;
use crate::actions::{EnsureDir, EnsureLink, EnsureSymlink};
use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
use crate::exec::{Action, Ctx};
use crate::plan::actual::{self, ActualState};
use crate::plan::{Operation, PlanStep};

use super::Materializer;

/// The default materialization backend (ADR-006): plain directories and
/// symlinks (file- or directory-level), dispatched to the existing
/// `actions::fs`/`actions::symlink` `Action` impls. Owns every
/// [`MaterializationIntent`] except
/// [`MaterializationIntent::Checkout`](crate::desired::MaterializationIntent::Checkout),
/// which #110 will give its own backend.
///
/// This is a pure move of the logic that used to live directly in
/// `plan::reconcile` (`classify`/`build_action`) — no behavior change.
pub struct SymlinkMaterializer;

#[async_trait(?Send)]
impl Materializer for SymlinkMaterializer {
    fn handles(&self, intent: &MaterializationIntent) -> bool {
        !matches!(intent, MaterializationIntent::Checkout)
    }

    /// Classify a single entry against current filesystem state. Read-only.
    fn classify(&self, entry: &DesiredEntry) -> Result<Operation> {
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
                    ActualState::Symlink { ref points_to } if points_to == source => {
                        Operation::Noop
                    }
                    other => Operation::Conflict { current: other },
                })
            }
            MaterializationIntent::Checkout => {
                unreachable!("MaterializerRegistry only calls classify() after handles() passed")
            }
        }
    }

    async fn materialize(&self, ctx: Ctx, step: &PlanStep) -> Result<()> {
        let action = build_action(ctx.clone(), &step.entry);
        action.execute(ctx).await
    }
}

/// Translate an entry into the existing, tested `Action` that materializes
/// it — the single place that knows about concrete `actions::fs`/
/// `actions::symlink` types.
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
            unreachable!("SymlinkMaterializer::handles filters out Checkout entries")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::symlink::LinkType;
    use crate::exec::Context;
    use std::path::PathBuf;

    fn entry(intent: MaterializationIntent) -> DesiredEntry {
        DesiredEntry {
            target: PathBuf::from("/tmp/does-not-matter-for-this-test"),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/repo/ov"),
            },
            intent,
        }
    }

    #[test]
    fn handles_returns_false_for_checkout() {
        let m = SymlinkMaterializer;
        assert!(!m.handles(&MaterializationIntent::Checkout));
    }

    #[test]
    fn handles_returns_true_for_directory_and_symlinks() {
        let m = SymlinkMaterializer;
        assert!(m.handles(&MaterializationIntent::Directory));
        assert!(m.handles(&MaterializationIntent::SymlinkFile {
            source: PathBuf::from("/src"),
            link_type: LinkType::Soft,
        }));
        assert!(m.handles(&MaterializationIntent::SymlinkDirectory {
            source: PathBuf::from("/src"),
            link_type: LinkType::Soft,
        }));
    }

    #[test]
    fn missing_directory_classifies_as_create() {
        let m = SymlinkMaterializer;
        let e = entry(MaterializationIntent::Directory);
        assert!(matches!(m.classify(&e).unwrap(), Operation::Create));
    }

    #[test]
    fn existing_directory_classifies_as_noop() {
        use assert_fs::TempDir;
        let td = TempDir::new().unwrap();
        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: td.path().to_path_buf(),
            ..entry(MaterializationIntent::Directory)
        };
        assert!(matches!(m.classify(&e).unwrap(), Operation::Noop));
    }

    #[test]
    fn file_where_directory_expected_is_a_conflict() {
        use assert_fs::TempDir;
        let td = TempDir::new().unwrap();
        let blocked = td.path().join("blocked");
        std::fs::write(&blocked, "in the way").unwrap();
        let m = SymlinkMaterializer;
        let e = DesiredEntry {
            target: blocked,
            ..entry(MaterializationIntent::Directory)
        };
        assert!(matches!(
            m.classify(&e).unwrap(),
            Operation::Conflict { .. }
        ));
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
