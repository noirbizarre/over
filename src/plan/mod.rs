//! The execution plan and reconciliation pipeline (#13).
//!
//! `over` used to apply overlays directly: `Overlay::apply` walked the
//! overlay tree once, building and immediately executing `EnsureDir`/
//! `EnsureLink`/`EnsureDirLink`/`EnsureSymlink` actions as it went. Under
//! `--dry-run` this walked the *exact same* mutating code path with each
//! `Action::execute` short-circuiting before touching disk — which meant
//! dry-run could never actually tell "already applied" from "about to
//! create", and conflicts were only discovered lazily, action by action
//! (ADR-006).
//!
//! This module inserts a real reconciliation step between
//! [`DesiredTree`](crate::desired::DesiredTree) (what we want, #107) and the
//! filesystem (what exists):
//!
//! ```text
//! DesiredTree                (crate::desired, #107)
//!       +
//! actual filesystem state    (this module, actual.rs)
//!       ↓
//! Plan                       (this module — one PlanStep per DesiredEntry)
//!       ↓
//! Plan::execute               (translates steps into the existing, tested
//!                              actions::fs / actions::symlink Action impls)
//! ```
//!
//! [`Plan::build`] is read-only, like `DesiredTree::build`; only
//! [`Plan::execute`] mutates anything, and it still honors `ctx.dry_run`
//! exactly like every `Action` already does. `Plan::build` works over *any*
//! `DesiredTree` — a single overlay's own entries (what `Overlay::apply`
//! uses today) or a full `uses`-graph — so the same type is reusable by
//! `status`/`diff` (#12/#109) later.
//!
//! ## What's deliberately *not* here
//!
//! - Git checkout materialization: [`MaterializationIntent::Checkout`]
//!   entries are classified as [`Operation::Deferred`] and never executed
//!   by [`Plan::execute`] — #108/#110 own turning them into real
//!   checkouts/worktrees. `Overlay::apply` still clones git repositories
//!   through the existing, separate `actions::git::clone_repositories`,
//!   entirely orthogonal to this module.
//! - Materialization-rule migrations (symlink ↔ checkout, file-level ↔
//!   directory-level symlink) and `defaults:`/`rules:` configuration —
//!   #113. [`Operation`] is designed to grow variants for this without a
//!   `Plan`/`PlanStep` shape change.
//! - `status`/`diff`/`unapply` commands themselves — #12/#109/#64. This
//!   module only makes `Plan` reusable for them.

mod actual;
mod reconcile;
mod step;

pub use actual::ActualState;
pub use reconcile::Plan;
pub use step::{Operation, PlanStep};
