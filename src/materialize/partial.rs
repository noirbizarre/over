//! Partial-file materialization backend (#66).
//!
//! Owns [`MaterializationIntent::PartialFile`]: a managed block injected
//! into an existing (possibly foreign) file, rather than a whole-target
//! symlink/directory/checkout. Classification is content-aware (it reads
//! the target file, unlike [`super::symlink::SymlinkMaterializer`]'s
//! purely structural `symlink_metadata` comparison) but still strictly
//! read-only — actual writes stay in
//! [`crate::actions::partial::EnsurePartialBlock`].

use std::fs;

use anyhow::Result;
use async_trait::async_trait;

use crate::actions::partial::{BlockState, EnsurePartialBlock, find_block};
use crate::desired::{DesiredEntry, MaterializationIntent};
use crate::exec::{Action, Ctx};
use crate::plan::actual::{self, ActualState};
use crate::plan::{Operation, PlanStep};

use super::Materializer;

/// Owns [`MaterializationIntent::PartialFile`]. See the module docs for
/// how this differs from [`super::symlink::SymlinkMaterializer`].
pub struct PartialFileMaterializer;

#[async_trait(?Send)]
impl Materializer for PartialFileMaterializer {
    fn handles(&self, intent: &MaterializationIntent) -> bool {
        matches!(intent, MaterializationIntent::PartialFile { .. })
    }

    /// Reads the target's content (when it's a plain file) to compare the
    /// managed block, not just its `symlink_metadata` — the one
    /// materializer backend for which "actual state" must mean more than
    /// structural type, since two different `PartialFile` entries can
    /// legitimately share the same target file (distinct `marker`s).
    fn classify(&self, entry: &DesiredEntry) -> Result<Operation> {
        let MaterializationIntent::PartialFile { content, marker } = &entry.intent else {
            unreachable!("MaterializerRegistry only calls classify() after handles() passed")
        };
        match actual::inspect(&entry.target)? {
            ActualState::Missing => Ok(Operation::Create),
            ActualState::File => {
                let current = fs::read_to_string(&entry.target)?;
                Ok(match find_block(&current, marker) {
                    // No block yet: inserting one is purely additive, same
                    // "nothing here yet" spirit as `Operation::Create`.
                    BlockState::Absent => Operation::Create,
                    BlockState::Found(existing) if existing == content => Operation::Noop,
                    // Drifted (hand-edited, or another overlay's stale
                    // write) or malformed markers: never touched
                    // automatically — surfaced as a conflict exactly like
                    // `SymlinkMaterializer` does for a mismatched symlink.
                    BlockState::Found(_) | BlockState::Malformed => Operation::Conflict {
                        current: ActualState::File,
                    },
                })
            }
            // A directory or symlink sits where a plain file was expected.
            other => Ok(Operation::Conflict { current: other }),
        }
    }

    async fn materialize(&self, ctx: Ctx, step: &PlanStep) -> Result<()> {
        let MaterializationIntent::PartialFile { content, marker } = &step.entry.intent else {
            unreachable!("MaterializerRegistry only calls materialize() after handles() passed")
        };
        EnsurePartialBlock::new(step.entry.target.clone(), marker.clone(), content.clone())
            .execute(ctx)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::Provenance;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use std::path::PathBuf;

    fn entry(target: PathBuf, content: &str, marker: &str) -> DesiredEntry {
        DesiredEntry {
            target,
            provenance: Provenance::PartialSidecar {
                overlay: "ov".to_string(),
                config: PathBuf::from("/repo/ov/aliases.partial.toml"),
            },
            intent: MaterializationIntent::PartialFile {
                content: content.to_string(),
                marker: marker.to_string(),
            },
        }
    }

    #[test]
    fn handles_only_partial_file_intent() {
        let m = PartialFileMaterializer;
        assert!(m.handles(&MaterializationIntent::PartialFile {
            content: "x".to_string(),
            marker: "m".to_string(),
        }));
        assert!(!m.handles(&MaterializationIntent::Directory));
        assert!(!m.handles(&MaterializationIntent::Checkout));
    }

    #[test]
    fn classify_missing_target_is_create() {
        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let e = entry(td.path().join("does-not-exist"), "alias x=y", "m");
        assert!(matches!(m.classify(&e).unwrap(), Operation::Create));
    }

    #[test]
    fn classify_file_without_block_is_create() {
        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("file.txt");
        target.write_str("unrelated content\n").unwrap();
        let e = entry(target.path().to_path_buf(), "alias x=y", "m");
        assert!(matches!(m.classify(&e).unwrap(), Operation::Create));
    }

    #[test]
    fn classify_file_with_matching_block_is_noop() {
        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("file.txt");
        target
            .write_str("# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n")
            .unwrap();
        let e = entry(target.path().to_path_buf(), "alias x=y", "m");
        assert!(matches!(m.classify(&e).unwrap(), Operation::Noop));
    }

    #[test]
    fn classify_file_with_drifted_block_is_conflict() {
        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("file.txt");
        target
            .write_str("# >>> over: m >>>\nhand-edited\n# <<< over: m <<<\n")
            .unwrap();
        let e = entry(target.path().to_path_buf(), "alias x=y", "m");
        assert!(matches!(
            m.classify(&e).unwrap(),
            Operation::Conflict { .. }
        ));
    }

    #[test]
    fn classify_malformed_markers_is_conflict() {
        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("file.txt");
        target
            .write_str("# >>> over: m >>>\nstray, no end marker\n")
            .unwrap();
        let e = entry(target.path().to_path_buf(), "alias x=y", "m");
        assert!(matches!(
            m.classify(&e).unwrap(),
            Operation::Conflict { .. }
        ));
    }

    #[test]
    fn classify_directory_in_the_way_is_conflict() {
        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("adir");
        target.create_dir_all().unwrap();
        let e = entry(target.path().to_path_buf(), "alias x=y", "m");
        assert!(matches!(
            m.classify(&e).unwrap(),
            Operation::Conflict {
                current: ActualState::Directory
            }
        ));
    }

    #[tokio::test]
    async fn materialize_creates_missing_target() {
        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        let e = entry(target.clone(), "alias x=y", "m");
        let step = PlanStep {
            entry: e,
            operation: Operation::Create,
        };
        let ctx = crate::exec::Context::builder().build();
        m.materialize(ctx, &step).await.unwrap();
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n"
        );
    }
}
