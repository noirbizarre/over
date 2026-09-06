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
//! everything it transitively `uses`, translating today's *implicit*
//! symlink-first rules (`link_dirs`, `exclude`, `.link.*` sidecars,
//! `.partial.*` sidecars (#66), `git`) into an explicit list of
//! [`DesiredEntry`] values. It never touches the filesystem beyond
//! read-only inspection already required by that resolution (e.g.
//! checking whether a `.link.*` sidecar target is a directory).
//!
//! ## What's deliberately *not* here
//!
//! - Overlay discovery without an `over.yml` marker, and a `defaults:`/
//!   `rules:` configuration syntax with path/subtree overrides — that's
//!   #113, layered on top of this model later.
//! - Rendered file content (`DesiredEntry` has no `content` field) — #61.
//! - Permission metadata (`DesiredEntry` has no `permissions` field) — #65.
//! - Comparing against actual filesystem state and materializing anything —
//!   that's [`crate::plan`] (#13) and [`crate::materialize`] (#108).
//!   `DesiredTree` is consumed by those, not a replacement for
//!   `Overlay::apply` itself.
//! - `install` config (package managers): not filesystem materialization, so
//!   it has no representation in `DesiredTree`.

mod entry;
mod tree;

pub use entry::{DesiredEntry, EntryKind, MaterializationIntent, Provenance};
pub use tree::DesiredTree;
