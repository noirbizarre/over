use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as AnyhowContext, Result};
use config::{Config, File, FileFormat, FileSourceFile};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};

use minijinja::Environment;

use crate::actions::git::config::{GitRepoConfig, deserialize_git_field};
use crate::actions::install::InstallConfig;
use crate::actions::{self, EnsureDir, EnsureSymlink};
use crate::exec::{self, Action, Ctx};
use crate::ui;
use crate::ui::{emojis, style};
use indicatif::ProgressBar;
use indicatif::ProgressStyle;
use std::sync::LazyLock;

use super::{DEFAULT_TARGET, Repository};

fn default_none() -> Option<Vec<String>> {
    None
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};

    struct StringOrVecVisitor;

    impl<'de> Visitor<'de> for StringOrVecVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or a list of strings")
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![value.to_owned()])
        }

        fn visit_seq<S>(self, mut seq: S) -> std::result::Result<Self::Value, S::Error>
        where
            S: de::SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(elem) = seq.next_element()? {
                vec.push(elem);
            }
            Ok(vec)
        }
    }

    let value = deserializer.deserialize_any(StringOrVecVisitor)?;
    Ok(Some(value))
}

static SYMLINK_SPINNER_STYLE: LazyLock<ProgressStyle> = LazyLock::new(|| {
    ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
        .unwrap()
        .tick_chars(style::TICK_CHARS_BRAILLE_4_6_DOWN.as_str())
});

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Overlay {
    pub name: String,

    pub root: PathBuf,

    pub description: Option<String>,

    pub target: String,

    pub uses: Option<Vec<String>>,

    #[serde(
        default = "default_none",
        deserialize_with = "deserialize_string_or_vec"
    )]
    pub exclude: Option<Vec<String>>,

    #[serde(default, deserialize_with = "deserialize_git_field")]
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
            .set_default("target", DEFAULT_TARGET)?
            .build()?;

        Ok(s.try_deserialize()?)
    }

    pub fn resolve_target(&self, ctx: &exec::Context) -> Result<PathBuf> {
        let env = Environment::new();
        let path = PathBuf::from(env.render_str(self.target.as_str(), ctx).with_context(|| {
            format!(
                "failed to render target template '{}' for overlay '{}'",
                self.target, self.name
            )
        })?);

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

    /// Check if a relative path matches any `exclude` glob pattern.
    /// Returns `false` if no exclude patterns are configured.
    /// Checks the full path, each leading prefix, and each trailing suffix
    /// so that directory patterns (e.g. `.git`) match both the directory itself
    /// and any nested contents (e.g. `.git/config`, `src/__pycache__/mod.pyc`).
    pub fn is_excluded(&self, rel_path: &Path) -> bool {
        let patterns = match &self.exclude {
            Some(p) if !p.is_empty() => p,
            _ => return false,
        };

        // Collect all path variants to check: full path, prefixes, and suffixes
        let components: Vec<_> = rel_path.components().collect();
        let mut paths_to_check: Vec<PathBuf> = vec![rel_path.to_path_buf()];

        // Prefixes: `.git`, `src/__pycache__`
        for i in 1..components.len() {
            paths_to_check.push(components[..i].iter().collect());
        }

        // Suffixes: `config`, `__pycache__/mod.pyc`
        for i in 1..components.len() {
            paths_to_check.push(components[i..].iter().collect());
        }

        for path in &paths_to_check {
            for pattern in patterns {
                if let Ok(glob) = GlobBuilder::new(pattern).literal_separator(true).build()
                    && glob.compile_matcher().is_match(path)
                {
                    return true;
                }
            }
        }

        false
    }

    pub async fn apply(&self, ctx: &Ctx) -> Result<()> {
        let mut visited = HashSet::new();
        let mut stack = Vec::new();
        self.apply_inner(ctx, &mut visited, &mut stack).await
    }

    fn apply_inner<'a>(
        &'a self,
        ctx: &'a Ctx,
        visited: &'a mut HashSet<String>,
        stack: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            // Already fully applied via another dependency path — skip silently
            if visited.contains(&self.name) {
                return Ok(());
            }
            // Currently in the recursion stack — true cycle
            if stack.contains(&self.name) {
                stack.push(self.name.clone());
                return Err(anyhow::anyhow!(
                    "Cycle detected: overlay '{}' forms a cycle (path: {})",
                    self.name,
                    stack.join(" -> ")
                ));
            }
            stack.push(self.name.clone());

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
                        .apply_inner(&ctx.with_overlay(overlay.clone()), visited, stack)
                        .await?;
                }
            }

            actions::git::clone_repositories(ctx.clone(), self, &target).await?;
            actions::fs::link(ctx.clone(), self, &target).await?;

            let ctx_with_target =
                ctx.with_resolved_overlay(self.name.clone(), target.to_string_lossy().to_string());
            self.apply_symlinks(ctx_with_target.clone(), &target)
                .await?;

            stack.pop();
            visited.insert(self.name.clone());

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

    pub async fn apply_symlinks(&self, ctx: Ctx, target: &Path) -> Result<()> {
        let symlinks = actions::symlink::discover_symlinks(&self.root)?;
        if symlinks.is_empty() {
            return Ok(());
        }

        ui::info(format!(
            "{} {}",
            emojis::LINK,
            style::white("Linking symlinks"),
        ))?;

        let progress = ProgressBar::new_spinner()
            .with_style(SYMLINK_SPINNER_STYLE.clone())
            .with_message("");

        for (name, config) in &symlinks {
            let resolved_target =
                actions::symlink::render_symlink_target(&config.target, &ctx.resolved_overlays)?;
            let target_path = PathBuf::from(&resolved_target);
            let is_dir = target_path.is_dir()
                || resolved_target.ends_with(std::path::MAIN_SEPARATOR_STR)
                || resolved_target.ends_with('/');
            let link_path = target.join(name);

            let action = EnsureSymlink::new(target_path, link_path, config.r#type.clone(), is_dir);
            if ctx.verbose || ctx.dry_run {
                progress.println(format!("{}", action));
            }
            progress.set_message(format!("{}", action));
            action.execute(ctx.clone()).await?;
        }

        progress.finish_and_clear();
        Ok(())
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
        let mut builder = Context::builder().root(root).repository(repo);
        if let Some(o) = overlay {
            builder = builder.overlay(o);
        }
        builder.build()
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

    /// Diamond dependency: A uses B and C, both B and C use D.
    /// D should only be applied once and no false cycle error should occur.
    #[tokio::test]
    async fn test_apply_diamond_dependency() {
        let (td, repo) = repo_and_root();

        // D: leaf overlay used by both B and C
        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml").write_str("target = \"~\"").unwrap();

        // B: uses D
        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"d\"]")
            .unwrap();

        // C: uses D
        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"d\"]")
            .unwrap();

        // A: uses B and C
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"b\", \"c\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let c = ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let result = overlay_a.apply(&c).await;
        assert!(
            result.is_ok(),
            "diamond dependency should not cause a cycle error: {:?}",
            result.err()
        );
    }

    /// True cycle: A uses B, B uses A. Should produce a cycle error.
    #[tokio::test]
    async fn test_apply_cycle_detected() {
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
        let c = ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let result = overlay_a.apply(&c).await;
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

    // ── git: <url> shorthand ─────────────────────────────────────────────

    #[test]
    fn test_git_simple_url_toml() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_git_url");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                r#"
target = "~"
git = "https://github.com/user/repo.git"
"#,
            )
            .unwrap();
        let overlay = repo.get("ov_git_url").unwrap();
        let git = overlay.git.as_ref().expect("git should be Some");
        assert_eq!(git.len(), 1);
        let cfg = &git["."];
        assert_eq!(cfg.url, "https://github.com/user/repo.git");
        assert_eq!(cfg.branch, None);
    }

    #[test]
    fn test_git_detailed_single_toml() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_git_det");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                r#"
target = "~"

[git]
url = "git@github.com:user/repo.git"
branch = "main"
recurse_submodules = true
"#,
            )
            .unwrap();
        let overlay = repo.get("ov_git_det").unwrap();
        let git = overlay.git.as_ref().expect("git should be Some");
        assert_eq!(git.len(), 1);
        let cfg = &git["."];
        assert_eq!(cfg.url, "git@github.com:user/repo.git");
        assert_eq!(cfg.branch.as_deref(), Some("main"));
        assert!(cfg.recurse_submodules);
    }

    #[test]
    fn test_git_map_form_still_works_toml() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_git_map");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                r#"
target = "~"

[git]
".tmux/plugins/tpm" = "https://github.com/tmux-plugins/tpm"

[git.".config/nvim"]
url = "git@github.com:user/nvim-config.git"
branch = "main"
"#,
            )
            .unwrap();
        let overlay = repo.get("ov_git_map").unwrap();
        let git = overlay.git.as_ref().expect("git should be Some");
        assert_eq!(git.len(), 2);
        assert_eq!(
            git[".tmux/plugins/tpm"].url,
            "https://github.com/tmux-plugins/tpm"
        );
        assert_eq!(
            git[".config/nvim"].url,
            "git@github.com:user/nvim-config.git"
        );
    }

    #[test]
    fn test_git_absent_remains_none() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_no_git");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_no_git").unwrap();
        assert!(overlay.git.is_none());
    }

    #[tokio::test]
    async fn test_apply_creates_symlinks() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_symlink");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("test.link.toml")
            .write_str("target = \"/tmp/test_symlink_target\"")
            .unwrap();

        let overlay = repo.get("ov_symlink").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.apply(&c).await;
        assert!(
            result.is_ok(),
            "apply with symlinks should succeed: {:?}",
            result.err()
        );

        let target_path = td.path().join("test");
        assert!(
            target_path.is_symlink(),
            "symlink should be created at target"
        );
        assert_eq!(
            fs::read_link(&target_path).unwrap(),
            PathBuf::from("/tmp/test_symlink_target")
        );
    }

    #[tokio::test]
    async fn test_apply_symlinks_with_env_template() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_sympl");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("templated.link.toml")
            .write_str("target = \"/tmp/test_symlink_src\"")
            .unwrap();

        let overlay = repo.get("ov_sympl").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.apply(&c).await;
        assert!(
            result.is_ok(),
            "apply with templated symlink should succeed: {:?}",
            result.err()
        );

        let target_path = td.path().join("templated");
        assert!(
            target_path.is_symlink(),
            "symlink should be created at overlay target"
        );
        assert_eq!(
            fs::read_link(&target_path).unwrap(),
            PathBuf::from("/tmp/test_symlink_src")
        );
    }

    // ── exclude: string or list deserialization ──────────────────────────

    #[rstest]
    fn test_exclude_as_single_string() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_exc_str");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();
        let overlay = repo.get("ov_exc_str").unwrap();
        let exclude = overlay.exclude.as_ref().expect("exclude should be Some");
        assert_eq!(exclude.len(), 1);
        assert_eq!(exclude[0], "*.bak");
    }

    #[rstest]
    fn test_exclude_as_list() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_exc_list");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"*.bak\", \".git\", \"*.tmp\"]")
            .unwrap();
        let overlay = repo.get("ov_exc_list").unwrap();
        let exclude = overlay.exclude.as_ref().expect("exclude should be Some");
        assert_eq!(exclude.len(), 3);
        assert_eq!(exclude[0], "*.bak");
        assert_eq!(exclude[1], ".git");
        assert_eq!(exclude[2], "*.tmp");
    }

    #[rstest]
    fn test_exclude_absent_is_none() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_no_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_no_exc").unwrap();
        assert!(overlay.exclude.is_none());
    }

    #[rstest]
    fn test_exclude_empty_list_is_some_empty_vec() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_empty_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = []")
            .unwrap();
        let overlay = repo.get("ov_empty_exc").unwrap();
        let exclude = overlay.exclude.as_ref().expect("exclude should be Some");
        assert!(exclude.is_empty());
    }

    // ── is_excluded method ───────────────────────────────────────────────

    #[rstest]
    fn test_is_excluded_matches_single_pattern() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_is_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();
        let overlay = repo.get("ov_is_exc").unwrap();

        assert!(overlay.is_excluded(Path::new("file.bak")));
        assert!(!overlay.is_excluded(Path::new("file.txt")));
    }

    #[rstest]
    fn test_is_excluded_matches_nested_pattern() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_is_exc_nested");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"**/*.bak\"")
            .unwrap();
        let overlay = repo.get("ov_is_exc_nested").unwrap();

        assert!(overlay.is_excluded(Path::new("file.bak")));
        assert!(overlay.is_excluded(Path::new("subdir/file.bak")));
        assert!(!overlay.is_excluded(Path::new("file.txt")));
    }

    #[rstest]
    fn test_is_excluded_matches_multiple_patterns() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_is_exc_multi");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"*.bak\", \".git\", \"*.tmp\"]")
            .unwrap();
        let overlay = repo.get("ov_is_exc_multi").unwrap();

        assert!(overlay.is_excluded(Path::new("file.bak")));
        assert!(overlay.is_excluded(Path::new(".git")));
        assert!(overlay.is_excluded(Path::new(".git/config")));
        assert!(overlay.is_excluded(Path::new("file.tmp")));
        assert!(!overlay.is_excluded(Path::new("file.txt")));
        assert!(!overlay.is_excluded(Path::new("normal/file.conf")));
    }

    #[rstest]
    fn test_is_excluded_none_returns_false() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_no_exc_is");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_no_exc_is").unwrap();

        assert!(!overlay.is_excluded(Path::new("anything")));
        assert!(!overlay.is_excluded(Path::new("file.bak")));
    }

    #[rstest]
    fn test_is_excluded_directory_pattern() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_dir_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"__pycache__\", \"*.pyc\"]")
            .unwrap();
        let overlay = repo.get("ov_dir_exc").unwrap();

        assert!(overlay.is_excluded(Path::new("__pycache__")));
        assert!(overlay.is_excluded(Path::new("__pycache__/module.cpython.pyc")));
        assert!(overlay.is_excluded(Path::new("src/__pycache__")));
        assert!(overlay.is_excluded(Path::new("module.pyc")));
        assert!(!overlay.is_excluded(Path::new("module.py")));
    }

    // ── apply with exclude ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_apply_excludes_files() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_apply_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("keep").unwrap();
        overlay_dir.child("file.bak").write_str("skip").unwrap();

        let overlay = repo.get("ov_apply_exc").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.apply(&c).await;
        assert!(result.is_ok(), "apply should succeed: {:?}", result.err());

        let target = td.path().to_path_buf();
        assert!(
            target.join("file.txt").is_symlink(),
            "file.txt should be linked"
        );
        assert!(
            !target.join("file.bak").exists(),
            "file.bak should not exist at target"
        );
    }

    #[tokio::test]
    async fn test_apply_excludes_multiple_patterns() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_apply_exc_multi");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"*.bak\", \".git\", \"*.tmp\"]")
            .unwrap();
        overlay_dir.child("config.yml").write_str("keep").unwrap();
        overlay_dir.child("file.bak").write_str("skip").unwrap();
        overlay_dir.child("file.tmp").write_str("skip").unwrap();
        overlay_dir.child(".git").create_dir_all().unwrap();
        overlay_dir.child(".git/config").write_str("skip").unwrap();

        let overlay = repo.get("ov_apply_exc_multi").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.apply(&c).await;
        assert!(result.is_ok(), "apply should succeed: {:?}", result.err());

        let target = td.path().to_path_buf();
        assert!(
            target.join("config.yml").is_symlink(),
            "config.yml should be linked"
        );
        assert!(
            !target.join("file.bak").exists(),
            "file.bak should not exist"
        );
        assert!(
            !target.join("file.tmp").exists(),
            "file.tmp should not exist"
        );
        assert!(!target.join(".git").exists(), ".git should not exist");
    }

    // ── add_dir with exclude ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_add_dir_excludes_files() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_add_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();

        let overlay = repo.get("ov_add_exc").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_dir = td.path().join("srcdir");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("file.txt"), "keep").unwrap();
        fs::write(src_dir.join("file.bak"), "skip").unwrap();

        let res = crate::actions::fs::add_dir(c.clone(), &overlay, &src_dir).await;
        assert!(res.is_ok(), "add_dir should succeed: {:?}", res.err());

        assert!(
            overlay.root.join("srcdir/file.txt").exists(),
            "srcdir/file.txt should be in overlay"
        );
        assert!(
            !overlay.root.join("srcdir/file.bak").exists(),
            "srcdir/file.bak should not be in overlay"
        );
    }

    #[tokio::test]
    async fn test_add_dir_excludes_nested_files() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_add_exc_nested");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"**/*.pyc\", \"__pycache__\"]")
            .unwrap();

        let overlay = repo.get("ov_add_exc_nested").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_dir = td.path().join("srcdir");
        fs::create_dir_all(src_dir.join("__pycache__")).unwrap();
        fs::write(src_dir.join("module.py"), "keep").unwrap();
        fs::write(src_dir.join("module.pyc"), "skip").unwrap();
        fs::write(src_dir.join("__pycache__/module.cpython.pyc"), "skip").unwrap();

        let res = crate::actions::fs::add_dir(c.clone(), &overlay, &src_dir).await;
        assert!(res.is_ok(), "add_dir should succeed: {:?}", res.err());

        assert!(
            overlay.root.join("srcdir/module.py").exists(),
            "srcdir/module.py should be in overlay"
        );
        assert!(
            !overlay.root.join("srcdir/module.pyc").exists(),
            "srcdir/module.pyc should not be in overlay"
        );
        assert!(
            !overlay.root.join("srcdir/__pycache__").exists(),
            "srcdir/__pycache__ should not be in overlay"
        );
    }
}
