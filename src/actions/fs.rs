use std::env::current_dir;
use std::fmt;
use std::fs::{self, create_dir_all};
use std::path::{Path, PathBuf};

use dialoguer::Confirm;

use anyhow::Result;
use async_trait::async_trait;
use globset::GlobBuilder;
use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;
use symlink::{remove_symlink_file, symlink_file};

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
        .filter_map(Result::ok)
        .filter(|e| !exclude.is_match(e.path()));

    for file in files {
        // progress.tick();
        let rel_path = file.path().strip_prefix(&overlay.root)?;
        let target = to.join(rel_path);
        let path = file.path();
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

    let move_action = MoveFile::new(ctx.clone(), src.clone(), target.clone());
    let link_action = EnsureLink::new(ctx.clone(), target, src.to_path_buf());

    if ctx.verbose || ctx.dry_run {
        println!("{}", move_action);
    }
    move_action.execute(ctx.clone()).await?;

    if ctx.verbose || ctx.dry_run {
        println!("{}", link_action);
    }
    link_action.execute(ctx.clone()).await?;

    Ok(())
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
        // write!(f, "{} -> {}", self.source.display(), self.target.display())
        // We operate on string as path normalization is broken in rust
        // See:
        //  - https://users.rust-lang.org/t/trailing-in-paths/43166/9
        //  - https://github.com/rust-lang/rfcs/issues/2208
        let overlay = self.ctx.overlay.as_ref().unwrap();
        let rel_path = self
            .source
            .to_str()
            .unwrap()
            .strip_prefix(overlay.root.to_str().unwrap())
            .unwrap();
        let target_root = self
            .target
            .to_str()
            .unwrap()
            .strip_suffix(rel_path)
            .unwrap();
        write!(
            f,
            "{} {} {}{} {} {}{}{}",
            emojis::LINK,
            style::white("link:"),
            style::white("{"),
            short_path(overlay.root.to_str().unwrap()),
            style::white("->"),
            short_path(target_root),
            style::white("}"),
            rel_path,
        )
    }
}

#[async_trait]
impl Action for EnsureLink {
    async fn execute(&self, ctx: Ctx) -> Result<()> {
        if self.target.exists() {
            if self.target.is_symlink() {
                let src = fs::read_link(self.target.as_path())?;
                if src != self.source {
                    if ctx.force
                        || Confirm::with_theme(&DialogTheme::default())
                            .with_prompt(format!(
                                " Do you want to overwrite {} currently linked to {}?",
                                style::yellow(short_path(self.target.to_str().unwrap())),
                                style::yellow(short_path(src.to_str().unwrap())),
                            ))
                            .interact()
                            .unwrap()
                    {
                        remove_symlink_file(self.target.as_path())?;
                    } else {
                        return Err(anyhow::anyhow!("Link {} exists", self.target.display()));
                    }
                } else {
                    return Ok(());
                }
            } else if self.target.is_file() {
                // TODO: handle file absorption
                return Err(anyhow::anyhow!("File {} exists", self.target.display()));
            } else {
                return Err(anyhow::anyhow!("{} is a directory", self.target.display()));
            }
        }
        if !ctx.dry_run {
            symlink_file(self.source.as_path(), self.target.as_path())?;
        }

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
        let overlay = self.ctx.overlay.as_ref().unwrap();
        let src_root = overlay.resolve_target(&self.ctx).unwrap();

        let rel_path = self
            .src
            .to_str()
            .unwrap()
            .strip_prefix(src_root.to_str().unwrap())
            .unwrap();
        let target_root = self.dst.to_str().unwrap().strip_suffix(rel_path).unwrap();
        write!(
            f,
            "{} {} {}{} {} {}{}{}",
            emojis::MOVE_FILE,
            style::white("move file:"),
            style::white("{"),
            short_path(src_root.to_str().unwrap()),
            style::white("->"),
            short_path(target_root),
            style::white("}"),
            rel_path,
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
