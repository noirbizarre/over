use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::Args;
use config::{Config, File, FileFormat, FileSourceFile};
use dialoguer::{Input, MultiSelect};
use dirs::home_dir;

use crate::exec::Context;
use crate::overlays::{BASENAME, DEFAULT_TARGET, Format, Repository};
use crate::ui::style::{self, DialogTheme};

use super::CLI;

#[derive(Args, Debug)]
pub struct Params {
    /// Overlay path within the repository (e.g. apps/myapp)
    path: Option<String>,

    /// Target directory for the overlay
    target: Option<String>,

    /// Overlay descriptor format
    #[clap(long, short, value_enum)]
    format: Option<Format>,

    /// Run without applying changes
    #[clap(long, short = 'n', help = "Run without applying changes")]
    dry_run: bool,

    /// Overwrite without prompting
    #[clap(long, help = "Overwrite without prompting")]
    force: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    let home = cli.resolve_home()?;
    let repo = Repository::new(home.clone());
    let theme = DialogTheme::default();

    // Resolve format: --format flag > root config preference > default (toml)
    let format = args
        .format
        .or_else(|| repo.preferred_format())
        .unwrap_or_default();

    // Resolve overlay path (prompt if not provided)
    let path = match &args.path {
        Some(p) => p.clone(),
        None => Input::with_theme(&theme)
            .with_prompt("Overlay path")
            .interact_text()
            .map_err(|e| anyhow!("prompt cancelled: {}", e))?,
    };

    // Resolve target (prompt with default if not provided)
    let target_str = match &args.target {
        Some(t) => t.clone(),
        None => Input::with_theme(&theme)
            .with_prompt("Target directory")
            .default(DEFAULT_TARGET.to_string())
            .interact_text()
            .map_err(|e| anyhow!("prompt cancelled: {}", e))?,
    };

    let overlay_root = home.join(&path);

    // Check the overlay does not already exist
    let descriptor = overlay_root.join(format!("{}.{}", BASENAME, format.extension()));
    if descriptor.exists() && !args.force {
        return Err(anyhow!(
            "overlay already exists at {}",
            overlay_root.display()
        ));
    }

    // Resolve the actual target path for filesystem operations
    let resolved_target = resolve_target_path(&target_str)?;

    if cli.debug {
        eprintln!("overlay root: {}", overlay_root.display());
        eprintln!("descriptor:   {}", descriptor.display());
        eprintln!("target:       {}", target_str);
        eprintln!("resolved:     {}", resolved_target.display());
        eprintln!("format:       {}", format);
    }

    // Collect git entries and files to absorb before writing anything
    let mut git_entries: HashMap<String, String> = HashMap::new();
    let mut files_to_absorb: Vec<PathBuf> = Vec::new();

    if resolved_target.is_dir() && !args.force {
        discover_and_prompt_absorb(
            &resolved_target,
            &theme,
            &mut git_entries,
            &mut files_to_absorb,
        )?;
    }

    // Create the overlay directory
    if !args.dry_run {
        fs::create_dir_all(&overlay_root)?;
    }
    println!(
        "{} {} {}",
        style::white_b("Created overlay directory"),
        style::cyan(&path),
        style::white_b(&format!("({})", format)),
    );

    // Write the overlay descriptor
    // Only include the target if it differs from the inherited value
    let inherited_target = resolve_inherited_target(&home, &overlay_root);
    let descriptor_target = if target_str == inherited_target {
        None
    } else {
        Some(target_str.as_str())
    };
    let content = build_descriptor(descriptor_target, format, &git_entries);
    if args.dry_run {
        println!("{}", style::white_b("Descriptor content (dry-run):"));
        println!("{}", content);
    } else {
        fs::write(&descriptor, &content)?;
    }

    // Absorb selected files/directories
    if !files_to_absorb.is_empty() {
        let overlay = repo.get(&path)?;
        let ctx = Context::builder()
            .dry_run(args.dry_run)
            .debug(cli.debug)
            .verbose(cli.verbose)
            .force(args.force)
            .root(home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?)
            .repository(repo)
            .overlay(overlay.clone())
            .build();
        let ctx = std::sync::Arc::new(ctx);

        overlay.add_files(&ctx, &files_to_absorb).await?;
    }

    println!(
        "{} {} {} {}{}",
        style::white_b("Overlay"),
        style::cyan(&path),
        style::white_b("created targeting"),
        style::cyan(&target_str),
        if files_to_absorb.is_empty() {
            String::new()
        } else {
            format!(
                " ({} {})",
                files_to_absorb.len(),
                if files_to_absorb.len() == 1 {
                    "path absorbed"
                } else {
                    "paths absorbed"
                },
            )
        },
    );

    Ok(())
}

/// Expand `~` to the user home directory.
fn resolve_target_path(target: &str) -> Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    let path = match target {
        "~" => home,
        t if t.starts_with("~/") => home.join(t.strip_prefix("~/").unwrap()),
        t => PathBuf::from(t),
    };
    Ok(path)
}

/// Walk the target directory, categorise entries, and prompt the user to
/// select which files/directories/repos to absorb into the new overlay.
fn discover_and_prompt_absorb(
    target: &Path,
    theme: &DialogTheme,
    git_entries: &mut HashMap<String, String>,
    files_to_absorb: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = fs::read_dir(target)?;

    let mut items: Vec<(String, PathBuf, bool)> = Vec::new(); // (label, path, is_git_repo)

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_str().unwrap_or_default().to_string();
        let is_git = path.is_dir() && path.join(".git").exists();
        let label = if is_git {
            format!("{name}/ (git repo)")
        } else if path.is_dir() {
            format!("{name}/")
        } else {
            name.clone()
        };
        items.push((label, path, is_git));
    }

    if items.is_empty() {
        return Ok(());
    }

    items.sort_by(|a, b| a.0.cmp(&b.0));

    let labels: Vec<&str> = items.iter().map(|(l, _, _)| l.as_str()).collect();

    let selections = MultiSelect::with_theme(theme)
        .with_prompt("Target exists — select entries to absorb")
        .items(&labels)
        .interact()
        .map_err(|e| anyhow!("prompt cancelled: {}", e))?;

    for idx in selections {
        let (_, ref path, is_git) = items[idx];
        if is_git {
            // Extract remote origin URL via git2
            if let Some((name, url)) = extract_git_remote(path) {
                git_entries.insert(name, url);
            }
        } else {
            files_to_absorb.push(path.clone());
        }
    }

    Ok(())
}

/// Open a git repository and extract the origin remote URL.
/// Returns `(directory_name, url)`.
fn extract_git_remote(path: &Path) -> Option<(String, String)> {
    let repo = git2::Repository::open(path).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url()?.to_string();
    let name = path.file_name()?.to_str()?.to_string();
    Some((name, url))
}

/// Resolve the target that would be inherited from ancestor overlay configs.
///
/// Walks from the overlay's parent directory up to the repository root,
/// collecting any `over.{toml,yaml,yml}` configs that define a `target`.
/// Returns the effective inherited target, falling back to [`DEFAULT_TARGET`].
fn resolve_inherited_target(repo_root: &Path, overlay_root: &Path) -> String {
    let mut sources: Vec<File<FileSourceFile, FileFormat>> = Vec::new();
    let mut dir = overlay_root;

    // Walk from overlay's parent up to repo root
    while let Some(parent) = dir.parent() {
        dir = parent;
        let basename = dir.join(BASENAME);
        if let Some(basename_str) = basename.to_str() {
            sources.push(File::with_name(basename_str).required(false));
        }
        if dir == repo_root {
            break;
        }
    }

    if sources.is_empty() {
        return DEFAULT_TARGET.to_string();
    }

    // Reverse so ancestor configs are lower priority (repo root first)
    sources.reverse();

    Config::builder()
        .add_source(sources)
        .set_default("target", DEFAULT_TARGET)
        .ok()
        .and_then(|b| b.build().ok())
        .and_then(|c| c.get_string("target").ok())
        .unwrap_or_else(|| DEFAULT_TARGET.to_string())
}

/// Build the overlay descriptor file content in the given format.
fn build_descriptor(
    target: Option<&str>,
    format: Format,
    git_entries: &HashMap<String, String>,
) -> String {
    match format {
        Format::Toml => build_toml_descriptor(target, git_entries),
        Format::Yaml => build_yaml_descriptor(target, git_entries),
    }
}

fn build_toml_descriptor(target: Option<&str>, git_entries: &HashMap<String, String>) -> String {
    let mut out = String::new();

    if let Some(target) = target {
        out.push_str(&format!("target = \"{target}\"\n"));
    }

    if !git_entries.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("[git]\n");
        let mut entries: Vec<_> = git_entries.iter().collect();
        entries.sort_by_key(|(k, _)| *k);
        for (name, url) in entries {
            out.push_str(&format!("{name} = {{ url = \"{url}\" }}\n"));
        }
    }

    out
}

fn build_yaml_descriptor(target: Option<&str>, git_entries: &HashMap<String, String>) -> String {
    let mut out = String::new();

    if let Some(target) = target {
        out.push_str(&format!("target: \"{target}\"\n"));
    }

    if !git_entries.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("git:\n");
        let mut entries: Vec<_> = git_entries.iter().collect();
        entries.sort_by_key(|(k, _)| *k);
        for (name, url) in entries {
            out.push_str(&format!("  {name}:\n    url: \"{url}\"\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    // ── resolve_target_path ─────────────────────────────────────────────

    #[test]
    fn test_resolve_target_path_tilde() {
        let home = home_dir().unwrap();
        assert_eq!(resolve_target_path("~").unwrap(), home);
    }

    #[test]
    fn test_resolve_target_path_tilde_subdir() {
        let home = home_dir().unwrap();
        assert_eq!(
            resolve_target_path("~/apps/foo").unwrap(),
            home.join("apps/foo")
        );
    }

    #[test]
    fn test_resolve_target_path_absolute() {
        #[cfg(unix)]
        let abs_path = "/tmp/overlay-test";
        #[cfg(windows)]
        let abs_path = "C:\\overlay-test";

        assert_eq!(
            resolve_target_path(abs_path).unwrap(),
            PathBuf::from(abs_path)
        );
    }

    #[test]
    fn test_resolve_target_path_relative() {
        assert_eq!(
            resolve_target_path("relative/path").unwrap(),
            PathBuf::from("relative/path")
        );
    }

    #[test]
    fn test_resolve_target_path_tilde_deep_nesting() {
        let home = home_dir().unwrap();
        assert_eq!(
            resolve_target_path("~/a/b/c/d").unwrap(),
            home.join("a/b/c/d")
        );
    }

    // ── build_toml_descriptor ───────────────────────────────────────────

    #[test]
    fn test_build_toml_descriptor_no_target() {
        let content = build_toml_descriptor(None, &HashMap::new());
        assert_eq!(content, "");
    }

    #[test]
    fn test_build_toml_descriptor_with_target() {
        let content = build_toml_descriptor(Some("~/apps/myapp"), &HashMap::new());
        assert_eq!(content, "target = \"~/apps/myapp\"\n");
    }

    #[test]
    fn test_build_toml_descriptor_with_git() {
        let mut git = HashMap::new();
        git.insert(
            "myrepo".to_string(),
            "https://github.com/user/myrepo.git".to_string(),
        );
        let content = build_toml_descriptor(Some("~/apps/myapp"), &git);
        assert_eq!(
            content,
            "target = \"~/apps/myapp\"\n\n[git]\nmyrepo = { url = \"https://github.com/user/myrepo.git\" }\n"
        );
    }

    #[test]
    fn test_build_toml_descriptor_git_only() {
        let mut git = HashMap::new();
        git.insert("zrepo".to_string(), "https://example.com/z.git".to_string());
        git.insert("arepo".to_string(), "https://example.com/a.git".to_string());
        let content = build_toml_descriptor(None, &git);
        assert!(
            !content.contains("target"),
            "should not contain target when None"
        );
        let a_pos = content.find("arepo").unwrap();
        let z_pos = content.find("zrepo").unwrap();
        assert!(a_pos < z_pos, "entries should be sorted alphabetically");
    }

    #[test]
    fn test_build_toml_descriptor_git_sorted() {
        let mut git = HashMap::new();
        git.insert("zrepo".to_string(), "https://example.com/z.git".to_string());
        git.insert("arepo".to_string(), "https://example.com/a.git".to_string());
        let content = build_toml_descriptor(Some("~"), &git);
        let a_pos = content.find("arepo").unwrap();
        let z_pos = content.find("zrepo").unwrap();
        assert!(a_pos < z_pos, "entries should be sorted alphabetically");
    }

    #[test]
    fn test_build_toml_descriptor_absolute_target() {
        let content = build_toml_descriptor(Some("/opt/myapp"), &HashMap::new());
        assert_eq!(content, "target = \"/opt/myapp\"\n");
    }

    #[test]
    fn test_build_toml_descriptor_multiple_git() {
        let mut git = HashMap::new();
        git.insert("alpha".to_string(), "https://example.com/a.git".to_string());
        git.insert("beta".to_string(), "https://example.com/b.git".to_string());
        git.insert("gamma".to_string(), "https://example.com/g.git".to_string());
        let content = build_toml_descriptor(None, &git);
        assert!(content.contains("[git]"));
        assert!(content.contains("alpha = { url ="));
        assert!(content.contains("beta = { url ="));
        assert!(content.contains("gamma = { url ="));
    }

    // ── build_yaml_descriptor ───────────────────────────────────────────

    #[test]
    fn test_build_yaml_descriptor_no_target() {
        let content = build_yaml_descriptor(None, &HashMap::new());
        assert_eq!(content, "");
    }

    #[test]
    fn test_build_yaml_descriptor_with_target() {
        let content = build_yaml_descriptor(Some("~/apps/myapp"), &HashMap::new());
        assert_eq!(content, "target: \"~/apps/myapp\"\n");
    }

    #[test]
    fn test_build_yaml_descriptor_with_git() {
        let mut git = HashMap::new();
        git.insert(
            "myrepo".to_string(),
            "https://github.com/user/myrepo.git".to_string(),
        );
        let content = build_yaml_descriptor(Some("~/apps/myapp"), &git);
        assert_eq!(
            content,
            "target: \"~/apps/myapp\"\n\ngit:\n  myrepo:\n    url: \"https://github.com/user/myrepo.git\"\n"
        );
    }

    #[test]
    fn test_build_yaml_descriptor_git_sorted() {
        let mut git = HashMap::new();
        git.insert("zrepo".to_string(), "https://example.com/z.git".to_string());
        git.insert("arepo".to_string(), "https://example.com/a.git".to_string());
        let content = build_yaml_descriptor(None, &git);
        let a_pos = content.find("arepo").unwrap();
        let z_pos = content.find("zrepo").unwrap();
        assert!(a_pos < z_pos, "entries should be sorted alphabetically");
    }

    // ── build_descriptor (dispatch) ─────────────────────────────────────

    #[test]
    fn test_build_descriptor_dispatches_toml() {
        let content = build_descriptor(Some("~/test"), Format::Toml, &HashMap::new());
        assert!(content.starts_with("target = "));
    }

    #[test]
    fn test_build_descriptor_dispatches_yaml() {
        let content = build_descriptor(Some("~/test"), Format::Yaml, &HashMap::new());
        assert!(content.starts_with("target: "));
    }

    #[test]
    fn test_build_descriptor_no_target_toml() {
        let content = build_descriptor(None, Format::Toml, &HashMap::new());
        assert_eq!(content, "");
    }

    #[test]
    fn test_build_descriptor_no_target_yaml() {
        let content = build_descriptor(None, Format::Yaml, &HashMap::new());
        assert_eq!(content, "");
    }

    #[test]
    fn test_build_descriptor_toml_with_git() {
        let mut git = HashMap::new();
        git.insert("repo".to_string(), "https://example.com/r.git".to_string());
        let content = build_descriptor(Some("~/test"), Format::Toml, &git);
        assert!(content.contains("[git]"));
        assert!(content.contains("repo = { url ="));
    }

    #[test]
    fn test_build_descriptor_yaml_with_git() {
        let mut git = HashMap::new();
        git.insert("repo".to_string(), "https://example.com/r.git".to_string());
        let content = build_descriptor(Some("~/test"), Format::Yaml, &git);
        assert!(content.contains("git:"));
        assert!(content.contains("  repo:"));
        assert!(content.contains("    url:"));
    }

    // ── extract_git_remote ──────────────────────────────────────────────

    #[test]
    fn test_extract_git_remote_valid_repo() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("myrepo");
        fs::create_dir_all(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        repo.remote("origin", "https://github.com/user/myrepo.git")
            .unwrap();

        let result = extract_git_remote(&repo_path);
        assert!(result.is_some());
        let (name, url) = result.unwrap();
        assert_eq!(name, "myrepo");
        assert_eq!(url, "https://github.com/user/myrepo.git");
    }

    #[test]
    fn test_extract_git_remote_no_origin() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("norepo");
        fs::create_dir_all(&repo_path).unwrap();
        git2::Repository::init(&repo_path).unwrap();
        // No origin remote added

        let result = extract_git_remote(&repo_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_git_remote_not_a_repo() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("plaindir");
        fs::create_dir_all(&dir).unwrap();

        let result = extract_git_remote(&dir);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_git_remote_nonexistent_path() {
        let result = extract_git_remote(Path::new("/nonexistent/path/xyz"));
        assert!(result.is_none());
    }

    // ── resolve_inherited_target ────────────────────────────────────────

    #[test]
    fn test_resolve_inherited_target_no_parent_configs() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        let overlay_root = repo_root.join("myoverlay");
        fs::create_dir_all(&overlay_root).unwrap();

        let result = resolve_inherited_target(repo_root, &overlay_root);
        assert_eq!(result, DEFAULT_TARGET);
    }

    #[test]
    fn test_resolve_inherited_target_parent_defines_target() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        fs::write(repo_root.join("over.toml"), "target = \"~/Documents\"").unwrap();
        let overlay_root = repo_root.join("myoverlay");
        fs::create_dir_all(&overlay_root).unwrap();

        let result = resolve_inherited_target(repo_root, &overlay_root);
        assert_eq!(result, "~/Documents");
    }

    #[test]
    fn test_resolve_inherited_target_parent_defines_default() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        fs::write(repo_root.join("over.toml"), "target = \"~\"").unwrap();
        let overlay_root = repo_root.join("myoverlay");
        fs::create_dir_all(&overlay_root).unwrap();

        let result = resolve_inherited_target(repo_root, &overlay_root);
        assert_eq!(result, DEFAULT_TARGET);
    }

    #[test]
    fn test_resolve_inherited_target_nested_parent_wins() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        // Root defines one target
        fs::write(repo_root.join("over.toml"), "target = \"~/Documents\"").unwrap();
        // Intermediate dir defines a closer target
        let parent = repo_root.join("apps");
        fs::create_dir_all(&parent).unwrap();
        fs::write(parent.join("over.toml"), "target = \"~/apps\"").unwrap();

        let overlay_root = parent.join("myapp");
        fs::create_dir_all(&overlay_root).unwrap();

        let result = resolve_inherited_target(repo_root, &overlay_root);
        assert_eq!(result, "~/apps");
    }

    #[test]
    fn test_resolve_inherited_target_overlay_at_repo_root() {
        // Overlay is directly at the repo root — no parents to check
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path();
        fs::write(repo_root.join("over.toml"), "target = \"~/custom\"").unwrap();

        // overlay_root == repo_root: no ancestors to walk
        let result = resolve_inherited_target(repo_root, repo_root);
        assert_eq!(result, DEFAULT_TARGET);
    }

    // ── format resolution logic ─────────────────────────────────────────

    #[test]
    fn test_format_resolution_flag_wins() {
        let flag = Some(Format::Yaml);
        let config = Some(Format::Toml);
        let result = flag.or(config).unwrap_or_default();
        assert_eq!(result, Format::Yaml);
    }

    #[test]
    fn test_format_resolution_config_fallback() {
        let flag: Option<Format> = None;
        let config = Some(Format::Yaml);
        let result = flag.or(config).unwrap_or_default();
        assert_eq!(result, Format::Yaml);
    }

    #[test]
    fn test_format_resolution_default() {
        let flag: Option<Format> = None;
        let config: Option<Format> = None;
        let result = flag.or(config).unwrap_or_default();
        assert_eq!(result, Format::Toml);
    }
}
