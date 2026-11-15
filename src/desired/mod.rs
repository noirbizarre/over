//! The canonical desired filesystem state model (#107).
//!
//! This module introduces [`DesiredTree`]/[`DesiredEntry`] as a separate,
//! read-only representation of what `over` *wants* the filesystem to look
//! like, independently from how that state gets materialized. `Overlay::
//! apply` (#13) builds a [`crate::plan::Plan`] from a `DesiredTree` and
//! executes it via a registered [`crate::materialize::Materializer`]
//! (#108) — git repository checkouts remain a separate,
//! `actions::git::clone_repositories` step, orthogonal to the plan, even
//! though #110's `CheckoutMaterializer` classifies them like any other
//! intent:
//!
//! ```text
//! root configuration
//!       +
//! optional overlay-local configuration
//!       +
//! Git/source tree
//!       +
//! templates/transformations
//!       ↓
//! resolved overlay hierarchy + rules   (Overlay/Repository, today's config surface)
//!       ↓
//! DesiredTree                          (this module)
//! ```
//!
//! [`DesiredTree::build`] walks an [`Overlay`](crate::overlays::Overlay) and
//! everything it transitively `uses`, translating symlink-first rules
//! (`mode`/`rules`, `exclude`, `.link.*` sidecars,
//! `.partial.*` sidecars (#66), `git`) into an explicit list of
//! [`DesiredEntry`] values. It never touches the filesystem beyond
//! read-only inspection already required by that resolution (e.g.
//! checking whether a `.link.*` sidecar target is a directory).
//!
//! ## What's deliberately *not* here
//!
//! - Overlay discovery, including descriptor-less overlays, belongs to
//!   [`Repository`](crate::overlays::Repository). Materialization configuration
//!   (`mode`/`rules`) is resolved via `Overlay::materialization_for` and
//!   `crate::overlays::rules` before this module walks the overlay tree.
//! - Rendered file bytes and source identity live in [`content`]; checkout units
//!   carry complete desired-leaf inventories without overlapping child entries.
//! - Permission policy applies to content Over writes directly: partial files,
//!   encrypted whole files, and rendered artifacts use `DesiredEntry.permissions`;
//!   generated checkout leaves carry assignments in their rendered inventory.
//!   Ordinary checkout files have no independent permission policy. Ordinary
//!   symlinks share their source's inode, so chmod would mutate source content
//!   and stays out of scope (see ADR-020).
//! - Comparing against actual filesystem state and materializing anything —
//!   that's [`crate::plan`] (#13) and [`crate::materialize`] (#108).
//!   `DesiredTree` is consumed by those, not a replacement for
//!   `Overlay::apply` itself.
//! - Lifecycle hooks: executable side effects have no representation in
//!   `DesiredTree`, including opted-in install hooks.

mod entry;
pub(crate) mod ownership;
mod tree;

pub use entry::{CheckoutScope, DesiredEntry, EntryKind, MaterializationIntent, Provenance};
pub use tree::DesiredTree;
pub mod content;
