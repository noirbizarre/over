//! Checkout directory policy is finite and finalized after child publication.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use rstest::rstest;
use serde_json::json;

struct Fixture {
    temp: tempfile::TempDir,
    source: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn new(generated: bool, failing: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::create_dir_all(source.join("ov/checkout/parent/child")).unwrap();
        fs::create_dir_all(target.join("checkout/parent/unrelated/deep")).unwrap();
        fs::write(source.join("over.yaml"), "{}").unwrap();
        fs::write(source.join("ov/over.yaml"), json!({"rules": [
            {"path": "checkout", "mode": "checkout"},
            {"path": "checkout/parent", "permissions": "0500", "sudo": true},
            {"path": "checkout/parent/child", "permissions": "0550", "sudo": false},
            {"path": "checkout/parent/child/*", "permissions": "0600", "sudo": false},
            {"path": "checkout/parent/child/settings.tmpl", "template": generated, "strip_extension": generated}
        ]}).to_string()).unwrap();
        fs::write(
            source.join("ov/checkout/parent/child/settings.tmpl"),
            "settings",
        )
        .unwrap();
        fs::write(source.join("ov/checkout/parent/child/plain"), "ordinary").unwrap();
        if failing {
            fs::write(source.join("ov/zz"), "owned").unwrap();
            fs::write(target.join("zz"), "foreign").unwrap();
        }
        let repo = git2::Repository::init(&source).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        // This same-UID helper exercises privilege routing, not root-only filesystem access.
        fs::create_dir(temp.path().join("bin")).unwrap();
        let sudo = temp.path().join("bin/sudo");
        fs::write(&sudo, "#!/bin/sh\nwhile [ \"$1\" = -n ] || [ \"$1\" = -b ] || [ \"$1\" = -- ]; do shift; done\nexec \"$@\"\n").unwrap();
        fs::set_permissions(sudo, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            temp,
            source,
            target,
        }
    }

    fn apply(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("over"));
        let path = std::env::join_paths(std::iter::once(self.temp.path().join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        command
            .env("PATH", path)
            .env("XDG_STATE_HOME", self.temp.path().join("state"))
            .env("XDG_CACHE_HOME", self.temp.path().join("cache"))
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .env_remove("OVER_HOME")
            .env_remove("OVER_HOOK_ACTIVE")
            .arg("--home")
            .arg(&self.source)
            .args(["apply", "ov", "--no-prompt", "--target"])
            .arg(&self.target);
        command
    }

    fn mode(&self, path: &str) -> u32 {
        fs::metadata(self.target.join(path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Reopen restrictive parents so the fixture can remove its children.
        for path in ["checkout", "checkout/parent", "checkout/parent/child"] {
            if self.target.join(path).exists() {
                fs::set_permissions(self.target.join(path), fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
        }
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn nested_checkout_metadata_preserves_ordinary_modes_and_unrelated_directories(
    #[case] generated: bool,
) {
    let fixture = Fixture::new(generated, false);
    let unrelated_mode = fixture.mode("checkout/parent/unrelated/deep");
    fixture.apply().assert().success();
    assert_eq!(fixture.mode("checkout/parent"), 0o500);
    assert_eq!(fixture.mode("checkout/parent/child"), 0o550);
    assert_eq!(fixture.mode("checkout/parent/child/plain"), 0o644);
    assert_eq!(
        fixture.mode("checkout/parent/unrelated/deep"),
        unrelated_mode
    );
    let name = if generated {
        "settings"
    } else {
        "settings.tmpl"
    };
    assert_eq!(
        fs::read_to_string(fixture.target.join("checkout/parent/child").join(name)).unwrap(),
        "settings"
    );
    // Metadata-only reconciliation must repair intermediate drift without rewriting leaves.
    fs::set_permissions(
        fixture.target.join("checkout/parent"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::set_permissions(
        fixture.target.join("checkout/parent/child"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    fixture.apply().assert().success();
    assert_eq!(fixture.mode("checkout/parent"), 0o500);
    assert_eq!(fixture.mode("checkout/parent/child"), 0o550);
}

#[test]
fn later_publication_failure_still_finalizes_intermediate_checkout_directories() {
    let fixture = Fixture::new(false, true);
    fixture.apply().assert().failure();
    assert_eq!(fixture.mode("checkout/parent"), 0o500);
    assert_eq!(fixture.mode("checkout/parent/child"), 0o550);
    assert_eq!(
        fs::read_to_string(fixture.target.join("zz")).unwrap(),
        "foreign"
    );
}

#[test]
fn refusing_an_unassociated_checkout_conflict_preserves_existing_metadata_and_state() {
    use std::os::unix::fs::MetadataExt;

    let fixture = Fixture::new(false, false);
    let descriptor = fixture.source.join("ov/over.yaml");
    let mut policy: serde_json::Value =
        serde_json::from_slice(&fs::read(&descriptor).unwrap()).unwrap();
    policy["rules"][0]["permissions"] = json!("0500");
    fs::write(&descriptor, policy.to_string()).unwrap();
    fs::create_dir_all(fixture.target.join("checkout/parent/child")).unwrap();
    fs::write(
        fixture.target.join("checkout/parent/child/plain"),
        "foreign",
    )
    .unwrap();
    // Apply always takes its lifecycle lock; precreate only that empty sentinel,
    // leaving no association, so state publication is observable independently.
    let state_root = fixture.temp.path().join("state");
    fs::create_dir_all(state_root.join("over")).unwrap();
    fs::write(state_root.join("over/virtual_checkout.mutation.lock"), "").unwrap();
    // ctime catches even metadata writes that happen to restore the original values.
    let snapshot = |root: &std::path::Path| {
        walkdir::WalkDir::new(root)
            .into_iter()
            .map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path();
                let node = fs::symlink_metadata(path).unwrap();
                (
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    (
                        node.mode(),
                        node.uid(),
                        node.gid(),
                        node.ctime(),
                        node.ctime_nsec(),
                        node.is_file().then(|| fs::read(path).unwrap()),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let target = snapshot(&fixture.target);
    let source = snapshot(&fixture.source);
    let state = snapshot(&state_root);
    fixture
        .apply()
        .assert()
        .failure()
        .stderr(predicates::str::contains("virtual checkout conflict"));
    assert_eq!(snapshot(&fixture.target), target);
    assert_eq!(snapshot(&fixture.source), source);
    assert_eq!(snapshot(&state_root), state);
    assert!(!fixture.temp.path().join("cache").exists());
    assert!(!fixture.temp.path().join("config").exists());
}
