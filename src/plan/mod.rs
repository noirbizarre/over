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
//! Plan::execute               (delegates each step to the registered
//!                              crate::materialize::Materializer that owns
//!                              its intent, #108)
//! ```
//!
//! [`Plan::build`] is read-only, like `DesiredTree::build`; only
//! [`Plan::execute`] mutates anything, and it still honors `ctx.dry_run`
//! exactly like every `Action` already does. `Plan::build` works over *any*
//! `DesiredTree` — a single overlay's own entries (what `Overlay::apply`
//! uses today) or a full `uses`-graph — so the same type is reusable by
//! `status`/`diff` (#12/#109) later.
//!
//! Neither `Plan::build` nor `Plan::execute` know about concrete
//! `actions::fs`/`actions::symlink` types, or about Git: both ask a
//! [`crate::materialize::MaterializerRegistry`] for the backend that owns
//! each entry's `MaterializationIntent` (#108).
//!
//! ## What's deliberately *not* here
//!
//! - A real Git-checkout backend: [`MaterializationIntent::Checkout`]
//!   entries have no registered [`crate::materialize::Materializer`] yet,
//!   so they're classified as [`Operation::Deferred`] and never executed —
//!   #110 registers a `CheckoutMaterializer` to change that, without this
//!   module changing. `Overlay::apply` still clones git repositories
//!   through the existing, separate `actions::git::clone_repositories`,
//!   entirely orthogonal to this module.
//! - Materialization-rule migrations (symlink ↔ checkout, file-level ↔
//!   directory-level symlink) and `defaults:`/`rules:` configuration —
//!   #113. [`Operation`] is designed to grow variants for this without a
//!   `Plan`/`PlanStep` shape change.
//! - `status`/`diff`/`unapply` commands themselves — #12/#109/#64. This
//!   module only makes `Plan` reusable for them.

pub(crate) mod actual;
mod reconcile;
mod step;

pub use actual::ActualState;
pub use reconcile::Plan;
pub use step::{Operation, PlanStep};
