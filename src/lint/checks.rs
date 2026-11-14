use std::collections::HashSet;

use globset::GlobBuilder;
use walkdir::WalkDir;

use crate::actions::partial::discover_partials_selected;
use crate::actions::symlink::{LinkType, discover_symlinks_selected};
use crate::overlays::Overlay;

use super::Diagnostic;

/// Run all per-overlay checks and return collected diagnostics.
#[cfg(test)]
pub fn check_overlay(overlay: &Overlay) -> Vec<Diagnostic> {
    check_overlay_selected(overlay, None)
}

pub fn check_overlay_selected(
    overlay: &Overlay,
    policy: Option<&crate::overlays::ignore::SourcePolicy>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    if let Err(error) = crate::overlays::mapping::validate_sources(overlay) {
        diagnostics.push(Diagnostic::error(&overlay.name, error.to_string()));
    }

    diagnostics.extend(check_uses(overlay));
    diagnostics.extend(check_empty_exclude(overlay));
    diagnostics.extend(check_invalid_exclude_globs(overlay));
    diagnostics.extend(check_empty_rules(overlay));
    diagnostics.extend(check_invalid_rules_globs(overlay));
    diagnostics.extend(check_duplicate_rule_paths(overlay));
    diagnostics.extend(check_rules_paths_exist(overlay));
    if !overlay.has_target_rules() {
        // Target boundaries require the selected consumer's rendered routing context.
        diagnostics.extend(check_terminal_rules(overlay));
    }
    diagnostics.extend(check_git(overlay));
    diagnostics.extend(check_symlinks_selected(overlay, policy));
    diagnostics.extend(check_partials_selected(overlay, policy));

    diagnostics
}

// ── uses checks ──────────────────────────────────────────────────────────

fn check_uses(overlay: &Overlay) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let name = &overlay.name;

    let Some(uses) = &overlay.uses else {
        return diagnostics;
    };

    // Empty uses list
    if uses.is_empty() {
        diagnostics.push(
            Diagnostic::warning(name, "empty `uses` list is redundant")
                .with_hint("remove the `uses` field or add overlay references"),
        );
        return diagnostics;
    }

    // Self-reference
    for dep in uses {
        if dep == name {
            diagnostics.push(Diagnostic::error(
                name,
                "overlay references itself in `uses`",
            ));
        }
    }

    // Duplicate entries
    let mut seen = HashSet::new();
    for dep in uses {
        if !seen.insert(dep) {
            diagnostics.push(
                Diagnostic::warning(name, format!("`uses` contains duplicate entry \"{dep}\""))
                    .with_hint("remove the duplicate reference"),
            );
        }
    }

    diagnostics
}

// ── exclude checks ───────────────────────────────────────────────────────

fn check_empty_exclude(overlay: &Overlay) -> Vec<Diagnostic> {
    if matches!(&overlay.exclude, Some(list) if list.is_empty()) {
        vec![
            Diagnostic::warning(&overlay.name, "empty `exclude` list is redundant")
                .with_hint("remove the `exclude` field or add glob patterns"),
        ]
    } else {
        Vec::new()
    }
}

fn check_invalid_exclude_globs(overlay: &Overlay) -> Vec<Diagnostic> {
    let Some(exclude) = &overlay.exclude else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();
    for pattern in exclude {
        if let Err(err) = GlobBuilder::new(pattern).literal_separator(true).build() {
            diagnostics.push(
                Diagnostic::error(
                    &overlay.name,
                    format!("invalid glob pattern in `exclude`: \"{pattern}\""),
                )
                .with_hint(format!("{err}")),
            );
        }
    }
    diagnostics
}

// ── materialization rules checks (#113/#126) ────────────────────────────

fn check_empty_rules(overlay: &Overlay) -> Vec<Diagnostic> {
    if matches!(&overlay.rules, Some(list) if list.is_empty()) {
        vec![
            Diagnostic::warning(&overlay.name, "empty `rules` list is redundant")
                .with_hint("remove the `rules` field or add path overrides"),
        ]
    } else {
        Vec::new()
    }
}

fn check_invalid_rules_globs(overlay: &Overlay) -> Vec<Diagnostic> {
    let Some(rules) = &overlay.rules else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();
    for rule in rules {
        if let Err(err) = rule.validate() {
            diagnostics.push(
                Diagnostic::error(
                    &overlay.name,
                    format!("invalid selector in `rules`: \"{}\"", rule.path),
                )
                .with_hint(format!("{err}")),
            );
        }
    }
    diagnostics
}

/// Warn about same-scope overlap; inheritance alone is intentional policy composition.
fn check_duplicate_rule_paths(overlay: &Overlay) -> Vec<Diagnostic> {
    use crate::overlays::mapping::lexical;
    use crate::overlays::rules::resolve_with_origins;

    let mut diagnostics = Vec::new();
    for scope in &overlay.scopes {
        for (index, rule) in scope.rules.iter().enumerate() {
            for (later_index, later) in scope.rules.iter().enumerate().skip(index + 1) {
                let mode = rule.mode.is_some() && later.mode.is_some();
                let permissions = rule.permissions.is_some() && later.permissions.is_some();
                let target = rule.target.is_some() && later.target.is_some();
                let template = rule.template.is_some() && later.template.is_some();
                let strip_extension =
                    rule.strip_extension.is_some() && later.strip_extension.is_some();
                if !mode && !permissions && !target && !template && !strip_extension {
                    continue;
                }
                let first = lexical(&scope.root.join(&rule.path));
                let second = lexical(&scope.root.join(&later.path));
                let literal = !rule.is_glob() && !later.is_glob();
                let witness = if literal {
                    if first.starts_with(&second) {
                        Some((first.clone(), first.is_dir()))
                    } else if second.starts_with(&first) {
                        Some((second.clone(), second.is_dir()))
                    } else {
                        None
                    }
                } else {
                    // Observed overlap is evidence, not a claim of glob-language containment.
                    let mut witness = None;
                    for entry in WalkDir::new(&overlay.root).sort_by_file_name() {
                        match entry {
                            Ok(entry)
                                if scope.matches(
                                    rule,
                                    entry.path(),
                                    entry.file_type().is_dir(),
                                ) && scope.matches(
                                    later,
                                    entry.path(),
                                    entry.file_type().is_dir(),
                                ) =>
                            {
                                witness =
                                    Some((entry.path().to_path_buf(), entry.file_type().is_dir()));
                                break;
                            }
                            Ok(_) => {}
                            Err(error) => {
                                diagnostics.push(Diagnostic::error(
                                    &overlay.name,
                                    format!("cannot inspect rule overlap: {error}"),
                                ));
                                break;
                            }
                        }
                    }
                    witness
                };
                let Some((witness, is_dir)) = witness else {
                    continue;
                };
                let Ok(relative) = witness.strip_prefix(&overlay.root) else {
                    continue;
                };
                let resolved = resolve_with_origins(overlay, relative, is_dir);
                let mut winners = Vec::new();
                for (property, active, origin, value) in [
                    (
                        "template",
                        template,
                        resolved.template_origin,
                        resolved.template.to_string(),
                    ),
                    (
                        "strip_extension",
                        strip_extension,
                        resolved.strip_extension_origin,
                        resolved.strip_extension.to_string(),
                    ),
                    (
                        "target",
                        target,
                        resolved.target_origin,
                        resolved
                            .target_origin
                            .and_then(|(scope, index)| {
                                index.and_then(|index| scope.rules[index].target.clone())
                            })
                            .unwrap_or_default(),
                    ),
                    (
                        "mode",
                        mode,
                        resolved.mode_origin,
                        resolved.mode.to_string(),
                    ),
                    (
                        "permissions",
                        permissions,
                        resolved.permissions_origin,
                        resolved
                            .permissions
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                    ),
                ] {
                    if active && let Some((winner_scope, winner_index)) = origin {
                        winners.push(format!(
                            "{property}={value} wins from {} in {}",
                            winner_index
                                .map(|index| format!("rule {}", index + 1))
                                .unwrap_or_else(|| "scope baseline".to_owned()),
                            winner_scope.descriptor.display()
                        ));
                    }
                }
                let relation = if literal && first.starts_with(&second) {
                    "duplicate rule path or shadowed prefix"
                } else {
                    "overlapping rule paths"
                };
                diagnostics.push(Diagnostic::warning(&overlay.name, format!("{relation} '{}' and '{}': rules {} and {} overlap at '{}'; {}", rule.path, later.path, index + 1, later_index + 1, witness.display(), winners.join("; ")))
                    .with_file(scope.descriptor.to_string_lossy().into_owned())
                    .with_hint("later matching assignments win per property; nearer scope baselines reset inherited values; remove redundant assignments or narrow selectors"));
            }
        }
    }
    diagnostics
}

/// A rule whose glob never matches anything under the overlay is either a
/// typo or leftover from a rename — flag it rather than silently doing
/// nothing.
fn check_rules_paths_exist(overlay: &Overlay) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (scope, rule) in overlay
        .scopes
        .iter()
        .flat_map(|scope| scope.rules.iter().map(move |rule| (scope, rule)))
    {
        if rule.mode.is_none() || rule.validate().is_err() {
            // Already reported by `check_invalid_rules_globs`.
            continue;
        }
        let exists = WalkDir::new(&overlay.root)
            .into_iter()
            .filter_map(Result::ok)
            .any(|e| scope.matches(rule, e.path(), e.file_type().is_dir()));
        // An ancestor scope may intentionally configure another overlay's tree.
        if !exists
            && (scope.root == overlay.root
                || !rule.is_glob() && scope.root.join(&rule.path).starts_with(&overlay.root))
        {
            diagnostics.push(
                Diagnostic::warning(
                    &overlay.name,
                    format!(
                        "rule path \"{}\" does not match any file or directory in the overlay",
                        rule.path
                    ),
                )
                .with_hint("remove the rule or fix the path"),
            );
        }
    }
    diagnostics
}

// ── permission rules checks (#65) ────────────────────────────────────────
//
// No "paths exist" check here (unlike `check_rules_paths_exist`): a
// `permissions` rule matches a `PartialFile` entry's own sidecar stem, not
// a real path in the overlay's walked tree, so "exists in the walked
// tree" doesn't apply the same way.

/// Checkout mappings form separate traversal roots; permissions on partials are independent.
fn check_terminal_rules(overlay: &Overlay) -> Vec<Diagnostic> {
    check_terminal_rules_with_boundaries(overlay, &std::collections::BTreeMap::new())
}

pub(super) fn check_routed_terminal_rules(
    overlay: &Overlay,
    routing: &crate::overlays::mapping::ResolvedMappings,
) -> Vec<Diagnostic> {
    check_terminal_rules_with_boundaries(overlay, &routing.boundaries)
}

fn check_terminal_rules_with_boundaries(
    overlay: &Overlay,
    boundaries: &std::collections::BTreeMap<std::path::PathBuf, std::path::PathBuf>,
) -> Vec<Diagnostic> {
    use crate::overlays::MaterializationKind;
    let mut diagnostics = Vec::new();
    for scope in &overlay.scopes {
        for (index, rule) in scope
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule.mode.is_some())
        {
            let mut candidates = vec![crate::overlays::mapping::lexical(
                &scope.root.join(&rule.path),
            )];
            if rule.is_glob() {
                candidates = WalkDir::new(&overlay.root)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry.file_type().is_dir() && scope.matches(rule, entry.path(), true)
                    })
                    .map(|entry| entry.into_path())
                    .collect();
            }
            for candidate in candidates {
                if candidate.is_file() && rule.mode != Some(MaterializationKind::Symlink) {
                    diagnostics.push(
                        Diagnostic::error(
                            &overlay.name,
                            format!(
                                "rule {} ('{}') in {} selects a file, but {} requires a directory",
                                index + 1,
                                rule.path,
                                scope.descriptor.display(),
                                rule.mode.expect("mode-filtered rule")
                            ),
                        )
                        .with_file(scope.descriptor.to_string_lossy().into_owned())
                        .with_hint("select the containing directory or use mode: symlink"),
                    );
                    continue;
                }
                let Ok(relative) = candidate.strip_prefix(&overlay.root) else {
                    continue;
                };
                // Only effective target assignments can reopen a checkout traversal root.
                let mapped = boundaries
                    .keys()
                    .filter(|path| relative.starts_with(path))
                    .max_by_key(|path| path.components().count());
                for ancestor in relative.ancestors().skip(1) {
                    if mapped.is_some_and(|path| !ancestor.starts_with(path)) {
                        break;
                    }
                    let mode = overlay.materialization_for(ancestor);
                    if mode == MaterializationKind::Symlink
                        || (ancestor.as_os_str().is_empty()
                            && mode == MaterializationKind::SymlinkDirectory)
                    {
                        continue;
                    }
                    // Only final modes determine traversal; a later assignment may reopen a boundary.
                    let boundary = crate::overlays::rules::mode_origin(overlay, ancestor);
                    // A recursive directory glob may select nested boundaries in the same rule.
                    // The outermost match owns those descendants; the declaration still takes effect.
                    if boundary.is_some_and(|(owner, rule_index)| {
                        std::ptr::eq(owner, scope) && rule_index == Some(index)
                    }) {
                        break;
                    }
                    let origin = boundary
                        .map(|(scope, index)| match index {
                            Some(index) => format!(
                                "{} rule {} ('{}')",
                                scope.descriptor.display(),
                                index + 1,
                                scope.rules[index].path
                            ),
                            None => format!("{} mode baseline", scope.descriptor.display()),
                        })
                        .unwrap_or_else(|| "implicit mode".into());
                    diagnostics.push(Diagnostic::error(&overlay.name, format!("rule {} ('{}') in {} is unreachable below '{}' ({mode}), selected by {origin}", index + 1, rule.path, scope.descriptor.display(), ancestor.display()))
                        .with_hint("remove the descendant mode override or change the ancestor mode to symlink")
                        .with_file(scope.descriptor.to_string_lossy().into_owned()));
                    break;
                }
            }
        }
    }
    diagnostics
}

// ── git checks ───────────────────────────────────────────────────────────

fn check_git(overlay: &Overlay) -> Vec<Diagnostic> {
    let Some(git) = &overlay.git else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();

    for (path, config) in git {
        let ctx = if path == "." {
            "git repo (root)".to_string()
        } else {
            format!("git repo \"{path}\"")
        };

        // Empty URL
        if config.url.is_empty() {
            diagnostics.push(Diagnostic::error(
                &overlay.name,
                format!("{ctx}: `url` is empty"),
            ));
        }

        // Conflicting ref specifiers
        let ref_count = [&config.branch, &config.tag, &config.rev]
            .iter()
            .filter(|r| r.is_some())
            .count();
        if ref_count > 1 {
            let mut refs = Vec::new();
            if config.branch.is_some() {
                refs.push("`branch`");
            }
            if config.tag.is_some() {
                refs.push("`tag`");
            }
            if config.rev.is_some() {
                refs.push("`rev`");
            }
            diagnostics.push(
                Diagnostic::error(
                    &overlay.name,
                    format!("{ctx}: conflicting ref specifiers: {}", refs.join(", ")),
                )
                .with_hint("use only one of `branch`, `tag`, or `rev`"),
            );
        }

        // Worktree with tag/rev (silently ignored at runtime)
        if config.worktree || config.worktrees.is_some() {
            if config.tag.is_some() {
                diagnostics.push(
                    Diagnostic::warning(
                        &overlay.name,
                        format!("{ctx}: `tag` is ignored in worktree mode"),
                    )
                    .with_hint("remove `tag` or disable worktree mode"),
                );
            }
            if config.rev.is_some() {
                diagnostics.push(
                    Diagnostic::warning(
                        &overlay.name,
                        format!("{ctx}: `rev` is ignored in worktree mode"),
                    )
                    .with_hint("remove `rev` or disable worktree mode"),
                );
            }
        }

        // Empty worktrees map
        if matches!(&config.worktrees, Some(wts) if wts.is_empty()) {
            diagnostics.push(
                Diagnostic::warning(
                    &overlay.name,
                    format!("{ctx}: empty `worktrees` map is redundant"),
                )
                .with_hint("remove `worktrees` or add worktree entries"),
            );
        }

        if let Err(error) = crate::actions::git::config::validate_logical_key(path) {
            diagnostics.push(
                Diagnostic::error(&overlay.name, format!("{ctx}: {error}"))
                    .with_hint("use a path within the overlay target directory"),
            );
        }
    }

    diagnostics
}

// ── symlink checks ───────────────────────────────────────────────────────

#[cfg(test)]
fn check_symlinks(overlay: &Overlay) -> Vec<Diagnostic> {
    check_symlinks_selected(overlay, None)
}

fn check_symlinks_selected(
    overlay: &Overlay,
    policy: Option<&crate::overlays::ignore::SourcePolicy>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    let symlinks = match discover_symlinks_selected(&overlay.root, policy) {
        Ok(s) => s,
        Err(e) => {
            diagnostics.push(Diagnostic::error(
                &overlay.name,
                format!("failed to discover symlink configs: {e}"),
            ));
            return diagnostics;
        }
    };

    for (name, config) in &symlinks {
        if config.target.is_empty() {
            diagnostics.push(Diagnostic::error(
                &overlay.name,
                format!("symlink `{name}`: `target` is empty"),
            ));
        } else {
            // Runtime namespaces need not exist on the machine running lint.
            if let Err(e) = crate::exec::templates::create_env().template_from_str(&config.target) {
                diagnostics.push(Diagnostic::error(
                    &overlay.name,
                    format!("symlink `{name}`: invalid template in `target`: {e}"),
                ));
            }
        }

        if config.r#type == LinkType::Hard {
            diagnostics.push(
                Diagnostic::warning(
                    &overlay.name,
                    format!("symlink `{name}`: `type = \"hard\"` will fall back to soft link for directories"),
                )
                .with_hint("hard links are not supported for directories on most filesystems"),
            );
        }
    }

    diagnostics
}

// ── partial-file checks (#66) ────────────────────────────────────────────

#[cfg(test)]
fn check_partials(overlay: &Overlay) -> Vec<Diagnostic> {
    check_partials_selected(overlay, None)
}

fn check_partials_selected(
    overlay: &Overlay,
    policy: Option<&crate::overlays::ignore::SourcePolicy>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    let partials = match discover_partials_selected(&overlay.root, policy) {
        Ok(p) => p,
        Err(e) => {
            diagnostics.push(Diagnostic::error(
                &overlay.name,
                format!("failed to discover partial configs: {e}"),
            ));
            return diagnostics;
        }
    };

    for (name, config) in &partials {
        // `discover_partials` already skips (with a `tracing::warn!`) any
        // entry with an empty `target`/`content` before it ever reaches
        // `partials`, so neither can ever be empty here — the only thing
        // left worth flagging statically is a `target` template that
        // cannot compile, independently of the runtime metadata context.
        if let Err(e) = crate::exec::templates::create_env().template_from_str(&config.target) {
            diagnostics.push(Diagnostic::error(
                &overlay.name,
                format!("partial `{name}`: invalid template in `target`: {e}"),
            ));
        }
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;

    use super::super::Severity;

    fn setup_overlay(toml_content: &str) -> Overlay {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        let ov = td.child("test_ov");
        ov.create_dir_all().unwrap();
        ov.child("over.toml").write_str(toml_content).unwrap();
        // Keep td alive for the duration via leak (tests only)
        let overlay = repo.get("test_ov").unwrap();
        // We must keep td alive; use Box::leak for test simplicity
        Box::leak(Box::new(td));
        overlay
    }

    #[test]
    fn overlap_diagnostics_name_the_final_winner_for_each_property() {
        let overlay = setup_overlay(
            r#"
[[rules]]
path = 'config'
mode = 'checkout'
permissions = '600'
[[rules]]
path = 'config/editor'
mode = 'symlink-directory'
permissions = '644'
[[rules]]
path = 'config'
mode = 'symlink'
"#,
        );
        let diagnostics = check_duplicate_rule_paths(&overlay);
        let overlap = diagnostics
            .iter()
            .find(|diag| diag.message.contains("rules 1 and 2"))
            .unwrap();
        assert!(
            overlap.message.contains("mode=symlink wins from rule 3"),
            "{overlap:?}"
        );
        assert!(
            overlap.message.contains("permissions=644 wins from rule 2"),
            "{overlap:?}"
        );
        assert!(overlap.message.contains("overlapping rule paths"));
    }

    #[test]
    fn normalized_rule_aliases_share_the_same_precedence_namespace() {
        let overlay = setup_overlay(
            r#"
[[rules]]
path = './unused/../config'
permissions = '600'
[[rules]]
path = 'config'
permissions = '644'
"#,
        );
        let diagnostics = check_duplicate_rule_paths(&overlay);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("permissions=644 wins from rule 2")
        );
    }

    #[test]
    fn observed_glob_overlap_reports_one_witness_and_the_effective_winner() {
        let overlay = setup_overlay(
            r#"
[[rules]]
path = 'config/*'
mode = 'symlink-directory'
[[rules]]
path = 'config/editor*'
mode = 'symlink-directory'
"#,
        );
        std::fs::create_dir_all(overlay.root.join("config/editor-one")).unwrap();
        std::fs::create_dir_all(overlay.root.join("config/editor-two")).unwrap();
        let diagnostics = check_duplicate_rule_paths(&overlay);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("editor-one"));
        assert!(
            diagnostics[0]
                .message
                .contains("mode=symlink-directory wins from rule 2")
        );
    }

    #[test]
    fn overlap_explanations_respect_a_nearer_scope_baseline() {
        let temp = TempDir::new().unwrap();
        temp.child("over.yaml").write_str("rules:\n  - {path: child/config, mode: checkout}\n  - {path: child/config, mode: symlink-directory}").unwrap();
        temp.child("child/over.yaml")
            .write_str("mode: symlink")
            .unwrap();
        let overlay = Repository::new(temp.path().to_owned())
            .get("child")
            .unwrap();
        let diagnostics = check_duplicate_rule_paths(&overlay);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("mode=symlink wins from scope baseline")
        );
    }

    // ── uses checks ──────────────────────────────────────────────────────

    #[rstest]
    fn empty_uses_list_warns() {
        let overlay = setup_overlay("target = \"~\"\nuses = []");
        let diags = check_uses(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(diags[0].message.contains("empty `uses`"));
    }

    #[rstest]
    fn self_referencing_uses_entry_errors() {
        let overlay = setup_overlay("target = \"~\"\nuses = [\"test_ov\"]");
        let diags = check_uses(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("references itself"))
        );
    }

    #[rstest]
    fn duplicate_uses_entries_warn() {
        let overlay = setup_overlay("target = \"~\"\nuses = [\"other\", \"other\"]");
        let diags = check_uses(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("duplicate"))
        );
    }

    #[rstest]
    fn valid_uses_list_produces_no_diagnostics() {
        let overlay = setup_overlay("target = \"~\"\nuses = [\"other\"]");
        let diags = check_uses(&overlay);
        assert!(diags.is_empty());
    }

    // ── exclude checks ───────────────────────────────────────────────────

    #[rstest]
    fn empty_exclude_list_warns() {
        let overlay = setup_overlay("target = \"~\"\nexclude = []");
        let diags = check_empty_exclude(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
    }

    #[rstest]
    fn nonempty_exclude_list_produces_no_diagnostic() {
        let overlay = setup_overlay("target = \"~\"\nexclude = [\"*.bak\"]");
        let diags = check_empty_exclude(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn invalid_glob_in_exclude_errors() {
        let overlay = setup_overlay("target = \"~\"\nexclude = [\"[invalid\"]");
        let diags = check_invalid_exclude_globs(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("invalid glob"));
    }

    #[rstest]
    fn valid_globs_in_exclude_produce_no_diagnostic() {
        let overlay = setup_overlay("target = \"~\"\nexclude = [\"*.bak\", \".git\"]");
        let diags = check_invalid_exclude_globs(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn exclude_as_single_string_is_accepted() {
        let overlay = setup_overlay("target = \"~\"\nexclude = \"*.bak\"");
        let diags = check_invalid_exclude_globs(&overlay);
        assert!(diags.is_empty());
    }

    // ── terminal boundaries ──────────────────────────────────────────────

    #[rstest]
    fn empty_shared_rules_list_warns() {
        let overlay = setup_overlay("target = \"~\"\nrules = []");
        let diags = check_empty_rules(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
    }

    #[rstest]
    fn a_terminal_directory_reports_descendant_mode_overrides() {
        let overlay = setup_overlay(
            "mode = 'symlink'\n[[rules]]\npath = 'config'\nmode = 'symlink-directory'\n[[rules]]\npath = 'config/editor'\nmode = 'checkout'",
        );
        let diags = check_terminal_rules(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].message.contains("unreachable"));
    }

    #[rstest]
    fn reopening_a_directory_makes_descendant_rules_reachable() {
        let overlay = setup_overlay(
            "[[rules]]\npath = 'config'\nmode = 'symlink-directory'\n[[rules]]\npath = 'config'\nmode = 'symlink'\n[[rules]]\npath = 'config/editor'\nmode = 'checkout'",
        );
        let diags = check_terminal_rules(&overlay);
        assert!(diags.is_empty());
    }

    // ── materialization rules checks (#113/#126) ─────────────────────────

    #[rstest]
    fn empty_rules_list_warns() {
        let overlay = setup_overlay("target = \"~\"\nrules = []");
        let diags = check_empty_rules(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
    }

    #[rstest]
    fn checkout_boundaries_report_descendant_mode_overrides() {
        let overlay =
            setup_overlay("mode = 'checkout'\n[[rules]]\npath = 'config'\nmode = 'symlink'");
        let diags = check_terminal_rules(&overlay);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("checkout"));
    }

    #[rstest]
    fn valid_glob_in_rule_path_produces_no_diagnostic() {
        let overlay = setup_overlay(
            "target = \"~\"\n[[rules]]\npath = \".config/*\"\nmode = \"symlink-directory\"",
        );
        let diags = check_invalid_rules_globs(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn duplicate_rule_paths_warn() {
        let overlay = setup_overlay(
            r#"
target = "~"
[[rules]]
path = "dup"
mode = "symlink"

[[rules]]
path = "dup"
mode = "symlink-directory"
"#,
        );
        let diags = check_duplicate_rule_paths(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(diags[0].message.contains("duplicate rule path"));
    }

    #[rstest]
    fn nonexistent_rule_path_warns() {
        let overlay = setup_overlay(
            "target = \"~\"\n[[rules]]\npath = \"missing\"\nmode = \"symlink-directory\"",
        );
        let diags = check_rules_paths_exist(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(diags[0].message.contains("does not match any file"));
    }

    #[rstest]
    fn permission_only_paths_need_not_exist_in_the_walked_tree() {
        let overlay =
            setup_overlay("target = \"~\"\n[[rules]]\npath = \"ssh/agent\"\npermissions = \"600\"");
        let diags = check_rules_paths_exist(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn existing_rule_path_produces_no_diagnostic() {
        let overlay = setup_overlay(
            "target = \"~\"\n[[rules]]\npath = \"present\"\nmode = \"symlink-directory\"",
        );
        std::fs::create_dir_all(overlay.root.join("present")).unwrap();
        let diags = check_rules_paths_exist(&overlay);
        assert!(diags.is_empty());
    }

    // ── permission rules checks (#65) ─────────────────────────────────────

    #[rstest]
    fn permission_overrides_below_checkout_remain_reachable_for_partials() {
        let overlay =
            setup_overlay("mode = 'checkout'\n[[rules]]\npath = 'ssh/agent'\npermissions = '600'");
        assert!(check_terminal_rules(&overlay).is_empty());
    }

    #[rstest]
    fn independent_properties_at_the_same_path_are_not_duplicates() {
        let overlay = setup_overlay(
            "[[rules]]\npath = 'ssh'\nmode = 'symlink'\n[[rules]]\npath = 'ssh'\npermissions = '600'",
        );
        assert!(check_duplicate_rule_paths(&overlay).is_empty());
    }

    #[rstest]
    fn permission_prefixes_produce_no_invalid_selector_diagnostic() {
        let overlay =
            setup_overlay("target = \"~\"\n[[rules]]\npath = \"secrets\"\npermissions = \"600\"");
        let diags = check_invalid_rules_globs(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn duplicate_permission_rule_paths_warn() {
        let overlay = setup_overlay(
            r#"
target = "~"
[[rules]]
path = "dup"
permissions = "600"

[[rules]]
path = "dup"
permissions = "644"
"#,
        );
        let diags = check_duplicate_rule_paths(&overlay);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(diags[0].message.contains("duplicate rule path"));
    }

    // ── git checks ───────────────────────────────────────────────────────

    #[rstest]
    fn git_empty_url_errors() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git]
url = ""
"#,
        );
        let diags = check_git(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("`url` is empty"))
        );
    }

    #[rstest]
    fn git_branch_and_tag_conflict_errors() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git]
url = "https://example.com/repo.git"
branch = "main"
tag = "v1.0"
"#,
        );
        let diags = check_git(&overlay);
        assert!(
            diags.iter().any(|d| d.severity == Severity::Error
                && d.message.contains("conflicting ref specifiers"))
        );
    }

    #[rstest]
    fn git_tag_ignored_in_worktree_mode_warns() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git]
url = "https://example.com/repo.git"
tag = "v1.0"
worktree = true
"#,
        );
        let diags = check_git(&overlay);
        assert!(diags.iter().any(|d| d.severity == Severity::Warning
            && d.message.contains("`tag` is ignored in worktree mode")));
    }

    #[rstest]
    fn git_key_with_path_traversal_errors() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git."../../etc"]
url = "https://example.com/repo.git"
"#,
        );
        let diags = check_git(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("without traversal"))
        );
    }

    #[rstest]
    fn valid_git_config_produces_no_diagnostics() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git.".config/nvim"]
url = "https://example.com/repo.git"
branch = "main"
"#,
        );
        let diags = check_git(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn git_empty_worktrees_map_warns() {
        let overlay = setup_overlay(
            r#"
target = "~"

[git]
url = "https://example.com/repo.git"
worktree = true

[git.worktrees]
"#,
        );
        let diags = check_git(&overlay);
        assert!(diags.iter().any(
            |d| d.severity == Severity::Warning && d.message.contains("empty `worktrees` map")
        ));
    }

    // ── check_overlay aggregation ────────────────────────────────────────

    #[rstest]
    fn clean_overlay_produces_no_diagnostics() {
        let overlay = setup_overlay("target = \"~\"");
        let diags = check_overlay(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn overlay_with_multiple_issues_aggregates_all_warnings() {
        let overlay = setup_overlay("target = \"~\"\nuses = []\nexclude = []\nrules = []");
        let diags = check_overlay(&overlay);
        // Each explicitly empty collection should receive its own actionable warning.
        assert_eq!(diags.len(), 3);
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
    }

    #[rstest]
    fn hard_symlink_type_warns() {
        let overlay = setup_overlay("target = \"~\"");
        let link_file = overlay.root.join("test.link.toml");
        std::fs::write(&link_file, "target = \"/foo\"\ntype = \"hard\"").unwrap();
        let diags = check_symlinks(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("hard"))
        );
    }

    #[rstest]
    fn valid_symlink_config_produces_no_diagnostics() {
        let overlay = setup_overlay("target = \"~\"");
        let link_file = overlay.root.join("test.link.toml");
        std::fs::write(&link_file, "target = \"/foo\"").unwrap();
        let diags = check_symlinks(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn invalid_symlink_target_template_errors() {
        let overlay = setup_overlay("target = \"~\"");
        let link_file = overlay.root.join("test.link.toml");
        std::fs::write(&link_file, "target = \"{{ invalid\"").unwrap();
        let diags = check_symlinks(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("invalid template"))
        );
    }

    // ── partial checks (#66) ─────────────────────────────────────────────

    #[rstest]
    fn valid_partial_config_produces_no_diagnostics() {
        let overlay = setup_overlay("target = \"~\"");
        let partial_file = overlay.root.join("test.partial.toml");
        std::fs::write(&partial_file, "target = \"/foo\"\ncontent = \"x\"").unwrap();
        let diags = check_partials(&overlay);
        assert!(diags.is_empty());
    }

    #[rstest]
    fn invalid_partial_target_template_errors() {
        let overlay = setup_overlay("target = \"~\"");
        let partial_file = overlay.root.join("test.partial.toml");
        std::fs::write(&partial_file, "target = \"{{ invalid\"\ncontent = \"x\"").unwrap();
        let diags = check_partials(&overlay);
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("invalid template"))
        );
    }

    #[rstest]
    fn malformed_partial_toml_reports_discover_error() {
        let overlay = setup_overlay("target = \"~\"");
        let partial_file = overlay.root.join("bad.partial.toml");
        // Malformed TOML syntax (not just a missing/empty field) makes
        // `discover_partials` itself return an `Err`, not just skip an
        // entry.
        std::fs::write(&partial_file, "not valid toml [[[").unwrap();
        let diags = check_partials(&overlay);
        assert!(diags.iter().any(|d| d.severity == Severity::Error
            && d.message.contains("failed to discover partial configs")));
    }
}
