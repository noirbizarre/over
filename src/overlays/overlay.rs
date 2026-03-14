use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as AnyhowContext, Result};
use config::{Config, File, FileFormat, FileSourceFile};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};

use tera::{Context, Tera};

use crate::actions::git::config::GitRepoConfig;
use crate::actions::install::InstallConfig;
use crate::actions::{self, EnsureDir};
use crate::exec::{self, Action, Ctx};
use crate::ui::{emojis, style};

use super::Repository;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Overlay {
    pub name: String,

    pub root: PathBuf,

    pub description: Option<String>,

    pub target: String,

    pub uses: Option<Vec<String>>,

    pub exclude: Option<Vec<String>>,

    pub git: Option<HashMap<String, GitRepoConfig>>,

    pub install: Option<InstallConfig>,

    /// Glob patterns for directories that should be symlinked as a unit
    /// rather than recursed into when adding.
    pub link_dirs: Option<Vec<String>>,
}

impl fmt::Display for Overlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl Overlay {
    pub fn new(repository: &Repository, root: &Path) -> Result<Self> {
        let name = root
            .strip_prefix(repository.root.as_path())?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("overlay path is not valid UTF-8"))?;
        let mut sources: Vec<File<FileSourceFile, FileFormat>> = Vec::new();
        let mut dir = root;
        loop {
            let basename = dir.join("over");
            sources.push(
                File::with_name(
                    basename
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("config path is not valid UTF-8"))?,
                )
                .required(dir == root),
            );
            if dir == repository.root {
                break;
            }
            dir = dir
                .parent()
                .ok_or_else(|| anyhow::anyhow!("unexpected root path without parent"))?;
        }

        // Reverse so that ancestor configs are added first (lower priority)
        // and overlay-specific config is added last (higher priority / overrides)
        sources.reverse();

        let s = Config::builder()
            .add_source(sources)
            .set_override("name", name)?
            .set_override("root", root.to_str())?
            .set_default("target", "~")?
            .build()?;

        Ok(s.try_deserialize()?)
    }

    pub fn resolve_target(&self, ctx: &exec::Context) -> Result<PathBuf> {
        let path = PathBuf::from(&Tera::one_off(
            self.target.as_str(),
            &Context::from_serialize(ctx)?,
            true,
        )?);

        let path_str = path.to_string_lossy();
        Ok(match path_str.as_ref() {
            p if !p.starts_with("~") => path,
            "~" => ctx.root.clone(),
            _ => ctx.root.join(path.strip_prefix("~").unwrap_or(&path)),
        })
    }

    /// Check if a relative path matches any `link_dirs` glob pattern,
    /// meaning it should be symlinked as a whole directory rather than recursed into.
    pub fn is_link_dir(&self, rel_path: &Path) -> bool {
        let patterns = match &self.link_dirs {
            Some(p) if !p.is_empty() => p,
            _ => return false,
        };
        for pattern in patterns {
            if let Ok(glob) = GlobBuilder::new(pattern).literal_separator(true).build()
                && glob.compile_matcher().is_match(rel_path)
            {
                return true;
            }
        }
        false
    }

    pub async fn apply(&self, ctx: &Ctx) -> Result<()> {
        let mut visited = HashSet::new();
        self.apply_inner(ctx, &mut visited).await
    }

    fn apply_inner<'a>(
        &'a self,
        ctx: &'a Ctx,
        visited: &'a mut HashSet<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            if !visited.insert(self.name.clone()) {
                return Err(anyhow::anyhow!(
                    "Cycle detected: overlay '{}' was already visited (path: {})",
                    self.name,
                    visited.iter().cloned().collect::<Vec<_>>().join(" -> ")
                ));
            }
            let target = self.resolve_target(ctx)?;
            if !target.exists() {
                let mkdir = EnsureDir::new(target.to_path_buf());
                mkdir.execute(ctx.clone()).await?;
            }
            println!(
                "{} {} {} {} {}",
                emojis::PACKAGE,
                style::white_b("Applying overlay"),
                style::cyan(&self.name),
                style::white_b("to"),
                style::cyan(&target.to_string_lossy()),
            );
            if let Some(uses) = &self.uses {
                for name in uses {
                    let overlay = ctx
                        .repository
                        .get(name)
                        .with_context(|| format!("used overlay '{}' not found", name))?;
                    if ctx.debug {
                        println!("{:#?}", overlay);
                    }
                    overlay
                        .apply_inner(&ctx.with_overlay(overlay.clone()), visited)
                        .await?;
                }
            }

            actions::git::clone_repositories(ctx.clone(), self, &target).await?;
            actions::fs::link(ctx.clone(), self, &target).await?;

            println!(
                "{} {} {} {} {} {}",
                emojis::SPARKLE,
                style::white_b("Applied overlay"),
                style::cyan(&self.name),
                style::white_b("to"),
                style::cyan(&target.to_string_lossy()),
                style::white_b("with success"),
            );

            Ok(())
        })
    }

    pub async fn add_file(&self, ctx: &Ctx, file: &PathBuf) -> Result<()> {
        let _ = self.resolve_target(ctx)?;
        actions::fs::add_file(ctx.clone(), self, file).await?;
        Ok(())
    }

    pub async fn add_files(&self, ctx: &Ctx, files: &[PathBuf]) -> Result<()> {
        let _ = self.resolve_target(ctx)?;
        for file in files {
            actions::fs::add_path(ctx.clone(), self, file).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Context;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;
    use std::fs;

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    fn ctx(root: PathBuf, repo: Repository, overlay: Option<Overlay>) -> Ctx {
        Context::new(false, false, false, false, root, repo, overlay)
    }

    #[rstest]
    #[case("~", |root: &PathBuf| root.clone())]
    #[case("~/sub", |root: &PathBuf| root.join("sub"))]
    fn test_resolve_target(#[case] target: &str, #[case] expected: fn(&PathBuf) -> PathBuf) {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"{}\"", target))
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let root = td.path().to_path_buf();
        let c = ctx(root.clone(), repo.clone(), Some(overlay.clone()));
        let resolved = overlay.resolve_target(&c).unwrap();
        assert_eq!(resolved, expected(&root));
    }

    #[rstest]
    fn test_resolve_target_absolute() {
        let (td, repo) = repo_and_root();
        let abs = td.child("abs_root");
        abs.create_dir_all().unwrap();
        let overlay_dir = td.child("abs");
        overlay_dir.create_dir_all().unwrap();
        let target_str = abs.to_str().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"{}\"", target_str))
            .unwrap();
        let overlay = repo.get("abs").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let resolved = overlay.resolve_target(&c).unwrap();
        assert_eq!(resolved, PathBuf::from(target_str));
    }

    #[tokio::test]
    async fn test_apply_with_uses() {
        let (td, repo) = repo_and_root();
        let child_dir = td.child("child");
        child_dir.create_dir_all().unwrap();
        child_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        child_dir.child("file.txt").write_str("content").unwrap();

        let parent_dir = td.child("parent");
        parent_dir.create_dir_all().unwrap();
        parent_dir
            .child("over.toml")
            .write_str("target = \"~\"\nuses = [\"child\"]")
            .unwrap();

        let parent = repo.get("parent").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(parent.clone()));
        let result = parent.apply(&c).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_add_file_success() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_add");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let original_file = td.path().join("my.txt");
        fs::write(&original_file, "hello world").unwrap();

        let overlay = repo.get("ov_add").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.add_file(&c, &original_file).await;
        assert!(result.is_ok(), "add_file should succeed");

        let moved_path = overlay.root.join("my.txt");
        assert!(
            moved_path.exists(),
            "moved file should exist in overlay root"
        );
        assert_eq!(fs::read_to_string(&moved_path).unwrap(), "hello world");

        assert!(
            original_file.exists(),
            "original path should exist as symlink"
        );
        let symlink_target = fs::read_link(&original_file).unwrap();
        assert_eq!(symlink_target, moved_path);
    }

    #[tokio::test]
    async fn test_add_file_outside_target_errors() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_err");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let overlay = repo.get("ov_err").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let outside = assert_fs::TempDir::new().unwrap();
        let outside_file = outside.path().join("ext.txt");
        fs::write(&outside_file, "external").unwrap();

        let result = overlay.add_file(&c, &outside_file).await;
        assert!(
            result.is_err(),
            "adding file outside target root should error"
        );
    }

    #[rstest]
    fn test_is_link_dir_matches() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_ld");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nlink_dirs = [\".config/nvim\", \".local/share/*\"]")
            .unwrap();
        let overlay = repo.get("ov_ld").unwrap();

        assert!(overlay.is_link_dir(Path::new(".config/nvim")));
        assert!(overlay.is_link_dir(Path::new(".local/share/fonts")));
        assert!(!overlay.is_link_dir(Path::new(".config/other")));
        assert!(!overlay.is_link_dir(Path::new("random")));
    }

    #[rstest]
    fn test_is_link_dir_none() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_ld_none");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_ld_none").unwrap();

        assert!(!overlay.is_link_dir(Path::new(".config/nvim")));
        assert!(!overlay.is_link_dir(Path::new("anything")));
    }

    #[tokio::test]
    async fn test_add_files_multiple() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_multi");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let file_a = td.path().join("a.txt");
        let file_b = td.path().join("b.txt");
        fs::write(&file_a, "aaa").unwrap();
        fs::write(&file_b, "bbb").unwrap();

        let overlay = repo.get("ov_multi").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay
            .add_files(&c, &[file_a.clone(), file_b.clone()])
            .await;
        assert!(
            result.is_ok(),
            "add_files should succeed: {:?}",
            result.err()
        );

        assert!(overlay.root.join("a.txt").exists());
        assert!(overlay.root.join("b.txt").exists());
        assert!(file_a.is_symlink());
        assert!(file_b.is_symlink());
    }
}
