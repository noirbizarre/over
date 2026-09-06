use std::fmt;

use crate::desired::{DesiredEntry, MaterializationIntent};
use crate::ui::{emojis, style};
use crate::utils::short_path;

use super::actual::ActualState;

/// What a single [`PlanStep`] needs to do to reconcile actual state with
/// [`DesiredEntry`] intent.
///
/// Deliberately a small, closed set today. One extension point this issue
/// is asked to leave open, without implementing it yet:
///
/// - #113 will add rule-change transitions (symlink ↔ checkout, file-level
///   ↔ directory-level symlink) as new variants here (e.g. a future
///   `Migrate`) rather than requiring a different `Plan`/[`PlanStep`] shape.
///
/// #108 already replaced [`super::Plan::execute`]'s direct dispatch on
/// [`MaterializationIntent`] with a lookup into a registered
/// [`crate::materialize::Materializer`] per intent — `Operation` itself
/// didn't need to change for that.
#[derive(Debug, Clone)]
pub enum Operation {
    /// Target is missing; safe to materialize.
    Create,
    /// Target already matches the desired intent — nothing to do.
    Noop,
    /// Target exists and does not match the desired intent.
    Conflict { current: ActualState },
    /// No registered [`crate::materialize::Materializer`] claims this
    /// entry's intent yet (today, only
    /// [`MaterializationIntent::Checkout`] — #110 owns turning this into a
    /// real checkout/worktree). Carried so a plan preview can still report
    /// on it; [`super::Plan::execute`] never acts on it.
    Deferred,
}

/// One [`DesiredEntry`] paired with the [`Operation`] needed to reconcile it
/// against actual filesystem state.
#[derive(Debug, Clone)]
pub struct PlanStep {
    pub entry: DesiredEntry,
    pub operation: Operation,
}

impl fmt::Display for PlanStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = short_path(&self.entry.target.to_string_lossy());

        match (&self.entry.intent, &self.operation) {
            (MaterializationIntent::Directory, Operation::Create) => write!(
                f,
                "{} {} {}",
                emojis::DIRECTORY,
                style::white("create directory:"),
                target,
            ),
            (MaterializationIntent::Directory, Operation::Noop) => write!(
                f,
                "{} {} {} ({})",
                emojis::CHECKMARK,
                style::white("directory:"),
                target,
                style::white("already exists"),
            ),
            (MaterializationIntent::Directory, Operation::Conflict { current }) => write!(
                f,
                "{} {} expected directory at {}, found {}",
                emojis::WARNING,
                style::yellow("conflict:"),
                target,
                current,
            ),
            (
                MaterializationIntent::SymlinkFile { source, .. }
                | MaterializationIntent::SymlinkDirectory { source, .. },
                Operation::Create,
            ) => write!(
                f,
                "{} {} {} -> {}",
                emojis::LINK,
                style::white("link:"),
                short_path(&source.to_string_lossy()),
                target,
            ),
            (
                MaterializationIntent::SymlinkFile { source, .. }
                | MaterializationIntent::SymlinkDirectory { source, .. },
                Operation::Noop,
            ) => write!(
                f,
                "{} {} {} ({} {})",
                emojis::CHECKMARK,
                style::white("link:"),
                target,
                style::white("already linked to"),
                short_path(&source.to_string_lossy()),
            ),
            (
                MaterializationIntent::SymlinkFile { source, .. }
                | MaterializationIntent::SymlinkDirectory { source, .. },
                Operation::Conflict { current },
            ) => write!(
                f,
                "{} {} {} already exists ({}), overlay expects a link to {}",
                emojis::WARNING,
                style::yellow("conflict:"),
                target,
                current,
                short_path(&source.to_string_lossy()),
            ),
            (MaterializationIntent::Checkout, Operation::Create) => write!(
                f,
                "{} {} {}",
                emojis::THREAD,
                style::white("clone repository:"),
                target,
            ),
            (MaterializationIntent::Checkout, Operation::Noop) => write!(
                f,
                "{} {} {} ({})",
                emojis::CHECKMARK,
                style::white("checkout:"),
                target,
                style::white("present (use `over sync` to update)"),
            ),
            // `CheckoutMaterializer::classify` never returns `Conflict` (git
            // states are surfaced by `over status`/`over diff`/`over sync`
            // instead, never routed through filesystem conflict
            // resolution) — unreachable in practice, but a clear fallback
            // beats a silently wrong line.
            (MaterializationIntent::Checkout, Operation::Conflict { current }) => write!(
                f,
                "{} {} {} ({})",
                emojis::WARNING,
                style::yellow("conflict:"),
                target,
                current,
            ),
            (MaterializationIntent::PartialFile { marker, .. }, Operation::Create) => write!(
                f,
                "{} {} {} ({} '{}')",
                emojis::BLOCK,
                style::white("insert block:"),
                target,
                style::white("marker"),
                marker,
            ),
            (MaterializationIntent::PartialFile { marker, .. }, Operation::Noop) => write!(
                f,
                "{} {} {} ({} '{}' {})",
                emojis::CHECKMARK,
                style::white("block:"),
                target,
                style::white("marker"),
                marker,
                style::white("already present"),
            ),
            (
                MaterializationIntent::PartialFile { marker, .. },
                Operation::Conflict { current },
            ) => {
                write!(
                    f,
                    "{} {} block '{}' in {} doesn't match ({})",
                    emojis::WARNING,
                    style::yellow("conflict:"),
                    marker,
                    target,
                    current,
                )
            }
            // Every intent has a registered `Materializer` since #110; no
            // step should ever classify as `Deferred` anymore — unreachable
            // in practice, but a clear fallback beats a silently wrong line.
            (_, Operation::Deferred) => write!(f, "{} deferred: {}", emojis::WARNING, target),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::symlink::LinkType;
    use crate::desired::Provenance;
    use std::path::PathBuf;

    fn entry(intent: MaterializationIntent) -> DesiredEntry {
        DesiredEntry {
            target: PathBuf::from("/home/user/.config/app"),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/repo/ov/app"),
            },
            intent,
        }
    }

    #[test]
    fn create_directory_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::Directory),
            operation: Operation::Create,
        };
        let s = format!("{step}");
        assert!(s.contains("create directory:"));
        assert!(s.contains(".config/app"));
    }

    #[test]
    fn noop_directory_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::Directory),
            operation: Operation::Noop,
        };
        let s = format!("{step}");
        assert!(s.contains("already exists"));
    }

    #[test]
    fn conflict_directory_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::Directory),
            operation: Operation::Conflict {
                current: ActualState::File,
            },
        };
        let s = format!("{step}");
        assert!(s.contains("conflict:"));
        assert!(s.contains("a file"));
    }

    #[test]
    fn create_symlink_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::SymlinkFile {
                source: PathBuf::from("/repo/ov/app/file.txt"),
                link_type: LinkType::Soft,
            }),
            operation: Operation::Create,
        };
        let s = format!("{step}");
        assert!(s.contains("link:"));
        assert!(s.contains("file.txt"));
    }

    #[test]
    fn noop_symlink_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::SymlinkDirectory {
                source: PathBuf::from("/repo/ov/app/dir"),
                link_type: LinkType::Soft,
            }),
            operation: Operation::Noop,
        };
        let s = format!("{step}");
        assert!(s.contains("already linked to"));
    }

    #[test]
    fn conflict_symlink_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::SymlinkFile {
                source: PathBuf::from("/repo/ov/app/file.txt"),
                link_type: LinkType::Soft,
            }),
            operation: Operation::Conflict {
                current: ActualState::Symlink {
                    points_to: PathBuf::from("/somewhere/else"),
                },
            },
        };
        let s = format!("{step}");
        assert!(s.contains("conflict:"));
        assert!(s.contains("a symlink to"));
    }

    #[test]
    fn create_checkout_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::Checkout),
            operation: Operation::Create,
        };
        let s = format!("{step}");
        assert!(s.contains("clone repository:"));
    }

    #[test]
    fn noop_checkout_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::Checkout),
            operation: Operation::Noop,
        };
        let s = format!("{step}");
        assert!(s.contains("checkout:"));
        assert!(s.contains("over sync"));
    }

    #[test]
    fn create_partial_file_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::PartialFile {
                content: "alias x=y".to_string(),
                marker: "aliases".to_string(),
            }),
            operation: Operation::Create,
        };
        let s = format!("{step}");
        assert!(s.contains("insert block:"));
        assert!(s.contains("aliases"));
    }

    #[test]
    fn noop_partial_file_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::PartialFile {
                content: "alias x=y".to_string(),
                marker: "aliases".to_string(),
            }),
            operation: Operation::Noop,
        };
        let s = format!("{step}");
        assert!(s.contains("already present"));
    }

    #[test]
    fn conflict_partial_file_display() {
        let step = PlanStep {
            entry: entry(MaterializationIntent::PartialFile {
                content: "alias x=y".to_string(),
                marker: "aliases".to_string(),
            }),
            operation: Operation::Conflict {
                current: ActualState::File,
            },
        };
        let s = format!("{step}");
        assert!(s.contains("conflict:"));
        assert!(s.contains("aliases"));
    }

    #[test]
    fn deferred_checkout_display() {
        // Unreachable in practice since #110 (every intent has a
        // registered Materializer), but the fallback message must still
        // render something sensible if ever constructed directly.
        let step = PlanStep {
            entry: entry(MaterializationIntent::Checkout),
            operation: Operation::Deferred,
        };
        let s = format!("{step}");
        assert!(s.contains("deferred"));
    }
}
