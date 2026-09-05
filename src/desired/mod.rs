//! The canonical desired filesystem state model (#107).
//!
//! `over` currently applies overlays directly, side effect by side effect
//! (`Overlay::apply` → `actions::fs::link`/`actions::git::clone_repositories`
//! → symlinks/directories/clones on disk). This module introduces
//! [`DesiredTree`]/[`DesiredEntry`] as a separate, read-only representation
//! of what `over` *wants* the filesystem to look like, independently from how
//! that state gets materialized:
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
//! symlink-first rules (`link_dirs`, `exclude`, `.link.*` sidecars, `git`)
//! into an explicit list of [`DesiredEntry`] values. It never touches the
//! filesystem beyond read-only inspection already required by that
//! resolution (e.g. checking whether a `.link.*` sidecar target is a
//! directory).
//!
//! ## What's deliberately *not* here
//!
//! - Overlay discovery without an `over.yml` marker, and a `defaults:`/
//!   `rules:` configuration syntax with path/subtree overrides — that's
//!   #113, layered on top of this model later.
//! - Rendered file content (`DesiredEntry` has no `content` field) — #61.
//! - Permission metadata (`DesiredEntry` has no `permissions` field) — #65.
//! - Comparing against actual filesystem state, building an execution
//!   `Plan`, or materializing anything — #13 and #108. `DesiredTree` is
//!   consumed by those, not a replacement for `Overlay::apply` yet.
//! - `install` config (package managers): not filesystem materialization, so
//!   it has no representation in `DesiredTree`.

mod entry;
mod tree;

pub use entry::{DesiredEntry, EntryKind, MaterializationIntent, Provenance};
pub use tree::DesiredTree;
