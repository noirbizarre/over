use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::Args;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{FuzzySelect, MultiSelect};
use dirs::home_dir;

use crate::actions::git::config::{GitConfig, GitRepoConfig, RemoteConfig};
use crate::overlays::{self, Repository};
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
            .map(|p| matches!(p.key, ExportKey::Url(_) | ExportKey::Branch(_)))
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

            // Start from existing entry if present
            if let Some(existing) = existing_entry {
                url = existing.url.clone();
                branch = existing.branch.clone();
                if let Some(ref r) = existing.remotes {
                    remotes = r.clone();
                }
                if let Some(ref c) = existing.config {
                    config_entries = c.entries.clone();
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
                worktree: existing_entry.map(|e| e.worktree).unwrap_or(false),
                worktrees: existing_entry.and_then(|e| e.worktrees.clone()),
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

/// Supported overlay descriptor formats.
enum DescriptorFormat {
    Toml,
    Yaml,
}

/// Find the overlay descriptor file in the given directory.
///
/// Probes for `over.{yml,yaml,toml}` and returns the path and format of
/// the first match found.  If none exists, defaults to `over.yaml`.
fn find_descriptor(overlay_root: &Path) -> (PathBuf, DescriptorFormat) {
    for ext in overlays::EXTENSIONS {
        let path = overlay_root.join(format!("{}.{}", overlays::BASENAME, ext));
        if path.exists() {
            let format = match *ext {
                "toml" => DescriptorFormat::Toml,
                _ => DescriptorFormat::Yaml,
            };
            return (path, format);
        }
    }
    // Default to YAML when no descriptor exists yet
    (
        overlay_root.join(format!("{}.yaml", overlays::BASENAME)),
        DescriptorFormat::Yaml,
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
    if let Some(ref worktrees) = config.worktrees {
        let mut wt = serde_yml::Mapping::new();
        for (name, branch) in worktrees {
            wt.insert(
                serde_yml::Value::String(name.clone()),
                serde_yml::Value::String(branch.clone()),
            );
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
        DescriptorFormat::Toml => update_descriptor_toml(&descriptor_path, rel_path, config),
        DescriptorFormat::Yaml => update_descriptor_yaml(&descriptor_path, rel_path, config),
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
        content
            .parse()
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
