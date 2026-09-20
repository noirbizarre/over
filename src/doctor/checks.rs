//! Doctor's own two checks — everything else in [`super::Report`] is
//! aggregated from an existing pass ([`crate::lint`], [`crate::git_exclude`]).
//!
//! Both checks here are detection-only: neither ever writes anything (the
//! state-dir-writable probe cleans up after itself, see
//! [`probe_writable`]).

use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::materialize::virtual_checkout::state::{self as vc_state, VirtualCheckoutState};
use crate::overlays::Repository;
use crate::sync::state::{self as sync_state, SyncState};
use crate::xdg::XdgDirs;
use crate::xdg::state::StateFile;

use super::Finding;

/// git on `PATH`, and the XDG state directory actually writable — the two
/// pieces of environment plumbing several `over` features (git-based
/// overlays, `over sync`/virtual checkouts) silently depend on.
pub fn environment_findings() -> Vec<Finding> {
    let mut findings = Vec::new();

    match which::which("git") {
        Ok(path) => findings.push(Finding::ok(format!(
            "git found on PATH ({})",
            path.display()
        ))),
        Err(_) => findings.push(Finding::warning(
            "git was not found on PATH — git-based overlays, checkouts, and virtual checkouts \
             will fail (install git and ensure it is on PATH)",
        )),
    }

    match XdgDirs::new() {
        Ok(xdg) => {
            let state_dir = xdg.state_dir();
            match probe_writable(&state_dir) {
                Ok(()) => findings.push(Finding::ok(format!(
                    "XDG state directory is writable ({})",
                    state_dir.display()
                ))),
                Err(e) => findings.push(Finding::error(format!(
                    "XDG state directory '{}' is not writable: {e} (over sync and virtual \
                     checkouts persist bookkeeping there and will fail)",
                    state_dir.display()
                ))),
            }
        }
        Err(e) => findings.push(Finding::error(format!(
            "could not resolve the XDG state directory: {e}"
        ))),
    }

    findings
}

/// Create `dir` if needed, then prove it's actually writable (not just
/// creatable — `create_dir_all` can succeed on an already-existing
/// directory that isn't writable) by writing and removing a probe file.
fn probe_writable(dir: &Path) -> Result<()> {
    XdgDirs::ensure_dir(dir)?;
    let probe = dir.join(format!(".doctor-probe-{}", std::process::id()));
    fs::write(&probe, b"")?;
    fs::remove_file(&probe)?;
    Ok(())
}

/// Stale `sync.toml`/`virtual_checkout.toml` records (turned up during
/// #149's exploration as a natural doctor check): a record whose target no
/// longer exists on disk, or whose overlay no longer resolves in `repo`.
pub async fn xdg_state_findings(repo: &Repository) -> Result<Vec<Finding>> {
    xdg_state_findings_in(repo, &sync_state::state_file()?, &vc_state::state_file()?).await
}

/// Test-support variant of [`xdg_state_findings`] taking isolated
/// `StateFile`s — mirrors `sync::state`/`virtual_checkout::state`'s own
/// `_in`-suffixed functions, so tests never touch the real
/// `$XDG_STATE_HOME`.
///
/// Both state files are explicitly non-authoritative bookkeeping (see
/// their own module docs) — losing or ignoring a stale record never
/// corrupts anything, so every finding here is a `Warning`, never an
/// `Error`. A load failure (corrupt file, unknown future schema) *is* an
/// `Error`: it means every future `over sync`/virtual-checkout operation
/// touching that file will hard-fail.
async fn xdg_state_findings_in(
    repo: &Repository,
    sync_file: &StateFile<SyncState>,
    vc_file: &StateFile<VirtualCheckoutState>,
) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();

    match sync_file.load().await {
        Ok(state) => {
            for (target, record) in &state.checkouts {
                findings.extend(stale_record_findings("sync", repo, target, &record.overlay));
            }
        }
        Err(e) => findings.push(Finding::error(format!("failed to read sync state: {e}"))),
    }

    match vc_file.load().await {
        Ok(state) => {
            for (target, record) in &state.checkouts {
                findings.extend(stale_record_findings(
                    "virtual checkout",
                    repo,
                    target,
                    &record.overlay,
                ));
            }
        }
        Err(e) => findings.push(Finding::error(format!(
            "failed to read virtual checkout state: {e}"
        ))),
    }

    Ok(findings)
}

fn stale_record_findings(
    kind: &str,
    repo: &Repository,
    target: &str,
    overlay: &str,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    if !Path::new(target).exists() {
        findings.push(Finding::warning(format!(
            "stale {kind} record: target '{target}' no longer exists (overlay '{overlay}')"
        )));
    }

    if repo.get(overlay).is_err() {
        findings.push(Finding::warning(format!(
            "stale {kind} record: overlay '{overlay}' no longer found (target '{target}')"
        )));
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materialize::virtual_checkout::state::VirtualCheckoutRecord;
    use crate::sync::state::CheckoutRecord;
    use tempfile::TempDir;

    #[test]
    fn environment_findings_reports_git_and_state_dir() {
        // Smoke test only: the real environment's git/XDG state may or may
        // not be present/writable in CI, so this just asserts the checks
        // run and produce exactly one finding per check.
        let findings = environment_findings();
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn probe_writable_succeeds_on_a_creatable_directory() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("nested/state");
        assert!(probe_writable(&dir).is_ok());
        assert!(dir.is_dir());
        // The probe file must not survive the check.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn probe_writable_fails_on_a_read_only_directory() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("readonly");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();

        let result = probe_writable(&dir);

        // Always restore write permission so `TempDir`'s own drop cleanup
        // can remove the directory.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
    }

    fn repo_with_overlay(tmp: &TempDir, name: &str) -> Repository {
        let ov = tmp.path().join(name);
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        Repository::new(tmp.path().to_path_buf())
    }

    fn empty_state_files(tmp: &TempDir) -> (StateFile<SyncState>, StateFile<VirtualCheckoutState>) {
        (
            StateFile::new(tmp.path().join("sync.toml")),
            StateFile::new(tmp.path().join("virtual_checkout.toml")),
        )
    }

    #[tokio::test]
    async fn xdg_state_findings_flags_a_target_that_no_longer_exists() {
        let tmp = TempDir::new().unwrap();
        let repo = repo_with_overlay(&tmp, "dotfiles");
        let (sync_file, vc_file) = empty_state_files(&tmp);

        sync_file
            .update(|s| {
                s.checkouts.insert(
                    "/nonexistent/target".to_string(),
                    CheckoutRecord {
                        overlay: "dotfiles".to_string(),
                        repo_key: ".".to_string(),
                        worktree: None,
                        last_synced_oid: None,
                        last_synced_at: None,
                        last_outcome: None,
                    },
                );
                Ok(())
            })
            .await
            .unwrap();

        let findings = xdg_state_findings_in(&repo, &sync_file, &vc_file)
            .await
            .unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].line.contains("no longer exists"));
    }

    #[tokio::test]
    async fn xdg_state_findings_flags_an_overlay_that_no_longer_resolves() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let (sync_file, vc_file) = empty_state_files(&tmp);
        // The record's target does exist, but its overlay doesn't.
        let target = tmp.path().join("target");
        fs::create_dir_all(&target).unwrap();

        sync_file
            .update(move |s| {
                s.checkouts.insert(
                    target.to_string_lossy().to_string(),
                    CheckoutRecord {
                        overlay: "gone".to_string(),
                        repo_key: ".".to_string(),
                        worktree: None,
                        last_synced_oid: None,
                        last_synced_at: None,
                        last_outcome: None,
                    },
                );
                Ok(())
            })
            .await
            .unwrap();

        let findings = xdg_state_findings_in(&repo, &sync_file, &vc_file)
            .await
            .unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].line.contains("no longer found"));
    }

    #[tokio::test]
    async fn xdg_state_findings_is_empty_for_a_healthy_record() {
        let tmp = TempDir::new().unwrap();
        let repo = repo_with_overlay(&tmp, "dotfiles");
        let (sync_file, vc_file) = empty_state_files(&tmp);
        let target = tmp.path().join("target");
        fs::create_dir_all(&target).unwrap();

        sync_file
            .update(move |s| {
                s.checkouts.insert(
                    target.to_string_lossy().to_string(),
                    CheckoutRecord {
                        overlay: "dotfiles".to_string(),
                        repo_key: ".".to_string(),
                        worktree: None,
                        last_synced_oid: None,
                        last_synced_at: None,
                        last_outcome: None,
                    },
                );
                Ok(())
            })
            .await
            .unwrap();

        let findings = xdg_state_findings_in(&repo, &sync_file, &vc_file)
            .await
            .unwrap();
        assert!(findings.is_empty());
    }

    #[tokio::test]
    async fn xdg_state_findings_covers_virtual_checkout_records_too() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let (sync_file, vc_file) = empty_state_files(&tmp);

        vc_file
            .update(|s| {
                s.checkouts.insert(
                    "/nonexistent/vc-target".to_string(),
                    VirtualCheckoutRecord {
                        overlay: "gone".to_string(),
                        managed_path: std::path::PathBuf::new(),
                        base_oid: "deadbeef".to_string(),
                        created_at: 0,
                        last_commit_at: None,
                    },
                );
                Ok(())
            })
            .await
            .unwrap();

        let findings = xdg_state_findings_in(&repo, &sync_file, &vc_file)
            .await
            .unwrap();
        // Both the missing target and the missing overlay are flagged.
        assert_eq!(findings.len(), 2);
    }

    #[tokio::test]
    async fn xdg_state_findings_reports_an_error_for_a_corrupt_state_file() {
        let tmp = TempDir::new().unwrap();
        let repo = Repository::new(tmp.path().to_path_buf());
        let (sync_file, vc_file) = empty_state_files(&tmp);

        // A schema version this crate doesn't know how to migrate.
        fs::write(tmp.path().join("sync.toml"), "version = 999999\n[state]\n").unwrap();

        let findings = xdg_state_findings_in(&repo, &sync_file, &vc_file)
            .await
            .unwrap();
        assert_eq!(findings.len(), 1);
        assert!(matches!(
            findings[0].severity,
            super::super::Severity::Error
        ));
    }
}
