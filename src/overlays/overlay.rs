use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use config::{Config, File, FileFormat, FileSourceFile, Source};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};

use crate::actions;
use crate::actions::git::config::{GitRepoConfig, deserialize_git_field};
use crate::actions::install::InstallConfig;
use crate::desired::DesiredTree;
use crate::exec::{self, Ctx};
use crate::git_exclude;
use crate::plan::Plan;
use crate::ui;
use crate::ui::{emojis, style};

use super::permissions::{self, FileMode};
use super::rules::{self, MaterializationKind, PolicyScope, Rule};
use super::{DEFAULT_TARGET, Repository};

fn default_none() -> Option<Vec<String>> {
    None
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};

    struct StringOrVecVisitor;

    impl<'de> Visitor<'de> for StringOrVecVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or a list of strings")
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![value.to_owned()])
        }

        fn visit_seq<S>(self, mut seq: S) -> std::result::Result<Self::Value, S::Error>
        where
            S: de::SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(elem) = seq.next_element()? {
                vec.push(elem);
            }
            Ok(vec)
        }
    }

    let value = deserializer.deserialize_any(StringOrVecVisitor)?;
    Ok(Some(value))
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Overlay {
    /// Executable commands never inherit through the descriptor cascade.
    #[serde(skip)]
    pub hooks: crate::hooks::Hooks,
    pub name: String,

    pub root: PathBuf,

    pub description: Option<String>,

    /// Consumer-owned metadata, inherited just like other descriptor fields.
    #[serde(default)]
    pub data: super::DataMap,

    pub target: String,

    /// Directory prefixes relocated independently of the base target.
    #[serde(default)]
    pub map: std::collections::BTreeMap<String, super::mapping::DirectoryMapping>,

    pub uses: Option<Vec<String>>,

    #[serde(
        default = "default_none",
        deserialize_with = "deserialize_string_or_vec"
    )]
    pub exclude: Option<Vec<String>>,

    #[serde(default, deserialize_with = "deserialize_git_field")]
    pub git: Option<HashMap<String, GitRepoConfig>>,

    pub install: Option<InstallConfig>,

    pub mode: Option<MaterializationKind>,
    pub permissions: Option<FileMode>,
    pub rules: Option<Vec<Rule>>,
    /// Runtime policy retains declaration order and origins lost by ordinary merging.
    #[serde(skip)]
    pub scopes: Vec<PolicyScope>,
    #[serde(skip)]
    pub(crate) target_origin: PathBuf,
    #[serde(skip)]
    pub(crate) mapping_origins: std::collections::BTreeMap<String, PathBuf>,
    /// Reporting metadata contains declaration paths, never configuration values.
    #[serde(skip)]
    pub(crate) field_origins: std::collections::BTreeMap<String, PathBuf>,
}

impl fmt::Display for Overlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl Overlay {
    /// Build an overlay from its own directory, cascading config from
    /// every ancestor up to and including the repository root (ADR-002).
    /// The overlay's own descriptor is *not* required to exist (#127,
    /// ADR-018): a directory declared only via the repository root's
    /// `overlays:` key (or requested directly via `Repository::get`) with
    /// zero local `over.*` anywhere in its ancestor chain still resolves,
    /// entirely from inherited config plus hardcoded defaults (e.g.
    /// `target`'s `set_default` below).
    pub fn new(repository: &Repository, root: &Path) -> Result<Self> {
        // Normalize to `/` so overlay names are portable identifiers that
        // don't leak the platform's native path separator (`\` on Windows).
        let name = root
            .strip_prefix(repository.root.as_path())?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("overlay path is not valid UTF-8"))?
            .replace('\\', "/");
        let mut sources: Vec<File<FileSourceFile, FileFormat>> = Vec::new();
        let mut dir = root;
        loop {
            let basename = dir.join("over");
            sources.push(
                File::with_name(
                    basename
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("config path is not valid UTF-8"))?,
                )
                // Never required (#127, ADR-018): every level of the
                // cascade, including the overlay's own directory, is
                // optional — a fully descriptor-less overlay still
                // resolves via ancestor config + repository defaults.
                .required(false),
            );
            if dir == repository.root {
                break;
            }
            dir = dir
                .parent()
                .ok_or_else(|| anyhow::anyhow!("unexpected root path without parent"))?;
        }

        // Reverse so that ancestor configs are added first (lower priority)
        // and overlay-specific config is added last (higher priority / overrides)
        sources.reverse();

        let mut local_hooks = crate::hooks::Hooks::default();
        let mut scopes = Vec::new();
        let mut scoped_sources = Vec::new();
        let mut mapping_origins = std::collections::BTreeMap::new();
        let mut target_origin = root.to_path_buf();
        let mut field_origins = std::collections::BTreeMap::new();
        let directories: Vec<_> = root
            .ancestors()
            .take_while(|path| path.starts_with(&repository.root))
            .collect();
        for (index, source) in sources.iter().enumerate() {
            // Validate separately before the cascade can hide an invalid parent hook.
            let mut values = source.collect().map_err(|_| {
                anyhow::anyhow!("descriptor syntax error (source contents omitted)")
            })?;
            let directory = directories[directories.len() - 1 - index];
            let descriptor = super::EXTENSIONS
                .iter()
                .map(|extension| directory.join(format!("over.{extension}")))
                .find(|path| path.is_file())
                .unwrap_or_else(|| directory.join("over"));
            for key in ["default", "defaults", "materialization", "link_dirs"] {
                anyhow::ensure!(
                    !values.contains_key(key),
                    "{}: `{key}` is no longer supported; use `mode`, scalar `permissions`, and `rules` (directory links use `mode: symlink-directory`)",
                    descriptor.display()
                );
            }
            if let Some(value) = values.get("permissions") {
                anyhow::ensure!(
                    !matches!(value.kind, config::ValueKind::Array(_)),
                    "{}: `permissions` must now be an octal string; move permission overrides into `rules` with a `permissions` property",
                    descriptor.display()
                );
            }
            if let Some(config::Value {
                kind: config::ValueKind::Array(rules),
                ..
            }) = values.get("rules")
            {
                for (index, rule) in rules.iter().enumerate() {
                    if let config::ValueKind::Table(fields) = &rule.kind {
                        anyhow::ensure!(
                            !fields.contains_key("materialization"),
                            "{}: rule {} uses removed `materialization`; replace it with `mode`",
                            descriptor.display(),
                            index + 1
                        );
                    }
                }
            }
            let mode = values
                .get("mode")
                .map(|value| value.clone().try_deserialize())
                .transpose()?;
            let permissions = values.get("permissions").map(|value| value.clone().try_deserialize::<FileMode>()).transpose().with_context(|| format!("{}: `permissions` must be an octal string; move path overrides into `rules`", descriptor.display()))?;
            let rules = values
                .get("rules")
                .map(|value| value.clone().try_deserialize::<Vec<Rule>>())
                .transpose()
                .with_context(|| {
                    format!(
                        "{}: rules use `mode` and/or `permissions`, not `materialization`",
                        descriptor.display()
                    )
                })?
                .unwrap_or_default();
            for (rule_index, rule) in rules.iter().enumerate() {
                rule.validate().with_context(|| {
                    format!("{}: rule {}", descriptor.display(), rule_index + 1)
                })?;
            }
            let exclude = values
                .get("exclude")
                .map(|value| {
                    #[derive(Deserialize)]
                    #[serde(untagged)]
                    enum Exclude {
                        One(String),
                        Many(Vec<String>),
                    }
                    value
                        .clone()
                        .try_deserialize::<Exclude>()
                        .map(|value| match value {
                            Exclude::One(value) => vec![value],
                            Exclude::Many(values) => values,
                        })
                })
                .transpose()?;
            let origin = super::paths::absolute_origin(directory)?;
            for key in ["target", "uses", "install", "git"] {
                if values.contains_key(key) {
                    field_origins.insert(key.to_owned(), descriptor.clone());
                }
            }
            scopes.push(PolicyScope {
                root: origin.clone(),
                descriptor,
                mode,
                permissions,
                rules,
                exclude,
            });
            if values.contains_key("target") {
                target_origin = origin.clone();
            }
            if let Some(value) = values.get_mut("map") {
                let table = value.clone().into_table()?;
                let mut normalized = config::Map::new();
                for (key, mapping) in table {
                    if let Some(logical) = super::paths::logical_path(&key, directory, root)? {
                        if mapping.clone().into_table()?.contains_key("target") {
                            mapping_origins.insert(logical.clone(), origin.clone());
                        }
                        normalized.insert(logical, mapping);
                    }
                }
                value.kind = config::ValueKind::Table(normalized);
            }
            if let Some(value) = values.get_mut("install") {
                super::paths::resolve_install_paths(value, &origin);
            }
            if let Some(value) = values.get_mut("git") {
                super::paths::resolve_git_paths(value, &origin, root)?;
            }
            if let Some(value) = values.get("hooks") {
                let hooks = value.clone().try_deserialize::<crate::hooks::Hooks>()?;
                // Root hooks remain repository-scoped, including for a root overlay.
                if index + 1 == sources.len() && root != repository.root {
                    local_hooks = hooks;
                }
            }
            super::data::validate_source(source)
                .with_context(|| format!("invalid data in descriptor source {source:?}"))?;
            scoped_sources.push(super::paths::ScopedSource(values));
        }

        let s = Config::builder()
            .add_source(scoped_sources)
            .set_override("name", name.clone())?
            .set_override("root", root.to_str())?
            .set_default("target", DEFAULT_TARGET)?
            .build()
            .with_context(|| format!("invalid overlay descriptor in '{}'", root.display(),))?;

        let mut overlay: Self = s.try_deserialize().with_context(|| {
            format!(
                "invalid overlay descriptor for '{}': check required fields (target)",
                name,
            )
        })?;
        overlay.hooks = local_hooks;
        overlay.scopes = scopes;
        overlay.target_origin = target_origin;
        overlay.mapping_origins = mapping_origins;
        overlay.field_origins = field_origins;
        Ok(overlay)
    }

    pub fn resolve_target(&self, ctx: &exec::Context) -> Result<PathBuf> {
        super::mapping::resolve_path(&self.target, &self.target_origin, ctx)
    }

    pub fn resolve_mappings(
        &self,
        ctx: &exec::Context,
    ) -> Result<super::mapping::ResolvedMappings> {
        super::mapping::ResolvedMappings::new(self, ctx)
    }

    /// Resolve a directory's mode using the ordered descriptor cascade.
    pub fn materialization_for(&self, rel_path: &Path) -> MaterializationKind {
        rules::resolve(self, rel_path)
    }

    /// Whether a directory owns its subtree as one symlink unit.
    pub fn is_link_dir(&self, rel_path: &Path) -> bool {
        self.materialization_for(rel_path) == MaterializationKind::SymlinkDirectory
    }

    /// Resolve permissions independently through the same ordered rules.
    /// Only meaningful for entries `over`
    /// writes content for directly (`PartialFile`, #65); symlinked entries
    /// share their source's permission and are never looked up this way.
    pub fn permission_for(&self, rel_path: &Path) -> Option<FileMode> {
        permissions::resolve(self, rel_path)
    }

    /// Check if a relative path matches any `exclude` glob pattern.
    /// Returns `false` if no exclude patterns are configured.
    /// Patterns stay anchored to their declaring scope; basename patterns
    /// also match nested basenames within that scope.
    pub fn is_excluded(&self, rel_path: &Path) -> bool {
        let source = self
            .scopes
            .last()
            .map(|scope| &scope.root)
            .unwrap_or(&self.root)
            .join(rel_path);
        // Exclude lists keep their replacement inheritance, but never lose their declaring base.
        let Some(scope) = self
            .scopes
            .iter()
            .rev()
            .find(|scope| scope.exclude.is_some())
        else {
            return false;
        };
        let Ok(relative) = source.strip_prefix(&scope.root) else {
            return false;
        };
        for pattern in scope.exclude.iter().flatten() {
            if let Ok(glob) = GlobBuilder::new(pattern).literal_separator(true).build() {
                let matcher = glob.compile_matcher();
                let candidate = if Path::new(pattern).is_absolute() {
                    source.as_path()
                } else {
                    relative
                };
                for path in candidate.ancestors() {
                    if matcher.is_match(path) {
                        return true;
                    }
                    // Basename patterns retain their recursive meaning within the declaring scope.
                    if !pattern.contains('/')
                        && path
                            .file_name()
                            .is_some_and(|name| matcher.is_match(Path::new(name)))
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub async fn apply(&self, ctx: &Ctx) -> Result<()> {
        // A flat union has no winner: reject every claim before any participant writes.
        DesiredTree::build(ctx, self)?.validate_ownership()?;
        let mut visited = HashSet::new();
        let mut stack = Vec::new();
        let mut participants = Vec::new();
        self.collect_participants(ctx, &mut visited, &mut stack, &mut participants)?;
        let root_hooks = ctx.repository.hooks()?;
        let hooks_enabled =
            !root_hooks.is_empty() || participants.iter().any(|overlay| !overlay.hooks.is_empty());
        if hooks_enabled {
            // New executable configuration requires an explicit validated repository;
            // descriptor-less repositories without hooks retain their existing behavior.
            ctx.repository.validate_initialization()?;
        }
        let mut applied = Vec::new();
        for overlay in &participants {
            let overlay_ctx = ctx.with_overlay(overlay.clone());
            overlay.apply_own(&overlay_ctx).await?;
            applied.push((overlay, overlay.resolve_target(&overlay_ctx)?));
        }
        if ctx.dry_run || !hooks_enabled || crate::hooks::nested() {
            return Ok(());
        }
        // Defer all hooks until every participant succeeds, including later siblings.
        let selected_target = self.resolve_target(ctx)?;
        crate::hooks::run(
            &root_hooks,
            crate::hooks::Event::PostApply,
            crate::hooks::HookContext {
                repository: &ctx.repository.root,
                cwd: &ctx.repository.root,
                scope: "repository root",
                overlay: Some((&self.name, &self.root, &selected_target)),
                target_root: Some(&ctx.root),
            },
        )
        .await?;
        for (overlay, target) in applied {
            crate::hooks::run(
                &overlay.hooks,
                crate::hooks::Event::PostApply,
                crate::hooks::HookContext {
                    repository: &ctx.repository.root,
                    cwd: &overlay.root,
                    scope: &format!("overlay '{}'", overlay.name),
                    overlay: Some((&overlay.name, &overlay.root, &target)),
                    target_root: Some(&ctx.root),
                },
            )
            .await?;
        }
        Ok(())
    }

    /// Validate before external installers can make irreversible changes.
    pub fn validate_apply(&self, ctx: &Ctx) -> Result<()> {
        // Installers run before apply, so they need the same composed preflight.
        DesiredTree::build(ctx, self)?.validate_ownership()?;
        let mut participants = Vec::new();
        self.collect_participants(ctx, &mut HashSet::new(), &mut Vec::new(), &mut participants)?;
        if !ctx.repository.hooks()?.is_empty()
            || participants.iter().any(|overlay| !overlay.hooks.is_empty())
        {
            ctx.repository.validate_initialization()?;
        }
        for overlay in participants {
            let overlay_ctx = ctx.with_overlay(overlay.clone());
            let desired = DesiredTree::build_own(&overlay_ctx, &overlay)?;
            let plan = Plan::build(&desired)?;
            plan.validate_boundaries()?;
            anyhow::ensure!(
                !ctx.no_prompt || ctx.force || !plan.has_conflicts(),
                "unresolved apply conflicts prevent installation under --no-prompt"
            );
        }
        Ok(())
    }

    fn collect_participants(
        &self,
        ctx: &Ctx,
        visited: &mut HashSet<String>,
        stack: &mut Vec<String>,
        participants: &mut Vec<Self>,
    ) -> Result<()> {
        // Already included via another dependency path — skip silently.
        if visited.contains(&self.name) {
            return Ok(());
        }
        // Currently in the recursion stack — true cycle
        if stack.contains(&self.name) {
            stack.push(self.name.clone());
            anyhow::bail!(
                "Cycle detected: overlay '{}' forms a cycle (path: {})",
                self.name,
                stack.join(" -> ")
            );
        }
        stack.push(self.name.clone());

        if ctx.no_uses {
            tracing::debug!(overlay = %self.name, "skipping uses (--no-uses)");
        } else {
            for name in self.uses.iter().flatten() {
                let overlay = ctx
                    .repository
                    .get(name)
                    .with_context(|| format!("used overlay '{name}' not found"))?;
                if ctx.debug {
                    tracing::debug!(?overlay, "used overlay");
                }
                overlay.collect_participants(ctx, visited, stack, participants)?;
            }
        }
        stack.pop();
        visited.insert(self.name.clone());
        // Keep a snapshot so hooks cannot change configuration for later hooks.
        participants.push(self.clone());
        Ok(())
    }

    async fn apply_own(&self, ctx: &Ctx) -> Result<()> {
        let target = self.resolve_target(ctx)?;
        ui::info(format!(
            "{} {} {} {} {}",
            emojis::PACKAGE,
            style::white_b("Applying overlay"),
            style::cyan(&self.name),
            style::white_b("to"),
            style::cyan(&target.to_string_lossy()),
        ))
        .ok();

        // Preflight before clone-time reconciliation, then rebuild after
        // provisioning so no stale migration deletes a newly cloned repository.
        let ctx_with_target =
            ctx.with_resolved_overlay(self.name.clone(), target.to_string_lossy().to_string());
        let desired = DesiredTree::build_own(&ctx_with_target, self)?;
        let mut plan = Plan::build(&desired)?;
        if ctx.verbose || ctx.dry_run {
            ui::info(format!("{plan}")).ok();
        }
        plan.validate_boundaries()?;
        if self.git.is_some() && plan.has_blocked_children() {
            // Parallel clone provisioning cannot run through an obstructed scaffold.
            // The plan authorizes its replacement before provisioning inspects children.
            plan.prepare_scaffolds(ctx_with_target.clone()).await?;
            plan = Plan::build(&desired)?;
            plan.validate_boundaries()?;
        }
        // Classify every ownership scope before cloning can mutate a destination.
        if !ctx.dry_run || !plan.has_blocked_children() {
            // A preview cannot inspect clone destinations through a scaffold it
            // deliberately leaves intact; the plan reports those pending children.
            actions::git::clone_repositories(ctx.clone(), self, &target).await?;
        }
        if self.git.is_some() {
            // Cloning may already perform a legacy-checkout migration;
            // never execute the stale pre-clone destructive operation.
            plan = Plan::build(&desired)?;
        }
        plan.execute(ctx_with_target).await?;

        // #146: exclude this overlay's managed paths (symlinks,
        // checkouts) from any git repository they landed in, so a
        // target repo's `git status` stops listing them as untracked.
        // Skipped on `--dry-run` (nothing was actually materialized to
        // exclude). Real I/O failures propagate like any other apply
        // step; tracked-path conflicts and malformed blocks are
        // reported as warnings by `reconcile` itself, never as errors —
        // it only ever touches `over`'s own marker block, never the
        // repo's tracked state.
        if !ctx.dry_run {
            git_exclude::reconcile_desired(&desired).with_context(|| {
                format!(
                    "failed to reconcile git excludes for overlay '{}'",
                    self.name
                )
            })?;
        }

        ui::info(format!(
            "{} {} {} {} {} {}",
            emojis::SPARKLE,
            style::white_b("Applied overlay"),
            style::cyan(&self.name),
            style::white_b("to"),
            style::cyan(&target.to_string_lossy()),
            style::white_b("with success"),
        ))
        .ok();

        Ok(())
    }

    pub async fn add_file(&self, ctx: &Ctx, file: &PathBuf) -> Result<()> {
        self.add_files(ctx, std::slice::from_ref(file)).await
    }

    pub async fn add_files(&self, ctx: &Ctx, files: &[PathBuf]) -> Result<()> {
        let remaining = crate::stage::add(ctx, self, files).await?;
        for file in remaining {
            actions::fs::add_path(ctx.clone(), self, &file).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Context;
    use crate::overlays::Repository;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use rstest::rstest;
    use std::fs;

    fn repo_and_root() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::new(td.path().to_path_buf());
        (td, repo)
    }

    #[rstest]
    #[case(false, false)]
    #[case(true, false)]
    #[case(false, true)]
    #[case(true, true)]
    #[tokio::test]
    async fn composed_ownership_refusal_precedes_all_participant_side_effects(
        #[case] reverse: bool,
        #[case] force: bool,
    ) {
        let (td, repo) = repo_and_root();
        for name in ["first", "second"] {
            td.child(format!("{name}/over.yaml"))
                .write_str("target: '~'")
                .unwrap();
            td.child(format!("{name}/same")).write_str(name).unwrap();
        }
        td.child("first/otherwise-valid")
            .write_str("untouched")
            .unwrap();
        td.child("selected/over.yaml")
            .write_str(&format!(
                "uses: [{}]\ngit:\n  clone: /nonexistent/ownership-preflight.git",
                if reverse {
                    "second, first"
                } else {
                    "first, second"
                }
            ))
            .unwrap();
        let overlay = repo.get("selected").unwrap();
        let root = td.path().join("target");
        let context = Context::builder()
            .root(root.clone())
            .repository(repo)
            .no_prompt(true)
            .force(force)
            .build();
        let before_install = overlay.validate_apply(&context).unwrap_err();
        assert!(
            before_install
                .to_string()
                .contains("conflicting target ownership")
        );
        let error = overlay.apply(&context).await.unwrap_err();
        assert!(error.to_string().contains("conflicting target ownership"));
        assert!(
            !root.exists(),
            "no directory, child link, or clone may be created"
        );
        assert_eq!(
            fs::read_to_string(td.path().join("first/same")).unwrap(),
            "first"
        );
    }

    fn ctx(root: PathBuf, repo: Repository, overlay: Option<Overlay>) -> Ctx {
        let mut builder = Context::builder().root(root).repository(repo);
        if let Some(o) = overlay {
            builder = builder.overlay(o);
        }
        builder.build()
    }

    #[rstest]
    #[case("~", |root: &PathBuf| root.clone())]
    #[case("~/sub", |root: &PathBuf| root.join("sub"))]
    fn tilde_target_resolves_relative_to_context_root(
        #[case] target: &str,
        #[case] expected: fn(&PathBuf) -> PathBuf,
    ) {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"{}\"", target))
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let root = td.path().to_path_buf();
        let c = ctx(root.clone(), repo.clone(), Some(overlay.clone()));
        let resolved = overlay.resolve_target(&c).unwrap();
        assert_eq!(resolved, expected(&root));
    }

    #[rstest]
    fn absolute_target_is_used_verbatim() {
        let (td, repo) = repo_and_root();
        let abs = td.child("abs_root");
        abs.create_dir_all().unwrap();
        let overlay_dir = td.child("abs");
        overlay_dir.create_dir_all().unwrap();
        let target_str = abs.to_str().unwrap();
        // Escape backslashes so Windows paths don't get misparsed as TOML
        // unicode escape sequences.
        let target_toml = target_str.replace('\\', "\\\\");
        overlay_dir
            .child("over.toml")
            .write_str(&format!("target = \"{}\"", target_toml))
            .unwrap();
        let overlay = repo.get("abs").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let resolved = overlay.resolve_target(&c).unwrap();
        assert_eq!(resolved, PathBuf::from(target_str));
    }

    #[test]
    fn resolve_target_renders_machine_os_template_variable() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~/{{ machine.os }}\"")
            .unwrap();
        let overlay = repo.get("ov").unwrap();
        let root = td.path().to_path_buf();
        let c = ctx(root.clone(), repo.clone(), Some(overlay.clone()));
        let resolved = overlay.resolve_target(&c).unwrap();
        assert_eq!(resolved, root.join(std::env::consts::OS));
    }

    #[tokio::test]
    async fn apply_also_applies_used_overlays() {
        let (td, repo) = repo_and_root();
        let child_dir = td.child("child");
        child_dir.create_dir_all().unwrap();
        child_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        child_dir.child("file.txt").write_str("content").unwrap();

        let parent_dir = td.child("parent");
        parent_dir.create_dir_all().unwrap();
        parent_dir
            .child("over.toml")
            .write_str("target = \"~\"\nuses = [\"child\"]")
            .unwrap();

        let parent = repo.get("parent").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(parent.clone()));
        let result = parent.apply(&c).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn add_file_moves_file_into_overlay_and_symlinks_original_path() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_add");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let original_file = td.path().join("my.txt");
        fs::write(&original_file, "hello world").unwrap();

        let overlay = repo.get("ov_add").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.add_file(&c, &original_file).await;
        assert!(result.is_ok(), "add_file should succeed");

        let moved_path = overlay.root.join("my.txt");
        assert!(
            moved_path.exists(),
            "moved file should exist in overlay root"
        );
        assert_eq!(fs::read_to_string(&moved_path).unwrap(), "hello world");

        assert!(
            original_file.exists(),
            "original path should exist as symlink"
        );
        let symlink_target = fs::read_link(&original_file).unwrap();
        assert_eq!(symlink_target, moved_path);
    }

    #[tokio::test]
    async fn add_file_outside_overlay_target_root_errors() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_err");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let overlay = repo.get("ov_err").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let outside = assert_fs::TempDir::new().unwrap();
        let outside_file = outside.path().join("ext.txt");
        fs::write(&outside_file, "external").unwrap();

        let result = overlay.add_file(&c, &outside_file).await;
        assert!(
            result.is_err(),
            "adding file outside target root should error"
        );
    }

    #[rstest]
    fn directory_link_rules_match_literal_prefixes_and_directory_globs() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_ld");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\n[[rules]]\npath = '.config/nvim'\nmode = 'symlink-directory'\n[[rules]]\npath = '.local/share/*'\nmode = 'symlink-directory'")
            .unwrap();
        let overlay = repo.get("ov_ld").unwrap();

        assert!(overlay.is_link_dir(Path::new(".config/nvim")));
        assert!(overlay.is_link_dir(Path::new(".local/share/fonts")));
        assert!(!overlay.is_link_dir(Path::new(".config/other")));
        assert!(!overlay.is_link_dir(Path::new("random")));
    }

    #[test]
    fn overlay_with_zero_local_descriptor_resolves_via_ancestor_config() {
        let (td, repo) = repo_and_root();
        // Repository root descriptor, providing the only `target` in the
        // whole ancestor chain.
        td.child("over.toml")
            .write_str("target = \"~/inherited\"")
            .unwrap();
        // The overlay's own directory has no `over.*` file at all.
        let overlay_dir = td.child("hosts/laptop");
        overlay_dir.create_dir_all().unwrap();

        let overlay = repo.get("hosts/laptop").unwrap();
        assert_eq!(overlay.name, "hosts/laptop");
        assert_eq!(overlay.target, "~/inherited");
    }

    #[rstest]
    fn directories_are_not_linked_as_units_without_a_mode_assignment() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_ld_none");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_ld_none").unwrap();

        assert!(!overlay.is_link_dir(Path::new(".config/nvim")));
        assert!(!overlay.is_link_dir(Path::new("anything")));
    }

    #[tokio::test]
    async fn add_files_moves_and_symlinks_every_given_file() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_multi");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();

        let file_a = td.path().join("a.txt");
        let file_b = td.path().join("b.txt");
        fs::write(&file_a, "aaa").unwrap();
        fs::write(&file_b, "bbb").unwrap();

        let overlay = repo.get("ov_multi").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay
            .add_files(&c, &[file_a.clone(), file_b.clone()])
            .await;
        assert!(
            result.is_ok(),
            "add_files should succeed: {:?}",
            result.err()
        );

        assert!(overlay.root.join("a.txt").exists());
        assert!(overlay.root.join("b.txt").exists());
        assert!(file_a.is_symlink());
        assert!(file_b.is_symlink());
    }

    /// Diamond dependency: A uses B and C, both B and C use D.
    /// D should only be applied once and no false cycle error should occur.
    #[tokio::test]
    async fn diamond_shared_dependency_is_applied_only_once() {
        let (td, repo) = repo_and_root();

        // D: leaf overlay used by both B and C
        let d = td.child("d");
        d.create_dir_all().unwrap();
        d.child("over.toml").write_str("target = \"~\"").unwrap();

        // B: uses D
        let b = td.child("b");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"d\"]")
            .unwrap();

        // C: uses D
        let c_ov = td.child("c");
        c_ov.create_dir_all().unwrap();
        c_ov.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"d\"]")
            .unwrap();

        // A: uses B and C
        let a = td.child("a");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"b\", \"c\"]")
            .unwrap();

        let overlay_a = repo.get("a").unwrap();
        let c = ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let result = overlay_a.apply(&c).await;
        assert!(
            result.is_ok(),
            "diamond dependency should not cause a cycle error: {:?}",
            result.err()
        );
    }

    /// True cycle: A uses B, B uses A. Should produce a cycle error.
    #[tokio::test]
    async fn mutual_uses_cycle_errors_naming_the_cycling_overlay() {
        let (td, repo) = repo_and_root();

        let a = td.child("a_cycle");
        a.create_dir_all().unwrap();
        a.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"b_cycle\"]")
            .unwrap();

        let b = td.child("b_cycle");
        b.create_dir_all().unwrap();
        b.child("over.toml")
            .write_str("target = \"~\"\nuses = [\"a_cycle\"]")
            .unwrap();

        let overlay_a = repo.get("a_cycle").unwrap();
        let c = ctx(
            td.path().to_path_buf(),
            repo.clone(),
            Some(overlay_a.clone()),
        );
        let result = overlay_a.apply(&c).await;
        assert!(result.is_err(), "cycle should be detected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Cycle detected"),
            "error should mention cycle: {err_msg}"
        );
        assert!(
            err_msg.contains("a_cycle"),
            "error should mention the cycling overlay: {err_msg}"
        );
    }

    // ── git: <url> shorthand ─────────────────────────────────────────────

    #[test]
    fn git_shorthand_url_string_resolves_under_dot_key() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_git_url");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                r#"
target = "~"
git = "https://github.com/user/repo.git"
"#,
            )
            .unwrap();
        let overlay = repo.get("ov_git_url").unwrap();
        let git = overlay.git.as_ref().expect("git should be Some");
        assert_eq!(git.len(), 1);
        let cfg = &git["."];
        assert_eq!(cfg.url, "https://github.com/user/repo.git");
        assert_eq!(cfg.branch, None);
    }

    #[test]
    fn git_detailed_table_parses_branch_and_submodules() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_git_det");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                r#"
target = "~"

[git]
url = "git@github.com:user/repo.git"
branch = "main"
recurse_submodules = true
"#,
            )
            .unwrap();
        let overlay = repo.get("ov_git_det").unwrap();
        let git = overlay.git.as_ref().expect("git should be Some");
        assert_eq!(git.len(), 1);
        let cfg = &git["."];
        assert_eq!(cfg.url, "git@github.com:user/repo.git");
        assert_eq!(cfg.branch.as_deref(), Some("main"));
        assert!(cfg.recurse_submodules);
    }

    #[test]
    fn git_map_form_configures_multiple_paths_independently() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_git_map");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str(
                r#"
target = "~"

[git]
".tmux/plugins/tpm" = "https://github.com/tmux-plugins/tpm"

[git.".config/nvim"]
url = "git@github.com:user/nvim-config.git"
branch = "main"
"#,
            )
            .unwrap();
        let overlay = repo.get("ov_git_map").unwrap();
        let git = overlay.git.as_ref().expect("git should be Some");
        assert_eq!(git.len(), 2);
        assert_eq!(
            git[".tmux/plugins/tpm"].url,
            "https://github.com/tmux-plugins/tpm"
        );
        assert_eq!(
            git[".config/nvim"].url,
            "git@github.com:user/nvim-config.git"
        );
    }

    #[test]
    fn git_absent_from_config_stays_none() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_no_git");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_no_git").unwrap();
        assert!(overlay.git.is_none());
    }

    #[tokio::test]
    async fn apply_creates_symlink_from_link_sidecar_descriptor() {
        let (td, repo) = repo_and_root();
        let referent = td.path().join("test_symlink_target");
        let overlay_dir = td.child("ov_symlink");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("test.link.toml")
            .write_str(&format!("target = {:?}", referent.to_str().unwrap()))
            .unwrap();

        let overlay = repo.get("ov_symlink").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.apply(&c).await;
        assert!(
            result.is_ok(),
            "apply with symlinks should succeed: {:?}",
            result.err()
        );

        let target_path = td.path().join("test");
        assert!(
            target_path.is_symlink(),
            "symlink should be created at target"
        );
        assert_eq!(fs::read_link(&target_path).unwrap(), referent);
    }

    #[tokio::test]
    async fn apply_symlink_sidecar_filename_determines_target_name() {
        let (td, repo) = repo_and_root();
        let referent = td.path().join("test_symlink_src");
        let overlay_dir = td.child("ov_sympl");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        overlay_dir
            .child("templated.link.toml")
            .write_str(&format!("target = {:?}", referent.to_str().unwrap()))
            .unwrap();

        let overlay = repo.get("ov_sympl").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.apply(&c).await;
        assert!(
            result.is_ok(),
            "apply with templated symlink should succeed: {:?}",
            result.err()
        );

        let target_path = td.path().join("templated");
        assert!(
            target_path.is_symlink(),
            "symlink should be created at overlay target"
        );
        assert_eq!(fs::read_link(&target_path).unwrap(), referent);
    }

    // ── exclude: string or list deserialization ──────────────────────────

    #[rstest]
    fn exclude_as_single_string_becomes_one_pattern() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_exc_str");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();
        let overlay = repo.get("ov_exc_str").unwrap();
        let exclude = overlay.exclude.as_ref().expect("exclude should be Some");
        assert_eq!(exclude.len(), 1);
        assert_eq!(exclude[0], "*.bak");
    }

    #[rstest]
    fn exclude_as_list_preserves_every_pattern_in_order() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_exc_list");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"*.bak\", \".git\", \"*.tmp\"]")
            .unwrap();
        let overlay = repo.get("ov_exc_list").unwrap();
        let exclude = overlay.exclude.as_ref().expect("exclude should be Some");
        assert_eq!(exclude.len(), 3);
        assert_eq!(exclude[0], "*.bak");
        assert_eq!(exclude[1], ".git");
        assert_eq!(exclude[2], "*.tmp");
    }

    #[rstest]
    fn exclude_absent_from_config_is_none() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_no_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_no_exc").unwrap();
        assert!(overlay.exclude.is_none());
    }

    #[rstest]
    fn exclude_empty_list_is_some_of_empty_vec_not_none() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_empty_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = []")
            .unwrap();
        let overlay = repo.get("ov_empty_exc").unwrap();
        let exclude = overlay.exclude.as_ref().expect("exclude should be Some");
        assert!(exclude.is_empty());
    }

    // ── is_excluded method ───────────────────────────────────────────────

    #[rstest]
    fn is_excluded_matches_a_single_configured_pattern() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_is_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();
        let overlay = repo.get("ov_is_exc").unwrap();

        assert!(overlay.is_excluded(Path::new("file.bak")));
        assert!(!overlay.is_excluded(Path::new("file.txt")));
    }

    #[rstest]
    fn is_excluded_matches_pattern_in_nested_subdirectory() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_is_exc_nested");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"**/*.bak\"")
            .unwrap();
        let overlay = repo.get("ov_is_exc_nested").unwrap();

        assert!(overlay.is_excluded(Path::new("file.bak")));
        assert!(overlay.is_excluded(Path::new("subdir/file.bak")));
        assert!(!overlay.is_excluded(Path::new("file.txt")));
    }

    #[rstest]
    fn is_excluded_matches_any_of_several_configured_patterns() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_is_exc_multi");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"*.bak\", \".git\", \"*.tmp\"]")
            .unwrap();
        let overlay = repo.get("ov_is_exc_multi").unwrap();

        assert!(overlay.is_excluded(Path::new("file.bak")));
        assert!(overlay.is_excluded(Path::new(".git")));
        assert!(overlay.is_excluded(Path::new(".git/config")));
        assert!(overlay.is_excluded(Path::new("file.tmp")));
        assert!(!overlay.is_excluded(Path::new("file.txt")));
        assert!(!overlay.is_excluded(Path::new("normal/file.conf")));
    }

    #[rstest]
    fn is_excluded_without_any_configured_patterns_returns_false() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_no_exc_is");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"")
            .unwrap();
        let overlay = repo.get("ov_no_exc_is").unwrap();

        assert!(!overlay.is_excluded(Path::new("anything")));
        assert!(!overlay.is_excluded(Path::new("file.bak")));
    }

    #[rstest]
    fn is_excluded_directory_pattern_also_matches_its_contents() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_dir_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"__pycache__\", \"*.pyc\"]")
            .unwrap();
        let overlay = repo.get("ov_dir_exc").unwrap();

        assert!(overlay.is_excluded(Path::new("__pycache__")));
        assert!(overlay.is_excluded(Path::new("__pycache__/module.cpython.pyc")));
        assert!(overlay.is_excluded(Path::new("src/__pycache__")));
        assert!(overlay.is_excluded(Path::new("module.pyc")));
        assert!(!overlay.is_excluded(Path::new("module.py")));
    }

    // ── apply with exclude ───────────────────────────────────────────────

    #[tokio::test]
    async fn apply_skips_files_matching_exclude_pattern() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_apply_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();
        overlay_dir.child("file.txt").write_str("keep").unwrap();
        overlay_dir.child("file.bak").write_str("skip").unwrap();

        let overlay = repo.get("ov_apply_exc").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.apply(&c).await;
        assert!(result.is_ok(), "apply should succeed: {:?}", result.err());

        let target = td.path().to_path_buf();
        assert!(
            target.join("file.txt").is_symlink(),
            "file.txt should be linked"
        );
        assert!(
            !target.join("file.bak").exists(),
            "file.bak should not exist at target"
        );
    }

    #[tokio::test]
    async fn apply_skips_files_and_directories_matching_any_exclude_pattern() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_apply_exc_multi");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"*.bak\", \".git\", \"*.tmp\"]")
            .unwrap();
        overlay_dir.child("config.yml").write_str("keep").unwrap();
        overlay_dir.child("file.bak").write_str("skip").unwrap();
        overlay_dir.child("file.tmp").write_str("skip").unwrap();
        overlay_dir.child(".git").create_dir_all().unwrap();
        overlay_dir.child(".git/config").write_str("skip").unwrap();

        let overlay = repo.get("ov_apply_exc_multi").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));
        let result = overlay.apply(&c).await;
        assert!(result.is_ok(), "apply should succeed: {:?}", result.err());

        let target = td.path().to_path_buf();
        assert!(
            target.join("config.yml").is_symlink(),
            "config.yml should be linked"
        );
        assert!(
            !target.join("file.bak").exists(),
            "file.bak should not exist"
        );
        assert!(
            !target.join("file.tmp").exists(),
            "file.tmp should not exist"
        );
        assert!(!target.join(".git").exists(), ".git should not exist");
    }

    // ── add_dir with exclude ─────────────────────────────────────────────

    #[tokio::test]
    async fn add_dir_skips_excluded_files_when_copying_into_overlay() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_add_exc");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = \"*.bak\"")
            .unwrap();

        let overlay = repo.get("ov_add_exc").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_dir = td.path().join("srcdir");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("file.txt"), "keep").unwrap();
        fs::write(src_dir.join("file.bak"), "skip").unwrap();

        let res = crate::actions::fs::add_dir(c.clone(), &overlay, &src_dir).await;
        assert!(res.is_ok(), "add_dir should succeed: {:?}", res.err());

        assert!(
            overlay.root.join("srcdir/file.txt").exists(),
            "srcdir/file.txt should be in overlay"
        );
        assert!(
            !overlay.root.join("srcdir/file.bak").exists(),
            "srcdir/file.bak should not be in overlay"
        );
    }

    #[tokio::test]
    async fn add_dir_skips_excluded_files_and_directories_when_nested() {
        let (td, repo) = repo_and_root();
        let overlay_dir = td.child("ov_add_exc_nested");
        overlay_dir.create_dir_all().unwrap();
        overlay_dir
            .child("over.toml")
            .write_str("target = \"~\"\nexclude = [\"**/*.pyc\", \"__pycache__\"]")
            .unwrap();

        let overlay = repo.get("ov_add_exc_nested").unwrap();
        let c = ctx(td.path().to_path_buf(), repo.clone(), Some(overlay.clone()));

        let src_dir = td.path().join("srcdir");
        fs::create_dir_all(src_dir.join("__pycache__")).unwrap();
        fs::write(src_dir.join("module.py"), "keep").unwrap();
        fs::write(src_dir.join("module.pyc"), "skip").unwrap();
        fs::write(src_dir.join("__pycache__/module.cpython.pyc"), "skip").unwrap();

        let res = crate::actions::fs::add_dir(c.clone(), &overlay, &src_dir).await;
        assert!(res.is_ok(), "add_dir should succeed: {:?}", res.err());

        assert!(
            overlay.root.join("srcdir/module.py").exists(),
            "srcdir/module.py should be in overlay"
        );
        assert!(
            !overlay.root.join("srcdir/module.pyc").exists(),
            "srcdir/module.pyc should not be in overlay"
        );
        assert!(
            !overlay.root.join("srcdir/__pycache__").exists(),
            "srcdir/__pycache__ should not be in overlay"
        );
    }
}
