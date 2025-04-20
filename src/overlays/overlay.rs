use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Result;
use config::{Config, File, FileFormat, FileSourceFile};
use serde::{Deserialize, Serialize};

use tera::{Context, Tera};

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

    pub git: Option<HashMap<String, String>>,

    pub install: Option<InstallConfig>,
}

impl fmt::Display for Overlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            self.name
        )
    }
}

impl Overlay {
    pub fn new(repository: &Repository, root: &Path) -> Result<Self> {
        let name = root
            .strip_prefix(repository.root.as_path())?
            .to_str()
            .unwrap();
        let mut sources: Vec<File<FileSourceFile, FileFormat>> = Vec::new();
        let mut dir = root;
        loop {
            let basename = dir.join("over");
            sources.push(File::with_name(basename.to_str().unwrap()).required(dir == root));
            if dir == repository.root {
                break;
            }
            dir = dir.parent().unwrap();
        }

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

        Ok(match path.to_str().unwrap() {
            p if !p.starts_with("~") => path,
            "~" => ctx.root.clone(),
            _ => ctx.root.join(path.strip_prefix("~").unwrap()),
        })
    }

    pub async fn apply(&self, ctx: &Ctx) -> Result<()> {
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
            style::cyan(target.to_str().unwrap()),
        );
        if let Some(uses) = &self.uses {
            for name in uses {
                let overlay = ctx.repository.get(name).expect("failed");
                if ctx.debug {
                    println!("{:#?}", overlay);
                }
                let _ = Box::pin(overlay.clone().apply(&ctx.with_overlay(overlay))).await;
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
            style::cyan(target.to_str().unwrap()),
            style::white_b("with success"),
        );

        Ok(())
    }

    pub async fn add_file(&self, ctx: &Ctx, file: &PathBuf) -> Result<()> {
        let root = self.resolve_target(ctx)?;
        actions::fs::add_file(ctx.clone(), self, file).await?;
        Ok(())
    }
}
