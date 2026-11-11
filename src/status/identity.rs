//! Deterministic resolved-claim identities for request-local inspection reuse.

use std::path::PathBuf;

use crate::desired::{DesiredEntry, MaterializationIntent, Provenance};
use crate::overlays::ignore::SourcePolicyIdentity;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Overlay(PathBuf),
    Git(String),
    Sidecar(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Intent {
    Directory,
    Checkout,
    VirtualCheckout {
        source_excludes: Vec<PathBuf>,
        target_excludes: Vec<PathBuf>,
        policy: Option<(PathBuf, SourcePolicyIdentity)>,
    },
    Symlink {
        source: PathBuf,
        hard: bool,
    },
    Partial {
        marker: String,
        content: String,
    },
    Encrypted,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClaimIdentity {
    owner: String,
    target: PathBuf,
    source: Source,
    intent: Intent,
    permissions: Option<String>,
}

impl ClaimIdentity {
    pub fn new(entry: &DesiredEntry) -> Self {
        let (owner, source) = match &entry.provenance {
            Provenance::Overlay { overlay, source } => (overlay, Source::Overlay(source.clone())),
            Provenance::Git {
                overlay, repo_key, ..
            } => (overlay, Source::Git(repo_key.clone())),
            Provenance::SymlinkSidecar {
                overlay, config, ..
            }
            | Provenance::PartialSidecar { overlay, config } => {
                (overlay, Source::Sidecar(config.clone()))
            }
        };
        let intent = match &entry.intent {
            MaterializationIntent::Directory => Intent::Directory,
            MaterializationIntent::Checkout => Intent::Checkout,
            MaterializationIntent::VirtualCheckout
            | MaterializationIntent::ScopedVirtualCheckout(_) => {
                let scope = entry.intent.checkout_scope();
                Intent::VirtualCheckout {
                    source_excludes: scope.source_excludes,
                    target_excludes: scope.target_excludes,
                    policy: scope
                        .source_policy
                        .map(|(source, policy)| (source, policy.identity())),
                }
            }
            MaterializationIntent::SymlinkFile { source, link_type }
            | MaterializationIntent::SymlinkDirectory { source, link_type } => Intent::Symlink {
                source: source.clone(),
                hard: *link_type == crate::actions::symlink::LinkType::Hard,
            },
            MaterializationIntent::PartialFile { marker, content } => Intent::Partial {
                marker: marker.clone(),
                content: content.clone(),
            },
            MaterializationIntent::EncryptedFile(_) => Intent::Encrypted,
        };
        Self {
            owner: owner.clone(),
            target: entry.target.clone(),
            source,
            intent,
            permissions: entry.permissions.map(|mode| mode.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::Arc};

    use crate::desired::CheckoutScope;
    use crate::overlays::{Repository, ignore::SourcePolicy};

    #[test]
    fn selection_identity_ignores_unrelated_maps_but_retains_rules_and_scope() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("overlay");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("over.toml"), "[git.one]\nurl = 'one'\n[git.one.config]\n'user.name' = 'One'\n'user.email' = 'one@example.com'\n[git.two]\nurl = 'two'\n").unwrap();
        let repo = Repository::new(temp.path().to_path_buf());
        let overlay = repo.get("overlay").unwrap();
        let entry_for = |overlay: &crate::overlays::Overlay| DesiredEntry {
            target: temp.path().join("target"),
            provenance: Provenance::Overlay {
                overlay: "overlay".into(),
                source: source.clone(),
            },
            intent: MaterializationIntent::ScopedVirtualCheckout(CheckoutScope {
                source_policy: Some((
                    source.clone(),
                    Arc::new(SourcePolicy::new(temp.path(), overlay).unwrap()),
                )),
                ..Default::default()
            }),
            permissions: None,
        };
        let original = ClaimIdentity::new(&entry_for(&overlay));
        let mut changed = overlay.clone();
        changed.git.as_mut().unwrap().remove("one");
        assert_eq!(original, ClaimIdentity::new(&entry_for(&changed)));
        fs::write(source.join(".overignore"), "private\n").unwrap();
        let selected = entry_for(&overlay);
        assert_ne!(original, ClaimIdentity::new(&selected));
        let mut scoped = selected.clone();
        let MaterializationIntent::ScopedVirtualCheckout(scope) = &mut scoped.intent else {
            unreachable!()
        };
        scope.target_excludes.push("mount".into());
        assert_ne!(ClaimIdentity::new(&selected), ClaimIdentity::new(&scoped));
    }
}
