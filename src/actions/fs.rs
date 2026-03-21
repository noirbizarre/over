use std::env::current_dir;
use std::fmt;
use std::fs::{self, create_dir_all};
use std::path::{Path, PathBuf};
use std::process::Command;

use dialoguer::Select;

use anyhow::Result;
use async_trait::async_trait;
use globset::GlobBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;
use symlink::{remove_symlink_dir, remove_symlink_file, symlink_dir, symlink_file};

use tokio::fs::rename;
use walkdir::WalkDir;

use crate::exec::{Action, Ctx};
use crate::overlays::{self, Overlay};
use crate::ui::style::DialogTheme;
use crate::ui::{self, emojis, style};
use crate::utils::short_path;

static SPINNER_STYLE: Lazy<ProgressStyle> = Lazy::new(|| {
    ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
        .unwrap()
        .tick_chars(style::TICK_CHARS_BRAILLE_4_6_DOWN.as_str())
});

/// Choices presented to the user when a conflict is detected during apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictChoice {
    Skip,
    Overwrite,
    Absorb,
    Diff,
}

impl ConflictChoice {
    /// All choices available for conflict resolution.
    const ALL: &[ConflictChoice] = &[
        ConflictChoice::Skip,
        ConflictChoice::Overwrite,
        ConflictChoice::Absorb,
        ConflictChoice::Diff,
    ];
}

impl fmt::Display for ConflictChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConflictChoice::Skip => write!(f, "Skip"),
            ConflictChoice::Overwrite => write!(f, "Overwrite"),
            ConflictChoice::Absorb => write!(f, "Absorb (adopt target into overlay)"),
            ConflictChoice::Diff => write!(f, "Diff (show differences, then decide)"),
        }
    }
}

/// Prompt the user to resolve a conflict between `source` (overlay) and `target` (existing).
fn prompt_conflict(source: &Path, target: &Path) -> Result<ConflictChoice> {
    let prompt = format!(
        "Conflict: {} already exists (overlay source: {})",
        style::yellow(short_path(&target.to_string_lossy())),
        style::yellow(short_path(&source.to_string_lossy())),
    );
    let selection = Select::with_theme(&DialogTheme::default())
        .with_prompt(prompt)
        .default(0)
        .items(ConflictChoice::ALL)
        .interact()
        .map_err(|e| anyhow::anyhow!("prompt failed: {}", e))?;
    Ok(ConflictChoice::ALL[selection])
}

/// Show a diff between the overlay source and the existing target using `git diff --no-index`.
fn show_diff(source: &Path, target: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("diff")
        .arg("--no-index")
        .arg("--")
        .arg(target)
        .arg(source)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run git diff: {}", e))?;
    // git diff --no-index exits with 1 when there are differences, which is expected
    if !status.success() && status.code() != Some(1) {
        return Err(anyhow::anyhow!(
            "git diff exited with unexpected status: {}",
            status
        ));
    }
    Ok(())
}

/// Absorb: copy the target file content into the overlay source, replacing it.
fn absorb_file(source: &Path, target: &Path) -> Result<()> {
    fs::copy(target, source).map_err(|e| {
        anyhow::anyhow!(
            "failed to absorb {} into {}: {}",
            target.display(),
            source.display(),
            e
        )
    })?;
    Ok(())
}

/// Absorb: recursively copy the target directory contents into the overlay source directory.
fn absorb_dir(source: &Path, target: &Path) -> Result<()> {
    // Remove existing overlay source directory and replace with target contents
    if source.exists() {
        fs::remove_dir_all(source).map_err(|e| {
            anyhow::anyhow!("failed to remove overlay dir {}: {}", source.display(), e)
        })?;
    }
    copy_dir_recursive(target, source)
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Remove a target path (file, symlink, or directory).
fn remove_target(target: &Path) -> Result<()> {
    if target.is_symlink() {
        // Determine if it's a file or dir symlink
        if target.is_dir() {
            remove_symlink_dir(target)?;
        } else {
            remove_symlink_file(target)?;
        }
    } else if target.is_dir() {
        fs::remove_dir_all(target)?;
    } else {
        fs::remove_file(target)?;
    }
    Ok(())
}

/// Resolve a file-level conflict using force/no_prompt flags or interactive prompt.
///
/// Returns `Ok(true)` if the caller should proceed with creating the link,
/// or `Ok(false)` if the file was skipped.
fn resolve_file_conflict(ctx: &Ctx, source: &Path, target: &Path) -> Result<bool> {
    if ctx.force {
        remove_target(target)?;
        return Ok(true);
    }
    if ctx.no_prompt {
        return Err(anyhow::anyhow!(
            "Conflict: {} already exists",
            target.display()
        ));
    }
    // Interactive prompt loop
    loop {
        match prompt_conflict(source, target)? {
            ConflictChoice::Skip => return Ok(false),
            ConflictChoice::Overwrite => {
                remove_target(target)?;
                return Ok(true);
            }
            ConflictChoice::Absorb => {
                absorb_file(source, target)?;
                remove_target(target)?;
                return Ok(true);
            }
            ConflictChoice::Diff => {
                show_diff(source, target)?;
                // Loop back to prompt
            }
        }
    }
}

/// Resolve a directory-level conflict using force/no_prompt flags or interactive prompt.
///
/// Returns `Ok(true)` if the caller should proceed with creating the link,
/// or `Ok(false)` if the directory was skipped.
fn resolve_dir_conflict(ctx: &Ctx, source: &Path, target: &Path) -> Result<bool> {
    if ctx.force {
        remove_target(target)?;
        return Ok(true);
    }
    if ctx.no_prompt {
        return Err(anyhow::anyhow!(
            "Conflict: {} already exists",
            target.display()
        ));
    }
    // Interactive prompt loop
    loop {
        match prompt_conflict(source, target)? {
            ConflictChoice::Skip => return Ok(false),
            ConflictChoice::Overwrite => {
                remove_target(target)?;
                return Ok(true);
            }
            ConflictChoice::Absorb => {
                absorb_dir(source, target)?;
                remove_target(target)?;
                return Ok(true);
            }
            ConflictChoice::Diff => {
                show_diff(source, target)?;
                // Loop back to prompt
            }
        }
    }
}

pub async fn link(ctx: Ctx, overlay: &Overlay, to: &Path) -> Result<()> {
    ui::info(format!(
        "{} {}",
        emojis::LINK,
        style::white("Linking files"),
    ))?;

    let progress = ProgressBar::new_spinner()
        .with_style(SPINNER_STYLE.clone())
        .with_message("");

    let exclude = GlobBuilder::new(&overlays::GLOB_PATTERN)
        .literal_separator(true)
        .build()?
        .compile_matcher();
    let files = WalkDir::new(&overlay.root)
        .min_depth(1)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(e) => Some(e),
            Err(e) => {
                eprintln!("Warning: skipping entry due to error: {}", e);
                None
            }
        })
        .filter(|e| !exclude.is_match(e.path()));

    for file in files {
        // progress.tick();
        let rel_path = file.path().strip_prefix(&overlay.root)?;
        let target = to.join(rel_path);
        let path = file.path();

        // If this directory matches a link_dirs pattern, symlink it as a unit
        // and skip its children (WalkDir will still yield them but we handle the dir)
        if path.is_dir() && overlay.is_link_dir(rel_path) {
            let action = EnsureDirLink::new(ctx.clone(), path.to_path_buf(), target);
            if ctx.verbose || ctx.dry_run {
                progress.println(format!("{}", action));
            }
            progress.set_message(format!("{}", action));
            action.execute(ctx.clone()).await?;
            continue;
        }

        let action: Box<dyn Action> = match () {
            _ if path.is_dir() => Box::new(EnsureDir::new(target)),
            _ if path.is_file() => Box::new(EnsureLink::new(
                ctx.clone(),
                file.clone().into_path(),
                target,
            )),
            _ => Box::new(EnsureLink::new(
                ctx.clone(),
                file.clone().into_path(),
                target,
            )),
        };
        if ctx.verbose || ctx.dry_run {
            progress.println(format!("{}", action));
        }
        progress.set_message(format!("{}", action));
        action.execute(ctx.clone()).await?;
    }
    // progress.finish_with_message("DOne");
    progress.finish_and_clear();
    Ok(())
}

pub async fn add_file(ctx: Ctx, overlay: &Overlay, file: &PathBuf) -> Result<()> {
    let src = if file.is_relative() {
        &current_dir()?.join(file)
    } else {
        file
    };
    if ctx.debug {
        println!("{:#?}", src);
    }
    let root = overlay.resolve_target(&ctx)?;
    if ctx.debug {
        println!("{:#?}", root);
    }
    let rel_path = match src.strip_prefix(&root) {
        Ok(tail) => tail,
        Err(_) => {
            return Err(anyhow::anyhow!(
                "{} is not included in {}",
                src.display(),
                root.display(),
            ));
        }
    };
    let target = overlay.root.join(rel_path);

    // Ensure parent directories exist in the overlay before moving the file
    if let Some(parent) = target.parent()
        && !parent.exists()
    {
        let dir_action = EnsureDir::new(parent.to_path_buf());
        if ctx.verbose || ctx.dry_run {
            println!("{}", dir_action);
        }
        dir_action.execute(ctx.clone()).await?;
    }

    let move_action = MoveFile::new(ctx.clone(), src.clone(), target.clone());
    let link_action = EnsureLink::new(ctx.clone(), target.clone(), src.to_path_buf());

    if ctx.verbose || ctx.dry_run {
        println!("{}", move_action);
    }
    move_action.execute(ctx.clone()).await?;

    if ctx.verbose || ctx.dry_run {
        println!("{}", link_action);
    }
    if let Err(e) = link_action.execute(ctx.clone()).await {
        // Rollback: move file back to original location
        if !ctx.dry_run {
            let _ = rename(&target, src).await;
        }
        return Err(e);
    }

    Ok(())
}

/// Add a directory to an overlay, either as a whole directory symlink (if it matches
/// `link_dirs`) or by recursing into it and adding each file individually.
pub async fn add_dir(ctx: Ctx, overlay: &Overlay, dir: &Path) -> Result<()> {
    let src = if dir.is_relative() {
        current_dir()?.join(dir)
    } else {
        dir.to_path_buf()
    };
    let root = overlay.resolve_target(&ctx)?;
    let rel_path = match src.strip_prefix(&root) {
        Ok(tail) => tail.to_path_buf(),
        Err(_) => {
            return Err(anyhow::anyhow!(
                "{} is not included in {}",
                src.display(),
                root.display(),
            ));
        }
    };

    if overlay.is_link_dir(&rel_path) {
        // Symlink the entire directory as a unit
        let target = overlay.root.join(&rel_path);

        // Ensure parent directories exist in the overlay
        if let Some(parent) = target.parent()
            && !parent.exists()
        {
            let dir_action = EnsureDir::new(parent.to_path_buf());
            if ctx.verbose || ctx.dry_run {
                println!("{}", dir_action);
            }
            dir_action.execute(ctx.clone()).await?;
        }

        let move_action = MoveFile::new(ctx.clone(), src.clone(), target.clone());
        let link_action = EnsureDirLink::new(ctx.clone(), target, src);

        if ctx.verbose || ctx.dry_run {
            println!("{}", move_action);
        }
        move_action.execute(ctx.clone()).await?;

        if ctx.verbose || ctx.dry_run {
            println!("{}", link_action);
        }
        link_action.execute(ctx.clone()).await?;
    } else {
        // Recurse into the directory and add each file individually
        let files: Vec<PathBuf> = WalkDir::new(&src)
            .min_depth(1)
            .into_iter()
            .filter_map(|entry| match entry {
                Ok(e) => Some(e),
                Err(e) => {
                    eprintln!("Warning: skipping entry due to error: {}", e);
                    None
                }
            })
            .filter(|e| e.path().is_file())
            .map(|e| e.path().to_path_buf())
            .collect();

        for file in files {
            add_file(ctx.clone(), overlay, &file).await?;
        }
    }

    Ok(())
}

/// Add a path (file or directory) to an overlay.
pub async fn add_path(ctx: Ctx, overlay: &Overlay, path: &Path) -> Result<()> {
    let src = if path.is_relative() {
        current_dir()?.join(path)
    } else {
        path.to_path_buf()
    };

    if src.is_dir() {
        add_dir(ctx, overlay, &src).await
    } else if src.is_file() {
        add_file(ctx, overlay, &src).await
    } else {
        Err(anyhow::anyhow!(
            "{} does not exist or is not a file/directory",
            src.display(),
        ))
    }
}

pub struct EnsureLink {
    pub ctx: Ctx,
    pub source: PathBuf,
    pub target: PathBuf,
}

impl EnsureLink {
    pub fn new(ctx: Ctx, source: PathBuf, target: PathBuf) -> Self {
        Self {
            ctx,
            source,
            target,
        }
    }
}

impl fmt::Display for EnsureLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(overlay) = self.ctx.overlay.as_ref()
            && let Ok(rel_path) = self.source.strip_prefix(&overlay.root)
        {
            let rel_str = rel_path.to_string_lossy();
            let target_root = self
                .target
                .to_string_lossy()
                .strip_suffix(rel_str.as_ref())
                .unwrap_or(&self.target.to_string_lossy())
                .to_string();
            return write!(
                f,
                "{} {} {}{} {} {}{}{}",
                emojis::LINK,
                style::white("link:"),
                style::white("{"),
                short_path(&overlay.root.to_string_lossy()),
                style::white("->"),
                short_path(&target_root),
                style::white("}"),
                rel_str,
            );
        }
        write!(
            f,
            "{} {} {} -> {}",
            emojis::LINK,
            style::white("link:"),
            self.source.display(),
            self.target.display(),
        )
    }
}

#[async_trait]
impl Action for EnsureLink {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        if self.target.exists() || self.target.is_symlink() {
            if self.target.is_symlink() {
                let src = fs::read_link(self.target.as_path())?;
                if src == self.source {
                    // Already correctly linked, no conflict
                    return Ok(());
                }
            }
            // Conflict: target exists and is not the correct symlink
            if !resolve_file_conflict(&ctx, &self.source, &self.target)? {
                return Ok(()); // Skipped
            }
        }
        symlink_file(self.source.as_path(), self.target.as_path())?;

        Ok(())
    }
}

pub struct EnsureDir {
    pub path: PathBuf,
    // pub target: PathBuf,
}

impl EnsureDir {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl fmt::Display for EnsureDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}",
            emojis::DIRECTORY,
            style::white("create directory:"),
            self.path.display(),
        )
    }
}

#[async_trait]
impl Action for EnsureDir {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if !ctx.dry_run {
            create_dir_all(self.path.as_path())?;
        }
        Ok(())
    }
}

pub struct EnsureDirLink {
    pub ctx: Ctx,
    pub source: PathBuf,
    pub target: PathBuf,
}

impl EnsureDirLink {
    pub fn new(ctx: Ctx, source: PathBuf, target: PathBuf) -> Self {
        Self {
            ctx,
            source,
            target,
        }
    }
}

impl fmt::Display for EnsureDirLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(overlay) = self.ctx.overlay.as_ref()
            && let Ok(rel_path) = self.source.strip_prefix(&overlay.root)
        {
            let rel_str = rel_path.to_string_lossy();
            let target_root = self
                .target
                .to_string_lossy()
                .strip_suffix(rel_str.as_ref())
                .unwrap_or(&self.target.to_string_lossy())
                .to_string();
            return write!(
                f,
                "{} {} {}{} {} {}{}{}",
                emojis::LINK,
                style::white("link dir:"),
                style::white("{"),
                short_path(&overlay.root.to_string_lossy()),
                style::white("->"),
                short_path(&target_root),
                style::white("}"),
                rel_str,
            );
        }
        write!(
            f,
            "{} {} {} -> {}",
            emojis::LINK,
            style::white("link dir:"),
            self.source.display(),
            self.target.display(),
        )
    }
}

#[async_trait]
impl Action for EnsureDirLink {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        if self.target.exists() || self.target.is_symlink() {
            if self.target.is_symlink() {
                let src = fs::read_link(self.target.as_path())?;
                if src == self.source {
                    // Already correctly linked, no conflict
                    return Ok(());
                }
            }
            // Conflict: target exists and is not the correct symlink
            if !resolve_dir_conflict(&ctx, &self.source, &self.target)? {
                return Ok(()); // Skipped
            }
        }
        symlink_dir(self.source.as_path(), self.target.as_path())?;

        Ok(())
    }
}

pub struct MoveFile {
    pub ctx: Ctx,
    pub src: PathBuf,
    pub dst: PathBuf,
}

impl MoveFile {
    pub fn new(ctx: Ctx, src: PathBuf, dst: PathBuf) -> Self {
        Self { ctx, src, dst }
    }
}

impl fmt::Display for MoveFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(overlay) = self.ctx.overlay.as_ref()
            && let Ok(src_root) = overlay.resolve_target(&self.ctx)
            && let Ok(rel_path) = self.src.strip_prefix(&src_root)
        {
            let rel_str = rel_path.to_string_lossy();
            let target_root = self
                .dst
                .to_string_lossy()
                .strip_suffix(rel_str.as_ref())
                .unwrap_or(&self.dst.to_string_lossy())
                .to_string();
            return write!(
                f,
                "{} {} {}{} {} {}{}{}",
                emojis::MOVE_FILE,
                style::white("move file:"),
                style::white("{"),
                short_path(&src_root.to_string_lossy()),
                style::white("->"),
                short_path(&target_root),
                style::white("}"),
                rel_str,
            );
        }
        write!(
            f,
            "{} {} {} -> {}",
            emojis::MOVE_FILE,
            style::white("move file:"),
            self.src.display(),
            self.dst.display(),
        )
    }
}

#[async_trait]
impl Action for MoveFile {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if !ctx.dry_run {
            rename(&self.src, &self.dst).await?;
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
    use std::fs;

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    fn ctx(root: PathBuf, repo: Repository, overlay: Option<Overlay>) -> Ctx {
        Context::new(false, false, true, true, false, root, repo, overlay)
    }

    #[tokio::test]
    async fn ensure_dir_creates_directory() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let dir_path = td.path().join("new_dir");
        let action = EnsureDir::new(dir_path.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(dir_path.exists(), "directory should be created");
    }

    #[tokio::test]
    async fn move_file_moves() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_move");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_move").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let original = td.child("file.txt");
        original.write_str("content").unwrap();
        let dst = overlay.root.join("file.txt");
        let action = MoveFile::new(c.clone(), original.path().to_path_buf(), dst.clone());
        action.execute(c.clone()).await.unwrap();
        assert!(dst.exists(), "file moved into overlay root");
        assert!(!original.path().exists(), "original should be moved away");
    }

    #[tokio::test]
    async fn ensure_link_creates_symlink() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_link");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_link").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_file = overlay.root.join("inner.txt");
        fs::write(&src_file, "hello").unwrap();
        let target = td.path().join("inner.txt");
        let action = EnsureLink::new(c.clone(), src_file.clone(), target.clone());
        action.execute(c.clone()).await.unwrap();
        let link_target = fs::read_link(&target).unwrap();
        assert_eq!(link_target, src_file);
    }

    #[tokio::test]
    async fn add_file_errors_when_outside_target() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_add_err");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_add_err").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        // outside file
        let outside_dir = assert_fs::TempDir::new().unwrap();
        let outside_file = outside_dir.path().join("ext.txt");
        fs::write(&outside_file, "ext").unwrap();
        let res = add_file(c.clone(), &overlay, &outside_file).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn add_file_creates_parent_dirs_in_overlay() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_nested");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_nested").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        // Create a nested file: <root>/.config/app/settings.conf
        let nested_dir = td.path().join(".config").join("app");
        fs::create_dir_all(&nested_dir).unwrap();
        let nested_file = nested_dir.join("settings.conf");
        fs::write(&nested_file, "key=value").unwrap();

        let res = add_file(c.clone(), &overlay, &nested_file).await;
        assert!(res.is_ok(), "add_file should succeed: {:?}", res.err());

        // Check the file was moved to the overlay
        let moved_path = overlay.root.join(".config/app/settings.conf");
        assert!(moved_path.exists(), "file should exist in overlay");
        assert_eq!(fs::read_to_string(&moved_path).unwrap(), "key=value");

        // Check symlink was created at original location
        assert!(nested_file.is_symlink(), "original should be a symlink");
        assert_eq!(fs::read_link(&nested_file).unwrap(), moved_path);
    }

    #[tokio::test]
    async fn add_dir_recursively_adds_files() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_dir");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_dir").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        // Create a directory with files: <root>/mydir/{a.txt, sub/b.txt}
        let dir = td.path().join("mydir");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "aaa").unwrap();
        fs::write(dir.join("sub").join("b.txt"), "bbb").unwrap();

        let res = add_dir(c.clone(), &overlay, &dir).await;
        assert!(res.is_ok(), "add_dir should succeed: {:?}", res.err());

        // Both files should be moved to overlay and symlinked back
        let moved_a = overlay.root.join("mydir/a.txt");
        let moved_b = overlay.root.join("mydir/sub/b.txt");
        assert!(moved_a.exists(), "a.txt should be in overlay");
        assert!(moved_b.exists(), "b.txt should be in overlay");
        assert_eq!(fs::read_to_string(&moved_a).unwrap(), "aaa");
        assert_eq!(fs::read_to_string(&moved_b).unwrap(), "bbb");

        // Original locations should be symlinks
        let orig_a = dir.join("a.txt");
        let orig_b = dir.join("sub").join("b.txt");
        assert!(orig_a.is_symlink(), "a.txt should be a symlink");
        assert!(orig_b.is_symlink(), "b.txt should be a symlink");
    }

    #[tokio::test]
    async fn add_dir_as_link_dir_symlinks_whole_directory() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_linkdir");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nlink_dirs = [\"mydir\"]")
            .unwrap();
        let overlay = repo.get("ov_linkdir").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        // Create a directory with files
        let dir = td.path().join("mydir");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("file.txt"), "content").unwrap();

        let res = add_dir(c.clone(), &overlay, &dir).await;
        assert!(res.is_ok(), "add_dir should succeed: {:?}", res.err());

        // The whole directory should be moved to the overlay
        let moved_dir = overlay.root.join("mydir");
        assert!(moved_dir.is_dir(), "mydir should exist as dir in overlay");
        assert!(
            moved_dir.join("file.txt").exists(),
            "file.txt should be in overlay dir"
        );

        // Original location should be a symlink to the overlay directory
        assert!(dir.is_symlink(), "original dir should be a symlink");
        assert_eq!(fs::read_link(&dir).unwrap(), moved_dir);
    }

    #[tokio::test]
    async fn add_path_dispatches_file() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_path_file");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_path_file").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let file = td.path().join("pathfile.txt");
        fs::write(&file, "data").unwrap();

        let res = add_path(c.clone(), &overlay, &file).await;
        assert!(
            res.is_ok(),
            "add_path for file should succeed: {:?}",
            res.err()
        );

        let moved = overlay.root.join("pathfile.txt");
        assert!(moved.exists(), "file should be in overlay");
        assert!(file.is_symlink(), "original should be a symlink");
    }

    #[tokio::test]
    async fn add_path_dispatches_directory() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_path_dir");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_path_dir").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let dir = td.path().join("pathdir");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x.txt"), "xxx").unwrap();

        let res = add_path(c.clone(), &overlay, &dir).await;
        assert!(
            res.is_ok(),
            "add_path for dir should succeed: {:?}",
            res.err()
        );

        let moved = overlay.root.join("pathdir/x.txt");
        assert!(moved.exists(), "file should be in overlay");
    }

    #[tokio::test]
    async fn add_path_nonexistent_errors() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_path_err");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_path_err").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let nonexistent = td.path().join("does_not_exist.txt");
        let res = add_path(c.clone(), &overlay, &nonexistent).await;
        assert!(res.is_err(), "add_path for nonexistent should error");
    }

    #[tokio::test]
    async fn ensure_dir_link_creates_dir_symlink() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_dirlink");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_dirlink").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        // Create source directory inside overlay
        let src_dir = overlay.root.join("linked_dir");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("file.txt"), "hello").unwrap();

        let target = td.path().join("linked_dir");
        let action = EnsureDirLink::new(c.clone(), src_dir.clone(), target.clone());
        action.execute(c.clone()).await.unwrap();

        assert!(target.is_symlink(), "target should be a symlink");
        assert_eq!(fs::read_link(&target).unwrap(), src_dir);
        assert!(
            target.join("file.txt").exists(),
            "should be able to access files through symlink"
        );
    }
}
