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
use std::path::Path;

use anyhow::{Context as _, Result};
use async_trait::async_trait;

use crate::actions::fs::SetPermissions;
use crate::actions::partial::{BlockState, EnsurePartialBlock, find_block};
use crate::desired::{DesiredEntry, MaterializationIntent};
use crate::exec::{Action, Ctx};
use crate::overlays::FileMode;
use crate::plan::actual::{self, ActualState};
use crate::plan::{Operation, PlanStep};

use super::Materializer;

/// Compare `entry`'s declared permission (if any) against what's actually
/// on disk at `path`, returning `Some((current, desired))` on a mismatch —
/// `None` either because nothing is declared (unmanaged, pre-#65 behavior)
/// or because it already matches.
#[cfg(unix)]
fn permission_drift(
    path: &Path,
    desired: Option<FileMode>,
) -> Result<Option<(FileMode, FileMode)>> {
    use std::os::unix::fs::PermissionsExt;

    let Some(desired) = desired else {
        return Ok(None);
    };
    let current = FileMode::from_bits(
        fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?
            .permissions()
            .mode(),
    );
    Ok((current.bits() != desired.bits()).then_some((current, desired)))
}

/// Permission bits aren't a meaningful concept to enforce here on
/// non-unix platforms (see ADR-020) — never any drift to report.
#[cfg(not(unix))]
fn permission_drift(
    _path: &Path,
    _desired: Option<FileMode>,
) -> Result<Option<(FileMode, FileMode)>> {
    Ok(None)
}

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
                let current = fs::read_to_string(&entry.target)
                    .with_context(|| format!("failed to read {}", entry.target.display()))?;
                Ok(match find_block(&current, marker) {
                    // No block yet: inserting one is purely additive, same
                    // "nothing here yet" spirit as `Operation::Create`.
                    BlockState::Absent => Operation::Create,
                    // Content already correct — but a declared permission
                    // (#65) might still be wrong, in which case only a
                    // `chmod` is needed, not a rewrite.
                    BlockState::Found(existing) if existing == content => {
                        match permission_drift(&entry.target, entry.permissions)? {
                            Some((current, desired)) => Operation::Repair { current, desired },
                            None => Operation::Noop,
                        }
                    }
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
        // Content already matches (#65) — only the permission mode needs
        // fixing, so skip `EnsurePartialBlock` entirely (it would otherwise
        // rewrite an already-correct block for no reason).
        if let Operation::Repair { desired, .. } = &step.operation {
            return SetPermissions::new(step.entry.target.clone(), *desired)
                .execute(ctx)
                .await;
        }
        EnsurePartialBlock::new(step.entry.target.clone(), marker.clone(), content.clone())
            .with_permissions(step.entry.permissions)
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
            permissions: None,
        }
    }

    fn entry_with_permissions(
        target: PathBuf,
        content: &str,
        marker: &str,
        mode: &str,
    ) -> DesiredEntry {
        DesiredEntry {
            permissions: Some(FileMode::parse(mode).unwrap()),
            ..entry(target, content, marker)
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
    #[cfg(unix)]
    fn classify_reports_context_when_target_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("file.txt");
        target.write_str("unrelated content\n").unwrap();
        fs::set_permissions(target.path(), fs::Permissions::from_mode(0o000)).unwrap();

        let e = entry(target.path().to_path_buf(), "alias x=y", "m");
        let result = m.classify(&e);

        fs::set_permissions(target.path(), fs::Permissions::from_mode(0o644)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("failed to read"));
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

    // ── #65: permission classification/materialization ──────────────────

    #[test]
    fn classify_matching_block_without_declared_permission_is_noop() {
        // No `permissions` declared at all — must behave exactly like
        // before #65, regardless of the target's actual mode.
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
    #[cfg(unix)]
    fn classify_matching_block_with_correct_permission_is_noop() {
        use std::os::unix::fs::PermissionsExt;

        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("file.txt");
        target
            .write_str("# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n")
            .unwrap();
        fs::set_permissions(target.path(), fs::Permissions::from_mode(0o600)).unwrap();
        let e = entry_with_permissions(target.path().to_path_buf(), "alias x=y", "m", "600");
        assert!(matches!(m.classify(&e).unwrap(), Operation::Noop));
    }

    #[test]
    #[cfg(unix)]
    fn classify_matching_block_with_wrong_permission_is_repair() {
        use std::os::unix::fs::PermissionsExt;

        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("file.txt");
        target
            .write_str("# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\n")
            .unwrap();
        fs::set_permissions(target.path(), fs::Permissions::from_mode(0o644)).unwrap();
        let e = entry_with_permissions(target.path().to_path_buf(), "alias x=y", "m", "600");
        match m.classify(&e).unwrap() {
            Operation::Repair { current, desired } => {
                assert_eq!(current.bits(), 0o644);
                assert_eq!(desired.bits(), 0o600);
            }
            other => panic!("expected Repair, got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn materialize_repair_only_changes_permission_not_content() {
        use std::os::unix::fs::PermissionsExt;

        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.child("file.txt");
        let original = "before\n# >>> over: m >>>\nalias x=y\n# <<< over: m <<<\nafter\n";
        target.write_str(original).unwrap();
        fs::set_permissions(target.path(), fs::Permissions::from_mode(0o644)).unwrap();

        let e = entry_with_permissions(target.path().to_path_buf(), "alias x=y", "m", "600");
        let step = PlanStep {
            entry: e,
            operation: Operation::Repair {
                current: FileMode::parse("644").unwrap(),
                desired: FileMode::parse("600").unwrap(),
            },
        };
        let ctx = crate::exec::Context::builder().build();
        m.materialize(ctx, &step).await.unwrap();

        assert_eq!(
            fs::read_to_string(target.path()).unwrap(),
            original,
            "Repair must never touch content"
        );
        let mode = fs::metadata(target.path()).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn materialize_create_applies_declared_permission() {
        use std::os::unix::fs::PermissionsExt;

        let m = PartialFileMaterializer;
        let td = TempDir::new().unwrap();
        let target = td.path().join("file.txt");
        let e = entry_with_permissions(target.clone(), "alias x=y", "m", "600");
        let step = PlanStep {
            entry: e,
            operation: Operation::Create,
        };
        let ctx = crate::exec::Context::builder().build();
        m.materialize(ctx, &step).await.unwrap();

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600);
    }
}
