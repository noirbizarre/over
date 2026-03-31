use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::Args;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{FuzzySelect, MultiSelect};
use dirs::home_dir;

use crate::actions::git::config::{GitConfig, GitRepoConfig, RemoteConfig, WorktreeEntry};
use crate::overlays::{self, Format, Repository};
use crate::ui::{emojis, style};
use crate::utils::short_path;

use super::{
    CLI, discover_repo, get_overlay_config, main_repo_root, repo_relative_path, set_overlay_config,
};

#[derive(Args, Debug)]
pub struct Params {
    #[clap(short, long, help = "Name of the target overlay")]
    overlay: Option<String>,
}

/// Properties that can be exported from the local git repo to the overlay config.
#[derive(Debug, Clone)]
struct ExportableProperty {
    label: String,
    key: ExportKey,
}

#[derive(Debug, Clone)]
enum ExportKey {
    Url(String),
    Branch(String),
    Remote { name: String, config: RemoteConfig },
    Config { key: String, value: String },
    WorktreeConfig { key: String, value: String },
    Worktree,
    PerWorktreeConfig,
    Worktrees(HashMap<String, WorktreeEntry>),
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    let git_repo = discover_repo()?;
    let repo_root = main_repo_root(&git_repo)?;
    let workdir = git_repo
        .workdir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| repo_root.clone());

    if cli.debug {
        println!("Repository: {}", repo_root.display());
        if git_repo.is_worktree() {
            println!("Worktree: {}", workdir.display());
        } else if git_repo.is_bare() {
            println!("Worktree workspace (bare repo)");
        }
    }

    let home = cli.resolve_home()?;
    let over_repo = Repository::new(home);

    // Resolve overlay (flag > git config > prompt)
    let overlay = if let Some(ref name) = args.overlay {
        over_repo.get(name)?
    } else if let Some(name) = get_overlay_config(&git_repo)? {
        let overlay = over_repo.get(&name)?;
        println!(
            "{} {} {}",
            emojis::PACKAGE,
            style::white("Using overlay from git config:"),
            style::cyan(&name),
        );
        overlay
    } else {
        let overlays = over_repo.overlays()?;
        if overlays.is_empty() {
            return Err(anyhow!("no overlays found in repository"));
        }
        let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
            .with_prompt("Choose the target overlay")
            .default(0)
            .items(&overlays[..])
            .interact()
            .map_err(|e| anyhow!("overlay selection cancelled: {}", e))?;
        overlays[selection].clone()
    };

    if cli.debug {
        println!("{:#?}", overlay);
    }

    // Compute relative path from overlay target to repo root (not worktree)
    let root = home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    let rel_path = repo_relative_path(&overlay, &root, &repo_root)?;
    let rel_path_str = rel_path
        .to_str()
        .ok_or_else(|| anyhow!("repository path is not valid UTF-8"))?;

    println!(
        "{} {} {} {} {}",
        emojis::LINK,
        style::white("Mounting"),
        style::cyan(&short_path(&repo_root.to_string_lossy())),
        style::white("to overlay"),
        style::cyan(&overlay.name),
    );

    // ── Inspect local repo properties ────────────────────────────────────

    let mut exportable: Vec<ExportableProperty> = Vec::new();

    // Origin URL
    if let Ok(remote) = git_repo.find_remote("origin")
        && let Some(url) = remote.url()
    {
        exportable.push(ExportableProperty {
            label: format!("origin url: {}", url),
            key: ExportKey::Url(url.to_string()),
        });
    }

    // Current branch
    if let Ok(head) = git_repo.head()
        && let Some(branch) = head.shorthand()
    {
        exportable.push(ExportableProperty {
            label: format!("branch: {}", branch),
            key: ExportKey::Branch(branch.to_string()),
        });
    }

    // Other remotes (not origin)
    if let Ok(remotes) = git_repo.remotes() {
        for remote_name in remotes.iter().flatten() {
            if remote_name == "origin" {
                continue;
            }
            if let Ok(remote) = git_repo.find_remote(remote_name)
                && let Some(url) = remote.url()
            {
                let fetch = remote
                    .get_refspec(0)
                    .and_then(|r| r.str().map(|s| s.to_string()));
                exportable.push(ExportableProperty {
                    label: format!("remote {}: {}", remote_name, url),
                    key: ExportKey::Remote {
                        name: remote_name.to_string(),
                        config: RemoteConfig {
                            url: url.to_string(),
                            fetch,
                            push: None,
                            tagopt: None,
                            extras: HashMap::new(),
                        },
                    },
                });
            }
        }
    }

    // Local git config entries (user.*, core.*)
    if let Ok(mut config) = git_repo.config()
        && let Ok(snapshot) = config.snapshot()
    {
        for prefix in &["user.name", "user.email", "core.autocrlf", "core.eol"] {
            if let Ok(value) = snapshot.get_str(prefix) {
                exportable.push(ExportableProperty {
                    label: format!("config {}: {}", prefix, value),
                    key: ExportKey::Config {
                        key: prefix.to_string(),
                        value: value.to_string(),
                    },
                });
            }
        }
    }

    // Worktree mode detection (bare repo or linked worktree)
    if git_repo.is_bare() || git_repo.is_worktree() {
        exportable.push(ExportableProperty {
            label: "worktree: true".to_string(),
            key: ExportKey::Worktree,
        });

        // Detect existing named worktrees
        if let Ok(wt_names) = git_repo.worktrees() {
            let mut wt_map: HashMap<String, WorktreeEntry> = HashMap::new();
            for name in wt_names.iter().flatten() {
                // Resolve each worktree to its checked-out branch
                let branch =
                    resolve_worktree_branch(&git_repo, name).unwrap_or_else(|| name.to_string());

                // Detect per-worktree config entries
                let wt_config = read_worktree_config(&git_repo, name);

                wt_map.insert(
                    name.to_string(),
                    WorktreeEntry {
                        branch,
                        config: wt_config,
                    },
                );
            }
            if !wt_map.is_empty() {
                let names: Vec<String> = wt_map
                    .iter()
                    .map(|(n, e)| format!("{n}={}", e.branch))
                    .collect();
                exportable.push(ExportableProperty {
                    label: format!("worktrees: {}", names.join(", ")),
                    key: ExportKey::Worktrees(wt_map),
                });
            }
        }

        // Detect per_worktree_config (extensions.worktreeConfig)
        if let Ok(mut config) = git_repo.config()
            && let Ok(snapshot) = config.snapshot()
            && snapshot.get_str("extensions.worktreeConfig").is_ok()
        {
            exportable.push(ExportableProperty {
                label: "per_worktree_config: true".to_string(),
                key: ExportKey::PerWorktreeConfig,
            });

            // Read bare repo's config.worktree entries
            let wt_config_path = git_repo.path().join("config.worktree");
            if wt_config_path.exists()
                && let Ok(mut wt_cfg) = git2::Config::open(&wt_config_path)
                && let Ok(wt_snap) = wt_cfg.snapshot()
            {
                // Export worktree config entries (skip auto-managed core.bare)
                for key in &["core.sparseCheckout", "core.sparseCheckoutCone"] {
                    if let Ok(value) = wt_snap.get_str(key) {
                        exportable.push(ExportableProperty {
                            label: format!("worktree config {}: {}", key, value),
                            key: ExportKey::WorktreeConfig {
                                key: key.to_string(),
                                value: value.to_string(),
                            },
                        });
                    }
                }
            }
        }
    }

    // ── Check if overlay already has an entry for this repo ──────────────

    let existing_entry = overlay.git.as_ref().and_then(|git| git.get(rel_path_str));

    if let Some(existing) = existing_entry {
        println!(
            "\n{} {} {}",
            emojis::PACKAGE,
            style::white_b("Overlay already has a git entry for"),
            style::cyan(rel_path_str),
        );
        println!("  url: {}", style::cyan(&existing.url));
        if let Some(ref branch) = existing.branch {
            println!("  branch: {}", style::cyan(branch));
        }
        if let Some(ref remotes) = existing.remotes {
            for (name, cfg) in remotes {
                println!("  remote {}: {}", style::cyan(name), cfg.url);
            }
        }
        if let Some(ref config) = existing.config {
            for (key, value) in &config.entries {
                println!("  config {}: {}", style::cyan(key), value);
            }
        }
        if existing.per_worktree_config {
            println!("  per_worktree_config: {}", style::cyan("true"));
        }
        if let Some(ref wt_config) = existing.worktree_config {
            for (key, value) in &wt_config.entries {
                println!("  worktree_config {}: {}", style::cyan(key), value);
            }
        }
        println!();
    }

    // ── Prompt user to select properties to export ───────────────────────

    if exportable.is_empty() {
        println!(
            "{} {}",
            emojis::CHECKMARK,
            style::white("No exportable properties found in local repo"),
        );
    } else {
        let labels: Vec<&str> = exportable.iter().map(|p| p.label.as_str()).collect();
        let defaults: Vec<bool> = exportable
            .iter()
            .map(|p| {
                matches!(
                    p.key,
                    ExportKey::Url(_)
                        | ExportKey::Branch(_)
                        | ExportKey::Worktree
                        | ExportKey::PerWorktreeConfig
                        | ExportKey::Worktrees(_)
                )
            })
            .collect();

        let selections = MultiSelect::with_theme(&ColorfulTheme::default())
            .with_prompt("Select properties to export to overlay config")
            .items(&labels)
            .defaults(&defaults)
            .interact()
            .map_err(|e| anyhow!("selection cancelled: {}", e))?;

        if selections.is_empty() {
            println!(
                "{} {}",
                emojis::CHECKMARK,
                style::white("No properties selected, skipping export"),
            );
        } else {
            // Build the GitRepoConfig from selected properties
            let mut url = String::new();
            let mut branch: Option<String> = None;
            let mut remotes: HashMap<String, RemoteConfig> = HashMap::new();
            let mut config_entries: HashMap<String, String> = HashMap::new();
            let mut worktree_config_entries: HashMap<String, String> = HashMap::new();
            let mut worktree = false;
            let mut per_worktree_config = false;
            let mut worktrees: Option<HashMap<String, WorktreeEntry>> = None;

            // Start from existing entry if present
            if let Some(existing) = existing_entry {
                url = existing.url.clone();
                branch = existing.branch.clone();
                worktree = existing.worktree;
                per_worktree_config = existing.per_worktree_config;
                worktrees = existing.worktrees.clone();
                if let Some(ref r) = existing.remotes {
                    remotes = r.clone();
                }
                if let Some(ref c) = existing.config {
                    config_entries = c.entries.clone();
                }
                if let Some(ref c) = existing.worktree_config {
                    worktree_config_entries = c.entries.clone();
                }
            }

            for &idx in &selections {
                match &exportable[idx].key {
                    ExportKey::Url(u) => url = u.clone(),
                    ExportKey::Branch(b) => branch = Some(b.clone()),
                    ExportKey::Remote { name, config } => {
                        remotes.insert(name.clone(), config.clone());
                    }
                    ExportKey::Config { key, value } => {
                        config_entries.insert(key.clone(), value.clone());
                    }
                    ExportKey::WorktreeConfig { key, value } => {
                        worktree_config_entries.insert(key.clone(), value.clone());
                    }
                    ExportKey::Worktree => worktree = true,
                    ExportKey::PerWorktreeConfig => per_worktree_config = true,
                    ExportKey::Worktrees(wt) => worktrees = Some(wt.clone()),
                }
            }

            if url.is_empty() {
                return Err(anyhow!(
                    "no URL available for git entry; add an origin remote first"
                ));
            }

            let git_repo_config = GitRepoConfig {
                url,
                branch,
                tag: existing_entry.and_then(|e| e.tag.clone()),
                rev: existing_entry.and_then(|e| e.rev.clone()),
                recurse_submodules: existing_entry
                    .map(|e| e.recurse_submodules)
                    .unwrap_or(false),
                worktree,
                per_worktree_config,
                worktrees,
                remotes: if remotes.is_empty() {
                    None
                } else {
                    Some(remotes)
                },
                config: if config_entries.is_empty() {
                    None
                } else {
                    Some(GitConfig {
                        entries: config_entries,
                    })
                },
                worktree_config: if worktree_config_entries.is_empty() {
                    None
                } else {
                    Some(GitConfig {
                        entries: worktree_config_entries,
                    })
                },
            };

            // ── Update overlay descriptor ─────────────────────────────

            let overlay_root = overlay.root.clone();
            let rel_path_owned = rel_path_str.to_string();
            let git_repo_config_owned = git_repo_config.clone();
            tokio::task::spawn_blocking(move || {
                update_overlay_descriptor(&overlay_root, &rel_path_owned, &git_repo_config_owned)
            })
            .await??;

            println!(
                "{} {} {} {} {}",
                emojis::SPARKLE,
                style::white_b("Exported git config for"),
                style::cyan(rel_path_str),
                style::white_b("to overlay"),
                style::cyan(&overlay.name),
            );
        }
    }

    // ── Write overlay name to git config ─────────────────────────────────

    set_overlay_config(&git_repo, &overlay.name)?;
    println!(
        "{} {} {}",
        emojis::CHECKMARK,
        style::white("Set over.overlay ="),
        style::cyan(&overlay.name),
    );

    Ok(())
}

/// Find the overlay descriptor file in the given directory.
///
/// Probes for `over.{yml,yaml,toml}` and returns the path and format of
/// the first match found.  If none exists, defaults to `over.yaml`.
fn find_descriptor(overlay_root: &Path) -> (PathBuf, Format) {
    for ext in overlays::EXTENSIONS {
        let path = overlay_root.join(format!("{}.{}", overlays::BASENAME, ext));
        if path.exists() {
            let format = match *ext {
                "toml" => Format::Toml,
                _ => Format::Yaml,
            };
            return (path, format);
        }
    }
    // Default to YAML when no descriptor exists yet
    (
        overlay_root.join(format!("{}.yaml", overlays::BASENAME)),
        Format::Yaml,
    )
}

/// Build a git entry as a generic map suitable for both TOML and YAML serialization.
fn build_git_entry(config: &GitRepoConfig) -> serde_yml::Mapping {
    let mut entry = serde_yml::Mapping::new();
    entry.insert(
        serde_yml::Value::String("url".into()),
        serde_yml::Value::String(config.url.clone()),
    );

    if let Some(ref branch) = config.branch {
        entry.insert(
            serde_yml::Value::String("branch".into()),
            serde_yml::Value::String(branch.clone()),
        );
    }
    if let Some(ref tag) = config.tag {
        entry.insert(
            serde_yml::Value::String("tag".into()),
            serde_yml::Value::String(tag.clone()),
        );
    }
    if let Some(ref rev) = config.rev {
        entry.insert(
            serde_yml::Value::String("rev".into()),
            serde_yml::Value::String(rev.clone()),
        );
    }
    if config.recurse_submodules {
        entry.insert(
            serde_yml::Value::String("recurse_submodules".into()),
            serde_yml::Value::Bool(true),
        );
    }
    if config.worktree {
        entry.insert(
            serde_yml::Value::String("worktree".into()),
            serde_yml::Value::Bool(true),
        );
    }
    // Only emit per_worktree_config when it differs from the default
    // (default is true when worktree is true, false otherwise)
    if config.per_worktree_config != config.worktree {
        entry.insert(
            serde_yml::Value::String("per_worktree_config".into()),
            serde_yml::Value::Bool(config.per_worktree_config),
        );
    }
    if let Some(ref worktrees) = config.worktrees {
        let mut wt = serde_yml::Mapping::new();
        for (name, wt_entry) in worktrees {
            if wt_entry.config.is_some() {
                // Detailed form: { branch: "...", config: { ... } }
                let mut wt_detail = serde_yml::Mapping::new();
                wt_detail.insert(
                    serde_yml::Value::String("branch".into()),
                    serde_yml::Value::String(wt_entry.branch.clone()),
                );
                if let Some(ref wt_config) = wt_entry.config {
                    let mut config_map = serde_yml::Mapping::new();
                    for (key, value) in &wt_config.entries {
                        config_map.insert(
                            serde_yml::Value::String(key.clone()),
                            serde_yml::Value::String(value.clone()),
                        );
                    }
                    wt_detail.insert(
                        serde_yml::Value::String("config".into()),
                        serde_yml::Value::Mapping(config_map),
                    );
                }
                wt.insert(
                    serde_yml::Value::String(name.clone()),
                    serde_yml::Value::Mapping(wt_detail),
                );
            } else {
                // Simple form: just the branch name
                wt.insert(
                    serde_yml::Value::String(name.clone()),
                    serde_yml::Value::String(wt_entry.branch.clone()),
                );
            }
        }
        entry.insert(
            serde_yml::Value::String("worktrees".into()),
            serde_yml::Value::Mapping(wt),
        );
    }
    if let Some(ref remotes) = config.remotes {
        let mut remotes_map = serde_yml::Mapping::new();
        for (name, remote_cfg) in remotes {
            let mut remote_entry = serde_yml::Mapping::new();
            remote_entry.insert(
                serde_yml::Value::String("url".into()),
                serde_yml::Value::String(remote_cfg.url.clone()),
            );
            if let Some(ref fetch) = remote_cfg.fetch {
                remote_entry.insert(
                    serde_yml::Value::String("fetch".into()),
                    serde_yml::Value::String(fetch.clone()),
                );
            }
            if let Some(ref push) = remote_cfg.push {
                remote_entry.insert(
                    serde_yml::Value::String("push".into()),
                    serde_yml::Value::String(push.clone()),
                );
            }
            if let Some(ref tagopt) = remote_cfg.tagopt {
                remote_entry.insert(
                    serde_yml::Value::String("tagopt".into()),
                    serde_yml::Value::String(tagopt.clone()),
                );
            }
            for (k, v) in &remote_cfg.extras {
                remote_entry.insert(
                    serde_yml::Value::String(k.clone()),
                    serde_yml::Value::String(v.clone()),
                );
            }
            remotes_map.insert(
                serde_yml::Value::String(name.clone()),
                serde_yml::Value::Mapping(remote_entry),
            );
        }
        entry.insert(
            serde_yml::Value::String("remotes".into()),
            serde_yml::Value::Mapping(remotes_map),
        );
    }
    if let Some(ref git_config) = config.config {
        let mut config_map = serde_yml::Mapping::new();
        for (key, value) in &git_config.entries {
            config_map.insert(
                serde_yml::Value::String(key.clone()),
                serde_yml::Value::String(value.clone()),
            );
        }
        entry.insert(
            serde_yml::Value::String("config".into()),
            serde_yml::Value::Mapping(config_map),
        );
    }
    if let Some(ref wt_config) = config.worktree_config {
        let mut config_map = serde_yml::Mapping::new();
        for (key, value) in &wt_config.entries {
            config_map.insert(
                serde_yml::Value::String(key.clone()),
                serde_yml::Value::String(value.clone()),
            );
        }
        entry.insert(
            serde_yml::Value::String("worktree_config".into()),
            serde_yml::Value::Mapping(config_map),
        );
    }
    entry
}

/// Read-modify-write the overlay descriptor to add/merge a git entry.
///
/// Detects the existing descriptor format (TOML or YAML) and writes
/// back in the same format to avoid creating a conflicting file.
fn update_overlay_descriptor(
    overlay_root: &Path,
    rel_path: &str,
    config: &GitRepoConfig,
) -> Result<()> {
    let (descriptor_path, format) = find_descriptor(overlay_root);

    match format {
        Format::Toml => update_descriptor_toml(&descriptor_path, rel_path, config),
        Format::Yaml => update_descriptor_yaml(&descriptor_path, rel_path, config),
    }
}

fn update_descriptor_toml(
    descriptor_path: &Path,
    rel_path: &str,
    config: &GitRepoConfig,
) -> Result<()> {
    let content = if descriptor_path.exists() {
        std::fs::read_to_string(descriptor_path)?
    } else {
        String::new()
    };

    let mut doc: toml::Value = if content.is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        toml::from_str(&content)
            .map_err(|e| anyhow!("failed to parse {}: {}", descriptor_path.display(), e))?
    };

    let table = doc
        .as_table_mut()
        .ok_or_else(|| anyhow!("{} root is not a table", descriptor_path.display()))?;

    if !table.contains_key("git") {
        table.insert("git".to_string(), toml::Value::Table(toml::map::Map::new()));
    }

    let git_table = table
        .get_mut("git")
        .and_then(|v| v.as_table_mut())
        .ok_or_else(|| anyhow!("git section is not a table"))?;

    // Convert serde_yml::Mapping → toml::Value::Table
    let yaml_entry = build_git_entry(config);
    let toml_entry = yaml_mapping_to_toml(&yaml_entry)?;
    git_table.insert(rel_path.to_string(), toml_entry);

    let output = toml::to_string_pretty(&doc)
        .map_err(|e| anyhow!("failed to serialize {}: {}", descriptor_path.display(), e))?;
    std::fs::write(descriptor_path, output)?;

    Ok(())
}

fn update_descriptor_yaml(
    descriptor_path: &Path,
    rel_path: &str,
    config: &GitRepoConfig,
) -> Result<()> {
    let content = if descriptor_path.exists() {
        std::fs::read_to_string(descriptor_path)?
    } else {
        String::new()
    };

    let mut doc: serde_yml::Value = if content.is_empty() {
        serde_yml::Value::Mapping(serde_yml::Mapping::new())
    } else {
        serde_yml::from_str(&content)
            .map_err(|e| anyhow!("failed to parse {}: {}", descriptor_path.display(), e))?
    };

    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("{} root is not a mapping", descriptor_path.display()))?;

    let git_key = serde_yml::Value::String("git".into());
    if !root.contains_key(&git_key) {
        root.insert(
            git_key.clone(),
            serde_yml::Value::Mapping(serde_yml::Mapping::new()),
        );
    }

    let git_map = root
        .get_mut(&git_key)
        .and_then(|v| v.as_mapping_mut())
        .ok_or_else(|| anyhow!("git section is not a mapping"))?;

    let entry = build_git_entry(config);
    git_map.insert(
        serde_yml::Value::String(rel_path.to_string()),
        serde_yml::Value::Mapping(entry),
    );

    let output = serde_yml::to_string(&doc)
        .map_err(|e| anyhow!("failed to serialize {}: {}", descriptor_path.display(), e))?;
    std::fs::write(descriptor_path, output)?;

    Ok(())
}

/// Convert a serde_yml::Mapping to a toml::Value::Table.
fn yaml_mapping_to_toml(mapping: &serde_yml::Mapping) -> Result<toml::Value> {
    let mut table = toml::map::Map::new();
    for (k, v) in mapping {
        let key = k
            .as_str()
            .ok_or_else(|| anyhow!("non-string key in git entry"))?
            .to_string();
        let value = yaml_value_to_toml(v)?;
        table.insert(key, value);
    }
    Ok(toml::Value::Table(table))
}

fn yaml_value_to_toml(v: &serde_yml::Value) -> Result<toml::Value> {
    match v {
        serde_yml::Value::String(s) => Ok(toml::Value::String(s.clone())),
        serde_yml::Value::Bool(b) => Ok(toml::Value::Boolean(*b)),
        serde_yml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                Err(anyhow!("unsupported number in git entry"))
            }
        }
        serde_yml::Value::Mapping(m) => yaml_mapping_to_toml(m),
        _ => Err(anyhow!("unsupported value type in git entry")),
    }
}

/// Read per-worktree config entries from a named worktree's `config.worktree` file.
///
/// Looks for `.git/worktrees/<name>/config.worktree` and reads user-relevant
/// entries (skipping auto-managed keys like `core.bare`).  Returns `None` when
/// no config file exists or it contains no exportable entries.
fn read_worktree_config(repo: &git2::Repository, name: &str) -> Option<GitConfig> {
    let wt_config_path = repo
        .path()
        .join("worktrees")
        .join(name)
        .join("config.worktree");
    if !wt_config_path.exists() {
        return None;
    }
    let mut cfg = git2::Config::open(&wt_config_path).ok()?;
    let snap = cfg.snapshot().ok()?;
    let mut entries = HashMap::new();
    // Read known per-worktree config keys (skip auto-managed core.bare)
    for key in &[
        "user.name",
        "user.email",
        "core.sparseCheckout",
        "core.sparseCheckoutCone",
    ] {
        if let Ok(value) = snap.get_str(key) {
            entries.insert(key.to_string(), value.to_string());
        }
    }
    if entries.is_empty() {
        None
    } else {
        Some(GitConfig { entries })
    }
}

/// Resolve the branch checked out in a named worktree.
///
/// Opens the worktree by name, looks up its HEAD, and returns the
/// short branch name (e.g. `"main"`).  Returns `None` when the branch
/// cannot be determined (detached HEAD, pruned worktree, etc.).
fn resolve_worktree_branch(repo: &git2::Repository, name: &str) -> Option<String> {
    let wt = repo.find_worktree(name).ok()?;
    let wt_repo = git2::Repository::open_from_worktree(&wt).ok()?;
    let head = wt_repo.head().ok()?;
    head.shorthand().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::git::config::GitConfig;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;

    // ── build_git_entry ─────────────────────────────────────────────────

    #[test]
    fn test_build_git_entry_url_only() {
        let config = GitRepoConfig {
            url: "https://github.com/user/repo.git".into(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let entry = build_git_entry(&config);
        assert_eq!(
            entry.get(&serde_yml::Value::String("url".into())),
            Some(&serde_yml::Value::String(
                "https://github.com/user/repo.git".into()
            )),
        );
        assert_eq!(entry.len(), 1);
    }

    #[test]
    fn test_build_git_entry_with_branch() {
        let config = GitRepoConfig {
            url: "git@github.com:user/repo.git".into(),
            branch: Some("main".into()),
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let entry = build_git_entry(&config);
        assert_eq!(
            entry.get(&serde_yml::Value::String("branch".into())),
            Some(&serde_yml::Value::String("main".into())),
        );
    }

    #[test]
    fn test_build_git_entry_with_tag() {
        let config = GitRepoConfig {
            url: "https://example.com/repo.git".into(),
            branch: None,
            tag: Some("v1.0.0".into()),
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let entry = build_git_entry(&config);
        assert_eq!(
            entry.get(&serde_yml::Value::String("tag".into())),
            Some(&serde_yml::Value::String("v1.0.0".into())),
        );
    }

    #[test]
    fn test_build_git_entry_with_rev() {
        let config = GitRepoConfig {
            url: "https://example.com/repo.git".into(),
            branch: None,
            tag: None,
            rev: Some("abc123".into()),
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let entry = build_git_entry(&config);
        assert_eq!(
            entry.get(&serde_yml::Value::String("rev".into())),
            Some(&serde_yml::Value::String("abc123".into())),
        );
    }

    #[test]
    fn test_build_git_entry_with_recurse_submodules() {
        let config = GitRepoConfig {
            url: "https://example.com/repo.git".into(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: true,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let entry = build_git_entry(&config);
        assert_eq!(
            entry.get(&serde_yml::Value::String("recurse_submodules".into())),
            Some(&serde_yml::Value::Bool(true)),
        );
    }

    #[test]
    fn test_build_git_entry_with_worktree() {
        let config = GitRepoConfig {
            url: "https://example.com/repo.git".into(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: true,
            per_worktree_config: true,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let entry = build_git_entry(&config);
        assert_eq!(
            entry.get(&serde_yml::Value::String("worktree".into())),
            Some(&serde_yml::Value::Bool(true)),
        );
    }

    #[test]
    fn test_build_git_entry_with_worktrees() {
        let mut worktrees = HashMap::new();
        worktrees.insert(
            "feature".to_string(),
            WorktreeEntry {
                branch: "feature-branch".to_string(),
                config: None,
            },
        );
        let config = GitRepoConfig {
            url: "https://example.com/repo.git".into(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: Some(worktrees),
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let entry = build_git_entry(&config);
        let wt = entry
            .get(&serde_yml::Value::String("worktrees".into()))
            .unwrap()
            .as_mapping()
            .unwrap();
        assert_eq!(
            wt.get(&serde_yml::Value::String("feature".into())),
            Some(&serde_yml::Value::String("feature-branch".into())),
        );
    }

    #[test]
    fn test_build_git_entry_with_remotes() {
        let mut remotes = HashMap::new();
        remotes.insert(
            "upstream".into(),
            RemoteConfig {
                url: "https://github.com/upstream/repo.git".into(),
                fetch: Some("+refs/heads/*:refs/remotes/upstream/*".into()),
                push: None,
                tagopt: None,
                extras: HashMap::new(),
            },
        );
        let config = GitRepoConfig {
            url: "git@github.com:user/repo.git".into(),
            branch: Some("main".into()),
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: Some(remotes),
            config: None,
            worktree_config: None,
        };
        let entry = build_git_entry(&config);
        let remotes_map = entry
            .get(&serde_yml::Value::String("remotes".into()))
            .unwrap()
            .as_mapping()
            .unwrap();
        let upstream = remotes_map
            .get(&serde_yml::Value::String("upstream".into()))
            .unwrap()
            .as_mapping()
            .unwrap();
        assert_eq!(
            upstream.get(&serde_yml::Value::String("url".into())),
            Some(&serde_yml::Value::String(
                "https://github.com/upstream/repo.git".into()
            )),
        );
        assert_eq!(
            upstream.get(&serde_yml::Value::String("fetch".into())),
            Some(&serde_yml::Value::String(
                "+refs/heads/*:refs/remotes/upstream/*".into()
            )),
        );
    }

    #[test]
    fn test_build_git_entry_with_config() {
        let mut entries = HashMap::new();
        entries.insert("user.name".into(), "Test User".into());
        entries.insert("user.email".into(), "test@example.com".into());
        let config = GitRepoConfig {
            url: "git@github.com:user/repo.git".into(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: Some(GitConfig { entries }),
            worktree_config: None,
        };
        let entry = build_git_entry(&config);
        let config_map = entry
            .get(&serde_yml::Value::String("config".into()))
            .unwrap()
            .as_mapping()
            .unwrap();
        assert_eq!(
            config_map.get(&serde_yml::Value::String("user.name".into())),
            Some(&serde_yml::Value::String("Test User".into())),
        );
        assert_eq!(
            config_map.get(&serde_yml::Value::String("user.email".into())),
            Some(&serde_yml::Value::String("test@example.com".into())),
        );
    }

    // ── yaml_value_to_toml / yaml_mapping_to_toml ───────────────────────

    #[test]
    fn test_yaml_value_to_toml_string() {
        let v = serde_yml::Value::String("hello".into());
        assert_eq!(
            yaml_value_to_toml(&v).unwrap(),
            toml::Value::String("hello".into())
        );
    }

    #[test]
    fn test_yaml_value_to_toml_bool() {
        let v = serde_yml::Value::Bool(true);
        assert_eq!(yaml_value_to_toml(&v).unwrap(), toml::Value::Boolean(true));
    }

    #[test]
    fn test_yaml_value_to_toml_integer() {
        let v = serde_yml::Value::Number(serde_yml::Number::from(42));
        assert_eq!(yaml_value_to_toml(&v).unwrap(), toml::Value::Integer(42));
    }

    #[test]
    fn test_yaml_value_to_toml_float() {
        let v = serde_yml::Value::Number(serde_yml::Number::from(3.14));
        let result = yaml_value_to_toml(&v).unwrap();
        match result {
            toml::Value::Float(f) => assert!((f - 3.14).abs() < f64::EPSILON),
            _ => panic!("expected float"),
        }
    }

    #[test]
    fn test_yaml_value_to_toml_mapping() {
        let mut m = serde_yml::Mapping::new();
        m.insert(
            serde_yml::Value::String("key".into()),
            serde_yml::Value::String("value".into()),
        );
        let result = yaml_value_to_toml(&serde_yml::Value::Mapping(m)).unwrap();
        match result {
            toml::Value::Table(t) => {
                assert_eq!(t.get("key"), Some(&toml::Value::String("value".into())));
            }
            _ => panic!("expected table"),
        }
    }

    #[test]
    fn test_yaml_value_to_toml_null_errors() {
        let v = serde_yml::Value::Null;
        assert!(yaml_value_to_toml(&v).is_err());
    }

    #[test]
    fn test_yaml_value_to_toml_sequence_errors() {
        let v = serde_yml::Value::Sequence(vec![serde_yml::Value::String("a".into())]);
        assert!(yaml_value_to_toml(&v).is_err());
    }

    #[test]
    fn test_yaml_mapping_to_toml_nested() {
        let mut inner = serde_yml::Mapping::new();
        inner.insert(
            serde_yml::Value::String("nested_key".into()),
            serde_yml::Value::Bool(true),
        );
        let mut outer = serde_yml::Mapping::new();
        outer.insert(
            serde_yml::Value::String("str".into()),
            serde_yml::Value::String("hello".into()),
        );
        outer.insert(
            serde_yml::Value::String("inner".into()),
            serde_yml::Value::Mapping(inner),
        );

        let result = yaml_mapping_to_toml(&outer).unwrap();
        match result {
            toml::Value::Table(t) => {
                assert_eq!(t.get("str"), Some(&toml::Value::String("hello".into())));
                let inner_table = t.get("inner").unwrap().as_table().unwrap();
                assert_eq!(
                    inner_table.get("nested_key"),
                    Some(&toml::Value::Boolean(true))
                );
            }
            _ => panic!("expected table"),
        }
    }

    #[test]
    fn test_yaml_mapping_to_toml_non_string_key_errors() {
        let mut m = serde_yml::Mapping::new();
        m.insert(
            serde_yml::Value::Number(serde_yml::Number::from(42)),
            serde_yml::Value::String("val".into()),
        );
        assert!(yaml_mapping_to_toml(&m).is_err());
    }

    // ── find_descriptor ─────────────────────────────────────────────────

    #[test]
    fn test_find_descriptor_yaml() {
        let td = TempDir::new().unwrap();
        td.child("over.yaml").write_str("target: ~").unwrap();
        let (path, format) = find_descriptor(td.path());
        assert_eq!(path, td.path().join("over.yaml"));
        assert!(matches!(format, Format::Yaml));
    }

    #[test]
    fn test_find_descriptor_yml() {
        let td = TempDir::new().unwrap();
        td.child("over.yml").write_str("target: ~").unwrap();
        let (path, format) = find_descriptor(td.path());
        assert_eq!(path, td.path().join("over.yml"));
        assert!(matches!(format, Format::Yaml));
    }

    #[test]
    fn test_find_descriptor_toml() {
        let td = TempDir::new().unwrap();
        td.child("over.toml").write_str("target = \"~\"").unwrap();
        let (path, format) = find_descriptor(td.path());
        assert_eq!(path, td.path().join("over.toml"));
        assert!(matches!(format, Format::Toml));
    }

    #[test]
    fn test_find_descriptor_default_when_none() {
        let td = TempDir::new().unwrap();
        let (path, format) = find_descriptor(td.path());
        assert_eq!(path, td.path().join("over.yaml"));
        assert!(matches!(format, Format::Yaml));
    }

    // ── update_descriptor_yaml ──────────────────────────────────────────

    #[test]
    fn test_update_descriptor_yaml_new_file() {
        let td = TempDir::new().unwrap();
        let config = GitRepoConfig {
            url: "https://github.com/user/repo.git".into(),
            branch: Some("main".into()),
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let descriptor = td.path().join("over.yaml");
        update_descriptor_yaml(&descriptor, "projects/myapp", &config).unwrap();

        let content = std::fs::read_to_string(&descriptor).unwrap();
        assert!(content.contains("projects/myapp"));
        assert!(content.contains("https://github.com/user/repo.git"));
        assert!(content.contains("main"));
    }

    #[test]
    fn test_update_descriptor_yaml_existing_file() {
        let td = TempDir::new().unwrap();
        td.child("over.yaml")
            .write_str("target: ~\nuses:\n  - base\n")
            .unwrap();

        let config = GitRepoConfig {
            url: "git@github.com:user/repo.git".into(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let descriptor = td.path().join("over.yaml");
        update_descriptor_yaml(&descriptor, "my/repo", &config).unwrap();

        let content = std::fs::read_to_string(&descriptor).unwrap();
        // Should preserve existing content
        assert!(content.contains("target"));
        assert!(content.contains("base"));
        // And add new git entry
        assert!(content.contains("my/repo"));
        assert!(content.contains("git@github.com:user/repo.git"));
    }

    // ── update_descriptor_toml ──────────────────────────────────────────

    #[test]
    fn test_update_descriptor_toml_new_file() {
        let td = TempDir::new().unwrap();
        let config = GitRepoConfig {
            url: "https://github.com/user/repo.git".into(),
            branch: Some("main".into()),
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let descriptor = td.path().join("over.toml");
        update_descriptor_toml(&descriptor, "projects/myapp", &config).unwrap();

        let content = std::fs::read_to_string(&descriptor).unwrap();
        assert!(content.contains("projects/myapp"));
        assert!(content.contains("https://github.com/user/repo.git"));
        assert!(content.contains("main"));
    }

    #[test]
    fn test_update_descriptor_toml_existing_file() {
        let td = TempDir::new().unwrap();
        td.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"base\"]\n")
            .unwrap();

        let config = GitRepoConfig {
            url: "git@github.com:user/repo.git".into(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        let descriptor = td.path().join("over.toml");
        update_descriptor_toml(&descriptor, "my/repo", &config).unwrap();

        let content = std::fs::read_to_string(&descriptor).unwrap();
        // Should preserve existing content
        assert!(content.contains("target"));
        assert!(content.contains("base"));
        // And add new git entry
        assert!(content.contains("my/repo"));
        assert!(content.contains("git@github.com:user/repo.git"));
    }

    // ── update_overlay_descriptor (integration) ─────────────────────────

    #[test]
    fn test_update_overlay_descriptor_detects_yaml() {
        let td = TempDir::new().unwrap();
        td.child("over.yaml").write_str("target: ~\n").unwrap();

        let config = GitRepoConfig {
            url: "https://example.com/repo.git".into(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        update_overlay_descriptor(td.path(), "my/path", &config).unwrap();

        let content = std::fs::read_to_string(td.path().join("over.yaml")).unwrap();
        assert!(content.contains("my/path"));
        assert!(content.contains("https://example.com/repo.git"));
    }

    #[test]
    fn test_update_overlay_descriptor_detects_toml() {
        let td = TempDir::new().unwrap();
        td.child("over.toml").write_str("target = \"~\"\n").unwrap();

        let config = GitRepoConfig {
            url: "https://example.com/repo.git".into(),
            branch: None,
            tag: None,
            rev: None,
            recurse_submodules: false,
            worktree: false,
            per_worktree_config: false,
            worktrees: None,
            remotes: None,
            config: None,
            worktree_config: None,
        };
        update_overlay_descriptor(td.path(), "my/path", &config).unwrap();

        let content = std::fs::read_to_string(td.path().join("over.toml")).unwrap();
        assert!(content.contains("my/path"));
        assert!(content.contains("https://example.com/repo.git"));
    }
}
