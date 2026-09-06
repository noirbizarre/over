use std::path::PathBuf;

use crate::actions::git::config::GitRepoConfig;
use crate::actions::symlink::LinkType;

/// What kind of filesystem node a [`DesiredEntry`] should be, independent of
/// *how* it gets there (see [`MaterializationIntent`]).
///
/// `File` is reserved for #61 (content-templated files): today's
/// symlink-first apply (ADR-006, ADR-010) never writes real file content, so
/// [`super::DesiredTree::build`] never produces it — only `Directory` and
/// `Symlink` are reachable for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
}

/// How a [`DesiredEntry`] is (or will be) turned into a real filesystem node.
///
/// Distinct from [`EntryKind`]: this is the resolved strategy/rule that
/// produced the entry, not just the node type it results in. Keeping the two
/// separate matters for #113's later migration detection (e.g. detecting a
/// `SymlinkDirectory -> Checkout` rule change as an explicit transition
/// rather than an implicit delete/recreate).
#[derive(Debug, Clone)]
pub enum MaterializationIntent {
    /// Plain `mkdir` — the default for directories that don't match
    /// `link_dirs`.
    Directory,
    /// Symlink a single file from `source` (file-level materialization).
    SymlinkFile {
        source: PathBuf,
        link_type: LinkType,
    },
    /// Symlink a whole directory from `source` as one unit
    /// (`link_dirs`, or a `.link.*` sidecar resolving to a directory).
    SymlinkDirectory {
        source: PathBuf,
        link_type: LinkType,
    },
    /// A Git-managed path (`overlay.git`). Materialized by
    /// [`crate::materialize::CheckoutMaterializer`] (#110): ensures the
    /// repository/worktree is present and configured, the same "presence"
    /// concern `actions::git::clone_repositories` already implements.
    /// Content-level bidirectional synchronization (fetch/merge/push) is a
    /// separate, explicit operation (`over sync`, `crate::sync`), not part
    /// of materialization.
    Checkout,
    /// A managed block injected into an existing (possibly foreign) file,
    /// delimited by marker lines unique to `marker` (#66). The first intent
    /// to ever produce [`EntryKind::File`]: unlike every other intent, the
    /// target isn't replaced wholesale — only the delimited region is
    /// written/compared, so unrelated content in the same file is never
    /// touched. `content` is used verbatim (no template rendering — that's
    /// #61's job, kept independent per #66's own scope).
    PartialFile { content: String, marker: String },
}

impl MaterializationIntent {
    /// The [`EntryKind`] this intent results in on disk.
    pub fn kind(&self) -> EntryKind {
        match self {
            MaterializationIntent::Directory | MaterializationIntent::Checkout => {
                EntryKind::Directory
            }
            MaterializationIntent::SymlinkFile { .. }
            | MaterializationIntent::SymlinkDirectory { .. } => EntryKind::Symlink,
            MaterializationIntent::PartialFile { .. } => EntryKind::File,
        }
    }
}

/// Where a [`DesiredEntry`] comes from, for reconciliation diagnostics.
#[derive(Debug, Clone)]
pub enum Provenance {
    /// A file/directory tracked directly in the overlay's own tree.
    Overlay { overlay: String, source: PathBuf },
    /// A `*.link.{toml,yaml,yml}` sidecar declaring an arbitrary symlink.
    ///
    /// Carries both the raw (pre-render) template and its resolved result —
    /// the "template provenance/rendering information" called for by #107.
    SymlinkSidecar {
        overlay: String,
        config: PathBuf,
        template: String,
        resolved: PathBuf,
    },
    /// A path backed by a Git repository/worktree declared on the overlay
    /// (`overlay.git`). Carries the repo config for "source
    /// repository/revision" diagnostics.
    ///
    /// Boxed: `GitRepoConfig` is comparatively large (worktrees/remotes/git
    /// config maps), and this variant would otherwise dominate the size of
    /// every `Provenance` value, most of which never carry one.
    Git {
        overlay: String,
        repo_key: String,
        config: Box<GitRepoConfig>,
    },
    /// A `*.partial.{toml,yaml,yml}` sidecar declaring a managed block
    /// injected into an external file (#66).
    PartialSidecar { overlay: String, config: PathBuf },
}

/// A single desired filesystem node — the canonical unit [`super::DesiredTree`]
/// is made of.
///
/// Deliberately does not carry rendered content (#61) or permission metadata
/// (#65): both depend on this type and will extend it rather than have this
/// issue guess their shape.
#[derive(Debug, Clone)]
pub struct DesiredEntry {
    /// Absolute path this entry should exist at.
    pub target: PathBuf,
    pub provenance: Provenance,
    pub intent: MaterializationIntent,
}

impl DesiredEntry {
    /// The [`EntryKind`] this entry should have once materialized.
    pub fn kind(&self) -> EntryKind {
        self.intent.kind()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_intent_kind_is_directory() {
        assert_eq!(
            MaterializationIntent::Directory.kind(),
            EntryKind::Directory
        );
    }

    #[test]
    fn checkout_intent_kind_is_directory() {
        assert_eq!(MaterializationIntent::Checkout.kind(), EntryKind::Directory);
    }

    #[test]
    fn symlink_file_intent_kind_is_symlink() {
        let intent = MaterializationIntent::SymlinkFile {
            source: PathBuf::from("/src/file.txt"),
            link_type: LinkType::Soft,
        };
        assert_eq!(intent.kind(), EntryKind::Symlink);
    }

    #[test]
    fn partial_file_intent_kind_is_file() {
        let intent = MaterializationIntent::PartialFile {
            content: "alias ll='ls -la'".to_string(),
            marker: "aliases".to_string(),
        };
        assert_eq!(intent.kind(), EntryKind::File);
    }

    #[test]
    fn symlink_directory_intent_kind_is_symlink() {
        let intent = MaterializationIntent::SymlinkDirectory {
            source: PathBuf::from("/src/dir"),
            link_type: LinkType::Hard,
        };
        assert_eq!(intent.kind(), EntryKind::Symlink);
    }

    #[test]
    fn desired_entry_kind_delegates_to_intent() {
        let entry = DesiredEntry {
            target: PathBuf::from("/target/file.txt"),
            provenance: Provenance::Overlay {
                overlay: "ov".to_string(),
                source: PathBuf::from("/overlay/file.txt"),
            },
            intent: MaterializationIntent::SymlinkFile {
                source: PathBuf::from("/overlay/file.txt"),
                link_type: LinkType::Soft,
            },
        };
        assert_eq!(entry.kind(), EntryKind::Symlink);
    }
}
