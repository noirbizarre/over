use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use super::target_files;
use crate::desired::CheckoutScope;
use crate::target_fs::{Access, Session};

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn target_enumeration_uses_entry_access_instead_of_stale_scope_access(#[case] projected: bool) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("owned"), "content").unwrap();
    let mut stale_access = Access::default();
    let session = Arc::new(Session::default());
    session.shutdown().unwrap();
    // A closed session makes stale-scope use fail without invoking real sudo.
    stale_access.fs = session.target(true, true, true);
    let scope = CheckoutScope {
        access: Box::new(stale_access),
        projection: projected.then(|| Box::new(BTreeSet::from([PathBuf::from("owned")]))),
        ..Default::default()
    };
    assert_eq!(
        target_files(root.path(), &scope, &Access::default()).unwrap(),
        vec![PathBuf::from("owned")]
    );
}

#[test]
fn projected_enumeration_ignores_unowned_siblings_and_missing_leaves() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    std::fs::write(root.path().join("nested/owned"), "content").unwrap();
    std::fs::write(root.path().join("foreign"), "unrelated").unwrap();
    let scope = CheckoutScope {
        projection: Some(Box::new(BTreeSet::from([
            PathBuf::from("nested/owned"),
            PathBuf::from("missing"),
        ]))),
        ..Default::default()
    };
    // Execute-only containers allow known leaf inspection but reject directory enumeration.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o111)).unwrap();
    }
    let result = target_files(root.path(), &scope, &Access::default());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(result.unwrap(), vec![PathBuf::from("nested/owned")]);
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn target_enumeration_preserves_source_target_and_metadata_exclusions(#[case] projected: bool) {
    let root = tempfile::tempdir().unwrap();
    for name in ["owned", "source-hole", "target-hole", "over.yaml"] {
        std::fs::write(root.path().join(name), "content").unwrap();
    }
    let scope = CheckoutScope {
        source_excludes: vec![PathBuf::from("source-hole")],
        target_excludes: vec![PathBuf::from("target-hole")],
        projection: projected.then(|| {
            Box::new(
                ["owned", "source-hole", "target-hole", "over.yaml"]
                    .into_iter()
                    .map(PathBuf::from)
                    .collect(),
            )
        }),
        ..Default::default()
    };
    assert_eq!(
        target_files(root.path(), &scope, &Access::default()).unwrap(),
        vec![PathBuf::from("owned")]
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
#[case(true, true)]
fn target_enumeration_rejects_owned_symlinks_and_special_nodes(
    #[case] projected: bool,
    #[case] socket: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("unsupported");
    // A socket proves rejection is not limited to symlinks, without blocking on a FIFO.
    let _socket = if socket {
        Some(std::os::unix::net::UnixListener::bind(&path).unwrap())
    } else {
        std::os::unix::fs::symlink("missing", &path).unwrap();
        None
    };
    let scope = CheckoutScope {
        projection: projected.then(|| Box::new(BTreeSet::from([PathBuf::from("unsupported")]))),
        ..Default::default()
    };
    let error = target_files(root.path(), &scope, &Access::default()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported virtual checkout target entry")
    );
}

#[test]
fn a_missing_legacy_target_has_no_added_files() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        target_files(
            &root.path().join("missing"),
            &CheckoutScope::default(),
            &Access::default(),
        )
        .unwrap()
        .is_empty()
    );
}
