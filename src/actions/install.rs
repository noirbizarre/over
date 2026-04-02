use std::collections::{BTreeSet, HashSet};
use std::{env::consts::OS, fs, sync::OnceLock};

use crate::{exec::Ctx, overlays::Overlay};
use anyhow::{Context as AnyhowContext, Result};
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
    Winget,
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
    static DISTRO: OnceLock<Option<String>> = OnceLock::new();
    DISTRO
        .get_or_init(|| {
            let content = fs::read_to_string("/etc/os-release").ok()?;
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("ID=") {
                    let id = rest.trim_matches('"').to_string();
                    return Some(id);
                }
            }
            None
        })
        .clone()
}

/// Resolve the platform-specific config override for the current OS.
fn resolve_platform_override(install: &InstallConfig) -> Option<&PlatformInstallConfig> {
    if OS == "macos" {
        install.platforms.get("macos")
    } else if OS == "linux" {
        detect_linux_distro().and_then(|distro| install.platforms.get(&distro))
    } else if OS == "windows" {
        install.platforms.get("windows")
    } else {
        None
    }
}

/// Generic recursive package collection across overlays and their `uses` deps.
///
/// `extract` receives the install config and optional platform override, and returns
/// the packages for a single overlay (without recursion into `uses`).
async fn collect_packages<T, F>(
    ctx: &Ctx,
    overlay: &Overlay,
    visited: &mut HashSet<String>,
    extract: F,
) -> Result<BTreeSet<T>>
where
    T: Clone + Ord,
    F: Fn(&InstallConfig, Option<&PlatformInstallConfig>) -> BTreeSet<T> + Copy,
{
    if !visited.insert(overlay.name.clone()) {
        return Ok(BTreeSet::new());
    }
    let mut packages = overlay
        .install
        .as_ref()
        .map(|install| {
            let platform = resolve_platform_override(install);
            extract(install, platform)
        })
        .unwrap_or_default();
    if let Some(uses) = &overlay.uses {
        for name in uses {
            let used = ctx
                .repository
                .get(name)
                .with_context(|| format!("used overlay '{name}' not found"))?;
            let used_pkgs = Box::pin(collect_packages(ctx, &used, visited, extract)).await?;
            packages = packages.union(&used_pkgs).cloned().collect();
        }
    }
    Ok(packages)
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

/// Install cross-platform language managers (cargo, python, node).
///
/// This block is identical across all three platform orchestrators, so it
/// is extracted here to avoid repetition.
async fn install_language_managers(
    ctx: &Ctx,
    overlay: &Overlay,
    install_cfg: &InstallConfig,
    platform_cfg: Option<&PlatformInstallConfig>,
) -> Result<()> {
    // cargo
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.cargo.as_ref())
        .or(install_cfg.cargo.as_ref())
    {
        if let Some(pre) = &cfg.pre {
            run_scripts(ctx, pre).await?;
        }
        let crates = get_cargo_packages(ctx, overlay, &mut HashSet::new()).await?;
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
        let packages = get_python_packages(ctx, overlay, &mut HashSet::new()).await?;
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
        let packages = get_node_packages(ctx, overlay, &mut HashSet::new()).await?;
        if !packages.is_empty() {
            install_node_packages(ctx, packages).await?;
        }
        if let Some(post) = &cfg.post {
            run_scripts(ctx, post).await?;
        }
    }
    Ok(())
}

async fn install_arch_pkgs(ctx: &Ctx, pkgs: BTreeSet<String>) -> Result<()> {
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
        eprintln!("No arch package manager found for archlinux packages");
    }
    Ok(())
}

async fn install_apt_pkgs(ctx: &Ctx, pkgs: BTreeSet<String>) -> Result<()> {
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
    taps: BTreeSet<String>,
    pkgs: BTreeSet<BrewPackage>,
) -> Result<()> {
    if taps.is_empty() && pkgs.is_empty() {
        return Ok(());
    }
    if which("brew").is_err() {
        eprintln!("brew not found");
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

async fn install_winget_pkgs(ctx: &Ctx, pkgs: BTreeSet<WingetPackage>) -> Result<()> {
    if pkgs.is_empty() {
        return Ok(());
    }
    if which("winget").is_err() {
        if ctx.verbose {
            eprintln!("winget not found");
        }
        return Ok(());
    }
    for pkg in pkgs.iter() {
        let mut args: Vec<&str> = vec![
            "install",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ];
        if let Some(id) = &pkg.id {
            args.push("--id");
            args.push(id.as_str());
        } else if let Some(name) = &pkg.name {
            args.push(name.as_str());
        } else {
            continue; // skip packages with neither name nor id
        }
        if let Some(opts) = &pkg.options {
            for part in opts.split_whitespace() {
                args.push(part);
            }
        }
        run_cmd(ctx, "winget", args.as_slice()).await?;
    }
    Ok(())
}

async fn install_cargo_crates(ctx: &Ctx, crates: BTreeSet<CargoPackage>) -> Result<()> {
    if crates.is_empty() {
        return Ok(());
    }
    if which("cargo").is_err() {
        if ctx.verbose {
            eprintln!("cargo not found");
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

async fn install_python_packages(ctx: &Ctx, packages: BTreeSet<PythonPackage>) -> Result<()> {
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
                // Use `uv tool install` for global CLI tool installs
                args.extend(["tool", "install"].map(|s| s.to_string()));
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

async fn install_node_packages(ctx: &Ctx, packages: BTreeSet<NodePackage>) -> Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    if which("npm").is_err() {
        if ctx.verbose {
            eprintln!("npm not found");
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
        _ => eprintln!("Unsupported OS: {}", OS),
    }
    Ok(())
}

async fn install_windows(ctx: &Ctx, overlay: &Overlay) -> Result<()> {
    let Some(install_cfg) = overlay.install.as_ref() else {
        return Ok(());
    };
    let platform_cfg = install_cfg.platforms.get("windows");

    if let Some(pre) = &install_cfg.pre {
        run_scripts(ctx, pre).await?;
    }
    if let Some(pre) = platform_cfg.and_then(|p| p.pre.as_ref()) {
        run_scripts(ctx, pre).await?;
    }

    // winget (system manager)
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.winget.as_ref())
        .or(install_cfg.winget.as_ref())
        && let Some(pre) = &cfg.pre
    {
        run_scripts(ctx, pre).await?;
    }
    let pkgs = get_winget_packages(ctx, overlay, &mut HashSet::new()).await?;
    if !pkgs.is_empty() {
        install_winget_pkgs(ctx, pkgs).await?;
    }
    if let Some(cfg) = platform_cfg
        .and_then(|p| p.winget.as_ref())
        .or(install_cfg.winget.as_ref())
        && let Some(post) = &cfg.post
    {
        run_scripts(ctx, post).await?;
    }

    // Language managers after system
    install_language_managers(ctx, overlay, install_cfg, platform_cfg).await?;

    if let Some(post) = platform_cfg.and_then(|p| p.post.as_ref()) {
        run_scripts(ctx, post).await?;
    }
    if let Some(post) = &install_cfg.post {
        run_scripts(ctx, post).await?;
    }

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
        SystemManager::Winget => install_cfg.winget.is_some(),
    };
    let has_platform = |mgr: &SystemManager| {
        platform_cfg
            .map(|p| match mgr {
                SystemManager::Archlinux => p.archlinux.is_some(),
                SystemManager::Apt => p.apt.is_some(),
                SystemManager::Brew => p.brew.is_some(),
                SystemManager::Winget => p.winget.is_some(),
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
    let Some(install_cfg) = overlay.install.as_ref() else {
        return Ok(());
    };
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
                let pkgs = get_archlinux_packages(ctx, overlay, &mut HashSet::new()).await?;
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
                let pkgs = get_apt_packages(ctx, overlay, &mut HashSet::new()).await?;
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
                let (taps, pkgs) = get_brew_packages(ctx, overlay, &mut HashSet::new()).await?;
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
            SystemManager::Winget => {} // winget is not used on Linux
        }
    }

    // Language managers after system
    install_language_managers(ctx, overlay, install_cfg, platform_cfg).await?;

    if let Some(post) = platform_cfg.and_then(|p| p.post.as_ref()) {
        run_scripts(ctx, post).await?;
    }
    if let Some(post) = &install_cfg.post {
        run_scripts(ctx, post).await?;
    }

    Ok(())
}

async fn install_macos(ctx: &Ctx, overlay: &Overlay) -> Result<()> {
    let Some(install_cfg) = overlay.install.as_ref() else {
        return Ok(());
    };
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
    let (taps, pkgs) = get_brew_packages(ctx, overlay, &mut HashSet::new()).await?;
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

    // Language managers after system
    install_language_managers(ctx, overlay, install_cfg, platform_cfg).await?;

    if let Some(post) = platform_cfg.and_then(|p| p.post.as_ref()) {
        run_scripts(ctx, post).await?;
    }
    if let Some(post) = &install_cfg.post {
        run_scripts(ctx, post).await?;
    }

    Ok(())
}

async fn get_archlinux_packages(
    ctx: &Ctx,
    overlay: &Overlay,
    visited: &mut HashSet<String>,
) -> Result<BTreeSet<String>> {
    collect_packages(ctx, overlay, visited, |install, _platform| {
        // Archlinux has special platform resolution: "arch" / "archlinux" aliases
        let platform_override = if OS == "linux" {
            detect_linux_distro().and_then(|distro| {
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
            })
        } else {
            None
        };
        let source = platform_override
            .and_then(|p| p.archlinux.as_ref())
            .or(install.archlinux.as_ref());
        source
            .map(|cfg| cfg.packages.iter().cloned().collect())
            .unwrap_or_default()
    })
    .await
}

async fn get_apt_packages(
    ctx: &Ctx,
    overlay: &Overlay,
    visited: &mut HashSet<String>,
) -> Result<BTreeSet<String>> {
    collect_packages(ctx, overlay, visited, |install, platform| {
        let source = platform
            .and_then(|p| p.apt.as_ref())
            .or(install.apt.as_ref());
        source
            .map(|cfg| cfg.packages.iter().cloned().collect())
            .unwrap_or_default()
    })
    .await
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
    visited: &mut HashSet<String>,
) -> Result<(BTreeSet<String>, BTreeSet<BrewPackage>)> {
    if !visited.insert(overlay.name.clone()) {
        return Ok((BTreeSet::new(), BTreeSet::new()));
    }
    let mut taps: BTreeSet<String> = BTreeSet::new();
    let mut packages: BTreeSet<BrewPackage> = BTreeSet::new();
    if let Some(install) = &overlay.install {
        let platform = resolve_platform_override(install);
        let source = platform
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
            let used = ctx
                .repository
                .get(name)
                .with_context(|| format!("used overlay '{name}' not found"))?;
            let (used_taps, used_pkgs) = Box::pin(get_brew_packages(ctx, &used, visited)).await?;
            taps = taps.union(&used_taps).cloned().collect();
            packages = packages.union(&used_pkgs).cloned().collect();
        }
    }
    Ok((taps, packages))
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

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
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

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
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

async fn get_cargo_packages(
    ctx: &Ctx,
    overlay: &Overlay,
    visited: &mut HashSet<String>,
) -> Result<BTreeSet<CargoPackage>> {
    collect_packages(ctx, overlay, visited, |install, platform| {
        let source = platform
            .and_then(|p| p.cargo.as_ref())
            .or(install.cargo.as_ref());
        source
            .map(|cfg| cfg.packages.iter().cloned().collect())
            .unwrap_or_default()
    })
    .await
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

#[derive(Debug, Deserialize, Serialize, Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "lowercase")]
pub enum PythonTool {
    Uv,
    Pipx,
    Pip,
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
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

async fn get_python_packages(
    ctx: &Ctx,
    overlay: &Overlay,
    visited: &mut HashSet<String>,
) -> Result<BTreeSet<PythonPackage>> {
    collect_packages(ctx, overlay, visited, |install, platform| {
        let source = platform
            .and_then(|p| p.python.as_ref())
            .or(install.python.as_ref());
        source
            .map(|cfg| cfg.packages.iter().cloned().collect())
            .unwrap_or_default()
    })
    .await
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

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
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

async fn get_node_packages(
    ctx: &Ctx,
    overlay: &Overlay,
    visited: &mut HashSet<String>,
) -> Result<BTreeSet<NodePackage>> {
    collect_packages(ctx, overlay, visited, |install, platform| {
        let source = platform
            .and_then(|p| p.node.as_ref())
            .or(install.node.as_ref());
        source
            .map(|cfg| cfg.packages.iter().cloned().collect())
            .unwrap_or_default()
    })
    .await
}

async fn get_winget_packages(
    ctx: &Ctx,
    overlay: &Overlay,
    visited: &mut HashSet<String>,
) -> Result<BTreeSet<WingetPackage>> {
    collect_packages(ctx, overlay, visited, |install, platform| {
        let source = platform
            .and_then(|p| p.winget.as_ref())
            .or(install.winget.as_ref());
        source
            .map(|cfg| cfg.packages.iter().cloned().collect())
            .unwrap_or_default()
    })
    .await
}

// Winget (Windows Package Manager)
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(from = "AllWingetForms")]
pub struct WingetConfig {
    pub packages: Vec<WingetPackage>,
    pub pre: Option<Vec<String>>,
    pub post: Option<Vec<String>>,
}

impl From<AllWingetForms> for WingetConfig {
    fn from(f: AllWingetForms) -> Self {
        match f {
            AllWingetForms::Flat(packages) => Self {
                packages: packages
                    .into_iter()
                    .map(|name| WingetPackage {
                        name: Some(name),
                        id: None,
                        options: None,
                    })
                    .collect(),
                pre: None,
                post: None,
            },
            AllWingetForms::Full {
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
pub enum AllWingetForms {
    Flat(Vec<String>),
    Full {
        packages: Vec<WingetPackage>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        pre: Option<Vec<String>>,
        #[serde(default, deserialize_with = "option_string_or_vec")]
        post: Option<Vec<String>>,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[serde(from = "AllWingetPackageForms")]
pub struct WingetPackage {
    pub name: Option<String>,
    pub id: Option<String>,
    pub options: Option<String>,
}

impl From<AllWingetPackageForms> for WingetPackage {
    fn from(f: AllWingetPackageForms) -> Self {
        match f {
            AllWingetPackageForms::Str(name) => Self {
                name: Some(name),
                id: None,
                options: None,
            },
            AllWingetPackageForms::Full { name, id, options } => Self { name, id, options },
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum AllWingetPackageForms {
    Str(String),
    Full {
        name: Option<String>,
        id: Option<String>,
        options: Option<String>,
    },
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
    pub winget: Option<WingetConfig>,
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
    pub winget: Option<WingetConfig>,
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
  winget:
    - bat
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
  winget:
    packages:
      - name: bat
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
winget = ["bat"]
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
winget.packages = [{name="bat"}]
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
  winget:
    - bat
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
winget = ["bat"]
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
        assert_eq!(
            install.winget.unwrap().packages[0],
            WingetPackage {
                name: Some("bat".into()),
                id: None,
                options: None,
            }
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
  winget:
    packages:
      - bat
      - id: sharkdp.bat
      - name: Git
        options: "--source winget"
"#
    )]
    #[case::toml(
        FileFormat::Toml,
        r#"
[install]
winget.packages = ["bat", {id="sharkdp.bat"}, {name="Git", options="--source winget"}]
"#
    )]
    fn test_winget_config(#[case] format: FileFormat, #[case] content: &str) {
        let c = Config::builder()
            .add_source(File::from_str(content, format))
            .build()
            .unwrap();
        let data: Data = c.try_deserialize().unwrap();
        let winget = data.install.unwrap().winget.unwrap();
        assert_eq!(
            winget.packages,
            vec![
                WingetPackage {
                    name: Some(String::from("bat")),
                    id: None,
                    options: None,
                },
                WingetPackage {
                    name: None,
                    id: Some(String::from("sharkdp.bat")),
                    options: None,
                },
                WingetPackage {
                    name: Some(String::from("Git")),
                    id: None,
                    options: Some(String::from("--source winget")),
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
            winget: None,
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
                winget: None,
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
            winget: None,
            post: None,
            platforms,
        };
        let managers = decide_linux_managers("ubuntu", &install);
        assert_eq!(managers, vec![SystemManager::Apt, SystemManager::Brew]);
    }

    use assert_fs::TempDir;
    use assert_fs::prelude::*;

    use crate::exec::Context;
    use crate::overlays::Repository;

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    fn test_ctx(root: std::path::PathBuf, repo: Repository, overlay: Option<Overlay>) -> Ctx {
        let mut builder = Context::builder().root(root).repository(repo);
        if let Some(o) = overlay {
            builder = builder.overlay(o);
        }
        builder.build()
    }

    /// Diamond dependency for package collection:
    /// A uses B and C, both B and C use D.
    /// D's cargo packages should appear once (not duplicated, no infinite recursion).
    #[tokio::test]
    async fn test_get_cargo_packages_diamond() {
        let (td, repo) = repo_and_root();

        // D: leaf overlay with cargo packages
        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml")
            .write_str("[install.cargo]\npackages = [{name = \"shared-crate\"}]")
            .unwrap();

        // B: uses D, has its own cargo packages
        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("uses = [\"d\"]\n[install.cargo]\npackages = [{name = \"b-crate\"}]")
            .unwrap();

        // C: uses D, has its own cargo packages
        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("uses = [\"d\"]\n[install.cargo]\npackages = [{name = \"c-crate\"}]")
            .unwrap();

        // A: uses B and C
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("uses = [\"b\", \"c\"]\n[install.cargo]\npackages = [{name = \"a-crate\"}]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let ctx = test_ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let mut visited = HashSet::new();
        let pkgs = get_cargo_packages(&ctx, &overlay_a, &mut visited)
            .await
            .unwrap();

        let names: BTreeSet<_> = pkgs.iter().filter_map(|p| p.name.as_deref()).collect();
        assert_eq!(
            names,
            BTreeSet::from(["a-crate", "b-crate", "c-crate", "shared-crate"]),
            "all packages should be collected exactly once across diamond"
        );
        // visited should contain all 4 overlays
        assert_eq!(visited.len(), 4);
    }

    /// Cycle in `uses` for package collection should not infinite-loop.
    /// The visited set breaks the recursion; packages from the first visit are collected.
    #[tokio::test]
    async fn test_get_cargo_packages_cycle() {
        let (td, repo) = repo_and_root();

        let a = td.child("x");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("uses = [\"y\"]\n[install.cargo]\npackages = [{name = \"x-crate\"}]")
            .unwrap();

        let b = td.child("y");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("uses = [\"x\"]\n[install.cargo]\npackages = [{name = \"y-crate\"}]")
            .unwrap();

        let overlay_x = repo.get("x").unwrap();
        let ctx = test_ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_x.clone()),
        );
        let mut visited = HashSet::new();
        // Should not panic or infinite-loop
        let pkgs = get_cargo_packages(&ctx, &overlay_x, &mut visited)
            .await
            .unwrap();

        let names: BTreeSet<_> = pkgs.iter().filter_map(|p| p.name.as_deref()).collect();
        assert!(names.contains("x-crate"), "should collect packages from x");
        assert!(names.contains("y-crate"), "should collect packages from y");
    }

    // ── APT packages ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_apt_packages_single() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("[install]\napt = [\"curl\", \"git\"]")
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let mut visited = HashSet::new();
        let pkgs = get_apt_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();

        assert_eq!(pkgs, BTreeSet::from(["curl".into(), "git".into()]));
    }

    #[tokio::test]
    async fn test_get_apt_packages_diamond() {
        let (td, repo) = repo_and_root();

        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml")
            .write_str("[install]\napt = [\"shared-pkg\"]")
            .unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\napt = [\"b-pkg\"]")
            .unwrap();

        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\napt = [\"c-pkg\"]")
            .unwrap();

        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("uses = [\"b\", \"c\"]\n[install]\napt = [\"a-pkg\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let ctx = test_ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let mut visited = HashSet::new();
        let pkgs = get_apt_packages(&ctx, &overlay_a, &mut visited)
            .await
            .unwrap();

        assert_eq!(
            pkgs,
            BTreeSet::from([
                "a-pkg".into(),
                "b-pkg".into(),
                "c-pkg".into(),
                "shared-pkg".into()
            ]),
        );
        assert_eq!(visited.len(), 4);
    }

    // ── Brew packages ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_brew_packages_single() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                "[install.brew]\ntaps = [\"my/tap\"]\npackages = [\"pkg1\", {name=\"pkg2\", cask=true}]",
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let mut visited = HashSet::new();
        let (taps, pkgs) = get_brew_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();

        assert_eq!(taps, BTreeSet::from(["my/tap".into()]));
        assert!(pkgs.contains(&BrewPackage {
            name: "pkg1".into(),
            options: None,
            cask: None,
        }));
        assert!(pkgs.contains(&BrewPackage {
            name: "pkg2".into(),
            options: None,
            cask: Some(true),
        }));
    }

    #[tokio::test]
    async fn test_get_brew_packages_diamond() {
        let (td, repo) = repo_and_root();

        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml")
            .write_str("[install.brew]\ntaps = [\"shared/tap\"]\npackages = [\"shared-pkg\"]")
            .unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("uses = [\"d\"]\n[install.brew]\ntaps = [\"b/tap\"]\npackages = [\"b-pkg\"]")
            .unwrap();

        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str(
                "uses = [\"d\"]\n[install.brew]\ntaps = [\"shared/tap\"]\npackages = [\"c-pkg\"]",
            )
            .unwrap();

        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("uses = [\"b\", \"c\"]\n[install.brew]\npackages = [\"a-pkg\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let ctx = test_ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let mut visited = HashSet::new();
        let (taps, pkgs) = get_brew_packages(&ctx, &overlay_a, &mut visited)
            .await
            .unwrap();

        assert_eq!(
            taps,
            BTreeSet::from(["shared/tap".into(), "b/tap".into()]),
            "taps should be deduplicated"
        );
        let names: BTreeSet<_> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            BTreeSet::from(["a-pkg", "b-pkg", "c-pkg", "shared-pkg"]),
        );
        assert_eq!(visited.len(), 4);
    }

    // ── Python packages ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_python_packages_single() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("[install]\npython = [\"requests\"]")
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let mut visited = HashSet::new();
        let pkgs = get_python_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();

        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs.iter().next().unwrap().name, "requests");
    }

    #[tokio::test]
    async fn test_get_python_packages_full_form() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("[install.python]\npackages = [{name=\"requests\", tool=\"uv\", extras=[\"security\"], options=\"--force\"}]")
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let mut visited = HashSet::new();
        let pkgs = get_python_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();

        let pkg = pkgs.iter().next().unwrap();
        assert_eq!(pkg.name, "requests");
        assert_eq!(pkg.tool, Some(PythonTool::Uv));
        assert_eq!(pkg.extras, Some(vec!["security".into()]));
        assert_eq!(pkg.options, Some("--force".into()));
    }

    #[tokio::test]
    async fn test_get_python_packages_diamond() {
        let (td, repo) = repo_and_root();

        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml")
            .write_str("[install]\npython = [\"shared-lib\"]")
            .unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\npython = [\"b-lib\"]")
            .unwrap();

        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\npython = [\"c-lib\"]")
            .unwrap();

        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("uses = [\"b\", \"c\"]\n[install]\npython = [\"a-lib\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let ctx = test_ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let mut visited = HashSet::new();
        let pkgs = get_python_packages(&ctx, &overlay_a, &mut visited)
            .await
            .unwrap();

        let names: BTreeSet<_> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            BTreeSet::from(["a-lib", "b-lib", "c-lib", "shared-lib"]),
        );
    }

    // ── Node packages ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_node_packages_single() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("[install]\nnode = [\"typescript\"]")
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let mut visited = HashSet::new();
        let pkgs = get_node_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();

        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs.iter().next().unwrap().name, "typescript");
    }

    #[tokio::test]
    async fn test_get_node_packages_full_form() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("[install.node]\npackages = [{name=\"typescript\", options=\"--force\"}]")
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let mut visited = HashSet::new();
        let pkgs = get_node_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();

        let pkg = pkgs.iter().next().unwrap();
        assert_eq!(pkg.name, "typescript");
        assert_eq!(pkg.options, Some("--force".into()));
    }

    #[tokio::test]
    async fn test_get_node_packages_diamond() {
        let (td, repo) = repo_and_root();

        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml")
            .write_str("[install]\nnode = [\"shared-pkg\"]")
            .unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\nnode = [\"b-pkg\"]")
            .unwrap();

        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\nnode = [\"c-pkg\"]")
            .unwrap();

        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("uses = [\"b\", \"c\"]\n[install]\nnode = [\"a-pkg\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let ctx = test_ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let mut visited = HashSet::new();
        let pkgs = get_node_packages(&ctx, &overlay_a, &mut visited)
            .await
            .unwrap();

        let names: BTreeSet<_> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            BTreeSet::from(["a-pkg", "b-pkg", "c-pkg", "shared-pkg"]),
        );
    }

    // ── Winget packages ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_winget_packages_single() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("[install]\nwinget = [\"bat\"]")
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let mut visited = HashSet::new();
        let pkgs = get_winget_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();

        assert_eq!(pkgs.len(), 1);
        assert_eq!(
            pkgs.iter().next().unwrap(),
            &WingetPackage {
                name: Some("bat".into()),
                id: None,
                options: None,
            }
        );
    }

    #[tokio::test]
    async fn test_get_winget_packages_full_form() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                "[install.winget]\npackages = [{id=\"sharkdp.bat\"}, {name=\"Git\", options=\"--source winget\"}]",
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let mut visited = HashSet::new();
        let pkgs = get_winget_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();

        assert!(pkgs.contains(&WingetPackage {
            name: None,
            id: Some("sharkdp.bat".into()),
            options: None,
        }));
        assert!(pkgs.contains(&WingetPackage {
            name: Some("Git".into()),
            id: None,
            options: Some("--source winget".into()),
        }));
    }

    #[tokio::test]
    async fn test_get_winget_packages_diamond() {
        let (td, repo) = repo_and_root();

        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml")
            .write_str("[install]\nwinget = [\"shared-pkg\"]")
            .unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\nwinget = [\"b-pkg\"]")
            .unwrap();

        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\nwinget = [\"c-pkg\"]")
            .unwrap();

        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("uses = [\"b\", \"c\"]\n[install]\nwinget = [\"a-pkg\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let ctx = test_ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let mut visited = HashSet::new();
        let pkgs = get_winget_packages(&ctx, &overlay_a, &mut visited)
            .await
            .unwrap();

        let names: BTreeSet<_> = pkgs.iter().filter_map(|p| p.name.as_deref()).collect();
        assert_eq!(
            names,
            BTreeSet::from(["a-pkg", "b-pkg", "c-pkg", "shared-pkg"]),
        );
    }

    // ── Archlinux packages ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_archlinux_packages_single() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("[install]\narchlinux = [\"base-devel\", \"git\"]")
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let mut visited = HashSet::new();
        let pkgs = get_archlinux_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();

        assert_eq!(pkgs, BTreeSet::from(["base-devel".into(), "git".into()]));
    }

    #[tokio::test]
    async fn test_get_archlinux_packages_diamond() {
        let (td, repo) = repo_and_root();

        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml")
            .write_str("[install]\narchlinux = [\"shared-pkg\"]")
            .unwrap();

        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\narchlinux = [\"b-pkg\"]")
            .unwrap();

        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("uses = [\"d\"]\n[install]\narchlinux = [\"c-pkg\"]")
            .unwrap();

        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("uses = [\"b\", \"c\"]\n[install]\narchlinux = [\"a-pkg\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let ctx = test_ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let mut visited = HashSet::new();
        let pkgs = get_archlinux_packages(&ctx, &overlay_a, &mut visited)
            .await
            .unwrap();

        assert_eq!(
            pkgs,
            BTreeSet::from([
                "a-pkg".into(),
                "b-pkg".into(),
                "c-pkg".into(),
                "shared-pkg".into()
            ]),
        );
    }

    // ── decide_linux_managers edge cases ────────────────────────────────

    #[test]
    fn test_linux_precedence_no_config() {
        // No manager configured at all => empty
        let install = InstallConfig {
            pre: None,
            apt: None,
            archlinux: None,
            brew: None,
            cargo: None,
            python: None,
            node: None,
            winget: None,
            post: None,
            platforms: Default::default(),
        };
        let managers = decide_linux_managers("ubuntu", &install);
        assert!(managers.is_empty());
        let managers = decide_linux_managers("archlinux", &install);
        assert!(managers.is_empty());
        let managers = decide_linux_managers("fedora", &install);
        assert!(managers.is_empty());
    }

    #[test]
    fn test_linux_precedence_brew_only_top_level() {
        let install = InstallConfig {
            pre: None,
            apt: None,
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
            winget: None,
            post: None,
            platforms: Default::default(),
        };
        // On debian, brew is second in precedence but first found
        let managers = decide_linux_managers("ubuntu", &install);
        assert_eq!(managers, vec![SystemManager::Brew]);
        // On arch, brew is second in precedence but first found
        let managers = decide_linux_managers("archlinux", &install);
        assert_eq!(managers, vec![SystemManager::Brew]);
    }

    #[test]
    fn test_linux_precedence_generic_distro() {
        // Unknown distro falls back to generic precedence
        let install = InstallConfig {
            pre: None,
            apt: Some(AptConfig {
                packages: vec!["a".into()],
                pre: None,
                post: None,
            }),
            archlinux: None,
            brew: None,
            cargo: None,
            python: None,
            node: None,
            winget: None,
            post: None,
            platforms: Default::default(),
        };
        let managers = decide_linux_managers("fedora", &install);
        assert_eq!(managers, vec![SystemManager::Apt]);
    }

    #[test]
    fn test_linux_precedence_platform_section_empty_managers() {
        // Platform section exists but none of its managers are configured
        let mut platforms = std::collections::HashMap::new();
        platforms.insert(
            "ubuntu".into(),
            PlatformInstallConfig {
                pre: None,
                apt: None,
                archlinux: None,
                brew: None,
                cargo: None,
                python: None,
                node: None,
                winget: None,
                post: None,
            },
        );
        let install = InstallConfig {
            pre: None,
            apt: Some(AptConfig {
                packages: vec!["a".into()],
                pre: None,
                post: None,
            }),
            archlinux: None,
            brew: None,
            cargo: None,
            python: None,
            node: None,
            winget: None,
            post: None,
            platforms,
        };
        // Platform section takes priority over top-level, but has no managers
        let managers = decide_linux_managers("ubuntu", &install);
        assert!(managers.is_empty());
    }

    #[test]
    fn test_linux_precedence_arch_alias() {
        // "arch" should match the arch precedence
        let install = InstallConfig {
            pre: None,
            apt: None,
            archlinux: Some(ArchlinuxConfig {
                packages: vec!["x".into()],
                pre: None,
                post: None,
            }),
            brew: None,
            cargo: None,
            python: None,
            node: None,
            winget: None,
            post: None,
            platforms: Default::default(),
        };
        let managers = decide_linux_managers("arch", &install);
        assert_eq!(managers, vec![SystemManager::Archlinux]);
    }

    // ── run_cmd dry_run ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_run_cmd_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();
        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .dry_run(true)
            .verbose(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay)
            .build();
        // dry_run should not actually execute the command
        let result = run_cmd(&ctx, "false", &[]).await;
        assert!(result.is_ok(), "dry_run should not actually run command");
    }

    #[tokio::test]
    async fn test_run_cmd_verbose() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();
        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .verbose(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay)
            .build();
        // "true" command should succeed
        let result = run_cmd(&ctx, "true", &[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_cmd_failure() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();
        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay)
            .build();
        // "false" command should fail
        let result = run_cmd(&ctx, "false", &[]).await;
        assert!(result.is_err());
    }

    // ── run_scripts ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_run_scripts_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();
        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .dry_run(true)
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay)
            .build();
        let scripts = vec!["echo hello".to_string(), "exit 1".to_string()];
        // dry_run should not actually run the scripts
        let result = run_scripts(&ctx, &scripts).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_scripts_success() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();
        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay)
            .build();
        let scripts = vec!["true".to_string(), "true".to_string()];
        let result = run_scripts(&ctx, &scripts).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_scripts_failure_stops() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();
        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay)
            .build();
        let scripts = vec!["false".to_string(), "true".to_string()];
        let result = run_scripts(&ctx, &scripts).await;
        assert!(result.is_err(), "should stop on first failure");
    }

    // ── install with no config / dry_run ─────────────────────────────────

    #[tokio::test]
    async fn test_install_no_config() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();
        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();
        // No install section => should succeed as no-op
        let result = install(&ctx, &overlay).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_install_linux_no_install_section() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();
        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo.clone())
            .overlay(overlay.clone())
            .build();
        let result = install_linux(&ctx, &overlay).await;
        assert!(result.is_ok());
    }

    // ── config deserialization: platform overrides ─────────────────────────

    #[test]
    fn test_config_with_platform_override_toml() {
        let content = r#"
[install]
apt = ["base-pkg"]
[install.ubuntu]
apt = ["ubuntu-specific"]
"#;
        let c = Config::builder()
            .add_source(File::from_str(content, FileFormat::Toml))
            .build()
            .unwrap();
        let data: Data = c.try_deserialize().unwrap();
        let install = data.install.unwrap();
        assert_eq!(install.apt.unwrap().packages, ["base-pkg"]);
        let ubuntu = install.platforms.get("ubuntu").unwrap();
        assert_eq!(ubuntu.apt.as_ref().unwrap().packages, ["ubuntu-specific"]);
    }

    #[test]
    fn test_config_with_platform_override_yaml() {
        let content = r#"
install:
  brew:
    - base-pkg
  macos:
    brew:
      - mac-only
"#;
        let c = Config::builder()
            .add_source(File::from_str(content, FileFormat::Yaml))
            .build()
            .unwrap();
        let data: Data = c.try_deserialize().unwrap();
        let install = data.install.unwrap();
        assert!(install.brew.is_some());
        let macos = install.platforms.get("macos").unwrap();
        assert!(macos.brew.is_some());
    }

    #[test]
    fn test_config_with_pre_post_scripts_in_managers() {
        let content = r#"
[install.cargo]
packages = [{name = "ripgrep"}]
pre = ["echo before cargo"]
post = ["echo after cargo"]
[install.python]
packages = [{name = "requests"}]
pre = "echo before python"
post = "echo after python"
[install.node]
packages = [{name = "typescript"}]
pre = "echo before node"
post = "echo after node"
"#;
        let c = Config::builder()
            .add_source(File::from_str(content, FileFormat::Toml))
            .build()
            .unwrap();
        let data: Data = c.try_deserialize().unwrap();
        let install = data.install.unwrap();
        let cargo = install.cargo.unwrap();
        assert_eq!(cargo.pre.unwrap(), vec!["echo before cargo"]);
        assert_eq!(cargo.post.unwrap(), vec!["echo after cargo"]);
        let python = install.python.unwrap();
        assert_eq!(python.pre.unwrap(), vec!["echo before python"]);
        assert_eq!(python.post.unwrap(), vec!["echo after python"]);
        let node = install.node.unwrap();
        assert_eq!(node.pre.unwrap(), vec!["echo before node"]);
        assert_eq!(node.post.unwrap(), vec!["echo after node"]);
    }

    #[test]
    fn test_cargo_package_full_form_deserialization() {
        let content = r#"
[install.cargo]
packages = [
    {name = "ripgrep", version = "13.0", locked = true},
    {git = "https://github.com/user/repo.git", tag = "v1.0", name = "my-tool"},
    {git = "https://github.com/user/repo2.git", branch = "main"},
    {git = "https://github.com/user/repo3.git", rev = "abc123"},
    {path = "/local/path/to/crate"},
    {name = "tool", features = ["feat1", "feat2"], options = "--force"},
]
"#;
        let c = Config::builder()
            .add_source(File::from_str(content, FileFormat::Toml))
            .build()
            .unwrap();
        let data: Data = c.try_deserialize().unwrap();
        let cargo = data.install.unwrap().cargo.unwrap();
        assert_eq!(cargo.packages.len(), 6);
        // Versioned crate
        assert_eq!(cargo.packages[0].name, Some("ripgrep".into()));
        assert_eq!(cargo.packages[0].version, Some("13.0".into()));
        assert_eq!(cargo.packages[0].locked, Some(true));
        // Git with tag
        assert_eq!(
            cargo.packages[1].git,
            Some("https://github.com/user/repo.git".into())
        );
        assert_eq!(cargo.packages[1].tag, Some("v1.0".into()));
        assert_eq!(cargo.packages[1].name, Some("my-tool".into()));
        // Git with branch
        assert_eq!(cargo.packages[2].branch, Some("main".into()));
        // Git with rev
        assert_eq!(cargo.packages[3].rev, Some("abc123".into()));
        // Path
        assert_eq!(cargo.packages[4].path, Some("/local/path/to/crate".into()));
        // Features + options
        assert_eq!(
            cargo.packages[5].features,
            Some(vec!["feat1".into(), "feat2".into()])
        );
        assert_eq!(cargo.packages[5].options, Some("--force".into()));
    }

    #[test]
    fn test_winget_package_forms() {
        let content = r#"
[install.winget]
packages = [
    {id = "Microsoft.VisualStudioCode"},
    {name = "Git", options = "--source winget --override /SILENT"},
]
"#;
        let c = Config::builder()
            .add_source(File::from_str(content, FileFormat::Toml))
            .build()
            .unwrap();
        let data: Data = c.try_deserialize().unwrap();
        let winget = data.install.unwrap().winget.unwrap();
        assert_eq!(winget.packages.len(), 2);
        assert_eq!(
            winget.packages[0].id,
            Some("Microsoft.VisualStudioCode".into())
        );
        assert!(winget.packages[0].name.is_none());
        assert_eq!(winget.packages[1].name, Some("Git".into()));
        assert!(winget.packages[1].options.is_some());
    }

    // ── resolve_platform_override ────────────────────────────────────────

    #[test]
    fn test_resolve_platform_override_no_platforms() {
        let install = InstallConfig {
            pre: None,
            apt: None,
            archlinux: None,
            brew: None,
            cargo: None,
            python: None,
            node: None,
            winget: None,
            post: None,
            platforms: Default::default(),
        };
        let result = resolve_platform_override(&install);
        // On CI/test machines, there may or may not be a matching platform
        // The key point is this doesn't panic
        let _ = result;
    }

    // ── No install config ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_packages_no_install_section() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = test_ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let mut visited = HashSet::new();
        let apt = get_apt_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();
        assert!(apt.is_empty());

        let mut visited = HashSet::new();
        let brew = get_brew_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();
        assert!(brew.0.is_empty());
        assert!(brew.1.is_empty());

        let mut visited = HashSet::new();
        let python = get_python_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();
        assert!(python.is_empty());

        let mut visited = HashSet::new();
        let node = get_node_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();
        assert!(node.is_empty());

        let mut visited = HashSet::new();
        let winget = get_winget_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();
        assert!(winget.is_empty());

        let mut visited = HashSet::new();
        let arch = get_archlinux_packages(&ctx, &overlay, &mut visited)
            .await
            .unwrap();
        assert!(arch.is_empty());
    }

    // ── install_cargo_crates (dry_run) ──────────────────────────────────

    #[tokio::test]
    async fn test_install_cargo_crates_empty() {
        let (td, repo) = repo_and_root();
        let ctx = test_ctx(td.path().to_path_buf(), repo, None);
        let ctx = Context::builder()
            .root(ctx.root.clone())
            .repository(ctx.repository.clone())
            .dry_run(true)
            .build();
        let crates = BTreeSet::new();
        install_cargo_crates(&ctx, crates).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_cargo_crates_name_only_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut crates = BTreeSet::new();
        crates.insert(CargoPackage {
            name: Some("ripgrep".into()),
            git: None,
            branch: None,
            tag: None,
            rev: None,
            path: None,
            version: Some("14.0.0".into()),
            features: None,
            locked: None,
            options: None,
        });
        install_cargo_crates(&ctx, crates).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_cargo_crates_git_with_tag_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut crates = BTreeSet::new();
        crates.insert(CargoPackage {
            name: Some("my-crate".into()),
            git: Some("https://github.com/user/repo.git".into()),
            branch: None,
            tag: Some("v1.0.0".into()),
            rev: None,
            path: None,
            version: None,
            features: Some(vec!["feat1".into(), "feat2".into()]),
            locked: Some(true),
            options: Some("--force".into()),
        });
        install_cargo_crates(&ctx, crates).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_cargo_crates_git_with_branch_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut crates = BTreeSet::new();
        crates.insert(CargoPackage {
            name: None,
            git: Some("https://github.com/user/repo.git".into()),
            branch: Some("develop".into()),
            tag: None,
            rev: None,
            path: None,
            version: None,
            features: None,
            locked: None,
            options: None,
        });
        install_cargo_crates(&ctx, crates).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_cargo_crates_git_with_rev_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut crates = BTreeSet::new();
        crates.insert(CargoPackage {
            name: None,
            git: Some("https://github.com/user/repo.git".into()),
            branch: None,
            tag: None,
            rev: Some("abc1234".into()),
            path: None,
            version: None,
            features: None,
            locked: None,
            options: None,
        });
        install_cargo_crates(&ctx, crates).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_cargo_crates_path_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut crates = BTreeSet::new();
        crates.insert(CargoPackage {
            name: None,
            git: None,
            branch: None,
            tag: None,
            rev: None,
            path: Some("/path/to/crate".into()),
            version: None,
            features: None,
            locked: None,
            options: None,
        });
        install_cargo_crates(&ctx, crates).await.unwrap();
    }

    // ── install_python_packages (dry_run) ───────────────────────────────

    #[tokio::test]
    async fn test_install_python_packages_empty() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let pkgs = BTreeSet::new();
        install_python_packages(&ctx, pkgs).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_python_packages_with_extras_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut pkgs = BTreeSet::new();
        pkgs.insert(PythonPackage {
            name: "requests".into(),
            extras: Some(vec!["security".into(), "socks".into()]),
            tool: None,
            options: Some("--user".into()),
        });
        install_python_packages(&ctx, pkgs).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_python_packages_with_explicit_tool_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut pkgs = BTreeSet::new();
        pkgs.insert(PythonPackage {
            name: "black".into(),
            extras: None,
            tool: Some(PythonTool::Pip),
            options: None,
        });
        install_python_packages(&ctx, pkgs).await.unwrap();
    }

    // ── install_node_packages (dry_run) ─────────────────────────────────

    #[tokio::test]
    async fn test_install_node_packages_empty() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let pkgs = BTreeSet::new();
        install_node_packages(&ctx, pkgs).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_node_packages_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut pkgs = BTreeSet::new();
        pkgs.insert(NodePackage {
            name: "typescript".into(),
            options: Some("--force".into()),
        });
        install_node_packages(&ctx, pkgs).await.unwrap();
    }

    // ── install_apt_pkgs (dry_run) ──────────────────────────────────────

    #[tokio::test]
    async fn test_install_apt_pkgs_empty() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let pkgs = BTreeSet::new();
        install_apt_pkgs(&ctx, pkgs).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_apt_pkgs_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut pkgs = BTreeSet::new();
        pkgs.insert("git".into());
        pkgs.insert("vim".into());
        install_apt_pkgs(&ctx, pkgs).await.unwrap();
    }

    // ── install_arch_pkgs (dry_run) ─────────────────────────────────────

    #[tokio::test]
    async fn test_install_arch_pkgs_empty() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let pkgs = BTreeSet::new();
        install_arch_pkgs(&ctx, pkgs).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_arch_pkgs_dry_run() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .verbose(true)
            .build();
        let mut pkgs = BTreeSet::new();
        pkgs.insert("base-devel".into());
        // Regardless of whether yay/paru/pacman is available,
        // with dry_run=true this should succeed
        install_arch_pkgs(&ctx, pkgs).await.unwrap();
    }

    // ── install_brew_pkgs (dry_run) ─────────────────────────────────────

    #[tokio::test]
    async fn test_install_brew_pkgs_empty() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        install_brew_pkgs(&ctx, BTreeSet::new(), BTreeSet::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_install_brew_pkgs_no_brew() {
        // When brew is not found, it should print a message and return Ok
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut pkgs = BTreeSet::new();
        pkgs.insert(BrewPackage {
            name: "git".into(),
            cask: None,
            options: None,
        });
        // This will succeed either way (brew found → dry_run, brew not found → early return)
        install_brew_pkgs(&ctx, BTreeSet::new(), pkgs)
            .await
            .unwrap();
    }

    // ── install_winget_pkgs ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_install_winget_pkgs_empty() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        install_winget_pkgs(&ctx, BTreeSet::new()).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_winget_pkgs_not_found() {
        // winget is not available on Linux → should return Ok
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .verbose(true)
            .build();
        let mut pkgs = BTreeSet::new();
        pkgs.insert(WingetPackage {
            name: Some("Git.Git".into()),
            id: None,
            options: None,
        });
        install_winget_pkgs(&ctx, pkgs).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_winget_pkgs_not_found_non_verbose() {
        let (td, repo) = repo_and_root();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .dry_run(true)
            .build();
        let mut pkgs = BTreeSet::new();
        pkgs.insert(WingetPackage {
            name: None,
            id: Some("Microsoft.VisualStudioCode".into()),
            options: Some("--scope machine".into()),
        });
        install_winget_pkgs(&ctx, pkgs).await.unwrap();
    }

    // ── install_language_managers (dry_run) ──────────────────────────────

    #[tokio::test]
    async fn test_install_language_managers_with_all_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                r#"
target = "~"
[install]
cargo = ["ripgrep"]
python = ["black"]
node = ["typescript"]
"#,
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        let install_cfg = overlay.install.as_ref().unwrap();
        install_language_managers(&ctx, &overlay, install_cfg, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_install_language_managers_with_pre_post_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                r#"
target = "~"
[install.cargo]
pre = ["echo cargo-pre"]
packages = [{name = "ripgrep"}]
post = ["echo cargo-post"]
[install.python]
pre = ["echo py-pre"]
packages = [{name = "black"}]
post = ["echo py-post"]
[install.node]
pre = ["echo node-pre"]
packages = [{name = "typescript"}]
post = ["echo node-post"]
"#,
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        let install_cfg = overlay.install.as_ref().unwrap();
        install_language_managers(&ctx, &overlay, install_cfg, None)
            .await
            .unwrap();
    }

    // ── install_linux (full orchestration, dry_run) ─────────────────────

    #[tokio::test]
    async fn test_install_linux_full_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                r#"
target = "~"
[install]
pre = ["echo global-pre"]
apt = ["git", "curl"]
cargo = ["ripgrep"]
python = ["black"]
node = ["typescript"]
post = ["echo global-post"]
"#,
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_linux(&ctx, &overlay).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_linux_with_brew_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                r#"
target = "~"
[install]
brew = ["git"]
"#,
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_linux(&ctx, &overlay).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_linux_with_archlinux_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                r#"
target = "~"
[install]
archlinux = ["base-devel"]
"#,
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_linux(&ctx, &overlay).await.unwrap();
    }

    // ── install_macos (dry_run, called directly on Linux) ───────────────

    #[tokio::test]
    async fn test_install_macos_no_config() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_macos(&ctx, &overlay).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_macos_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                r#"
target = "~"
[install]
pre = ["echo mac-pre"]
brew = ["git"]
cargo = ["ripgrep"]
post = ["echo mac-post"]
"#,
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_macos(&ctx, &overlay).await.unwrap();
    }

    // ── install_windows (dry_run, called directly on Linux) ─────────────

    #[tokio::test]
    async fn test_install_windows_no_config() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml").write_str("target = \"~\"").unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_windows(&ctx, &overlay).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_windows_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                r#"
target = "~"
[install]
pre = ["echo win-pre"]
winget = ["Git.Git"]
cargo = ["ripgrep"]
post = ["echo win-post"]
"#,
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_windows(&ctx, &overlay).await.unwrap();
    }

    #[tokio::test]
    async fn test_install_windows_with_platform_pre_post_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str(
                r#"
target = "~"
[install]
pre = ["echo top-pre"]
winget = ["Git.Git"]
post = ["echo top-post"]

[install.platforms.windows]
pre = ["echo win-pre"]
post = ["echo win-post"]
"#,
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_windows(&ctx, &overlay).await.unwrap();
    }

    // ── install_linux with platform overrides ───────────────────────────

    #[tokio::test]
    async fn test_install_linux_with_platform_pre_post_dry_run() {
        let distro = detect_linux_distro().unwrap_or_else(|| "linux".to_string());
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.yaml")
            .write_str(&format!(
                r#"
target: "~"
install:
  pre:
    - echo top-pre
  apt:
    - git
  post:
    - echo top-post
  platforms:
    {distro}:
      pre:
        - echo platform-pre
      apt:
        packages:
          - curl
      post:
        - echo platform-post
"#
            ))
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_linux(&ctx, &overlay).await.unwrap();
    }

    // ── install_macos with brew pre/post ─────────────────────────────────

    #[tokio::test]
    async fn test_install_macos_with_brew_hooks_dry_run() {
        let (td, repo) = repo_and_root();
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.yaml")
            .write_str(
                r#"
target: "~"
install:
  brew:
    pre:
      - echo brew-pre
    packages:
      - git
    post:
      - echo brew-post
"#,
            )
            .unwrap();

        let overlay = repo.get("a").unwrap();
        let ctx = Context::builder()
            .root(td.path().to_path_buf())
            .repository(repo)
            .overlay(overlay.clone())
            .dry_run(true)
            .build();

        install_macos(&ctx, &overlay).await.unwrap();
    }
}
