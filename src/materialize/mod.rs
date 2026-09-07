//! Materialization backends (#108).
//!
//! [`crate::plan::Plan`] classifies and executes each
//! [`DesiredEntry`](crate::desired::DesiredEntry) by asking exactly one
//! registered [`Materializer`] backend: the one that
//! [`Materializer::handles`] the entry's
//! [`MaterializationIntent`](crate::desired::MaterializationIntent). `Plan`
//! itself never knows about concrete `actions::fs`/`actions::symlink` types,
//! nor (eventually) about Git — that lives entirely in the backends here.
//!
//! ```text
//! DesiredEntry (intent)
//!       ↓
//! MaterializerRegistry::find  — routes by intent to one backend
//!       ↓
//! Materializer::classify      — read-only actual-state inspection
//!       ↓
//! Materializer::materialize   — concrete filesystem/Git changes
//! ```
//!
//! [`MaterializerRegistry`] registers three backends: [`PartialFileMaterializer`]
//! (#66, [`MaterializationIntent::PartialFile`](crate::desired::MaterializationIntent::PartialFile)),
//! [`SymlinkMaterializer`] (every remaining intent except `Checkout`), and
//! [`CheckoutMaterializer`] (#110), which owns
//! [`MaterializationIntent::Checkout`](crate::desired::MaterializationIntent::Checkout) —
//! ensuring a git checkout/worktree is present and configured, exactly like
//! `actions::git::clone_repositories` already does. It does *not* implement
//! content-level bidirectional sync (fetch/merge/push): that's `over sync`
//! (`crate::sync`), a separate, explicit operation — see
//! [ADR-014](https://github.com/noirbizarre/over/blob/main/docs/adr/014-bidirectional-checkout-synchronization.md).
//! #113's rule-migration semantics are a later extension of the same
//! registry.

mod checkout;
mod partial;
mod symlink;

use anyhow::Result;
use async_trait::async_trait;

use crate::desired::{DesiredEntry, MaterializationIntent};
use crate::exec::Ctx;
use crate::plan::{Operation, PlanStep};

pub use checkout::CheckoutMaterializer;
pub use partial::PartialFileMaterializer;
pub use symlink::SymlinkMaterializer;

/// A materialization backend: owns both read-only actual-state inspection
/// and execution for the [`MaterializationIntent`] variants it
/// [`handles`](Materializer::handles).
///
/// `?Send`: `materialize` dispatches to `Box<dyn Action>` internally
/// ([`crate::exec::Action`] itself isn't `Send`-bound), and — like
/// `Plan::execute` before this issue — is only ever awaited inline, never
/// spawned onto another task.
#[async_trait(?Send)]
pub trait Materializer: Send + Sync {
    /// Does this backend own the given intent? Used by
    /// [`MaterializerRegistry::find`] to route each entry to exactly one
    /// materializer. An intent no backend claims is treated as
    /// `Operation::Deferred` by `Plan::build`.
    fn handles(&self, intent: &MaterializationIntent) -> bool;

    /// Inspect actual state and classify a single entry. Read-only — never
    /// mutates the filesystem (or any other backing store).
    fn classify(&self, entry: &DesiredEntry) -> Result<Operation>;

    /// Turn an actionable (`Create`/`Conflict`) step into concrete changes.
    /// Never called for `Noop`/`Deferred` steps.
    async fn materialize(&self, ctx: Ctx, step: &PlanStep) -> Result<()>;
}

/// The set of registered [`Materializer`] backends, consulted by
/// [`crate::plan::Plan`] to classify and execute every
/// [`DesiredEntry`].
pub struct MaterializerRegistry {
    backends: Vec<Box<dyn Materializer>>,
}

impl Default for MaterializerRegistry {
    fn default() -> Self {
        Self {
            backends: vec![
                // Must come before `SymlinkMaterializer`: `PartialFile` is
                // a distinct intent, but registration order matters for
                // any future intent whose `handles()` might overlap.
                Box::new(PartialFileMaterializer),
                Box::new(SymlinkMaterializer),
                Box::new(CheckoutMaterializer),
            ],
        }
    }
}

impl MaterializerRegistry {
    /// The backend that owns `intent`, if any is registered for it.
    pub fn find(&self, intent: &MaterializationIntent) -> Option<&dyn Materializer> {
        self.backends
            .iter()
            .find(|m| m.handles(intent))
            .map(|b| b.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::Provenance;
    use std::path::PathBuf;

    fn entry(intent: MaterializationIntent) -> DesiredEntry {
        DesiredEntry {
            target: PathBuf::from("/tmp/does-not-matter"),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/repo/ov"),
            },
            intent,
            permissions: None,
        }
    }

    #[test]
    fn registry_finds_symlink_materializer_for_directory() {
        let registry = MaterializerRegistry::default();
        assert!(registry.find(&MaterializationIntent::Directory).is_some());
    }

    #[test]
    fn registry_finds_symlink_materializer_for_symlink_file() {
        let registry = MaterializerRegistry::default();
        let intent = MaterializationIntent::SymlinkFile {
            source: PathBuf::from("/repo/ov/file.txt"),
            link_type: crate::actions::symlink::LinkType::Soft,
        };
        assert!(registry.find(&intent).is_some());
    }

    #[test]
    fn registry_finds_checkout_materializer_for_checkout() {
        let registry = MaterializerRegistry::default();
        assert!(registry.find(&MaterializationIntent::Checkout).is_some());
    }

    #[test]
    fn registry_finds_partial_file_materializer_for_partial_file() {
        let registry = MaterializerRegistry::default();
        let intent = MaterializationIntent::PartialFile {
            content: "alias x=y".to_string(),
            marker: "m".to_string(),
        };
        assert!(registry.find(&intent).is_some());
    }

    #[test]
    fn registry_find_uses_entry_intent() {
        let registry = MaterializerRegistry::default();
        let e = entry(MaterializationIntent::Directory);
        assert!(registry.find(&e.intent).is_some());
    }
}
