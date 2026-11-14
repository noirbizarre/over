//! Consumer-owned metadata, composed independently of filesystem operations.

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
    if let Some(value) = source.collect()?.get("data")
        && !matches!(value.kind, ValueKind::Table(_))
    {
        bail!("`data` must be a mapping");
    }
    Ok(())
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
    overlay: Overlay,
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
    for name in overlay.uses.iter().flatten() {
        let dependency = repo.get(name).with_context(|| {
            format!(
                "failed to resolve data dependency '{name}' of '{}'",
                overlay.name
            )
        })?;
        visit(repo, dependency, visited, stack, ordered)?;
    }
    stack.remove(&overlay.name);
    visited.insert(overlay.name.clone());
    ordered.push(overlay);
    Ok(())
}

impl Repository {
    /// Resolve host/default metadata plus an optional command target's metadata.
    ///
    /// Dependencies merge before their parent, in `uses` order. The default
    /// closure precedes the selected closure; shared overlays contribute once.
    /// The selected root always merges last, even if the default uses it.
    /// `ctx.no_uses` affects operations, never this metadata closure.
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
        self.resolve_data_many(ctx, selected.map(std::slice::from_ref).unwrap_or_default())
    }

    /// Merge ordered roots like a `uses` composition, after repository defaults.
    /// A single selected root retains its historical highest precedence.
    pub fn resolve_data_many(&self, ctx: &exec::Context, selected: &[Overlay]) -> Result<DataMap> {
        // Repeating a root must not discard its single-root precedence guarantee.
        let mut selected_names = HashSet::new();
        let selected: Vec<_> = selected
            .iter()
            .filter(|overlay| selected_names.insert(&overlay.name))
            .collect();
        let mut ordered = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = HashSet::new();
        if let Some(default) = self.default_overlay(ctx)? {
            visit(self, default, &mut visited, &mut stack, &mut ordered)?;
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
        if let [selected] = selected.as_slice() {
            // A host may use the command target: deduplication must not reverse
            // the explicitly selected root's promised precedence.
            if let Some(index) = ordered.iter().position(|o| o.name == selected.name) {
                let selected = ordered.remove(index);
                ordered.push(selected);
            }
        }
        let mut data = DataMap::new();
        for overlay in ordered {
            for (key, value) in overlay.data {
                if let Some(previous) = data.get_mut(&key) {
                    merge_value(previous, value);
                } else {
                    data.insert(key, value);
                }
            }
        }
        Ok(data)
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

    #[rstest]
    #[case("data: null")]
    #[case("data: []")]
    #[case("data: text")]
    #[case("data: 42")]
    #[case("data: true")]
    fn non_mapping_data_is_rejected_even_when_a_child_overrides_it(#[case] invalid: &str) {
        let temp = TempDir::new().unwrap();
        descriptor(&temp, "", invalid);
        descriptor(&temp, "selected", "data: {valid: true}");
        let repo = Repository::new(temp.path().into());
        let error = repo.get("selected").unwrap_err();
        assert!(format!("{error:#}").contains("`data` must be a mapping"));
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
    fn recursive_merging_replaces_lists_and_preserves_explicit_nulls() {
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
                "nested: {keep: yes, change: new}\nlist: [three]\ncleared: null\nscalar: {new: value}\nmap: replacement"
            )
        );
        assert!(data.contains_key("cleared"));
        assert!(!data.contains_key("absent"));
    }

    #[test]
    fn host_and_selected_closures_deduplicate_shared_dependencies_in_uses_order() {
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
        // Replaying shared while visiting either sibling would erase host priority.
        assert_eq!(
            data,
            mapping("priority: host\nshared: true\nhost: true\nsibling: second\nselected: true")
        );
        let host_only = repo.resolve_data(&ctx, None).unwrap();
        assert_eq!(
            host_only,
            mapping("priority: host\nshared: true\nhost: true")
        );
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
    #[case(vec!["parent", "dependency"], "parent")]
    #[case(vec!["dependency", "parent"], "parent")]
    #[case(vec!["parent", "sibling", "dependency"], "sibling")]
    fn multiple_explicit_roots_merge_in_dependency_order_without_replaying_contributors(
        #[case] names: Vec<&str>,
        #[case] winner: &str,
    ) {
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
        assert_eq!(data, mapping(&format!("host: true\npriority: {winner}")));
    }

    #[test]
    fn selected_inherited_values_override_host_values() {
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
            mapping("priority: inherited")
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
            mapping("dependency: true\nselected: true")
        );
    }

    #[rstest]
    #[case("default_overlay: missing", "data: {}")]
    #[case("default_overlay: '{{ broken'", "data: {}")]
    #[case("default_overlay: host", "uses: [missing]")]
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
            ctx.with_overlay(repo.get("legacy").unwrap()),
            ctx.with_progress(indicatif::ProgressBar::hidden()),
            ctx.with_multiprogress(indicatif::MultiProgress::new()),
            ctx.with_resolved_overlay("legacy".into(), "/target".into()),
        ];
        for copy in copies {
            assert_eq!(copy.data, ctx.data);
            assert_eq!(
                exec::templates::render_string("{{ data.nested.value }}", copy.as_ref()).unwrap(),
                "available"
            );
            assert_eq!(
                crate::actions::symlink::render_symlink_target("{{ data.nested.value }}", &copy)
                    .unwrap(),
                "available"
            );
        }
    }
}
