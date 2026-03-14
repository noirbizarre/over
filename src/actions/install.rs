use std::{collections::HashSet, env::consts::OS, fs};

use crate::{exec::Ctx, overlays::Overlay};
use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use which::which;

/// Serde helper: accept either a single string or a list of strings for
/// `Option<Vec<String>>` fields, normalising both to `Some(vec![…])`.
fn option_string_or_vec<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Vec(Vec<String>),
        Str(String),
    }
    Option::<StringOrVec>::deserialize(deserializer).map(|opt| match opt {
        Some(StringOrVec::Vec(v)) => Some(v),
        Some(StringOrVec::Str(s)) => Some(vec![s]),
        None => None,
    })
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum SystemManager {
    Archlinux,
    Apt,
    Brew,
}

// Precedence maps per platform/distro (system managers only)
const PRECEDENCE_ARCH: &[SystemManager] = &[SystemManager::Archlinux, SystemManager::Brew]; // arch helpers preferred, fallback to brew
const PRECEDENCE_DEBIAN: &[SystemManager] = &[SystemManager::Apt, SystemManager::Brew]; // apt before brew
const PRECEDENCE_GENERIC_LINUX: &[SystemManager] = &[
    SystemManager::Archlinux,
    SystemManager::Apt,
    SystemManager::Brew,
]; // attempt all known managers

fn detect_linux_distro() -> Option<String> {
    let content = fs::read_to_string("/etc/os-release").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("ID=") {
            let id = rest.trim_matches('"').to_string();
            return Some(id);
        }
    }
    None
}

async fn run_cmd(ctx: &Ctx, program: &str, args: &[&str]) -> Result<()> {
    use tokio::process::Command;
    if ctx.verbose || ctx.dry_run {
        println!("$ {} {}", program, args.join(" "));
    }
    if ctx.dry_run {
        return Ok(());
    }
    let status = Command::new(program).args(args).status().await?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "Command failed: {} {}",
            program,
            args.join(" ")
        ));
    }
    Ok(())
}

async fn run_scripts(ctx: &Ctx, scripts: &Vec<String>) -> Result<()> {
    for script in scripts {
        run_cmd(ctx, "sh", &["-c", script.as_str()]).await?;
    }
    Ok(())
}

async fn install_arch_pkgs(ctx: &Ctx, pkgs: HashSet<String>) -> Result<()> {
    if pkgs.is_empty() {
        return Ok(());
    }
    let helper = ["yay", "paru", "pacman"]
        .into_iter()
        .find(|b| which(b).is_ok());
    if let Some(bin) = helper {
        let mut args: Vec<String> = Vec::new();
        args.extend(["-S", "--needed"].map(|s| s.to_string()));
        args.extend(pkgs.iter().cloned());
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        if bin == "pacman" {
            let mut sudo_args = vec!["pacman"]; // sudo pacman -S --needed pkgs...
            sudo_args.extend(ref_args);
            run_cmd(ctx, "sudo", sudo_args.as_slice()).await?;
        } else {
            run_cmd(ctx, bin, ref_args.as_slice()).await?;
        }
    } else if ctx.verbose {
        println!("No arch package manager found for archlinux packages");
    }
    Ok(())
}

async fn install_apt_pkgs(ctx: &Ctx, pkgs: HashSet<String>) -> Result<()> {
    if pkgs.is_empty() {
        return Ok(());
    }
    run_cmd(ctx, "sudo", &["apt-get", "update"]).await?;
    let mut args: Vec<&str> = vec!["apt-get", "install", "-y"]; // apt-get install -y pkgs
    for pkg in pkgs.iter() {
        args.push(pkg.as_str());
    }
    run_cmd(ctx, "sudo", args.as_slice()).await?;
    Ok(())
}

async fn install_brew_pkgs(
    ctx: &Ctx,
    taps: HashSet<String>,
    pkgs: HashSet<BrewPackage>,
) -> Result<()> {
    if taps.is_empty() && pkgs.is_empty() {
        return Ok(());
    }
    if which("brew").is_err() {
        println!("brew not found");
        return Ok(());
    }
    for tap in taps.iter() {
        run_cmd(ctx, "brew", &["tap", tap.as_str()]).await?;
    }
    for pkg in pkgs.iter() {
        let mut args: Vec<&str> = vec!["install"]; // brew install
        // Dedicated cask flag (avoid duplication if already in options)
        if pkg.cask == Some(true) {
            args.push("--cask");
        }
        if let Some(opts) = &pkg.options {
            for part in opts.split_whitespace() {
                if part == "--cask" && pkg.cask == Some(true) {
                    continue; // already added
                }
                args.push(part);
            }
        }
        args.push(pkg.name.as_str());
        run_cmd(ctx, "brew", args.as_slice()).await?;
    }
    Ok(())
}

async fn install_cargo_crates(ctx: &Ctx, crates: HashSet<CargoPackage>) -> Result<()> {
    if crates.is_empty() {
        return Ok(());
    }
    if which("cargo").is_err() {
        if ctx.verbose {
            println!("cargo not found");
        }
        return Ok(());
    }
    for krate in crates.iter() {
        let mut args: Vec<String> = vec!["install".into()];
        if let Some(git) = &krate.git {
            args.push("--git".into());
            args.push(git.clone());
            if let Some(tag) = &krate.tag {
                args.push("--tag".into());
                args.push(tag.clone());
            }
            if let Some(branch) = &krate.branch {
                args.push("--branch".into());
                args.push(branch.clone());
            }
            if let Some(rev) = &krate.rev {
                args.push("--rev".into());
                args.push(rev.clone());
            }
            if let Some(name) = &krate.name {
                args.push(name.clone());
            }
        } else if let Some(path) = &krate.path {
            args.push("--path".into());
            args.push(path.clone());
        } else if let Some(name) = &krate.name {
            args.push(name.clone());
            if let Some(version) = &krate.version {
                args.push("--version".into());
                args.push(version.clone());
            }
        }
        if let Some(features) = &krate.features
            && !features.is_empty()
        {
            args.push("--features".into());
            args.push(features.join(","));
        }
        if krate.locked == Some(true) {
            args.push("--locked".into());
        }
        if let Some(opts) = &krate.options {
            for part in opts.split_whitespace() {
                args.push(part.into());
            }
        }
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_cmd(ctx, "cargo", ref_args.as_slice()).await?;
    }
    Ok(())
}

async fn install_python_packages(ctx: &Ctx, packages: HashSet<PythonPackage>) -> Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    // Tool detection priority: uv > pipx > pip
    let uv_available = which("uv").is_ok();
    let pipx_available = which("pipx").is_ok();
    let pip_available = which("pip").is_ok();
    for pkg in packages.iter() {
        // Determine effective tool
        let chosen = if let Some(explicit) = pkg.tool {
            // Copy so moved is fine
            match explicit {
                PythonTool::Uv if uv_available => Some("uv"),
                PythonTool::Pipx if pipx_available => Some("pipx"),
                PythonTool::Pip if pip_available => Some("pip"),
                _ => None,
            }
        } else if uv_available {
            Some("uv")
        } else if pipx_available {
            Some("pipx")
        } else if pip_available {
            Some("pip")
        } else {
            None
        };
        let Some(tool) = chosen else {
            continue;
        };
        let mut args: Vec<String> = Vec::new();
        match tool {
            "uv" => {
                // Use pip subcommand for global installs to match user's environment
                args.extend(["pip", "install"].map(|s| s.to_string()));
            }
            "pipx" => args.push("install".into()),
            "pip" => args.push("install".into()),
            _ => {}
        }
        // Build spec with extras if any
        let mut spec = pkg.name.clone();
        if let Some(extras) = &pkg.extras
            && !extras.is_empty()
        {
            spec.push('[');
            spec.push_str(&extras.join(","));
            spec.push(']');
        }
        args.push(spec);
        if let Some(opts) = &pkg.options {
            for part in opts.split_whitespace() {
                args.push(part.into());
            }
        }
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_cmd(ctx, tool, ref_args.as_slice()).await?;
    }
    Ok(())
}

async fn install_node_packages(ctx: &Ctx, packages: HashSet<NodePackage>) -> Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    if which("npm").is_err() {
        if ctx.verbose {
            println!("npm not found");
        }
        return Ok(());
    }
    for pkg in packages.iter() {
        let mut args: Vec<&str> = vec!["install", "-g"]; // npm install -g
        if let Some(opts) = &pkg.options {
            for part in opts.split_whitespace() {
                args.push(part);
            }
        }
        args.push(pkg.name.as_str());
        run_cmd(ctx, "npm", args.as_slice()).await?;
    }
    Ok(())
}

pub async fn install(ctx: &Ctx, overlay: &Overlay) -> Result<()> {
    match OS {
        "linux" => install_linux(ctx, overlay).await?,
        "macos" => install_macos(ctx, overlay).await?,
        "windows" => install_windows(ctx, overlay).await?,
        _ => println!("Unsupported OS: {}", OS),
    }
    Ok(())
}

async fn install_windows(_ctx: &Ctx, _overlay: &Overlay) -> Result<()> {
    // Windows support pending; placeholder respects interface
    println!("Windows install not yet implemented");
    Ok(())
}

// Decide which system managers will run on Linux given distro & config
fn decide_linux_managers(distro: &str, install_cfg: &InstallConfig) -> Vec<SystemManager> {
    let precedence: &[SystemManager] = match distro {
        "arch" | "archlinux" => PRECEDENCE_ARCH,
        "ubuntu" | "debian" => PRECEDENCE_DEBIAN,
        _ => PRECEDENCE_GENERIC_LINUX,
    };
    let platform_cfg = install_cfg.platforms.get(distro);
    let has_top = |mgr: &SystemManager| match mgr {
        SystemManager::Archlinux => install_cfg.archlinux.is_some(),
        SystemManager::Apt => install_cfg.apt.is_some(),
        SystemManager::Brew => install_cfg.brew.is_some(),
    };
    let has_platform = |mgr: &SystemManager| {
        platform_cfg
            .map(|p| match mgr {
                SystemManager::Archlinux => p.archlinux.is_some(),
                SystemManager::Apt => p.apt.is_some(),
                SystemManager::Brew => p.brew.is_some(),
            })
            .unwrap_or(false)
    };
    if platform_cfg.is_some() {
        let mut managers = Vec::new();
        for m in precedence {
            if has_platform(m) {
                managers.push(*m);
            }
        }
        managers
    } else {
        for m in precedence {
            if has_top(m) {
                return vec![*m];
            }
        }
        Vec::new()
    }
}

async fn install_linux(ctx: &Ctx, overlay: &Overlay) -> Result<()> {
    let distro = detect_linux_distro().unwrap_or_else(|| "linux".to_string());
    let install_cfg = overlay.install.as_ref();
    if install_cfg.is_none() {
        return Ok(());
    }
    let install_cfg = install_cfg.unwrap();
    let platform_cfg = install_cfg.platforms.get(&distro);

    if let Some(pre) = &install_cfg.pre {
        run_scripts(ctx, pre).await?;
    }
    if let Some(pre) = platform_cfg.and_then(|p| p.pre.as_ref()) {
        run_scripts(ctx, pre).await?;
    }

    // System managers first
    for mgr in decide_linux_managers(distro.as_str(), install_cfg) {
        match mgr {
            SystemManager::Archlinux => {
                if let Some(cfg) = platform_cfg
                    .and_then(|p| p.archlinux.as_ref())
                    .or(install_cfg.archlinux.as_ref())
                    && let Some(pre) = &cfg.pre
                {
                    run_scripts(ctx, pre).await?;
                }
                let pkgs = get_archlinux_packages(ctx, overlay).await;
                if !pkgs.is_empty() {
                    install_arch_pkgs(ctx, pkgs).await?;
                }
                if let Some(cfg) = platform_cfg
                    .and_then(|p| p.archlinux.as_ref())
                    .or(install_cfg.archlinux.as_ref())
                    && let Some(post) = &cfg.post
                {
                    run_scripts(ctx, post).await?;
                }
            }
            SystemManager::Apt => {
                if let Some(cfg) = platform_cfg
                    .and_then(|p| p.apt.as_ref())
                    .or(install_cfg.apt.as_ref())
                    && let Some(pre) = &cfg.pre
                {
                    run_scripts(ctx, pre).await?;
                }
                let pkgs = get_apt_packages(ctx, overlay).await;
                if !pkgs.is_empty() {
                    install_apt_pkgs(ctx, pkgs).await?;
                }
                if let Some(cfg) = platform_cfg
                    .and_then(|p| p.apt.as_ref())
                    .or(install_cfg.apt.as_ref())
                    && let Some(post) = &cfg.post
                {
                    run_scripts(ctx, post).await?;
                }
            }
            SystemManager::Brew => {
                if let Some(cfg) = platform_cfg
                    .and_then(|p| p.brew.as_ref())
                    .or(install_cfg.brew.as_ref())
                    && let Some(pre) = &cfg.pre
                {
                    run_scripts(ctx, pre).await?;
                }
                let (taps, pkgs) = get_brew_packages(ctx, overlay).await;
                if !taps.is_empty() || !pkgs.is_empty() {
                    install_brew_pkgs(ctx, taps, pkgs).await?;
                }
                if let Some(cfg) = platform_cfg
                    .and_then(|p| p.brew.as_ref())
                    .or(install_cfg.brew.as_ref())
                    && let Some(post) = &cfg.post
                {
                    run_scripts(ctx, post).await?;
                }
            }
        }
    }

    // Language managers after system
    // cargo
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.cargo.as_ref())
        .or(install_cfg.cargo.as_ref())
    {
        if let Some(pre) = &cfg.pre {
            run_scripts(ctx, pre).await?;
        }
        let crates = get_cargo_packages(ctx, overlay).await;
        if !crates.is_empty() {
            install_cargo_crates(ctx, crates).await?;
        }
        if let Some(post) = &cfg.post {
            run_scripts(ctx, post).await?;
        }
    }
    // python
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.python.as_ref())
        .or(install_cfg.python.as_ref())
    {
        if let Some(pre) = &cfg.pre {
            run_scripts(ctx, pre).await?;
        }
        let packages = get_python_packages(ctx, overlay).await;
        if !packages.is_empty() {
            install_python_packages(ctx, packages).await?;
        }
        if let Some(post) = &cfg.post {
            run_scripts(ctx, post).await?;
        }
    }
    // node
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.node.as_ref())
        .or(install_cfg.node.as_ref())
    {
        if let Some(pre) = &cfg.pre {
            run_scripts(ctx, pre).await?;
        }
        let packages = get_node_packages(ctx, overlay).await;
        if !packages.is_empty() {
            install_node_packages(ctx, packages).await?;
        }
        if let Some(post) = &cfg.post {
            run_scripts(ctx, post).await?;
        }
    }

    if let Some(post) = platform_cfg.and_then(|p| p.post.as_ref()) {
        run_scripts(ctx, post).await?;
    }
    if let Some(post) = &install_cfg.post {
        run_scripts(ctx, post).await?;
    }

    Ok(())
}

async fn install_macos(ctx: &Ctx, overlay: &Overlay) -> Result<()> {
    let install_cfg = overlay.install.as_ref();
    if install_cfg.is_none() {
        return Ok(());
    }
    let install_cfg = install_cfg.unwrap();
    let platform_cfg = install_cfg.platforms.get("macos");

    if let Some(pre) = &install_cfg.pre {
        run_scripts(ctx, pre).await?;
    }
    if let Some(pre) = platform_cfg.and_then(|p| p.pre.as_ref()) {
        run_scripts(ctx, pre).await?;
    }

    // brew (system) then languages
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.brew.as_ref())
        .or(install_cfg.brew.as_ref())
        && let Some(pre) = &cfg.pre
    {
        run_scripts(ctx, pre).await?;
    }
    let (taps, pkgs) = get_brew_packages(ctx, overlay).await;
    if !taps.is_empty() || !pkgs.is_empty() {
        install_brew_pkgs(ctx, taps, pkgs).await?;
    }
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.brew.as_ref())
        .or(install_cfg.brew.as_ref())
        && let Some(post) = &cfg.post
    {
        run_scripts(ctx, post).await?;
    }

    // cargo
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.cargo.as_ref())
        .or(install_cfg.cargo.as_ref())
    {
        if let Some(pre) = &cfg.pre {
            run_scripts(ctx, pre).await?;
        }
        let crates = get_cargo_packages(ctx, overlay).await;
        if !crates.is_empty() {
            install_cargo_crates(ctx, crates).await?;
        }
        if let Some(post) = &cfg.post {
            run_scripts(ctx, post).await?;
        }
    }
    // python
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.python.as_ref())
        .or(install_cfg.python.as_ref())
    {
        if let Some(pre) = &cfg.pre {
            run_scripts(ctx, pre).await?;
        }
        let packages = get_python_packages(ctx, overlay).await;
        if !packages.is_empty() {
            install_python_packages(ctx, packages).await?;
        }
        if let Some(post) = &cfg.post {
            run_scripts(ctx, post).await?;
        }
    }
    // node
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.node.as_ref())
        .or(install_cfg.node.as_ref())
    {
        if let Some(pre) = &cfg.pre {
            run_scripts(ctx, pre).await?;
        }
        let packages = get_node_packages(ctx, overlay).await;
        if !packages.is_empty() {
            install_node_packages(ctx, packages).await?;
        }
        if let Some(post) = &cfg.post {
            run_scripts(ctx, post).await?;
        }
    }

    if let Some(post) = platform_cfg.and_then(|p| p.post.as_ref()) {
        run_scripts(ctx, post).await?;
    }
    if let Some(post) = &install_cfg.post {
        run_scripts(ctx, post).await?;
    }

    Ok(())
}

async fn get_archlinux_packages(ctx: &Ctx, overlay: &Overlay) -> HashSet<String> {
    let mut packages: HashSet<String> = HashSet::new();
    if let Some(install) = &overlay.install {
        let platform_override = if OS == "linux" {
            if let Some(distro) = detect_linux_distro() {
                let key = if install.platforms.contains_key("arch") {
                    Some("arch")
                } else if install.platforms.contains_key("archlinux") {
                    Some("archlinux")
                } else if install.platforms.contains_key(&distro) {
                    Some(distro.as_str())
                } else {
                    None
                };
                key.and_then(|k| install.platforms.get(k))
            } else {
                None
            }
        } else {
            None
        };
        if let Some(platform) = platform_override {
            if let Some(archlinux) = &platform.archlinux {
                packages.extend(archlinux.packages.iter().cloned());
            }
        } else if let Some(archlinux) = &install.archlinux {
            packages.extend(archlinux.packages.iter().cloned());
        }
    }
    if let Some(uses) = &overlay.uses {
        for name in uses {
            let used = ctx.repository.get(name).expect("failed");
            packages = packages
                .union(&Box::pin(get_archlinux_packages(ctx, &used)).await)
                .cloned()
                .collect();
        }
    }
    packages
}

async fn get_apt_packages(ctx: &Ctx, overlay: &Overlay) -> HashSet<String> {
    let mut packages: HashSet<String> = HashSet::new();
    if let Some(install) = &overlay.install {
        let platform_override = if OS == "linux" {
            if let Some(distro) = detect_linux_distro() {
                install.platforms.get(&distro)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(platform) = platform_override {
            if let Some(apt) = &platform.apt {
                packages.extend(apt.packages.iter().cloned());
            }
        } else if let Some(apt) = &install.apt {
            packages.extend(apt.packages.iter().cloned());
        }
    }
    if let Some(uses) = &overlay.uses {
        for name in uses {
            let used = ctx.repository.get(name).expect("failed");
            packages = packages
                .union(&Box::pin(get_apt_packages(ctx, &used)).await)
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
    pub pre: Option<Vec<String>>,
    pub post: Option<Vec<String>>,
}

impl From<AllAptForms> for AptConfig {
    fn from(f: AllAptForms) -> Self {
        match f {
            AllAptForms::Flat(packages) => Self {
                packages,
                pre: None,
                post: None,
            },
            AllAptForms::Full {
                packages,
                pre,
                post,
            } => Self {
                packages,
                pre,
                post,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AllAptForms {
    Flat(Vec<String>),
    Full {
        packages: Vec<String>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        pre: Option<Vec<String>>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        post: Option<Vec<String>>,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(from = "AllArchlinuxForms")]
pub struct ArchlinuxConfig {
    pub packages: Vec<String>,
    pub pre: Option<Vec<String>>,
    pub post: Option<Vec<String>>,
}

impl From<AllArchlinuxForms> for ArchlinuxConfig {
    fn from(f: AllArchlinuxForms) -> Self {
        match f {
            AllArchlinuxForms::Flat(packages) => Self {
                packages,
                pre: None,
                post: None,
            },
            AllArchlinuxForms::Full {
                packages,
                pre,
                post,
            } => Self {
                packages,
                pre,
                post,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AllArchlinuxForms {
    Flat(Vec<String>),
    Full {
        packages: Vec<String>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        pre: Option<Vec<String>>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        post: Option<Vec<String>>,
    },
}

async fn get_brew_packages(
    ctx: &Ctx,
    overlay: &Overlay,
) -> (HashSet<String>, HashSet<BrewPackage>) {
    let mut taps: HashSet<String> = HashSet::new();
    let mut packages: HashSet<BrewPackage> = HashSet::new();
    if let Some(install) = &overlay.install {
        let platform_override = if OS == "macos" {
            install.platforms.get("macos")
        } else if OS == "linux" {
            if let Some(distro) = detect_linux_distro() {
                install.platforms.get(&distro)
            } else {
                None
            }
        } else {
            None
        };
        let source = platform_override
            .and_then(|p| p.brew.as_ref())
            .or(install.brew.as_ref());
        if let Some(brew) = source {
            if let Some(brew_taps) = &brew.taps {
                taps.extend(brew_taps.iter().cloned());
            }
            if let Some(brew_pkgs) = &brew.packages {
                packages.extend(brew_pkgs.iter().cloned());
            }
        }
    }
    if let Some(uses) = &overlay.uses {
        for name in uses {
            let used = ctx.repository.get(name).expect("failed");
            let (used_taps, used_pkgs) = Box::pin(get_brew_packages(ctx, &used)).await;
            taps = taps.union(&used_taps).cloned().collect();
            packages = packages.union(&used_pkgs).cloned().collect();
        }
    }
    (taps, packages)
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(from = "AllBrewForms")]
pub struct BrewConfig {
    pub taps: Option<Vec<String>>,
    pub packages: Option<Vec<BrewPackage>>,
    pub pre: Option<Vec<String>>,
    pub post: Option<Vec<String>>,
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
                            cask: None,
                        })
                        .collect(),
                ),
                pre: None,
                post: None,
            },
            AllBrewForms::Full {
                taps,
                packages,
                pre,
                post,
            } => Self {
                taps,
                packages,
                pre,
                post,
            },
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
        #[serde(default, deserialize_with = "option_string_or_vec")]
        pre: Option<Vec<String>>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        post: Option<Vec<String>>,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq, Hash)]
#[serde(from = "AllBrewPackageForms")]
pub struct BrewPackage {
    pub name: String,
    pub options: Option<String>,
    pub cask: Option<bool>,
}

impl From<AllBrewPackageForms> for BrewPackage {
    fn from(f: AllBrewPackageForms) -> Self {
        match f {
            AllBrewPackageForms::Str(name) => Self {
                name,
                options: None,
                cask: None,
            },
            AllBrewPackageForms::Full {
                name,
                options,
                cask,
            } => Self {
                name,
                options,
                cask,
            },
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
        cask: Option<bool>,
    },
}

// Cargo
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(from = "AllCargoForms")]
pub struct CargoConfig {
    pub packages: Vec<CargoPackage>,
    pub pre: Option<Vec<String>>,
    pub post: Option<Vec<String>>,
}

impl From<AllCargoForms> for CargoConfig {
    fn from(f: AllCargoForms) -> Self {
        match f {
            AllCargoForms::Flat(packages) => Self {
                packages: packages
                    .into_iter()
                    .map(|name| CargoPackage {
                        name: Some(name),
                        version: None,
                        git: None,
                        tag: None,
                        branch: None,
                        rev: None,
                        path: None,
                        features: None,
                        locked: None,
                        options: None,
                    })
                    .collect(),
                pre: None,
                post: None,
            },
            AllCargoForms::Full {
                packages,
                pre,
                post,
            } => Self {
                packages,
                pre,
                post,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AllCargoForms {
    Flat(Vec<String>),
    Full {
        packages: Vec<CargoPackage>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        pre: Option<Vec<String>>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        post: Option<Vec<String>>,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq, Hash)]
#[serde(from = "AllCargoPackageForms")]
pub struct CargoPackage {
    pub name: Option<String>,
    pub version: Option<String>,
    pub git: Option<String>,
    pub tag: Option<String>,
    pub branch: Option<String>,
    pub rev: Option<String>,
    pub path: Option<String>,
    pub features: Option<Vec<String>>,
    pub locked: Option<bool>,
    pub options: Option<String>,
}

impl From<AllCargoPackageForms> for CargoPackage {
    fn from(f: AllCargoPackageForms) -> Self {
        match f {
            AllCargoPackageForms::Str(name) => Self {
                name: Some(name),
                version: None,
                git: None,
                tag: None,
                branch: None,
                rev: None,
                path: None,
                features: None,
                locked: None,
                options: None,
            },
            AllCargoPackageForms::Full {
                name,
                version,
                git,
                tag,
                branch,
                rev,
                path,
                features,
                locked,
                options,
            } => Self {
                name,
                version,
                git,
                tag,
                branch,
                rev,
                path,
                features,
                locked,
                options,
            },
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum AllCargoPackageForms {
    Str(String),
    Full {
        name: Option<String>,
        version: Option<String>,
        git: Option<String>,
        tag: Option<String>,
        branch: Option<String>,
        rev: Option<String>,
        path: Option<String>,
        features: Option<Vec<String>>,
        locked: Option<bool>,
        options: Option<String>,
    },
}

async fn get_cargo_packages(ctx: &Ctx, overlay: &Overlay) -> HashSet<CargoPackage> {
    let mut packages: HashSet<CargoPackage> = HashSet::new();
    if let Some(install) = &overlay.install {
        let platform_override = if OS == "macos" {
            install.platforms.get("macos")
        } else if OS == "linux" {
            if let Some(distro) = detect_linux_distro() {
                install.platforms.get(&distro)
            } else {
                None
            }
        } else {
            None
        };
        let source = platform_override
            .and_then(|p| p.cargo.as_ref())
            .or(install.cargo.as_ref());
        if let Some(cfg) = source {
            packages.extend(cfg.packages.iter().cloned());
        }
    }
    if let Some(uses) = &overlay.uses {
        for name in uses {
            let used = ctx.repository.get(name).expect("failed");
            let used_pkgs = Box::pin(get_cargo_packages(ctx, &used)).await;
            packages = packages.union(&used_pkgs).cloned().collect();
        }
    }
    packages
}

// Python
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(from = "AllPythonForms")]
pub struct PythonConfig {
    pub packages: Vec<PythonPackage>,
    pub pre: Option<Vec<String>>,
    pub post: Option<Vec<String>>,
}

impl From<AllPythonForms> for PythonConfig {
    fn from(f: AllPythonForms) -> Self {
        match f {
            AllPythonForms::Flat(packages) => Self {
                packages: packages
                    .into_iter()
                    .map(|name| PythonPackage {
                        name,
                        tool: None,
                        extras: None,
                        options: None,
                    })
                    .collect(),
                pre: None,
                post: None,
            },
            AllPythonForms::Full {
                packages,
                pre,
                post,
            } => Self {
                packages,
                pre,
                post,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AllPythonForms {
    Flat(Vec<String>),
    Full {
        packages: Vec<PythonPackage>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        pre: Option<Vec<String>>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        post: Option<Vec<String>>,
    },
}

#[derive(Debug, Deserialize, Serialize, Copy, Clone, Eq, PartialEq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PythonTool {
    Uv,
    Pipx,
    Pip,
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq, Hash)]
#[serde(from = "AllPythonPackageForms")]
pub struct PythonPackage {
    pub name: String,
    pub tool: Option<PythonTool>, // uv|pipx|pip|auto(default)
    pub extras: Option<Vec<String>>,
    pub options: Option<String>,
}

impl From<AllPythonPackageForms> for PythonPackage {
    fn from(f: AllPythonPackageForms) -> Self {
        match f {
            AllPythonPackageForms::Str(name) => Self {
                name,
                tool: None,
                extras: None,
                options: None,
            },
            AllPythonPackageForms::Full {
                name,
                tool,
                extras,
                options,
            } => Self {
                name,
                tool,
                extras,
                options,
            },
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum AllPythonPackageForms {
    Str(String),
    Full {
        name: String,
        tool: Option<PythonTool>,
        extras: Option<Vec<String>>,
        options: Option<String>,
    },
}

async fn get_python_packages(ctx: &Ctx, overlay: &Overlay) -> HashSet<PythonPackage> {
    let mut packages: HashSet<PythonPackage> = HashSet::new();
    if let Some(install) = &overlay.install {
        let platform_override = if OS == "macos" {
            install.platforms.get("macos")
        } else if OS == "linux" {
            if let Some(distro) = detect_linux_distro() {
                install.platforms.get(&distro)
            } else {
                None
            }
        } else {
            None
        };
        let source = platform_override
            .and_then(|p| p.python.as_ref())
            .or(install.python.as_ref());
        if let Some(cfg) = source {
            packages.extend(cfg.packages.iter().cloned());
        }
    }
    if let Some(uses) = &overlay.uses {
        for name in uses {
            let used = ctx.repository.get(name).expect("failed");
            let used_pkgs = Box::pin(get_python_packages(ctx, &used)).await;
            packages = packages.union(&used_pkgs).cloned().collect();
        }
    }
    packages
}

// Node (npm)
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(from = "AllNodeForms")]
pub struct NodeConfig {
    pub packages: Vec<NodePackage>,
    pub pre: Option<Vec<String>>,
    pub post: Option<Vec<String>>,
}

impl From<AllNodeForms> for NodeConfig {
    fn from(f: AllNodeForms) -> Self {
        match f {
            AllNodeForms::Flat(packages) => Self {
                packages: packages
                    .into_iter()
                    .map(|name| NodePackage {
                        name,
                        options: None,
                    })
                    .collect(),
                pre: None,
                post: None,
            },
            AllNodeForms::Full {
                packages,
                pre,
                post,
            } => Self {
                packages,
                pre,
                post,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AllNodeForms {
    Flat(Vec<String>),
    Full {
        packages: Vec<NodePackage>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        pre: Option<Vec<String>>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        post: Option<Vec<String>>,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq, Hash)]
#[serde(from = "AllNodePackageForms")]
pub struct NodePackage {
    pub name: String,
    pub options: Option<String>,
}

impl From<AllNodePackageForms> for NodePackage {
    fn from(f: AllNodePackageForms) -> Self {
        match f {
            AllNodePackageForms::Str(name) => Self {
                name,
                options: None,
            },
            AllNodePackageForms::Full { name, options } => Self { name, options },
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum AllNodePackageForms {
    Str(String),
    Full {
        name: String,
        options: Option<String>,
    },
}

async fn get_node_packages(ctx: &Ctx, overlay: &Overlay) -> HashSet<NodePackage> {
    let mut packages: HashSet<NodePackage> = HashSet::new();
    if let Some(install) = &overlay.install {
        let platform_override = if OS == "macos" {
            install.platforms.get("macos")
        } else if OS == "linux" {
            if let Some(distro) = detect_linux_distro() {
                install.platforms.get(&distro)
            } else {
                None
            }
        } else {
            None
        };
        let source = platform_override
            .and_then(|p| p.node.as_ref())
            .or(install.node.as_ref());
        if let Some(cfg) = source {
            packages.extend(cfg.packages.iter().cloned());
        }
    }
    if let Some(uses) = &overlay.uses {
        for name in uses {
            let used = ctx.repository.get(name).expect("failed");
            let used_pkgs = Box::pin(get_node_packages(ctx, &used)).await;
            packages = packages.union(&used_pkgs).cloned().collect();
        }
    }
    packages
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PlatformInstallConfig {
    #[serde(default, deserialize_with = "option_string_or_vec")]
    pub pre: Option<Vec<String>>,
    pub apt: Option<AptConfig>,
    pub archlinux: Option<ArchlinuxConfig>,
    pub brew: Option<BrewConfig>,
    pub cargo: Option<CargoConfig>,
    pub python: Option<PythonConfig>,
    pub node: Option<NodeConfig>,
    #[serde(default, deserialize_with = "option_string_or_vec")]
    pub post: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct InstallConfig {
    #[serde(default, deserialize_with = "option_string_or_vec")]
    pub pre: Option<Vec<String>>,
    pub apt: Option<AptConfig>,
    pub archlinux: Option<ArchlinuxConfig>,
    pub brew: Option<BrewConfig>,
    pub cargo: Option<CargoConfig>,
    pub python: Option<PythonConfig>,
    pub node: Option<NodeConfig>,
    #[serde(default, deserialize_with = "option_string_or_vec")]
    pub post: Option<Vec<String>>,
    #[serde(flatten)]
    pub platforms: std::collections::HashMap<String, PlatformInstallConfig>,
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
  cargo:
    - ripgrep
  python:
    - requests
  node:
    - typescript
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
  cargo:
    packages:
      - name: ripgrep
        locked: true
  python:
    packages:
      - name: requests
        tool: uv
  node:
    packages:
      - name: typescript
        options: "--force"
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
cargo = ["ripgrep"]
python = ["requests"]
node = ["typescript"]
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
cargo.packages = [{name="ripgrep", locked=true}]
python.packages = [{name="requests", tool="uv"}]
node.packages = [{name="typescript", options="--force"}]
post = ['echo "Goodbye, World!"']
"#
    )]
    #[case::scalar_yaml(
        FileFormat::Yaml,
        r#"
install:
  pre: echo "Hello, World!"
  archlinux:
    - pkg1
    - pkg2
  apt:
    - pkg1
    - pkg2
  brew:
    - pkg1
    - pkg2
  cargo:
    - ripgrep
  python:
    - requests
  node:
    - typescript
  post: echo "Goodbye, World!"
"#
    )]
    #[case::scalar_toml(
        FileFormat::Toml,
        r#"
[install]
pre = 'echo "Hello, World!"'
archlinux = ["pkg1", "pkg2"]
apt = ["pkg1", "pkg2"]
brew = ["pkg1", "pkg2"]
cargo = ["ripgrep"]
python = ["requests"]
node = ["typescript"]
post = 'echo "Goodbye, World!"'
"#
    )]
    fn test_config(#[case] format: FileFormat, #[case] content: &str) {
        let c = Config::builder()
            .add_source(File::from_str(content, format))
            .build()
            .unwrap();
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
                    cask: None
                },
                BrewPackage {
                    name: String::from("pkg2"),
                    options: None,
                    cask: None
                },
            ]
        );
        assert_eq!(
            install.cargo.unwrap().packages[0].name,
            Some("ripgrep".into())
        );
        assert_eq!(install.python.unwrap().packages[0].name, "requests");
        assert_eq!(install.node.unwrap().packages[0].name, "typescript");
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
        cask: true
"#
    )]
    #[case::toml(
        FileFormat::Toml,
        r#"
[install]
brew.taps = ["my/repo"]
brew.packages = ["pkg1", {name="pkg2", options="--cask", cask=true}]
"#
    )]
    fn test_brew_config(#[case] format: FileFormat, #[case] content: &str) {
        let c = Config::builder()
            .add_source(File::from_str(content, format))
            .build()
            .unwrap();
        let data: Data = c.try_deserialize().unwrap();
        let brew = data.install.unwrap().brew.unwrap();
        assert_eq!(brew.taps.unwrap(), vec!["my/repo"]);
        assert_eq!(
            brew.packages.unwrap(),
            vec![
                BrewPackage {
                    name: String::from("pkg1"),
                    options: None,
                    cask: None
                },
                BrewPackage {
                    name: String::from("pkg2"),
                    options: Some(String::from("--cask")),
                    cask: Some(true)
                },
            ]
        );
    }

    #[rstest]
    #[case::yaml(
        FileFormat::Yaml,
        r#"
install:
  apt:
    packages:
      - pkg1
    pre: echo "before apt"
    post: echo "after apt"
"#
    )]
    #[case::toml(
        FileFormat::Toml,
        r#"
[install.apt]
packages = ["pkg1"]
pre = 'echo "before apt"'
post = 'echo "after apt"'
"#
    )]
    fn test_manager_scalar_scripts(#[case] format: FileFormat, #[case] content: &str) {
        let c = Config::builder()
            .add_source(File::from_str(content, format))
            .build()
            .unwrap();
        let data: Data = c.try_deserialize().unwrap();
        let apt = data.install.unwrap().apt.unwrap();
        assert_eq!(apt.pre.unwrap(), vec!["echo \"before apt\""]);
        assert_eq!(apt.post.unwrap(), vec!["echo \"after apt\""]);
    }

    #[test]
    fn test_linux_precedence_top_level() {
        let install = InstallConfig {
            pre: None,
            apt: Some(AptConfig {
                packages: vec!["a1".into()],
                pre: None,
                post: None,
            }),
            archlinux: Some(ArchlinuxConfig {
                packages: vec!["x1".into()],
                pre: None,
                post: None,
            }),
            brew: Some(BrewConfig {
                taps: None,
                packages: Some(vec![]),
                pre: None,
                post: None,
            }),
            cargo: None,
            python: None,
            node: None,
            post: None,
            platforms: Default::default(),
        };
        // simulate ubuntu distro: expect apt only
        let managers = decide_linux_managers("ubuntu", &install);
        assert_eq!(managers, vec![SystemManager::Apt]);
        let managers_arch = decide_linux_managers("archlinux", &install);
        assert_eq!(managers_arch, vec![SystemManager::Archlinux]);
    }

    #[test]
    fn test_linux_composition_platform_section() {
        let mut platforms = std::collections::HashMap::new();
        platforms.insert(
            "ubuntu".into(),
            PlatformInstallConfig {
                pre: None,
                apt: Some(AptConfig {
                    packages: vec!["a1".into()],
                    pre: None,
                    post: None,
                }),
                archlinux: None,
                brew: Some(BrewConfig {
                    taps: None,
                    packages: Some(vec![]),
                    pre: None,
                    post: None,
                }),
                cargo: None,
                python: None,
                node: None,
                post: None,
            },
        );
        let install = InstallConfig {
            pre: None,
            apt: None,
            archlinux: None,
            brew: None,
            cargo: None,
            python: None,
            node: None,
            post: None,
            platforms,
        };
        let managers = decide_linux_managers("ubuntu", &install);
        assert_eq!(managers, vec![SystemManager::Apt, SystemManager::Brew]);
    }
}
