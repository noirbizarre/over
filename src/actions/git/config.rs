use std::collections::HashMap;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// Configuration for a single git repository entry in an overlay.
///
/// Supports two forms:
/// - **Simple**: just a URL string (e.g. `".tmux/plugins/tpm" = "https://..."`)
/// - **Detailed**: an object with `url` and optional fields
#[derive(Debug, Serialize, Clone)]
#[serde(from = "AllGitRepoForms")]
pub struct GitRepoConfig {
    /// The clone URL (required).
    pub url: String,
    /// Checkout a specific branch after cloning.
    pub branch: Option<String>,
    /// Checkout a specific tag after cloning.
    pub tag: Option<String>,
    /// Checkout a specific revision (commit SHA) after cloning.
    pub rev: Option<String>,
    /// Clone with `--recurse-submodules`.
    pub recurse_submodules: bool,
    /// Enable worktree mode: clone as bare repo with worktrees.
    /// When `true`, the default branch worktree is created automatically.
    pub worktree: bool,
    /// Additional worktrees to create (name → branch).
    /// Each entry creates a worktree directory alongside the bare `.git`.
    pub worktrees: Option<HashMap<String, String>>,
    /// Extra remotes beyond origin (name → config).
    pub remotes: Option<HashMap<String, RemoteConfig>>,
    /// Arbitrary git config entries to set on the repository.
    pub config: Option<GitConfig>,
}

impl<'de> Deserialize<'de> for GitRepoConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let form = AllGitRepoForms::deserialize(deserializer)?;
        Ok(Self::from(form))
    }
}

impl From<AllGitRepoForms> for GitRepoConfig {
    fn from(f: AllGitRepoForms) -> Self {
        match f {
            AllGitRepoForms::Simple(url) => Self {
                url,
                branch: None,
                tag: None,
                rev: None,
                recurse_submodules: false,
                worktree: false,
                worktrees: None,
                remotes: None,
                config: None,
            },
            AllGitRepoForms::Detailed {
                url,
                branch,
                tag,
                rev,
                recurse_submodules,
                worktree,
                worktrees,
                remotes,
                config,
            } => Self {
                url,
                branch,
                tag,
                rev,
                recurse_submodules: recurse_submodules.unwrap_or(false),
                worktree: worktree.unwrap_or_else(|| worktrees.is_some()),
                worktrees,
                remotes,
                config: config.map(|c| *c),
            },
        }
    }
}

/// Intermediate enum for untagged deserialization of git repo entries.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AllGitRepoForms {
    /// Just a URL string.
    Simple(String),
    /// Full object with `url` and optional fields.
    Detailed {
        url: String,
        branch: Option<String>,
        tag: Option<String>,
        rev: Option<String>,
        recurse_submodules: Option<bool>,
        worktree: Option<bool>,
        worktrees: Option<HashMap<String, String>>,
        remotes: Option<HashMap<String, RemoteConfig>>,
        config: Option<Box<GitConfig>>,
    },
}

/// Configuration for a git remote.
///
/// Known fields (`url`, `fetch`, `push`, `tagopt`) are typed.
/// Any additional keys are captured in `extras` to support tools that write
/// custom options (e.g. `git-delete-merged-branches` adding `dmb-*` keys).
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RemoteConfig {
    /// The remote URL (required).
    pub url: String,
    /// Custom fetch refspec (e.g. `+refs/heads/*:refs/remotes/upstream/*`).
    pub fetch: Option<String>,
    /// Custom push refspec.
    pub push: Option<String>,
    /// Tag fetching option (e.g. `--tags`, `--no-tags`).
    pub tagopt: Option<String>,
    /// Arbitrary additional remote config keys.
    #[serde(flatten)]
    pub extras: HashMap<String, String>,
}

/// Arbitrary git config entries.
///
/// Supports two forms that can be mixed:
/// - **Flat dotted keys**: `"user.email" = "me@example.com"`
/// - **Nested sections**: `{ user: { email: "me@example.com" } }`
///
/// Internally flattened to dotted key → value pairs.
#[derive(Debug, Serialize, Clone)]
pub struct GitConfig {
    pub entries: HashMap<String, String>,
}

impl<'de> Deserialize<'de> for GitConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(GitConfigVisitor)
    }
}

struct GitConfigVisitor;

impl<'de> Visitor<'de> for GitConfigVisitor {
    type Value = GitConfig;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(
            "a map of dotted keys to values, or nested sections, \
             e.g. {\"user.email\": \"x\"} or {\"user\": {\"email\": \"x\"}}",
        )
    }

    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut entries = HashMap::new();

        while let Some(key) = access.next_key::<String>()? {
            let value: GitConfigValue = access.next_value()?;
            match value {
                GitConfigValue::Scalar(v) => {
                    entries.insert(key, v);
                }
                GitConfigValue::Section(map) => {
                    for (sub_key, sub_value) in map {
                        match sub_value {
                            GitConfigValue::Scalar(v) => {
                                entries.insert(format!("{key}.{sub_key}"), v);
                            }
                            GitConfigValue::Section(inner) => {
                                for (inner_key, inner_value) in inner {
                                    match inner_value {
                                        GitConfigValue::Scalar(v) => {
                                            entries
                                                .insert(format!("{key}.{sub_key}.{inner_key}"), v);
                                        }
                                        GitConfigValue::Section(_) => {
                                            return Err(de::Error::custom(
                                                "git config nesting too deep (max 3 levels)",
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(GitConfig { entries })
    }
}

/// A git config value: either a scalar string or a nested section map.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum GitConfigValue {
    Scalar(String),
    Section(HashMap<String, GitConfigValue>),
}

/// The root path key used when a single git repo is cloned to the overlay root.
pub const ROOT_PATH: &str = ".";

/// All supported shapes for the `git` field in an overlay config.
///
/// Tried in order by serde's `#[serde(untagged)]`:
/// 1. **Single** – a bare URL string *or* an object with a `url` key
///    (both handled by [`GitRepoConfig`]'s own deserializer).
///    The repo is cloned to the overlay target root (path `"."`).
/// 2. **Map** – a map of destination paths to repo configs (current behaviour).
///
/// *Edge case*: if you literally need a destination path called `"url"`, add
/// at least one other path entry so the map variant is selected instead.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AllGitForms {
    /// A single repo config (string URL or detailed object).
    Single(GitRepoConfig),
    /// Map of `<path> → <repo config>`.
    Map(HashMap<String, GitRepoConfig>),
}

impl From<AllGitForms> for HashMap<String, GitRepoConfig> {
    fn from(form: AllGitForms) -> Self {
        match form {
            AllGitForms::Single(cfg) => HashMap::from([(ROOT_PATH.to_string(), cfg)]),
            AllGitForms::Map(m) => m,
        }
    }
}

/// Custom deserializer for `Option<HashMap<String, GitRepoConfig>>` that
/// accepts a plain URL string, a single repo object, or the traditional map.
///
/// Used via `#[serde(default, deserialize_with = "deserialize_git_field")]`.
pub fn deserialize_git_field<'de, D>(
    deserializer: D,
) -> Result<Option<HashMap<String, GitRepoConfig>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<AllGitForms>::deserialize(deserializer).map(|opt| opt.map(Into::into))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    // ── Simple form ──────────────────────────────────────────────────────

    #[test]
    fn test_simple_string_form() {
        let yaml = r#""https://github.com/user/repo.git""#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.url, "https://github.com/user/repo.git");
        assert_eq!(cfg.branch, None);
        assert_eq!(cfg.tag, None);
        assert_eq!(cfg.rev, None);
        assert!(!cfg.recurse_submodules);
        assert!(!cfg.worktree);
        assert!(cfg.worktrees.is_none());
        assert!(cfg.remotes.is_none());
        assert!(cfg.config.is_none());
    }

    // ── Detailed form ────────────────────────────────────────────────────

    #[rstest]
    #[case::yaml(
        "yaml",
        r#"
url: "git@github.com:user/repo.git"
branch: main
recurse_submodules: true
"#
    )]
    #[case::toml(
        "toml",
        r#"
url = "git@github.com:user/repo.git"
branch = "main"
recurse_submodules = true
"#
    )]
    fn test_detailed_form_branch(#[case] format: &str, #[case] input: &str) {
        let cfg: GitRepoConfig = match format {
            "yaml" => serde_yaml::from_str(input).unwrap(),
            "toml" => toml::from_str(input).unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(cfg.url, "git@github.com:user/repo.git");
        assert_eq!(cfg.branch.as_deref(), Some("main"));
        assert!(cfg.recurse_submodules);
        assert!(!cfg.worktree);
    }

    #[test]
    fn test_detailed_form_tag() {
        let yaml = r#"
url: "https://github.com/user/repo.git"
tag: "v1.0.0"
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.tag.as_deref(), Some("v1.0.0"));
    }

    #[test]
    fn test_detailed_form_rev() {
        let yaml = r#"
url: "https://github.com/user/repo.git"
rev: "abc123"
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.rev.as_deref(), Some("abc123"));
    }

    // ── Worktree form ────────────────────────────────────────────────────

    #[test]
    fn test_worktree_enabled() {
        let yaml = r#"
url: "git@github.com:user/repo.git"
worktree: true
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.worktree);
        assert!(cfg.worktrees.is_none());
    }

    #[test]
    fn test_worktree_with_named_worktrees() {
        let yaml = r#"
url: "git@github.com:user/repo.git"
worktrees:
  feature-x: "feature/x"
  hotfix: "hotfix/123"
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        // worktree auto-enabled when worktrees are listed
        assert!(cfg.worktree);
        let wts = cfg.worktrees.unwrap();
        assert_eq!(wts.get("feature-x").unwrap(), "feature/x");
        assert_eq!(wts.get("hotfix").unwrap(), "hotfix/123");
    }

    #[test]
    fn test_worktree_explicit_false_with_named_worktrees() {
        let yaml = r#"
url: "git@github.com:user/repo.git"
worktree: false
worktrees:
  feature-x: "feature/x"
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        // explicit worktree: false disables auto default-branch worktree
        assert!(!cfg.worktree);
        assert!(cfg.worktrees.is_some());
    }

    // ── Remotes ──────────────────────────────────────────────────────────

    #[test]
    fn test_remotes() {
        let yaml = r#"
url: "git@github.com:user/repo.git"
remotes:
  upstream:
    url: "git@github.com:upstream/repo.git"
    fetch: "+refs/heads/*:refs/remotes/upstream/*"
    push: "refs/heads/main"
    tagopt: "--tags"
  fork:
    url: "git@github.com:fork/repo.git"
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        let remotes = cfg.remotes.unwrap();
        assert_eq!(remotes.len(), 2);

        let upstream = &remotes["upstream"];
        assert_eq!(upstream.url, "git@github.com:upstream/repo.git");
        assert_eq!(
            upstream.fetch.as_deref(),
            Some("+refs/heads/*:refs/remotes/upstream/*")
        );
        assert_eq!(upstream.push.as_deref(), Some("refs/heads/main"));
        assert_eq!(upstream.tagopt.as_deref(), Some("--tags"));

        let fork = &remotes["fork"];
        assert_eq!(fork.url, "git@github.com:fork/repo.git");
        assert!(fork.fetch.is_none());
    }

    #[test]
    fn test_remotes_with_extras() {
        let yaml = r#"
url: "git@github.com:user/repo.git"
remotes:
  upstream:
    url: "git@github.com:upstream/repo.git"
    dmb-hierarchical-branch-names: "true"
    dmb-protected-branches: "main,develop"
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        let upstream = &cfg.remotes.unwrap()["upstream"];
        assert_eq!(
            upstream
                .extras
                .get("dmb-hierarchical-branch-names")
                .unwrap(),
            "true"
        );
        assert_eq!(
            upstream.extras.get("dmb-protected-branches").unwrap(),
            "main,develop"
        );
    }

    // ── Git config ───────────────────────────────────────────────────────

    #[test]
    fn test_git_config_flat_dotted() {
        let yaml = r#"
url: "git@github.com:user/repo.git"
config:
  user.email: "work@example.com"
  core.autocrlf: "true"
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        let config = cfg.config.unwrap();
        assert_eq!(
            config.entries.get("user.email").unwrap(),
            "work@example.com"
        );
        assert_eq!(config.entries.get("core.autocrlf").unwrap(), "true");
    }

    #[test]
    fn test_git_config_nested_sections() {
        let yaml = r#"
url: "git@github.com:user/repo.git"
config:
  user:
    email: "work@example.com"
    name: "Work User"
  core:
    autocrlf: "true"
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        let config = cfg.config.unwrap();
        assert_eq!(
            config.entries.get("user.email").unwrap(),
            "work@example.com"
        );
        assert_eq!(config.entries.get("user.name").unwrap(), "Work User");
        assert_eq!(config.entries.get("core.autocrlf").unwrap(), "true");
    }

    #[test]
    fn test_git_config_mixed() {
        let yaml = r#"
url: "git@github.com:user/repo.git"
config:
  user.email: "work@example.com"
  core:
    autocrlf: "true"
"#;
        let cfg: GitRepoConfig = serde_yaml::from_str(yaml).unwrap();
        let config = cfg.config.unwrap();
        assert_eq!(
            config.entries.get("user.email").unwrap(),
            "work@example.com"
        );
        assert_eq!(config.entries.get("core.autocrlf").unwrap(), "true");
    }

    // ── HashMap of repos (as used by Overlay.git) ────────────────────────

    #[test]
    fn test_map_mixed_forms() {
        let yaml = r#"
".tmux/plugins/tpm": "https://github.com/tmux-plugins/tpm"
".config/nvim":
  url: "git@github.com:user/nvim-config.git"
  branch: main
  recurse_submodules: true
"projects/mylib":
  url: "git@github.com:user/mylib.git"
  worktree: true
  worktrees:
    feature-x: "feature/x"
  remotes:
    upstream:
      url: "git@github.com:upstream/mylib.git"
  config:
    user.email: "work@example.com"
"#;
        let repos: HashMap<String, GitRepoConfig> = serde_yaml::from_str(yaml).unwrap();

        // Simple form
        let tpm = &repos[".tmux/plugins/tpm"];
        assert_eq!(tpm.url, "https://github.com/tmux-plugins/tpm");
        assert_eq!(tpm.branch, None);

        // Detailed form
        let nvim = &repos[".config/nvim"];
        assert_eq!(nvim.url, "git@github.com:user/nvim-config.git");
        assert_eq!(nvim.branch.as_deref(), Some("main"));
        assert!(nvim.recurse_submodules);

        // Worktree form
        let mylib = &repos["projects/mylib"];
        assert!(mylib.worktree);
        assert_eq!(
            mylib.worktrees.as_ref().unwrap().get("feature-x").unwrap(),
            "feature/x"
        );
        assert!(mylib.remotes.is_some());
        assert!(mylib.config.is_some());
    }

    #[test]
    fn test_toml_map_mixed_forms() {
        let toml_str = r#"
".tmux/plugins/tpm" = "https://github.com/tmux-plugins/tpm"

[".config/nvim"]
url = "git@github.com:user/nvim-config.git"
branch = "main"
recurse_submodules = true

["projects/mylib"]
url = "git@github.com:user/mylib.git"
worktree = true

["projects/mylib".worktrees]
feature-x = "feature/x"

["projects/mylib".remotes.upstream]
url = "git@github.com:upstream/mylib.git"

["projects/mylib".config]
"user.email" = "work@example.com"
"#;
        let repos: HashMap<String, GitRepoConfig> = toml::from_str(toml_str).unwrap();

        let tpm = &repos[".tmux/plugins/tpm"];
        assert_eq!(tpm.url, "https://github.com/tmux-plugins/tpm");

        let nvim = &repos[".config/nvim"];
        assert_eq!(nvim.url, "git@github.com:user/nvim-config.git");
        assert_eq!(nvim.branch.as_deref(), Some("main"));

        let mylib = &repos["projects/mylib"];
        assert!(mylib.worktree);
        assert_eq!(
            mylib.worktrees.as_ref().unwrap().get("feature-x").unwrap(),
            "feature/x"
        );
    }

    // ── AllGitForms (git field: string / single / map) ───────────────────

    /// Helper: deserialize a YAML value through `deserialize_git_field`.
    fn git_field_from_yaml(yaml: &str) -> Option<HashMap<String, GitRepoConfig>> {
        // Wrap the value under a `git` key so we can use an Overlay-like struct.
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, deserialize_with = "super::deserialize_git_field")]
            git: Option<HashMap<String, GitRepoConfig>>,
        }
        let doc = format!("git: {yaml}");
        let w: Wrapper = serde_yaml::from_str(&doc).unwrap();
        w.git
    }

    #[test]
    fn test_git_field_simple_url() {
        let repos = git_field_from_yaml(r#""https://github.com/user/repo.git""#).unwrap();
        assert_eq!(repos.len(), 1);
        let cfg = &repos["."];
        assert_eq!(cfg.url, "https://github.com/user/repo.git");
        assert_eq!(cfg.branch, None);
        assert!(!cfg.recurse_submodules);
    }

    #[test]
    fn test_git_field_detailed_single() {
        let repos = git_field_from_yaml(
            r#"
  url: "git@github.com:user/repo.git"
  branch: main
  recurse_submodules: true
"#,
        )
        .unwrap();
        assert_eq!(repos.len(), 1);
        let cfg = &repos["."];
        assert_eq!(cfg.url, "git@github.com:user/repo.git");
        assert_eq!(cfg.branch.as_deref(), Some("main"));
        assert!(cfg.recurse_submodules);
    }

    #[test]
    fn test_git_field_map_form_unchanged() {
        let repos = git_field_from_yaml(
            r#"
  ".tmux/plugins/tpm": "https://github.com/tmux-plugins/tpm"
  ".config/nvim":
    url: "git@github.com:user/nvim-config.git"
    branch: main
"#,
        )
        .unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(
            repos[".tmux/plugins/tpm"].url,
            "https://github.com/tmux-plugins/tpm"
        );
        assert_eq!(
            repos[".config/nvim"].url,
            "git@github.com:user/nvim-config.git"
        );
        assert_eq!(repos[".config/nvim"].branch.as_deref(), Some("main"));
    }

    #[test]
    fn test_git_field_none_when_absent() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, deserialize_with = "super::deserialize_git_field")]
            git: Option<HashMap<String, GitRepoConfig>>,
        }
        let w: Wrapper = serde_yaml::from_str("other: value").unwrap();
        assert!(w.git.is_none());
    }
}
