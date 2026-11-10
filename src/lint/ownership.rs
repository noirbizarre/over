//! Present shared desired-state ownership errors as lint diagnostics.

use std::collections::HashSet;

use crate::desired::{DesiredTree, ownership};

use super::Diagnostic;

pub(super) type ConflictKey = ownership::OwnershipIssue;

pub(super) fn check(
    root: &str,
    tree: &DesiredTree,
    seen: &mut HashSet<ConflictKey>,
) -> Vec<Diagnostic> {
    ownership::check(tree)
        .into_iter()
        .filter_map(|issue| {
            let message = issue.message.clone();
            seen.insert(issue).then(|| {
                Diagnostic::error(root, message).with_hint(
                    "rename, remap, exclude, or remove one conflicting declaration; uses is a flat union and has no override order",
                )
            })
        })
        .collect()
}
