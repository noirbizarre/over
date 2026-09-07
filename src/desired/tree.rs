use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as AnyhowContext, Result};
use globset::GlobBuilder;
use walkdir::WalkDir;

use crate::actions::git::config::ROOT_PATH;
use crate::actions::{partial, symlink};
use crate::exec;
use crate::overlays::{self, Overlay};

use super::entry::{DesiredEntry, MaterializationIntent, Provenance};

/// The canonical desired filesystem state for an overlay and everything it
/// transitively `uses`. See the [module docs](super) for the overall model.
#[derive(Debug, Clone, Default)]
pub struct DesiredTree {
    entries: Vec<DesiredEntry>,
}

impl DesiredTree {
    /// Build the desired tree for `overlay` and everything it transitively
    /// `uses` (flat union, ADR-008), honoring `ctx.no_uses`.
    ///
    /// Mirrors [`Overlay::apply`]'s traversal and cycle detection, but only
    /// reads the filesystem (to resolve `.link.*` sidecar targets and detect
    /// directory-vs-file symlink targets) and never writes anything.
    pub fn build(ctx: &exec::Context, overlay: &Overlay) -> Result<Self> {
        let mut entries = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = Vec::new();
        build_inner(ctx, overlay, &mut visited, &mut stack, &mut entries)?;
        // Canonical, deterministic ordering — useful for #109 diff and tests.
        entries.sort_by(|a, b| a.target.cmp(&b.target));
        Ok(Self { entries })
    }

    /// This overlay's own entries only — no `uses` recursion.
    ///
    /// `Overlay::apply` (#13) uses this so its per-overlay-node
    /// orchestration (progress banners, cycle detection, git cloning) stays
    /// untouched while the directory/symlink surface goes through a
    /// [`crate::plan::Plan`]. [`Self::build`] (the full, recursive graph)
    /// remains the entry point for whole-overlay status/diff (#12/#109),
    /// which need every `uses` dependency's entries too.
    pub fn build_own(ctx: &exec::Context, overlay: &Overlay) -> Result<Self> {
        let target = overlay.resolve_target(ctx)?;
        let mut entries = collect_own_entries(ctx, overlay, &target)?;
        // Same deterministic ordering as `build()`.
        entries.sort_by(|a, b| a.target.cmp(&b.target));
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[DesiredEntry] {
        &self.entries
    }

    /// Test-only: build a tree directly from a list of entries, bypassing
    /// `Overlay`/config resolution. Used by other modules' tests
    /// (`crate::sync`) that exercise entry-level behavior without needing
    /// a full overlay/repository/target-resolution fixture.
    #[cfg(test)]
    pub(crate) fn from_entries(entries: Vec<DesiredEntry>) -> Self {
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn build_inner(
    ctx: &exec::Context,
    overlay: &Overlay,
    visited: &mut HashSet<String>,
    stack: &mut Vec<String>,
    entries: &mut Vec<DesiredEntry>,
) -> Result<()> {
    // Already fully processed via another dependency path — skip silently.
    if visited.contains(&overlay.name) {
        return Ok(());
    }
    // Currently in the recursion stack — true cycle.
    if stack.contains(&overlay.name) {
        stack.push(overlay.name.clone());
        return Err(anyhow::anyhow!(
            "Cycle detected: overlay '{}' forms a cycle (path: {})",
            overlay.name,
            stack.join(" -> ")
        ));
    }
    stack.push(overlay.name.clone());

    let target = overlay.resolve_target(ctx)?;

    if let Some(uses) = &overlay.uses {
        if ctx.no_uses {
            tracing::debug!(overlay = %overlay.name, "skipping uses (--no-uses)");
        } else {
            for name in uses {
                let used = ctx
                    .repository
                    .get(name)
                    .with_context(|| format!("used overlay '{}' not found", name))?;
                let sub_ctx = ctx.with_overlay(used.clone());
                build_inner(&sub_ctx, &used, visited, stack, entries)?;
            }
        }
    }

    // This overlay's own entries are collected after its `uses` so that,
    // should any target path collide (e.g. both default to the same root),
    // the overlay's own entry is the one a stable sort keeps last — the
    // final sort by target is what actually gives us determinism, but
    // keeping this order matches the intent that a dependent's own files
    // are what's being layered on top of what it `uses`.
    entries.extend(collect_own_entries(ctx, overlay, &target)?);

    stack.pop();
    visited.insert(overlay.name.clone());

    Ok(())
}

/// This overlay's own entries: its root directory, its declared `git`
/// checkouts, its own file tree, and its `.link.*` sidecars. Excludes
/// `uses` — callers that need the full transitive graph recurse separately
/// (see [`build_inner`]); [`DesiredTree::build_own`] calls this directly for
/// a single node.
fn collect_own_entries(
    ctx: &exec::Context,
    overlay: &Overlay,
    target: &Path,
) -> Result<Vec<DesiredEntry>> {
    let mut entries = Vec::new();

    // Every overlay needs a place to live, even one with no files of its own
    // (a `uses`-only or git-only overlay) — mirrors the unconditional
    // `EnsureDir` at the top of `Overlay::apply_inner`.
    entries.push(DesiredEntry {
        target: target.to_path_buf(),
        provenance: Provenance::Overlay {
            overlay: overlay.name.clone(),
            source: overlay.root.clone(),
        },
        intent: MaterializationIntent::Directory,
        permissions: None,
    });

    // Git-managed paths: materialized by CheckoutMaterializer (#110), which
    // ensures presence/configuration only — content-level bidirectional
    // sync is `over sync` (crate::sync), a separate explicit operation.
    if let Some(git_repos) = &overlay.git {
        for (repo_key, config) in git_repos {
            let repo_target = if repo_key == ROOT_PATH {
                target.to_path_buf()
            } else {
                target.join(repo_key)
            };
            entries.push(DesiredEntry {
                target: repo_target,
                provenance: Provenance::Git {
                    overlay: overlay.name.clone(),
                    repo_key: repo_key.clone(),
                    config: Box::new(config.clone()),
                },
                intent: MaterializationIntent::Checkout,
                permissions: None,
            });
        }
    }

    walk_overlay_tree(overlay, target, &mut entries)?;

    // Mirrors the same resolved-overlay bookkeeping `Overlay::apply` uses so
    // `.link.*` sidecar templates get the same (limited) `{{ overlays[...] }}`
    // support as before #13.
    let ctx_with_target =
        ctx.with_resolved_overlay(overlay.name.clone(), target.to_string_lossy().to_string());
    build_symlink_sidecars(&ctx_with_target, overlay, target, &mut entries)?;
    build_partial_sidecars(&ctx_with_target, overlay, &mut entries)?;

    Ok(entries)
}

/// Walk the overlay's own tree, producing one entry per file/directory —
/// overlay descriptor files, `.link.*` sidecars, and `exclude` globs are
/// skipped, mirroring what `Overlay::apply` used to filter for inline
/// before #13 moved materialization behind a `Plan`.
fn walk_overlay_tree(
    overlay: &Overlay,
    target: &Path,
    entries: &mut Vec<DesiredEntry>,
) -> Result<()> {
    let exclude = GlobBuilder::new(&overlays::GLOB_PATTERN)
        .literal_separator(true)
        .build()?
        .compile_matcher();
    let symlink_config = GlobBuilder::new("**/*.link.{toml,yaml,yml}")
        .literal_separator(true)
        .build()?
        .compile_matcher();
    let partial_config = GlobBuilder::new("**/*.partial.{toml,yaml,yml}")
        .literal_separator(true)
        .build()?
        .compile_matcher();

    let mut walker = WalkDir::new(&overlay.root).min_depth(1).into_iter();
    while let Some(entry) = walker.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("skipping entry due to error: {}", e);
                continue;
            }
        };
        let path = entry.path();

        if exclude.is_match(path) || symlink_config.is_match(path) || partial_config.is_match(path)
        {
            continue;
        }

        let rel_path = match path.strip_prefix(&overlay.root) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if overlay.is_excluded(rel_path) {
            continue;
        }

        let entry_target = target.join(rel_path);

        // A directory resolved to `SymlinkDirectory` — via a `rules`
        // entry, `defaults.materialization`, or a legacy `link_dirs` match,
        // all resolved through the same mechanism (#113/#126,
        // `Overlay::is_link_dir`) — is one materialization unit; its
        // children are not separately enumerated.
        if path.is_dir() && overlay.is_link_dir(rel_path) {
            entries.push(DesiredEntry {
                target: entry_target,
                provenance: Provenance::Overlay {
                    overlay: overlay.name.clone(),
                    source: path.to_path_buf(),
                },
                intent: MaterializationIntent::SymlinkDirectory {
                    source: path.to_path_buf(),
                    link_type: symlink::LinkType::Soft,
                },
                permissions: None,
            });
            walker.skip_current_dir();
            continue;
        }

        if path.is_dir() {
            entries.push(DesiredEntry {
                target: entry_target,
                provenance: Provenance::Overlay {
                    overlay: overlay.name.clone(),
                    source: path.to_path_buf(),
                },
                intent: MaterializationIntent::Directory,
                permissions: None,
            });
        } else {
            entries.push(DesiredEntry {
                target: entry_target,
                provenance: Provenance::Overlay {
                    overlay: overlay.name.clone(),
                    source: path.to_path_buf(),
                },
                intent: MaterializationIntent::SymlinkFile {
                    source: path.to_path_buf(),
                    link_type: symlink::LinkType::Soft,
                },
                permissions: None,
            });
        }
    }

    Ok(())
}

/// Resolve `.link.{toml,yaml,yml}` sidecars into entries (`.link.*` config
/// files declaring an arbitrary extra symlink, soft or hard).
fn build_symlink_sidecars(
    ctx: &exec::Context,
    overlay: &Overlay,
    target: &Path,
    entries: &mut Vec<DesiredEntry>,
) -> Result<()> {
    let symlinks = symlink::discover_symlinks(&overlay.root)?;
    for (name, config) in symlinks {
        let resolved_str = symlink::render_symlink_target(&config.target, ctx)?;
        let resolved = PathBuf::from(&resolved_str);
        let is_dir = resolved.is_dir()
            || resolved_str.ends_with(std::path::MAIN_SEPARATOR_STR)
            || resolved_str.ends_with('/');
        let entry_target = target.join(&name);
        let config_path = sidecar_config_path(&overlay.root, &name);

        let provenance = Provenance::SymlinkSidecar {
            overlay: overlay.name.clone(),
            config: config_path,
            template: config.target.clone(),
            resolved: resolved.clone(),
        };
        let intent = if is_dir {
            MaterializationIntent::SymlinkDirectory {
                source: resolved,
                link_type: config.r#type,
            }
        } else {
            MaterializationIntent::SymlinkFile {
                source: resolved,
                link_type: config.r#type,
            }
        };

        entries.push(DesiredEntry {
            target: entry_target,
            provenance,
            intent,
            // Symlink sidecars carry a source outside the overlay's own
            // walked tree, but the same reasoning applies: no independent
            // target permission without mutating that source (ADR-020).
            permissions: None,
        });
    }
    Ok(())
}

/// Reconstruct the sidecar file path for a symlink `stem`, since
/// `discover_symlinks` only returns the stem and parsed config, not the
/// originating file. Mirrors its own TOML > YAML > YML precedence.
fn sidecar_config_path(overlay_root: &Path, stem: &str) -> PathBuf {
    for ext in ["toml", "yaml", "yml"] {
        let candidate = overlay_root.join(format!("{stem}.link.{ext}"));
        if candidate.exists() {
            return candidate;
        }
    }
    // Shouldn't happen: `discover_symlinks` found this stem from one of
    // these three files. Fall back to the canonical (TOML) form for a
    // stable, if slightly inaccurate, diagnostic path.
    overlay_root.join(format!("{stem}.link.toml"))
}

/// Resolve `.partial.{toml,yaml,yml}` sidecars into
/// [`MaterializationIntent::PartialFile`] entries (#66): a managed block
/// injected into `target` (rendered the same way a `.link.*` sidecar's
/// `target` is — this reuses `symlink::render_symlink_target` directly,
/// since it's already a generic path-template renderer, not
/// symlink-specific in implementation).
///
/// Unlike `.link.*` sidecars, the entry's target is the rendered path
/// *itself*, not `target_root.join(stem)`: a managed block's target is an
/// arbitrary external file (e.g. `~/.zshrc`), not something living under
/// the overlay's own target root by naming convention.
fn build_partial_sidecars(
    ctx: &exec::Context,
    overlay: &Overlay,
    entries: &mut Vec<DesiredEntry>,
) -> Result<()> {
    let partials = partial::discover_partials(&overlay.root)?;
    for (name, config) in partials {
        let resolved = symlink::render_symlink_target(&config.target, ctx)?;
        let config_path = partial_sidecar_config_path(&overlay.root, &name);
        let marker = config.marker.unwrap_or_else(|| name.clone());
        // The only intent `over` writes real target content for directly
        // (#65) — resolved against the sidecar's own overlay-relative stem
        // (not the rendered, possibly-external `target`), so `permissions`
        // rules match the same identity `.link.*`/`.partial.*` naming
        // already uses elsewhere.
        let permissions = overlay.permission_for(Path::new(&name));

        entries.push(DesiredEntry {
            target: PathBuf::from(resolved),
            provenance: Provenance::PartialSidecar {
                overlay: overlay.name.clone(),
                config: config_path,
            },
            intent: MaterializationIntent::PartialFile {
                content: config.content,
                marker,
            },
            permissions,
        });
    }
    Ok(())
}

/// Reconstruct the sidecar file path for a partial `stem`, mirroring
/// [`sidecar_config_path`]'s own TOML > YAML > YML precedence.
fn partial_sidecar_config_path(overlay_root: &Path, stem: &str) -> PathBuf {
    for ext in ["toml", "yaml", "yml"] {
        let candidate = overlay_root.join(format!("{stem}.partial.{ext}"));
        if candidate.exists() {
            return candidate;
        }
    }
    overlay_root.join(format!("{stem}.partial.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::symlink::LinkType;
    use crate::exec::Context;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    fn ctx(root: PathBuf, repo: Repository) -> exec::Ctx {
        Context::builder().root(root).repository(repo).build()
    }

    fn ctx_no_uses(root: PathBuf, repo: Repository) -> exec::Ctx {
        Context::builder()
            .root(root)
            .repository(repo)
            .no_uses(true)
            .build()
    }

    #[rstest]
    fn empty_overlay_has_only_root_directory_entry() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());

        let tree = DesiredTree::build(&c, &overlay).unwrap();
        assert_eq!(tree.len(), 1);
        let entry = &tree.entries()[0];
        assert_eq!(entry.target, td.path().to_path_buf());
        assert!(matches!(entry.intent, MaterializationIntent::Directory));
    }

    #[rstest]
    fn plain_files_and_nested_dirs_produce_expected_entries() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();
        overlay_dir.child("sub").create_dir_all().unwrap();
        overlay_dir
            .child("sub/nested.txt")
            .write_str("nested")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        // root dir + file.txt + sub (dir) + sub/nested.txt
        assert_eq!(tree.len(), 4);

        let file_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("file.txt"))
            .expect("file.txt entry present");
        match &file_entry.intent {
            MaterializationIntent::SymlinkFile { source, link_type } => {
                assert_eq!(source, &overlay.root.join("file.txt"));
                assert_eq!(*link_type, LinkType::Soft);
            }
            other => panic!("unexpected intent: {other:?}"),
        }

        let sub_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("sub"))
            .expect("sub dir entry present");
        assert!(matches!(sub_entry.intent, MaterializationIntent::Directory));

        let nested_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("sub/nested.txt"))
            .expect("sub/nested.txt entry present");
        assert!(matches!(
            nested_entry.intent,
            MaterializationIntent::SymlinkFile { .. }
        ));
    }

    #[rstest]
    fn excluded_files_are_absent() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("keep").unwrap();
        overlay_dir.child("file.bak").write_str("skip").unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        assert!(
            tree.entries()
                .iter()
                .any(|e| e.target == root.join("file.txt"))
        );
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == root.join("file.bak"))
        );
    }

    #[rstest]
    fn link_dirs_produce_single_entry_without_children() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nlink_dirs = [\"mydir\"]")
            .unwrap();
        overlay_dir.child("mydir").create_dir_all().unwrap();
        overlay_dir
            .child("mydir/inner.txt")
            .write_str("content")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let dir_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("mydir"))
            .expect("mydir entry present");
        match &dir_entry.intent {
            MaterializationIntent::SymlinkDirectory { source, link_type } => {
                assert_eq!(source, &overlay.root.join("mydir"));
                assert_eq!(*link_type, LinkType::Soft);
            }
            other => panic!("unexpected intent: {other:?}"),
        }
        // The child must NOT be separately enumerated.
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == root.join("mydir/inner.txt"))
        );
    }

    /// #126: an explicit `rules` entry produces the same
    /// `SymlinkDirectory` shape as `link_dirs`, through the same
    /// resolution path.
    #[rstest]
    fn a_materialization_rule_produces_a_symlink_directory_entry() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                "target = \"~\"\n[[rules]]\npath = \"mydir\"\nmaterialization = \"symlink-directory\"",
            )
            .unwrap();
        overlay_dir.child("mydir").create_dir_all().unwrap();
        overlay_dir
            .child("mydir/inner.txt")
            .write_str("content")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let dir_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("mydir"))
            .expect("mydir entry present");
        assert!(matches!(
            dir_entry.intent,
            MaterializationIntent::SymlinkDirectory { .. }
        ));
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == root.join("mydir/inner.txt"))
        );
    }

    /// #113: `defaults.materialization` applies repository-/overlay-wide,
    /// to every directory with no more specific `rules` override.
    #[rstest]
    fn defaults_materialization_applies_to_every_directory_without_a_rule() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[defaults]\nmaterialization = \"symlink-directory\"")
            .unwrap();
        overlay_dir.child("sub").create_dir_all().unwrap();
        overlay_dir
            .child("sub/inner.txt")
            .write_str("content")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let sub_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("sub"))
            .expect("sub entry present");
        assert!(matches!(
            sub_entry.intent,
            MaterializationIntent::SymlinkDirectory { .. }
        ));
    }

    /// #113: a file added to a source subtree governed by a file-level
    /// rule appears in the next `DesiredTree` build with no rule change —
    /// `walk_overlay_tree` re-walks the filesystem every call, so this is
    /// a regression test, not new behavior.
    #[rstest]
    fn a_newly_added_file_is_picked_up_without_any_rule_change() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let root = td.path().to_path_buf();

        let before = DesiredTree::build(&c, &overlay).unwrap();
        assert!(
            !before
                .entries()
                .iter()
                .any(|e| e.target == root.join("new.txt"))
        );

        overlay_dir.child("new.txt").write_str("content").unwrap();

        let after = DesiredTree::build(&c, &overlay).unwrap();
        let entry = after
            .entries()
            .iter()
            .find(|e| e.target == root.join("new.txt"))
            .expect("newly added file should appear in the next build");
        assert!(matches!(
            entry.intent,
            MaterializationIntent::SymlinkFile { .. }
        ));
    }

    #[rstest]
    fn symlink_sidecar_file_target_produces_symlink_file_entry() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("nvim.link.toml")
            .write_str("target = \"/opt/nvim-config\"")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("nvim"))
            .expect("nvim entry present");
        match &entry.intent {
            MaterializationIntent::SymlinkFile { source, link_type } => {
                assert_eq!(source, &PathBuf::from("/opt/nvim-config"));
                assert_eq!(*link_type, LinkType::Soft);
            }
            other => panic!("unexpected intent: {other:?}"),
        }
        match &entry.provenance {
            Provenance::SymlinkSidecar {
                config,
                template,
                resolved,
                ..
            } => {
                assert_eq!(config, &overlay.root.join("nvim.link.toml"));
                assert_eq!(template, "/opt/nvim-config");
                assert_eq!(resolved, &PathBuf::from("/opt/nvim-config"));
            }
            other => panic!("unexpected provenance: {other:?}"),
        }
    }

    #[rstest]
    fn symlink_sidecar_directory_target_produces_symlink_directory_entry() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let real_dir = td.child("real_target_dir");
        real_dir.create_dir_all().unwrap();
        // Escape backslashes so Windows paths don't get misparsed as TOML
        // unicode escape sequences (see `Overlay::resolve_target` tests).
        let target_toml = real_dir.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("app.link.toml")
            .write_str(&format!("target = \"{}\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("app"))
            .expect("app entry present");
        assert!(matches!(
            entry.intent,
            MaterializationIntent::SymlinkDirectory { .. }
        ));
    }

    #[rstest]
    fn symlink_sidecar_hard_type_is_preserved() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("hard.link.toml")
            .write_str("target = \"/opt/hard-target\"\ntype = \"hard\"")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("hard"))
            .expect("hard entry present");
        match &entry.intent {
            MaterializationIntent::SymlinkFile { link_type, .. } => {
                assert_eq!(*link_type, LinkType::Hard);
            }
            other => panic!("unexpected intent: {other:?}"),
        }
    }

    #[rstest]
    fn partial_sidecar_produces_partial_file_entry_at_rendered_target() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let external = td.child("external.zshrc");
        external.write_str("export FOO=bar\n").unwrap();
        let target_toml = external.path().to_string_lossy().replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!(
                "target = \"{}\"\ncontent = \"alias x=y\"",
                target_toml
            ))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == external.path().to_path_buf())
            .expect("partial entry present at the rendered target");
        match &entry.intent {
            MaterializationIntent::PartialFile { content, marker } => {
                assert_eq!(content, "alias x=y");
                assert_eq!(marker, "aliases");
            }
            other => panic!("unexpected intent: {other:?}"),
        }
        match &entry.provenance {
            Provenance::PartialSidecar { config, .. } => {
                assert_eq!(config, &overlay.root.join("aliases.partial.toml"));
            }
            other => panic!("unexpected provenance: {other:?}"),
        }
    }

    #[rstest]
    fn partial_sidecar_custom_marker_is_used() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!(
                "target = \"{}\"\ncontent = \"x\"\nmarker = \"custom\"",
                target_toml
            ))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == td.path().join("out.txt"))
            .expect("partial entry present");
        match &entry.intent {
            MaterializationIntent::PartialFile { marker, .. } => assert_eq!(marker, "custom"),
            other => panic!("unexpected intent: {other:?}"),
        }
    }

    // ── #65: permission resolution ───────────────────────────────────────

    #[rstest]
    fn partial_sidecar_resolves_permission_from_defaults() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[defaults]\nmode = \"600\"")
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!("target = \"{}\"\ncontent = \"x\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == td.path().join("out.txt"))
            .expect("partial entry present");
        assert_eq!(
            entry.permissions,
            Some(crate::overlays::FileMode::parse("600").unwrap())
        );
    }

    #[rstest]
    fn partial_sidecar_specific_rule_overrides_defaults() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                "target = \"~\"\n[defaults]\nmode = \"644\"\n\n[[permissions]]\npath = \"aliases\"\nmode = \"600\"",
            )
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!("target = \"{}\"\ncontent = \"x\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == td.path().join("out.txt"))
            .expect("partial entry present");
        assert_eq!(
            entry.permissions,
            Some(crate::overlays::FileMode::parse("600").unwrap())
        );
    }

    #[rstest]
    fn partial_sidecar_without_any_rule_has_no_permission() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!("target = \"{}\"\ncontent = \"x\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == td.path().join("out.txt"))
            .expect("partial entry present");
        assert_eq!(entry.permissions, None);
    }

    /// Symlinked entries share their overlay source's inode — there is no
    /// independent target permission to manage without mutating that
    /// source (ADR-020), so a `permissions` rule matching a plain file's
    /// path must never populate `DesiredEntry.permissions` for it, even
    /// when the rule's `path` matches exactly.
    #[rstest]
    fn symlinked_file_never_carries_a_permission_even_with_a_matching_rule() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                "target = \"~\"\n[defaults]\nmode = \"600\"\n\n[[permissions]]\npath = \"file.txt\"\nmode = \"600\"",
            )
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let file_entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join("file.txt"))
            .expect("file.txt entry present");
        assert!(matches!(
            file_entry.intent,
            MaterializationIntent::SymlinkFile { .. }
        ));
        assert_eq!(file_entry.permissions, None);
    }

    #[rstest]
    fn partial_sidecar_config_files_are_not_entries() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let target_toml = td
            .path()
            .join("out.txt")
            .to_string_lossy()
            .replace('\\', "\\\\");
        overlay_dir
            .child("aliases.partial.toml")
            .write_str(&format!("target = \"{}\"\ncontent = \"x\"", target_toml))
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == overlay.root.join("aliases.partial.toml"))
        );
    }

    #[rstest]
    fn git_root_path_produces_checkout_entry_at_target_root() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\ngit = \"https://example.com/repo.git\"")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root && matches!(e.intent, MaterializationIntent::Checkout));
        let entry = entry.expect("checkout entry at target root present");
        match &entry.provenance {
            Provenance::Git {
                config, repo_key, ..
            } => {
                assert_eq!(config.url, "https://example.com/repo.git");
                assert_eq!(repo_key, ".");
            }
            other => panic!("unexpected provenance: {other:?}"),
        }
    }

    #[rstest]
    fn git_named_path_produces_checkout_entry_at_that_path() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[git]\n\".config/nvim\" = \"https://example.com/nvim.git\"")
            .unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let root = td.path().to_path_buf();
        let entry = tree
            .entries()
            .iter()
            .find(|e| e.target == root.join(".config/nvim"))
            .expect("checkout entry at .config/nvim present");
        assert!(matches!(entry.intent, MaterializationIntent::Checkout));
    }

    #[rstest]
    fn uses_composition_includes_both_overlays_own_targets() {
        let (td, repo) = repo_and_root();

        let child_dir = td.child("child");
        child_dir.create_dir_all().unwrap();
        child_dir
            .child("over.toml")
            .write_str("target = \"~/child-target\"")
            .unwrap();
        child_dir.child("file.txt").write_str("content").unwrap();

        let parent_dir = td.child("parent");
        parent_dir.create_dir_all().unwrap();
        parent_dir
            .child("over.toml")
            .write_str("target = \"~/parent-target\"\nuses = [\"child\"]")
            .unwrap();
        parent_dir.child("own.txt").write_str("own").unwrap();

        let parent = repo.get("parent").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &parent).unwrap();

        let root = td.path().to_path_buf();
        assert!(
            tree.entries()
                .iter()
                .any(|e| e.target == root.join("parent-target/own.txt"))
        );
        assert!(
            tree.entries()
                .iter()
                .any(|e| e.target == root.join("child-target/file.txt"))
        );
    }

    #[rstest]
    fn no_uses_excludes_used_overlay_entries() {
        let (td, repo) = repo_and_root();

        let child_dir = td.child("child");
        child_dir.create_dir_all().unwrap();
        child_dir
            .child("over.toml")
            .write_str("target = \"~/child-target\"")
            .unwrap();
        child_dir.child("file.txt").write_str("content").unwrap();

        let parent_dir = td.child("parent");
        parent_dir.create_dir_all().unwrap();
        parent_dir
            .child("over.toml")
            .write_str("target = \"~/parent-target\"\nuses = [\"child\"]")
            .unwrap();

        let parent = repo.get("parent").unwrap();
        let c = ctx_no_uses(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &parent).unwrap();

        let root = td.path().to_path_buf();
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == root.join("child-target/file.txt"))
        );
    }

    /// Diamond dependency: A uses B and C, both B and C use D.
    /// D's entries should appear only once and no false cycle error should occur.
    #[rstest]
    fn diamond_dependency_includes_shared_overlay_entries_once() {
        let (td, repo) = repo_and_root();

        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml")
            .write_str("target = \"~/d-target\"")
            .unwrap();
        d.child("shared.txt").write_str("shared").unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("target = \"~/b-target\"\nuses = [\"d\"]")
            .unwrap();

        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("target = \"~/c-target\"\nuses = [\"d\"]")
            .unwrap();

        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("target = \"~/a-target\"\nuses = [\"b\", \"c\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay_a).unwrap();

        let root = td.path().to_path_buf();
        let matches = tree
            .entries()
            .iter()
            .filter(|e| e.target == root.join("d-target/shared.txt"))
            .count();
        assert_eq!(matches, 1, "d's entry should appear exactly once");
    }

    /// True cycle: A uses B, B uses A. Should produce a cycle error.
    #[rstest]
    fn true_cycle_is_detected() {
        let (td, repo) = repo_and_root();

        let a = td.child("a_cycle");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"b_cycle\"]")
            .unwrap();

        let b = td.child("b_cycle");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"a_cycle\"]")
            .unwrap();

        let overlay_a = repo.get("a_cycle").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let result = DesiredTree::build(&c, &overlay_a);
        assert!(result.is_err(), "cycle should be detected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Cycle detected"),
            "error should mention cycle: {err_msg}"
        );
        assert!(
            err_msg.contains("a_cycle"),
            "error should mention the cycling overlay: {err_msg}"
        );
    }

    #[rstest]
    fn descriptor_and_sidecar_config_files_are_not_entries() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("app.link.toml")
            .write_str("target = \"/opt/app\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("content").unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == overlay.root.join("over.toml"))
        );
        assert!(
            !tree
                .entries()
                .iter()
                .any(|e| e.target == overlay.root.join("app.link.toml"))
        );
    }

    #[rstest]
    fn entries_are_sorted_by_target() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("zeta.txt").write_str("z").unwrap();
        overlay_dir.child("alpha.txt").write_str("a").unwrap();
        overlay_dir.child("mid").create_dir_all().unwrap();
        overlay_dir.child("mid/file.txt").write_str("m").unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let tree = DesiredTree::build(&c, &overlay).unwrap();

        let targets: Vec<_> = tree.entries().iter().map(|e| e.target.clone()).collect();
        let mut sorted = targets.clone();
        sorted.sort();
        assert_eq!(targets, sorted);
    }

    #[test]
    fn empty_tree_reports_empty() {
        let tree = DesiredTree::default();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
    }

    /// Install a tracing subscriber so `tracing::warn!` bodies (the
    /// `WalkDir` error branch below) actually execute during the test.
    fn init_test_tracing() {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::TRACE)
            .try_init();
    }

    #[rstest]
    #[cfg(unix)]
    fn unreadable_subdirectory_is_skipped_with_a_warning() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        init_test_tracing();
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir.child("good.txt").write_str("ok").unwrap();
        let bad_dir = overlay_dir.child("bad_dir");
        bad_dir.create_dir_all().unwrap();
        bad_dir.child("hidden.txt").write_str("secret").unwrap();
        fs::set_permissions(bad_dir.path(), fs::Permissions::from_mode(0o000)).unwrap();

        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone());
        let result = DesiredTree::build(&c, &overlay);

        // Restore permissions for cleanup regardless of outcome.
        fs::set_permissions(bad_dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        let tree = result.expect("build should succeed despite unreadable entries");
        let root = td.path().to_path_buf();
        assert!(
            tree.entries()
                .iter()
                .any(|e| e.target == root.join("good.txt")),
            "good.txt should still be discovered"
        );
    }
}
