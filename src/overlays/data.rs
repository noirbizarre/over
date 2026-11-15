//! Local metadata, explicit dependency namespaces, and host defaults/overrides.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context as _, Result, bail};
use config::{Source, ValueKind};

use super::{Overlay, Repository};
use crate::exec;

/// A descriptor value, including explicit nulls and nested collections.
pub use noyalib::compat::serde_yaml::Value as DataValue;

/// Consumer namespaces are deliberately not interpreted or validated by Over.
pub type DataMap = BTreeMap<String, DataValue>;

/// Validate before cascading so a child cannot hide an invalid ancestor field.
pub(crate) fn validate_source(source: &impl Source) -> Result<()> {
    let values = source.collect()?;
    for field in ["data", "host"] {
        if let Some(value) = values.get(field)
            && !matches!(value.kind, ValueKind::Table(_))
        {
            bail!("`{field}` must be a mapping");
        }
    }
    Ok(())
}

/// Stable public scope shared by command output, file templates, and hooks.
#[derive(Clone, Default, serde::Serialize)]
pub struct DataContext {
    pub data: DataMap,
    pub overlays: DataMap,
    pub host: DataMap,
}

pub(crate) fn merge_host(defaults: &DataMap, overrides: &DataMap) -> DataMap {
    let mut merged = defaults.clone();
    for (key, value) in overrides {
        if let Some(previous) = merged.get_mut(key) {
            merge_value(previous, value.clone());
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged
}

#[derive(Default)]
struct NamespaceNode<'a> {
    overlay: Option<&'a Overlay>,
    children: BTreeMap<String, NamespaceNode<'a>>,
}

impl NamespaceNode<'_> {
    fn value(&self, targets: &std::collections::HashMap<String, String>) -> Result<DataValue> {
        let mut attributes = DataMap::new();
        if let Some(overlay) = self.overlay {
            attributes.insert("name".into(), DataValue::String(overlay.name.clone()));
            attributes.insert(
                "root".into(),
                noyalib::compat::serde_yaml::to_value(&overlay.root)?,
            );
            attributes.insert(
                "data".into(),
                noyalib::compat::serde_yaml::to_value(&overlay.data)?,
            );
            attributes.insert(
                "host".into(),
                noyalib::compat::serde_yaml::to_value(&overlay.host)?,
            );
            if let Some(target) = targets.get(&overlay.name) {
                attributes.insert("target".into(), DataValue::String(target.clone()));
            }
        }
        for (name, child) in &self.children {
            if let Some(overlay) = self.overlay {
                // Reserve target even before binding it, so validation is phase-independent.
                anyhow::ensure!(
                    !["name", "root", "target", "data", "host"].contains(&name.as_str()),
                    "overlay namespace collision: child '{}/{name}' conflicts with attribute '{name}' of overlay '{}'",
                    overlay.name,
                    overlay.name
                );
            }
            attributes.insert(name.clone(), child.value(targets)?);
        }
        Ok(noyalib::compat::serde_yaml::to_value(attributes)?)
    }
}

/// Present canonical identities without changing their graph or filesystem meaning.
pub(crate) fn namespace(
    selected: Option<&Overlay>,
    overlays: &[Overlay],
    targets: &std::collections::HashMap<String, String>,
) -> Result<DataMap> {
    fn collect<'a>(
        name: &str,
        overlays: &'a [Overlay],
        included: &mut BTreeMap<String, &'a Overlay>,
    ) -> Result<()> {
        if included.contains_key(name) {
            return Ok(());
        }
        let overlay = overlays
            .iter()
            .find(|overlay| overlay.name == name)
            .with_context(|| {
                format!("overlay '{name}' is not in the prepared dependency snapshot")
            })?;
        included.insert(name.to_owned(), overlay);
        for dependency in overlay.uses.iter().flatten() {
            collect(dependency, overlays, included)?;
        }
        Ok(())
    }

    let mut included = BTreeMap::new();
    if let Some(selected) = selected {
        let selected = overlays
            .iter()
            .find(|overlay| overlay.name == selected.name)
            .with_context(|| {
                format!(
                    "overlay '{}' is not in the prepared dependency snapshot",
                    selected.name
                )
            })?;
        for dependency in selected.uses.iter().flatten() {
            collect(dependency, overlays, &mut included)?;
        }
    } else {
        included.extend(
            overlays
                .iter()
                .map(|overlay| (overlay.name.clone(), overlay)),
        );
    }
    let mut tree = NamespaceNode::default();
    for (name, overlay) in included {
        let mut node = &mut tree;
        // Splitting the empty canonical root retains it at overlays[''] without an alias.
        for segment in name.split('/') {
            node = node.children.entry(segment.to_owned()).or_default();
        }
        node.overlay = Some(overlay);
    }
    let DataValue::Mapping(mapping) = tree.value(targets)? else {
        unreachable!("namespace nodes serialize as mappings")
    };
    Ok(mapping.into_iter().collect())
}

fn merge_value(current: &mut DataValue, incoming: DataValue) {
    match (current, incoming) {
        (DataValue::Mapping(current), DataValue::Mapping(incoming)) => {
            for (key, value) in incoming {
                if let Some(previous) = current.get_mut(&key) {
                    merge_value(previous, value);
                } else {
                    current.insert(key, value);
                }
            }
        }
        (current, incoming) => *current = incoming,
    }
}

fn visit(
    repo: &Repository,
    mut overlay: Overlay,
    visited: &mut HashSet<String>,
    stack: &mut HashSet<String>,
    ordered: &mut Vec<Overlay>,
) -> Result<()> {
    if stack.contains(&overlay.name) {
        bail!("cycle in data dependencies at overlay '{}'", overlay.name);
    }
    if visited.contains(&overlay.name) {
        return Ok(());
    }
    stack.insert(overlay.name.clone());
    let mut canonical_dependencies = Vec::new();
    for name in overlay.uses.iter().flatten() {
        let dependency = repo.get(name).with_context(|| {
            format!(
                "failed to resolve data dependency '{name}' of '{}'",
                overlay.name
            )
        })?;
        canonical_dependencies.push(dependency.name.clone());
        visit(repo, dependency, visited, stack, ordered)?;
    }
    // Later snapshot lookups must not depend on authored aliases such as ./apps/ssh/.
    overlay.uses = overlay.uses.map(|_| canonical_dependencies);
    stack.remove(&overlay.name);
    visited.insert(overlay.name.clone());
    ordered.push(overlay);
    Ok(())
}

impl Repository {
    /// Return only the selected overlay's local data (or the configured default's).
    /// Use execution context preparation for the complete three-part scope.
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use over::exec::Context;
    /// use over::overlays::Repository;
    ///
    /// let repository = Repository::new("/path/to/overlays".into());
    /// let context = Context::builder().repository(repository.clone()).build();
    /// let selected = repository.get("base")?;
    /// let data = repository.resolve_data(&context, Some(&selected))?;
    /// let prepared = context.with_overlay(selected).resolve_data()?;
    /// assert_eq!(prepared.data, data);
    /// # Ok(())
    /// # }
    /// ```
    pub fn resolve_data(&self, ctx: &exec::Context, selected: Option<&Overlay>) -> Result<DataMap> {
        Ok(self.resolve_inputs(ctx, selected)?.0)
    }

    pub(crate) fn resolve_inputs(
        &self,
        ctx: &exec::Context,
        selected: Option<&Overlay>,
    ) -> Result<(DataMap, Vec<Overlay>)> {
        self.resolve_inputs_many(ctx, selected.map(std::slice::from_ref).unwrap_or_default())
    }

    /// Return local data for one unique root; a composition has no local data.
    pub fn resolve_data_many(&self, ctx: &exec::Context, selected: &[Overlay]) -> Result<DataMap> {
        Ok(self.resolve_inputs_many(ctx, selected)?.0)
    }

    pub(crate) fn resolve_inputs_many(
        &self,
        ctx: &exec::Context,
        selected: &[Overlay],
    ) -> Result<(DataMap, Vec<Overlay>)> {
        // Repeating one root must retain its local scope rather than invent a composition.
        let mut selected_names = HashSet::new();
        let default = self.default_overlay(ctx)?;
        let mut selected: Vec<_> = selected
            .iter()
            .filter(|overlay| selected_names.insert(&overlay.name))
            .collect();
        let mut ordered = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = HashSet::new();
        if selected.is_empty()
            && let Some(default) = &default
        {
            selected.push(default);
        }
        for selected in &selected {
            visit(
                self,
                (*selected).clone(),
                &mut visited,
                &mut stack,
                &mut ordered,
            )?;
        }
        let data = match selected.as_slice() {
            [selected] => selected.data.clone(),
            _ => DataMap::new(),
        };
        Ok((data, ordered))
    }
}

#[cfg(test)]
mod tests {
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use noyalib::compat::serde_yaml;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::*;
    use crate::exec::Context;

    fn descriptor(temp: &TempDir, name: &str, content: &str) {
        let dir = temp.child(name);
        dir.create_dir_all().unwrap();
        dir.child("over.yaml").write_str(content).unwrap();
    }

    fn mapping(yaml: &str) -> DataMap {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn host_overrides_merge_only_local_defaults_without_visiting_the_host_graph() {
        let temp = TempDir::new().unwrap();
        descriptor(
            &temp,
            "",
            "default_overlay: machine\nhost: {ancestor: forbidden}\ndata: {ancestor: forbidden}",
        );
        descriptor(
            &temp,
            "machine",
            "uses: [missing]\nhost: {ssh: {agent: override}, list: [host], scalar: {new: true}, cleared: null}",
        );
        descriptor(
            &temp,
            "dependency",
            "host: {dependency: forbidden}\ndata: {packages: [dependency]}",
        );
        descriptor(
            &temp,
            "selected",
            "uses: [dependency]\nhost: {ssh: {agent: default, socket: retained}, list: [default], scalar: old, cleared: old}\ndata: {packages: [local]}",
        );
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder()
            .repository(repo.clone())
            .overlay(repo.get("selected").unwrap())
            .build()
            .resolve_data()
            .unwrap();
        assert_eq!(ctx.data, mapping("packages: [local]"));
        assert_eq!(
            ctx.host,
            mapping(
                "ssh: {agent: override, socket: retained}\nlist: [host]\nscalar: {new: true}\ncleared: null"
            )
        );
        let scope = ctx.data_context().unwrap();
        assert_eq!(
            scope.overlays.keys().collect::<Vec<_>>(),
            vec!["dependency"]
        );
        let dependency = ctx.with_overlay(repo.get("dependency").unwrap());
        assert_eq!(dependency.data, mapping("packages: [dependency]"));
        assert_eq!(
            dependency.host["dependency"],
            DataValue::String("forbidden".into())
        );
        assert_eq!(
            dependency.host["ssh"],
            serde_yaml::from_str::<DataValue>("{agent: override}").unwrap()
        );
        // A later descriptor edit cannot change participant defaults or host overrides.
        descriptor(
            &temp,
            "dependency",
            "host: {dependency: changed}\ndata: {packages: [changed]}",
        );
        assert_eq!(
            ctx.with_overlay(repo.get("dependency").unwrap()).data,
            dependency.data
        );
    }

    #[test]
    fn without_a_host_only_current_defaults_apply_and_descriptorless_children_are_empty() {
        let temp = TempDir::new().unwrap();
        descriptor(
            &temp,
            "",
            "host: {ancestor: forbidden}\ndata: {ancestor: forbidden}",
        );
        descriptor(
            &temp,
            "parent",
            "host: {local: retained}\ndata: {local: retained}",
        );
        temp.child("parent/child").create_dir_all().unwrap();
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder()
            .repository(repo.clone())
            .overlay(repo.get("parent").unwrap())
            .build()
            .resolve_data()
            .unwrap();
        assert_eq!(ctx.host, mapping("local: retained"));
        let child = repo.get("parent/child").unwrap();
        assert!(child.data.is_empty());
        assert!(child.host.is_empty());
        let empty = Context::default().data_context().unwrap();
        assert_eq!(
            serde_json::to_value(empty).unwrap(),
            serde_json::json!({"data": {}, "host": {}, "overlays": {}})
        );
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn nested_dependency_namespaces_preserve_prefix_overlays_and_bracket_only_names(
        #[case] reverse: bool,
    ) {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "apps", "data: {prefix: true}");
        descriptor(&temp, "apps/ssh-agent", "data: {packages: [openssh]}");
        let uses = if reverse {
            "[apps/ssh-agent, apps]"
        } else {
            "[apps, ./apps/ssh-agent/]"
        };
        descriptor(&temp, "selected", &format!("uses: {uses}"));
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder()
            .repository(repo.clone())
            .overlay(repo.get("selected").unwrap())
            .build()
            .resolve_data()
            .unwrap();
        assert_eq!(
            exec::templates::render_content(
                "{{ overlays.apps.data.prefix }} {{ overlays.apps['ssh-agent'].data.packages[0] }}",
                &ctx
            )
            .unwrap(),
            "True openssh"
        );
        assert_eq!(ctx.data_context().unwrap().overlays.len(), 1);
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn overlay_attributes_cannot_be_overwritten_by_child_namespaces(#[case] reverse: bool) {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "apps", "{}");
        descriptor(&temp, "apps/data", "{}");
        let uses = if reverse {
            "[apps/data, apps]"
        } else {
            "[apps, apps/data]"
        };
        descriptor(&temp, "selected", &format!("uses: {uses}"));
        let repo = Repository::new(temp.path().into());
        let error = Context::builder()
            .repository(repo.clone())
            .overlay(repo.get("selected").unwrap())
            .build()
            .resolve_data()
            .unwrap_err();
        assert!(format!("{error:#}").contains("apps/data"));
        // Without an apps overlay node, data is simply an ordinary directory segment.
        descriptor(&temp, "selected", "uses: [apps/data]");
        assert!(
            Context::builder()
                .repository(repo.clone())
                .overlay(repo.get("selected").unwrap())
                .build()
                .resolve_data()
                .is_ok()
        );
    }

    #[test]
    fn an_explicit_root_dependency_has_an_empty_string_namespace_key() {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "", "data: {root: accessible}");
        descriptor(&temp, "selected", "uses: ['']");
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder()
            .repository(repo.clone())
            .overlay(repo.get("selected").unwrap())
            .build()
            .resolve_data()
            .unwrap();
        assert_eq!(
            exec::templates::render_content("{{ overlays[''].data.root }}", &ctx).unwrap(),
            "accessible"
        );
    }

    #[rstest]
    #[case("data: null")]
    #[case("data: []")]
    #[case("data: text")]
    #[case("data: 42")]
    #[case("data: true")]
    fn non_mapping_namespaces_are_rejected_even_when_a_child_overrides_them(
        #[case] invalid: &str,
        #[values("data", "host")] field: &str,
    ) {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "", &invalid.replace("data", field));
        descriptor(&temp, "selected", &format!("{field}: {{valid: true}}"));
        let repo = Repository::new(temp.path().into());
        let error = repo.get("selected").unwrap_err();
        assert!(format!("{error:#}").contains(&format!("`{field}` must be a mapping")));
        assert!(crate::lint::lint_repository(&repo).has_errors());
    }

    #[rstest]
    #[case(
        "yaml",
        "data:\n  nested:\n    value: true\n    missing: null\n  list: [one, 2]"
    )]
    #[case("toml", "[data.nested]\nvalue = true\n[data]\nlist = ['one', 2]")]
    fn nested_metadata_loads_in_supported_formats(#[case] ext: &str, #[case] content: &str) {
        let temp = TempDir::new().unwrap();
        temp.child(format!("over.{ext}"))
            .write_str(content)
            .unwrap();
        let repo = Repository::new(temp.path().into());
        let overlay = repo.get("").unwrap();
        assert_eq!(overlay.data["nested"]["value"], DataValue::Bool(true));
        assert_eq!(overlay.data["list"][0], DataValue::String("one".into()));
        assert!(!crate::lint::lint_repository(&repo).has_errors());
    }

    #[test]
    fn local_data_replaces_dependency_values_without_implicit_merging() {
        let temp = TempDir::new().unwrap();
        descriptor(
            &temp,
            "base",
            "data:\n  nested: {keep: yes, change: old}\n  list: [one, two]\n  cleared: old\n  scalar: old\n  map: {old: value}",
        );
        descriptor(
            &temp,
            "selected",
            "uses: [base]\ndata:\n  nested: {change: new}\n  list: [three]\n  cleared: null\n  scalar: {new: value}\n  map: replacement",
        );
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder().repository(repo.clone()).build();
        let data = repo
            .resolve_data(&ctx, Some(&repo.get("selected").unwrap()))
            .unwrap();
        assert_eq!(
            data,
            mapping(
                "nested: {change: new}\nlist: [three]\ncleared: null\nscalar: {new: value}\nmap: replacement"
            )
        );
        assert!(data.contains_key("cleared"));
        assert!(!data.contains_key("absent"));
    }

    #[test]
    fn descriptor_ancestors_do_not_contribute_local_data() {
        let temp = TempDir::new().unwrap();
        descriptor(
            &temp,
            "",
            "data: {app: {inherited: old, changed: old}, retained: true}",
        );
        descriptor(&temp, "selected", "data: {app: {changed: new}}");
        let repo = Repository::new(temp.path().into());
        assert_eq!(
            repo.get("selected").unwrap().data,
            mapping("app: {changed: new}")
        );
    }

    #[test]
    fn prepared_data_is_not_resolved_again_when_descriptors_change_during_execution() {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "selected", "data: {value: original}");
        let repo = Repository::new(temp.path().into());
        let selected = repo.get("selected").unwrap();
        let ctx = Context::builder()
            .repository(repo)
            .overlay(selected)
            .build()
            .resolve_data()
            .unwrap();
        descriptor(&temp, "selected", "data: {value: changed}");
        assert_eq!(
            ctx.resolve_data().unwrap().data["value"],
            DataValue::String("original".into())
        );
    }

    #[test]
    fn host_and_dependency_data_do_not_leak_into_selected_data() {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "", "default_overlay: 'hosts/{{ machine.hostname }}'");
        descriptor(&temp, "shared", "data: {priority: shared, shared: true}");
        descriptor(
            &temp,
            "hosts/workstation",
            "uses: [shared]\ndata: {priority: host, host: true}",
        );
        descriptor(&temp, "first", "uses: [shared]\ndata: {sibling: first}");
        descriptor(&temp, "second", "uses: [shared]\ndata: {sibling: second}");
        descriptor(
            &temp,
            "selected",
            "uses: [first, second]\ndata: {selected: true}",
        );
        descriptor(&temp, "unrelated", "data: {unrelated: true}");
        let repo = Repository::new(temp.path().into());
        let mut machine = exec::MachineInfo::detect();
        machine.hostname = "workstation".into();
        let ctx = Context::builder()
            .repository(repo.clone())
            .machine(machine)
            .no_uses(true)
            .build();
        let selected = repo.get("selected").unwrap();
        let data = repo.resolve_data(&ctx, Some(&selected)).unwrap();
        assert_eq!(data, mapping("selected: true"));
        let host_only = repo.resolve_data(&ctx, None).unwrap();
        assert_eq!(host_only, mapping("priority: host\nhost: true"));
    }

    #[rstest]
    #[case("host", "uses: [selected]\ndata: {priority: host}")]
    #[case("selected", "data: {priority: unused}")]
    fn the_selected_root_wins_even_when_it_is_already_in_the_default_closure(
        #[case] default: &str,
        #[case] host: &str,
    ) {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "", &format!("default_overlay: {default}"));
        descriptor(&temp, "host", host);
        descriptor(&temp, "selected", "data: {priority: selected}");
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder().repository(repo.clone()).build();
        assert_eq!(
            repo.resolve_data(&ctx, Some(&repo.get("selected").unwrap()))
                .unwrap(),
            mapping("priority: selected")
        );
        let repeated = vec![repo.get("selected").unwrap(); 2];
        assert_eq!(
            ctx.resolve_data_many(&repeated).unwrap().data,
            mapping("priority: selected")
        );
    }

    #[rstest]
    #[case(vec!["parent", "dependency"])]
    #[case(vec!["dependency", "parent"])]
    #[case(vec!["parent", "sibling", "dependency"])]
    fn multiple_distinct_roots_have_no_local_data(#[case] names: Vec<&str>) {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "", "default_overlay: host");
        descriptor(&temp, "host", "data: {host: true, priority: host}");
        descriptor(&temp, "dependency", "data: {priority: dependency}");
        descriptor(
            &temp,
            "parent",
            "uses: [dependency]\ndata: {priority: parent}",
        );
        descriptor(
            &temp,
            "sibling",
            "uses: [dependency]\ndata: {priority: sibling}",
        );
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder().repository(repo.clone()).build();
        let roots: Vec<_> = names.iter().map(|name| repo.get(name).unwrap()).collect();
        let data = repo.resolve_data_many(&ctx, &roots).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn empty_local_data_does_not_inherit_ancestor_or_host_values() {
        let temp = TempDir::new().unwrap();
        descriptor(
            &temp,
            "",
            "default_overlay: host\ndata: {priority: inherited}",
        );
        descriptor(&temp, "host", "data: {priority: host}");
        descriptor(&temp, "selected", "data: {}");
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder().repository(repo.clone()).build();
        assert_eq!(
            repo.resolve_data(&ctx, Some(&repo.get("selected").unwrap()))
                .unwrap(),
            DataMap::new()
        );
    }

    #[test]
    fn no_default_resolves_only_the_selected_closure_and_no_selection_is_empty() {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "dependency", "data: {dependency: true}");
        descriptor(
            &temp,
            "selected",
            "uses: [dependency]\ndata: {selected: true}",
        );
        descriptor(&temp, "unrelated", "data: {unrelated: true}");
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder().repository(repo.clone()).build();
        assert!(repo.resolve_data(&ctx, None).unwrap().is_empty());
        assert_eq!(
            repo.resolve_data(&ctx, Some(&repo.get("selected").unwrap()))
                .unwrap(),
            mapping("selected: true")
        );
    }

    #[rstest]
    #[case("default_overlay: missing", "data: {}")]
    #[case("default_overlay: '{{ broken'", "data: {}")]
    fn broken_defaults_fail_even_with_an_explicit_target(#[case] root: &str, #[case] host: &str) {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "", root);
        descriptor(&temp, "host", host);
        descriptor(&temp, "selected", "data: {}");
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder().repository(repo.clone()).build();
        assert!(
            repo.resolve_data(&ctx, Some(&repo.get("selected").unwrap()))
                .is_err()
        );
    }

    #[rstest]
    #[case("uses: [selected]", "cycle")]
    #[case("uses: [missing]", "missing")]
    fn invalid_dependency_graphs_return_contextual_errors(
        #[case] selected: &str,
        #[case] expected: &str,
    ) {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "selected", selected);
        let repo = Repository::new(temp.path().into());
        let ctx = Context::builder().repository(repo.clone()).build();
        let error = repo
            .resolve_data(&ctx, Some(&repo.get("selected").unwrap()))
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected));
    }

    #[test]
    fn legacy_descriptors_and_context_copies_keep_a_stable_metadata_snapshot() {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "legacy", "target: '~'");
        descriptor(&temp, "selected", "data: {nested: {value: available}}");
        let repo = Repository::new(temp.path().into());
        let selected = repo.get("selected").unwrap();
        let ctx = Context::builder()
            .repository(repo.clone())
            .overlay(selected)
            .root(temp.path().join("deployment"))
            .build()
            .resolve_data()
            .unwrap();
        assert!(repo.get("legacy").unwrap().data.is_empty());
        let copies = [
            ctx.with_progress(indicatif::ProgressBar::hidden()),
            ctx.with_multiprogress(indicatif::MultiProgress::new()),
            ctx.with_resolved_overlay("legacy".into(), "/target".into()),
        ];
        for copy in copies {
            assert_eq!(copy.data, ctx.data);
            assert_eq!(
                exec::templates::render_content("{{ data.nested.value }}", &copy).unwrap(),
                "available"
            );
            assert_eq!(
                crate::actions::symlink::render_symlink_target("{{ data.nested.value }}", &copy)
                    .unwrap(),
                "available"
            );
        }
        assert!(
            ctx.with_overlay(repo.get("legacy").unwrap())
                .data_context()
                .is_err()
        );
    }
}
