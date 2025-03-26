use std::{collections::HashSet, env::consts::OS};

use crate::{exec::Ctx, overlays::Overlay};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use which::which;

#[cfg(test)]
use mockall::{automock, mock, predicate::*};

pub async fn install(ctx: &Ctx, overlay: &Overlay) -> Result<()> {
    let finder = WhichFinder;
    match OS {
        "linux" => install_linux(ctx, overlay, finder).await?,
        "macos" => install_macos(ctx, overlay, finder).await?,
        "windows" => install_windows(ctx, overlay, finder).await?,
        _ => println!("Unsupported OS: {}", OS),
    }
    Ok(())
}

#[cfg_attr(test, automock)]
trait BinFinder {
    fn find_first(&self, bins: Vec<String>) -> Option<String>;
}

struct WhichFinder;

impl BinFinder for WhichFinder {
    fn find_first(&self, bins: Vec<String>) -> Option<String> {
        bins.into_iter().find(|bin| which(bin).is_ok())
    }
}


#[cfg_attr(test, automock)]
trait Installer {
    async fn install(&self, pkgs: Vec<String>) -> Result<()>;
}

struct ArchInstaller;

struct AptInstaller;

struct BrewInstaller;


async fn install_windows<T: BinFinder>(ctx: &Ctx, overlay: &Overlay, finder: T) -> Result<()> {
    todo!();
    Ok(())
}

async fn install_macos<T: BinFinder>(ctx: &Ctx, overlay: &Overlay, finder: T) -> Result<()> {
    if let Ok(brew) = which("brew") {
        println!("brew is installed at {}", brew.display());
    } else {
        println!("brew is not installed");
    }
    Ok(())
}

async fn install_linux<T: BinFinder>(ctx: &Ctx, overlay: &Overlay, finder: T) -> Result<()> {
    match finder.find_first(vec![
        "yay".to_string(),
        "paru".to_string(),
        "pacman".to_string(),
        "apt".to_string(),
    ]) {
        Some(bin) => match bin.as_str() {
            "yay" | "paru" | "pacman" => install_arch(ctx, overlay, &bin).await?,
            "apt" => install_apt(ctx, overlay, &bin).await?,
            _ => println!("Unknown bin {}", bin),
        },
        None => println!("No package manager found"),
    }
    let pkgs = get_archlinux_packages(ctx, overlay).await;
    println!("{:?}", pkgs);
    Ok(())
}

async fn install_apt(ctx: &Ctx, overlay: &Overlay, bin: &str) -> Result<()> {
    println!("Installer for apt using {}", bin);
    todo!();
    Ok(())
}

async fn install_arch(ctx: &Ctx, overlay: &Overlay, bin: &str) -> Result<()> {
    println!("Installer for Archlinux using {}", bin);
    todo!();
    Ok(())
}

async fn get_archlinux_packages(ctx: &Ctx, overlay: &Overlay) -> HashSet<String> {
    let mut packages: HashSet<String> = HashSet::new();
    if let Some(install) = &overlay.install {
        if let Some(archlinux) = &install.archlinux {
            packages.extend(archlinux.packages.iter().cloned());
        }
    }
    if let Some(uses) = &overlay.uses {
        for name in uses {
            let used = ctx.repository.get(name).expect("failed");
            if ctx.debug {
                println!("{:#?}", overlay);
            }
            packages = packages
                .union(&Box::pin(get_archlinux_packages(ctx, &used)).await)
                .cloned()
                .collect();
        }
    }
    packages
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(from = "AllAptForms")]
pub struct AptConfig {
    pub packages: Vec<String>,
}

impl From<AllAptForms> for AptConfig {
    fn from(f: AllAptForms) -> Self {
        match f {
            AllAptForms::Flat(packages) => Self { packages },
            AllAptForms::Full { packages } => Self { packages },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AllAptForms {
    Flat(Vec<String>),
    Full { packages: Vec<String> },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(from = "AllArchlinuxForms")]
pub struct ArchlinuxConfig {
    pub packages: Vec<String>,
}

impl From<AllArchlinuxForms> for ArchlinuxConfig {
    fn from(f: AllArchlinuxForms) -> Self {
        match f {
            AllArchlinuxForms::Flat(packages) => Self { packages },
            AllArchlinuxForms::Full { packages } => Self { packages },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AllArchlinuxForms {
    Flat(Vec<String>),
    Full { packages: Vec<String> },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(from = "AllBrewForms")]
pub struct BrewConfig {
    pub taps: Option<Vec<String>>,
    pub packages: Option<Vec<BrewPackage>>,
}

impl From<AllBrewForms> for BrewConfig {
    fn from(f: AllBrewForms) -> Self {
        match f {
            AllBrewForms::Flat(packages) => Self {
                taps: None,
                packages: Some(
                    packages
                        .into_iter()
                        .map(|name| BrewPackage {
                            name,
                            options: None,
                        })
                        .collect(),
                ),
            },
            AllBrewForms::Full { taps, packages } => Self { taps, packages },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AllBrewForms {
    Flat(Vec<String>),
    Full {
        taps: Option<Vec<String>>,
        packages: Option<Vec<BrewPackage>>,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq)]
#[serde(from = "AllBrewPackageForms")]
pub struct BrewPackage {
    pub name: String,
    pub options: Option<String>,
}

impl From<AllBrewPackageForms> for BrewPackage {
    fn from(f: AllBrewPackageForms) -> Self {
        match f {
            AllBrewPackageForms::Str(name) => Self {
                name,
                options: None,
            },
            AllBrewPackageForms::Full { name, options } => Self { name, options },
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum AllBrewPackageForms {
    Str(String),
    Full {
        name: String,
        options: Option<String>,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct InstallConfig {
    pub pre: Option<Vec<String>>,
    pub apt: Option<AptConfig>,
    pub archlinux: Option<ArchlinuxConfig>,
    pub brew: Option<BrewConfig>,
    pub post: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use config::{Config, File, FileFormat};
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::*;

    #[derive(Debug, Deserialize)]
    pub struct Data {
        pub install: Option<InstallConfig>,
    }

    #[rstest]
    #[case::basic_yaml(
        FileFormat::Yaml,
        r#"
install:
  pre:
    - echo "Hello, World!"
  archlinux:
    - pkg1
    - pkg2
  apt:
    - pkg1
    - pkg2
  brew:
    - pkg1
    - pkg2
  post:
    - echo "Goodbye, World!"
"#
    )]
    #[case::full_yaml(
        FileFormat::Yaml,
        r#"
install:
  pre:
    - echo "Hello, World!"
  archlinux:
    packages:
      - pkg1
      - pkg2
  apt:
    packages:
      - pkg1
      - pkg2
  brew:
    packages:
      - pkg1
      - pkg2
  post:
    - echo "Goodbye, World!"
"#
    )]
    #[case::basic_toml(
        FileFormat::Toml,
        r#"
[install]
pre = ['echo "Hello, World!"']
archlinux = ["pkg1", "pkg2"]
apt = ["pkg1", "pkg2"]
brew = ["pkg1", "pkg2"]
post = ['echo "Goodbye, World!"']
"#
    )]
    #[case::full_toml(
        FileFormat::Toml,
        r#"
[install]
pre = ['echo "Hello, World!"']
archlinux.packages = ["pkg1", "pkg2"]
apt.packages = ["pkg1", "pkg2"]
brew.packages = ["pkg1", "pkg2"]
post = ['echo "Goodbye, World!"']
"#
    )]
    fn test_config(#[case] format: FileFormat, #[case] content: &str) {
        let c = Config::builder()
            .add_source(File::from_str(content, format))
            .build()
            .unwrap();

        // Deserialize the entire file as single struct
        let data: Data = c.try_deserialize().unwrap();
        let install = data.install.unwrap();

        assert_eq!(install.pre.unwrap()[0], "echo \"Hello, World!\"");
        assert_eq!(install.archlinux.unwrap().packages, ["pkg1", "pkg2"]);
        assert_eq!(install.apt.unwrap().packages, ["pkg1", "pkg2"]);
        assert_eq!(
            install.brew.unwrap().packages.unwrap(),
            vec![
                BrewPackage {
                    name: String::from("pkg1"),
                    options: None,
                },
                BrewPackage {
                    name: String::from("pkg2"),
                    options: None,
                },
            ]
        );
        assert_eq!(install.post.unwrap()[0], "echo \"Goodbye, World!\"");
    }

    #[rstest]
    #[case::yaml(
        FileFormat::Yaml,
        r#"
install:
  brew:
    taps:
      - my/repo
    packages:
      - pkg1
      - name: pkg2
        options: '--cask'
"#
    )]
    #[case::toml(
        FileFormat::Toml,
        r#"
[install]
brew.taps = ["my/repo"]
brew.packages = ["pkg1", {name="pkg2", options="--cask"}]
"#
    )]
    fn test_brew_config(#[case] format: FileFormat, #[case] content: &str) {
        let c = Config::builder()
            .add_source(File::from_str(content, format))
            .build()
            .unwrap();

        // Deserialize the entire file as single struct
        let data: Data = c.try_deserialize().unwrap();
        let brew = data.install.unwrap().brew.unwrap();

        assert_eq!(brew.taps.unwrap(), vec!["my/repo"]);
        assert_eq!(
            brew.packages.unwrap(),
            vec![
                BrewPackage {
                    name: String::from("pkg1"),
                    options: None,
                },
                BrewPackage {
                    name: String::from("pkg2"),
                    options: Some(String::from("--cask")),
                },
            ]
        );
    }
}
