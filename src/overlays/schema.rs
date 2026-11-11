//! Inspect each source before merging so child descriptors cannot hide warnings.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use config::{Map, Value, ValueKind};

pub(crate) const OVERLAY_KEYS: &[&str] = &[
    "data",
    "default_overlay",
    "description",
    "exclude",
    "format",
    "git",
    "hooks",
    "map",
    "mode",
    "overlays",
    "permissions",
    "repository",
    "rules",
    "sync",
    "target",
    "uses",
];
const EVENTS: &[&str] = &["install", "post-clone", "post-init", "post-apply"];
const HOOK_FIELDS: &[&str] = &["command", "env", "cwd", "when"];
const REPO_FIELDS: &[&str] = &[
    "url",
    "branch",
    "tag",
    "rev",
    "recurse_submodules",
    "worktree",
    "per_worktree_config",
    "worktrees",
    "remotes",
    "config",
    "worktree_config",
];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Warning {
    pub file: PathBuf,
    pub message: String,
    pub hint: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Reporter {
    collected: bool,
    warnings: Arc<Mutex<BTreeSet<Warning>>>,
}

impl Reporter {
    pub fn collecting() -> Self {
        Self {
            collected: true,
            ..Self::default()
        }
    }

    pub fn report(&self, warnings: Vec<Warning>) {
        // Share only reporting state, not parsed descriptors: init rechecks must see file changes.
        let mut seen = self
            .warnings
            .lock()
            .expect("configuration warning lock poisoned");
        for warning in warnings {
            if seen.insert(warning.clone()) && !self.collected {
                tracing::warn!(
                    "{}: {}; {}",
                    warning.file.display(),
                    warning.message,
                    warning.hint
                );
            }
        }
    }

    pub fn warnings(&self) -> Vec<Warning> {
        self.warnings
            .lock()
            .expect("configuration warning lock poisoned")
            .iter()
            .cloned()
            .collect()
    }
}

pub(crate) fn descriptor_path(
    values: &Map<String, Value>,
    directory: &Path,
) -> anyhow::Result<PathBuf> {
    // Use the parser's selected source when several descriptor extensions coexist.
    let path = values
        .values()
        .find_map(|value| value.origin().map(PathBuf::from))
        .unwrap_or_else(|| directory.join("over"));
    super::mapping::absolute(&path)
}

pub(crate) fn normalize(values: &mut Map<String, Value>, file: &Path) -> Vec<Warning> {
    let mut warnings = Vec::new();
    fields(values, OVERLAY_KEYS, "", false, file, &mut warnings);
    for (key, value) in values {
        match key.as_str() {
            "sync" => {
                if let ValueKind::Table(table) = &mut value.kind {
                    fields(table, &["pull", "push"], "sync", false, file, &mut warnings);
                }
            }
            "rules" => entries(value, |value, index| {
                if let ValueKind::Table(table) = &mut value.kind {
                    fields(
                        table,
                        &["path", "mode", "permissions"],
                        &format!("rules[{}]", index + 1),
                        false,
                        file,
                        &mut warnings,
                    );
                }
            }),
            "overlays" => entries(value, |value, index| {
                if let ValueKind::Table(table) = &mut value.kind {
                    fields(
                        table,
                        &["path"],
                        &format!("overlays[{}]", index + 1),
                        false,
                        file,
                        &mut warnings,
                    );
                }
            }),
            "map" => {
                if let ValueKind::Table(mappings) = &mut value.kind {
                    for (source, mapping) in mappings {
                        if let ValueKind::Table(table) = &mut mapping.kind {
                            fields(
                                table,
                                &["target"],
                                &format!("map.{source}"),
                                false,
                                file,
                                &mut warnings,
                            );
                        }
                    }
                }
            }
            "hooks" => {
                if let ValueKind::Table(events) = &mut value.kind {
                    fields(events, EVENTS, "hooks", true, file, &mut warnings);
                    for (event, hooks) in events {
                        entries(hooks, |hook, index| {
                            if let ValueKind::Table(table) = &mut hook.kind {
                                fields(
                                    table,
                                    HOOK_FIELDS,
                                    &format!("hooks.{event}[{}]", index + 1),
                                    true,
                                    file,
                                    &mut warnings,
                                );
                            }
                        });
                    }
                }
            }
            "git" => {
                if let ValueKind::Table(table) = &mut value.kind {
                    if table.contains_key("url") {
                        git_repo(table, "git", file, &mut warnings);
                    } else {
                        for (destination, repo) in table {
                            if let ValueKind::Table(table) = &mut repo.kind {
                                git_repo(table, &format!("git.{destination}"), file, &mut warnings);
                            }
                        }
                    }
                }
            }
            // Metadata, Git options, remote extras, and environment names are open namespaces.
            _ => {}
        }
    }
    warnings.sort();
    warnings
}

fn entries(value: &mut Value, mut visit: impl FnMut(&mut Value, usize)) {
    if let ValueKind::Array(entries) = &mut value.kind {
        for (index, entry) in entries.iter_mut().enumerate() {
            visit(entry, index);
        }
    }
}

fn git_repo(
    table: &mut Map<String, Value>,
    location: &str,
    file: &Path,
    warnings: &mut Vec<Warning>,
) {
    fields(table, REPO_FIELDS, location, false, file, warnings);
    if let Some(Value {
        kind: ValueKind::Table(worktrees),
        ..
    }) = table.get_mut("worktrees")
    {
        for (name, worktree) in worktrees {
            if let ValueKind::Table(table) = &mut worktree.kind {
                fields(
                    table,
                    &["branch", "config"],
                    &format!("{location}.worktrees.{name}"),
                    false,
                    file,
                    warnings,
                );
            }
        }
    }
}

fn fields(
    table: &mut Map<String, Value>,
    valid: &[&str],
    location: &str,
    redact: bool,
    file: &Path,
    warnings: &mut Vec<Warning>,
) {
    let mut ignored = 0;
    table.retain(|key, _| {
        // Internal overrides are tolerated for compatibility, but are not suggested settings.
        if valid.contains(&key.as_str()) || (location.is_empty() && matches!(key.as_str(), "name" | "root")) {
            return true;
        }
        ignored += 1;
        let suffix = if location.is_empty() { String::new() } else { format!(" in {location}") };
        // Hook keys themselves can contain secrets; neither diagnostics nor Debug may retain them.
        let message = if redact { format!("unknown key ignored{suffix} (unknown field #{ignored})") } else { format!("unknown key `{key}` ignored{suffix}") };
        let migration = match (location, key.as_str()) {
            ("", "install") => Some("top-level `install` is no longer supported; migrate to `hooks.install` and run `over apply --install`"),
            ("", "default" | "defaults" | "materialization" | "link_dirs") => Some("use scalar `mode`/`permissions` and `rules`; directory links use mode: symlink-directory"),
            (_, "materialization") if location.starts_with("rules[") => Some("removed `materialization`; replace it with `mode`"),
            _ => None,
        };
        let hint = if redact {
            format!("valid keys are: {}", valid.iter().map(|key| format!("`{key}`")).collect::<Vec<_>>().join(", "))
        } else if let Some(migration) = migration {
            migration.into()
        } else if let Some(suggestion) = suggest_key(key, valid) {
            format!("did you mean `{suggestion}`?")
        } else {
            format!("valid keys are: {}", valid.iter().map(|key| format!("`{key}`")).collect::<Vec<_>>().join(", "))
        };
        warnings.push(Warning { file: file.into(), message, hint });
        false
    });
}

pub(crate) fn suggest_key<'a>(unknown: &str, valid: &'a [&str]) -> Option<&'a str> {
    valid
        .iter()
        .copied()
        .map(|key| (key, edit_distance(unknown, key)))
        .filter(|(_, distance)| *distance <= 2.max(unknown.chars().count() * 2 / 5))
        .min_by_key(|(_, distance)| *distance)
        .map(|(key, _)| key)
}

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    // Character-based rows keep non-ASCII unknown keys from distorting typo distances.
    let mut row: Vec<_> = (0..=b.chars().count()).collect();
    for (i, ac) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, bc) in b.chars().enumerate() {
            let previous = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(previous + 1)
                .min(diagonal + usize::from(ac != bc));
            diagonal = previous;
        }
    }
    *row.last().expect("distance row always has an initial cell")
}

#[cfg(test)]
mod tests {
    use config::{File, FileFormat, Source};
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("extra: {arbitrary: [1, 2]}", "`extra`")]
    #[case("rules: [{path: config, mode: checkout, extra: 1}]", "rules[1]")]
    #[case("map: {config: {target: '~', extra: 1}}", "map.config")]
    #[case("overlays: [{path: hosts, extra: 1}]", "overlays[1]")]
    #[case("git: {url: source, extra: 1}", "in git")]
    #[case("git: {vendor: {url: source, extra: 1}}", "git.vendor")]
    #[case(
        "git: {url: source, worktrees: {dev: {branch: main, extra: 1}}}",
        "git.worktrees.dev"
    )]
    #[case("hooks: {secret_sentinel: {arbitrary: true}}", "in hooks")]
    #[case(
        "hooks: {post-apply: [{command: ok, secret_sentinel: value}]}",
        "hooks.post-apply[1]"
    )]
    fn extras_are_removed_from_closed_schema_locations(
        #[case] source: &str,
        #[case] location: &str,
    ) {
        let mut values = File::from_str(source, FileFormat::Yaml).collect().unwrap();
        let warnings = normalize(&mut values, Path::new("over.yaml"));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains(location), "{warnings:?}");
        assert!(!format!("{warnings:?}").contains("secret_sentinel"));
        assert!(normalize(&mut values, Path::new("over.yaml")).is_empty());
    }

    #[test]
    fn open_namespaces_preserve_arbitrary_consumer_keys() {
        let mut values = File::from_str(
            r#"
data: {custom: {field: value}}
hooks: {post-apply: [{command: ok, env: {CUSTOM: value}}]}
git:
  url: source
  config: {custom: {field: value}}
  worktree_config: {custom: value}
  remotes: {origin: {url: source, custom: value}}
  worktrees: {dev: {branch: main, config: {custom: value}}}
"#,
            FileFormat::Yaml,
        )
        .collect()
        .unwrap();
        let before = values.clone();
        assert!(normalize(&mut values, Path::new("over.yaml")).is_empty());
        assert_eq!(values, before);
    }

    #[rstest]
    #[case("install: {old: value}")]
    #[case("default: value")]
    #[case("defaults: {old: value}")]
    #[case("materialization: checkout")]
    #[case("link_dirs: [config]")]
    #[case("rules: [{path: config, mode: checkout, materialization: old}]")]
    fn removed_keys_are_discarded_with_migration_hints(#[case] source: &str) {
        let mut values = File::from_str(source, FileFormat::Yaml).collect().unwrap();
        let warnings = normalize(&mut values, Path::new("over.yaml"));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].hint.contains("mode") || warnings[0].hint.contains("hooks.install"));
        assert!(normalize(&mut values, Path::new("over.yaml")).is_empty());
    }

    #[test]
    fn reporting_is_shared_across_clones_without_caching_configuration() {
        let reporter = Reporter::collecting();
        let warning = Warning {
            file: "over.yaml".into(),
            message: "unknown key ignored".into(),
            hint: "hint".into(),
        };
        reporter.report(vec![warning.clone()]);
        reporter.clone().report(vec![warning.clone()]);
        assert_eq!(reporter.warnings(), vec![warning]);
    }

    #[test]
    fn distinct_unknown_hook_keys_each_warn_without_retaining_their_names() {
        let mut values = File::from_str(
            "hooks: {secret_one: false, secret_two: 42}",
            FileFormat::Yaml,
        )
        .collect()
        .unwrap();
        let reporter = Reporter::collecting();
        reporter.report(normalize(&mut values, Path::new("over.yaml")));
        reporter.report(normalize(&mut values, Path::new("over.yaml")));
        let warnings = reporter.warnings();
        assert_eq!(warnings.len(), 2);
        assert!(!format!("{warnings:?}").contains("secret_"));
    }
}
