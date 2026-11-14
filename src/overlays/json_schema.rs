//! Editor schemas describe authored descriptors rather than resolved overlays.

use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, generate::SchemaSettings, json_schema};
use serde_json::Value;

use crate::actions::git::config::AllGitForms;
use crate::hooks::Hooks;

use super::repository::RootConfig;
use super::{FileMode, MaterializationKind, Rule};

pub const CONFIG_SCHEMA_URL: &str = "https://noirbizarre.github.io/over/schemas/config.json";
pub const OVERLAY_SCHEMA_URL: &str = "https://noirbizarre.github.io/over/schemas/overlay.json";

// Resolved Overlay requires injected paths and defaults; those are not authored requirements.
#[expect(
    dead_code,
    reason = "these adapters are used only by the schema derive"
)]
#[derive(JsonSchema)]
#[schemars(deny_unknown_fields)]
struct OverlayDescriptor {
    /// Document-local editor schema association; ignored by Over and never inherited.
    #[schemars(
        rename = "$schema",
        default,
        with = "String",
        skip_serializing_if = "Option::is_none"
    )]
    schema: Option<String>,
    /// Human-readable description of this overlay.
    description: Option<String>,
    /// Consumer-owned metadata; nested values are not interpreted by Over.
    #[schemars(
        default,
        with = "BTreeMap<String, Value>",
        skip_serializing_if = "Option::is_none"
    )]
    data: Option<BTreeMap<String, Value>>,
    /// Destination directory. Omit to inherit the ancestor's target (default: ~).
    #[schemars(default, with = "String", skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    /// Compose these repository-relative overlay names in declaration order.
    uses: Option<Vec<String>>,
    /// Source patterns excluded from materialization; a string or a list.
    #[schemars(
        default,
        with = "StringOrList",
        skip_serializing_if = "Option::is_none"
    )]
    exclude: Option<StringOrList>,
    /// Clone one repository or map destination paths to repository declarations.
    git: Option<AllGitForms>,
    /// Default materialization strategy for this scope.
    #[schemars(
        default,
        with = "MaterializationKind",
        skip_serializing_if = "Option::is_none"
    )]
    mode: Option<MaterializationKind>,
    /// Octal permissions for managed content.
    #[schemars(default, with = "FileMode", skip_serializing_if = "Option::is_none")]
    permissions: Option<FileMode>,
    /// Ordered declaration-relative policy overrides.
    #[schemars(default, with = "Vec<Rule>", skip_serializing_if = "Option::is_none")]
    rules: Option<Vec<Rule>>,
    /// Local lifecycle commands. Hooks never inherit; post-clone and post-init
    /// only execute when declared in the repository-root descriptor.
    #[schemars(default, with = "Hooks", skip_serializing_if = "Option::is_none")]
    hooks: Option<Hooks>,
}

#[expect(
    dead_code,
    reason = "these adapters are used only by the schema derive"
)]
#[derive(JsonSchema)]
#[schemars(deny_unknown_fields)]
struct RootDescriptor {
    #[schemars(flatten)]
    policy: OverlayDescriptor,
    // Reuse the loader's root preferences so new settings also reach the published schema.
    #[schemars(flatten)]
    preferences: RootConfig,
}

#[expect(
    dead_code,
    reason = "these adapters are used only by the schema derive"
)]
#[derive(JsonSchema)]
#[serde(untagged)]
enum StringOrList {
    String(String),
    List(Vec<String>),
}

/// Generate both standalone draft-07 schemas, with no network-dependent references.
pub fn schemas() -> [(&'static str, Schema); 2] {
    [
        (
            "config.json",
            document::<RootDescriptor>(CONFIG_SCHEMA_URL, "Over repository configuration"),
        ),
        (
            "overlay.json",
            document::<OverlayDescriptor>(OVERLAY_SCHEMA_URL, "Over overlay configuration"),
        ),
    ]
}

fn document<T: JsonSchema>(id: &str, title: &str) -> Schema {
    let mut schema = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<T>();
    schema.insert("$id".into(), id.into());
    schema.insert("title".into(), title.into());
    schema.insert("description".into(), "Configuration for Over. Filesystem, inheritance, and template validation is performed by over lint and runtime commands.".into());
    schema
}

pub(crate) fn file_mode(_: &mut SchemaGenerator) -> Schema {
    // Match the u32 parser's range while permitting leading zeroes, its optional plus sign,
    // and the repeated prefixes removed by trim_start_matches in FileMode::parse.
    json_schema!({"type": "string", "pattern": r"^(0o)*\+?0*([0-7]{1,10}|[0-3][0-7]{10})$"})
}

pub(crate) fn git_config(_: &mut SchemaGenerator) -> Schema {
    let mut value = serde_json::json!({"type": "string"});
    // The custom loader flattens up to three levels; deeper objects would fail at runtime.
    for _ in 0..2 {
        value = serde_json::json!({"anyOf": [
            {"type": "string"}, {"type": "object", "additionalProperties": value}
        ]});
    }
    json_schema!({"type": "object", "additionalProperties": value})
}

#[derive(JsonSchema)]
#[schemars(with = "String", extend("pattern" = r"^[^\u0000]*[^\s\u0000][^\u0000]*$"))]
pub(crate) struct HookCommand;

pub(crate) fn hook_env(_: &mut SchemaGenerator) -> Schema {
    // Mirror the recursion guards without making consumer environment names a closed namespace.
    let reserved: Vec<_> = crate::hooks::RESERVED_ENV
        .iter()
        .map(|name| {
            let insensitive: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphabetic() {
                        format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
                    } else {
                        c.to_string()
                    }
                })
                .collect();
            serde_json::json!({"pattern": format!("^{insensitive}$")})
        })
        .collect();
    json_schema!({
        "type": "object",
        "propertyNames": {"minLength": 1, "pattern": r"^[^=\u0000]+$", "not": {"anyOf": reserved}},
        "additionalProperties": {"type": "string", "pattern": r"^[^\u0000]*$"}
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::LazyLock;

    use noyalib::compat::serde_yaml;
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use serde_json::json;

    use super::*;
    use crate::overlays::{Repository, schema};

    static DOCUMENTS: LazyLock<BTreeMap<&str, Value>> = LazyLock::new(|| {
        schemas()
            .into_iter()
            .map(|(name, schema)| (name, serde_json::to_value(schema).unwrap()))
            .collect()
    });
    static VALIDATORS: LazyLock<BTreeMap<&str, jsonschema::Validator>> = LazyLock::new(|| {
        DOCUMENTS
            .iter()
            .map(|(name, schema)| (*name, jsonschema::validator_for(schema).unwrap()))
            .collect()
    });

    fn properties(value: &Value) -> BTreeSet<&str> {
        value["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn generated_schemas_conform_to_the_json_schema_metaschema() {
        for (_, schema) in schemas() {
            let value = serde_json::to_value(&schema).unwrap();
            jsonschema::meta::validate(&value).unwrap();
        }
    }

    #[test]
    fn documented_properties_match_runtime_recognized_keys_at_every_closed_location() {
        let root = &DOCUMENTS["config.json"];
        let child = &DOCUMENTS["overlay.json"];
        let root_only = [
            "format",
            "repository",
            "sync",
            "default_overlay",
            "overlays",
        ];
        assert_eq!(
            properties(root),
            schema::OVERLAY_KEYS.iter().copied().collect()
        );
        assert_eq!(
            properties(child),
            schema::OVERLAY_KEYS
                .iter()
                .copied()
                .filter(|key| !root_only.contains(key))
                .collect()
        );
        for (pointer, expected) in [
            ("/definitions/Hooks", schema::EVENTS),
            ("/definitions/HookDefinition", schema::HOOK_FIELDS),
            ("/definitions/SyncConfig", schema::SYNC_FIELDS),
            ("/definitions/Rule", schema::RULE_FIELDS),
            (
                "/definitions/WorktreeEntry/anyOf/1",
                schema::WORKTREE_FIELDS,
            ),
            ("/definitions/GitRepoConfig/anyOf/1", schema::REPO_FIELDS),
        ] {
            let definition = root.pointer(pointer).unwrap();
            assert_eq!(
                properties(definition),
                expected.iter().copied().collect(),
                "{pointer}"
            );
            assert_eq!(definition["additionalProperties"], false, "{pointer}");
        }
        assert_eq!(root["additionalProperties"], false);
        assert_eq!(child["additionalProperties"], false);
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"$schema": CONFIG_SCHEMA_URL}))]
    #[case(json!({"$schema": OVERLAY_SCHEMA_URL}))]
    #[case(json!({"target":"~", "description":"Editor", "uses":["base"]}))]
    #[case(json!({"exclude":"*.tmp"}))]
    #[case(json!({"exclude":["*.tmp", "cache"]}))]
    #[case(json!({"data":{"custom":{"nested":[null, true, 1, "text"]}}}))]
    #[case(json!({"rules":[{"path":"config","target":"~/.config"}]}))]
    #[case(json!({"mode":"checkout", "permissions":"0o600", "rules":[{"path":"config", "permissions":"0644"}]}))]
    #[case(json!({"rules":[{"path":"apps/*", "mode":"symlink-directory"}]}))]
    #[case(json!({"rules":[{"path":"config/*.pub", "permissions":"0666"}]}))]
    #[case(json!({"rules":[{"path":"config", "target":"~/settings", "mode":"checkout", "permissions":"0600"}]}))]
    #[case(json!({"rules":[{"path":"config", "mode":null, "permissions":"644"}]}))]
    #[case(json!({"rules":[{"path":"**/*.jinja", "template":true, "strip_extension":true}]}))]
    #[case(json!({"rules":[{"path":"config", "template":false}]}))]
    #[case(json!({"rules":[{"path":"config", "strip_extension":false}]}))]
    #[case(json!({"git":"git@example.com:repo.git"}))]
    #[case(json!({"git":{"url":"../source", "branch":"main"}}))]
    #[case(json!({"git":{"vendor":"../source", "other":{"url":"https://example.com/r.git"}}}))]
    #[case(json!({"git":{"url":"../source", "worktrees":{"dev":"main", "test":{"branch":"test", "config":{"user.email":"me@example.com"}}}}}))]
    #[case(json!({"git":{"url":"../source", "config":{"user":{"email":"me@example.com"}, "remote":{"origin":{"fetch":"refs/*"}}}, "remotes":{"backup":{"url":"../backup", "custom":"value"}}}}))]
    #[case(json!({"hooks":{"install":["echo hello", {"command":"echo world", "env":{"CUSTOM":"value"}, "cwd":"~", "when":"{{ true }}"}], "post-apply":[]}}))]
    fn supported_authored_shapes_are_valid_in_both_scopes(#[case] value: Value) {
        for validator in VALIDATORS.values() {
            assert!(
                validator.is_valid(&value),
                "{:?}",
                validator.iter_errors(&value).collect::<Vec<_>>()
            );
        }
    }

    #[rstest]
    #[case(json!({"format":"yaml"}))]
    #[case(json!({"repository":"git@example.com:dotfiles.git"}))]
    #[case(json!({"sync":{"pull":true}}))]
    #[case(json!({"default_overlay":"hosts/laptop"}))]
    #[case(json!({"overlays":["apps/*", "hosts/laptop"]}))]
    #[case(json!({"overlays":[]}))]
    fn repository_preferences_are_valid_only_at_the_root(#[case] value: Value) {
        assert!(VALIDATORS["config.json"].is_valid(&value));
        assert!(!VALIDATORS["overlay.json"].is_valid(&value));
    }

    #[rstest]
    #[case(json!({"name":"injected"}))]
    #[case(json!({"root":"/injected"}))]
    #[case(json!({"scopes":[]}))]
    #[case(json!({"unknown":"~"}))]
    #[case(json!({"$schema":null}))]
    #[case(json!({"$schema":true}))]
    #[case(json!({"target":null}))]
    #[case(json!({"data":null}))]
    #[case(json!({"data":[]}))]
    #[case(json!({"map":null}))]
    #[case(json!({"map":{"config":{}}}))]
    #[case(json!({"map":{"config":{"target":"~", "extra":true}}}))]
    #[case(json!({"uses":"base"}))]
    #[case(json!({"exclude":null}))]
    #[case(json!({"exclude":[true]}))]
    #[case(json!({"mode":"copy"}))]
    #[case(json!({"mode":null}))]
    #[case(json!({"permissions":644}))]
    #[case(json!({"permissions":"999"}))]
    #[case(json!({"permissions":null}))]
    #[case(json!({"rules":null}))]
    #[case(json!({"rules":[{"path":"config"}]}))]
    #[case(json!({"rules":[{"path":".", "target":"~/redirected"}]}))]
    #[case(json!({"rules":[{"path":"./", "mode":"checkout"}]}))]
    #[case(json!({"rules":[{"path":"./.", "permissions":"0600"}]}))]
    #[case(json!({"rules":[{"path":"config", "mode":null}]}))]
    #[case(json!({"rules":[{"path":"config", "template":null}]}))]
    #[case(json!({"rules":[{"path":"config", "strip_extension":"true"}]}))]
    #[case(json!({"rules":[{"path":" ", "mode":"checkout"}]}))]
    #[case(json!({"rules":[{"mode":"checkout"}]}))]
    #[case(json!({"rules":[{"path":"apps/*", "target":"~/apps"}]}))]
    #[case(json!({"rules":[{"path":"config", "target":" "}]}))]
    #[case(json!({"rules":[{"path":"config", "mode":"checkout", "materialization":"checkout"}]}))]
    #[case(json!({"git":{"vendor":{"branch":"main"}}}))]
    #[case(json!({"git":{"url":{"url":"../source"}, "vendor":"../vendor"}}))]
    #[case(json!({"git":{"url":"../source", "unknown":"main"}}))]
    #[case(json!({"git":{"url":"../source", "worktrees":{"dev":{}}}}))]
    #[case(json!({"git":{"url":"../source", "config":{"a":{"b":{"c":{"d":"too deep"}}}}}}))]
    #[case(json!({"git":{"url":"../source", "remotes":{"backup":{"url":"../backup", "custom":true}}}}))]
    #[case(json!({"hooks":null}))]
    #[case(json!({"hooks":{"unknown":[]}}))]
    #[case(json!({"hooks":{"install":"echo hello"}}))]
    #[case(json!({"hooks":{"install":[{}]}}))]
    #[case(json!({"hooks":{"install":[""]}}))]
    #[case(json!({"hooks":{"install":["\u{0}"]}}))]
    #[case(json!({"hooks":{"install":[" \n\t "]}}))]
    #[case(json!({"hooks":{"install":[{"command":"echo hello", "extra":true}]}}))]
    #[case(json!({"hooks":{"install":[{"command":"echo hello", "env":{"CUSTOM":1}}]}}))]
    #[case(json!({"hooks":{"install":[{"command":"echo hello", "env":{"over_hook_active":"1"}}]}}))]
    #[case(json!({"hooks":{"install":[{"command":"echo hello", "env":{"BAD=NAME":"value"}}]}}))]
    #[case(json!({"hooks":{"install":[{"command":"echo hello", "cwd":null}]}}))]
    #[case(json!({"hooks":{"install":[{"command":"echo hello", "when":"\u{0}"}]}}))]
    fn invalid_shapes_are_rejected_in_both_scopes(#[case] value: Value) {
        for (name, validator) in VALIDATORS.iter() {
            assert!(!validator.is_valid(&value), "{name} accepted {value}");
        }
    }

    #[rstest]
    #[case(json!({"format":"yml"}))]
    #[case(json!({"repository":null}))]
    #[case(json!({"sync":{"pull":"yes"}}))]
    #[case(json!({"sync":{"extra":true}}))]
    #[case(json!({"overlays":[{}]}))]
    #[case(json!({"overlays":[{"path":"apps/*"}]}))]
    #[case(json!({"overlays":["shared", {"path":"apps/*"}]}))]
    #[case(json!({"overlays":[true]}))]
    #[case(json!({"overlays":[42]}))]
    #[case(json!({"overlays":"apps/*"}))]
    #[case(json!({"overlays":[{"path":"apps/*", "extra":true}]}))]
    fn malformed_root_preferences_are_rejected(#[case] value: Value) {
        assert!(!VALIDATORS["config.json"].is_valid(&value));
    }

    #[rstest]
    #[case("0", true)]
    #[case("37777777777", true)]
    #[case("0000000000037777777777", true)]
    #[case("0o37777777777", true)]
    #[case("+644", true)]
    #[case("0o0o644", true)]
    #[case("40000000000", false)]
    #[case("0o40000000000", false)]
    #[case("777777777777777777777777777777", false)]
    fn permission_boundaries_match_the_runtime_parser(#[case] input: &str, #[case] valid: bool) {
        assert_eq!(FileMode::parse(input).is_ok(), valid);
        let value = json!({"permissions": input});
        for validator in VALIDATORS.values() {
            assert_eq!(validator.is_valid(&value), valid, "{input}");
        }
    }

    #[rstest]
    #[case("toml")]
    #[case("yaml")]
    #[case("yml")]
    fn parsed_descriptors_validate_and_load_with_the_same_meaning(#[case] extension: &str) {
        let (root, child) = if extension == "toml" {
            (
                "format = 'yaml'\ntarget = '~/shared'\noverlays = ['app']\n[sync]\npull = false\n",
                "exclude = ['*.tmp']\n[git.vendor]\nurl = '../source'\n[hooks]\npost-apply = ['echo ready']\n",
            )
        } else {
            (
                "format: yaml\ntarget: '~/shared'\noverlays: [app]\nsync: {pull: false}\n",
                "exclude: ['*.tmp']\ngit: {vendor: {url: ../source}}\nhooks: {post-apply: ['echo ready']}\n",
            )
        };
        let (root, child) = if extension == "toml" {
            (root.to_string(), child.to_string())
        } else {
            (
                format!("$schema: {CONFIG_SCHEMA_URL}\n{root}"),
                format!("$schema: {OVERLAY_SCHEMA_URL}\n{child}"),
            )
        };
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("app")).unwrap();
        for (source, scope, directory) in [
            (root, "config.json", temp.path().to_path_buf()),
            (child, "overlay.json", temp.path().join("app")),
        ] {
            let value = if extension == "toml" {
                serde_json::to_value(toml::from_str::<toml::Value>(&source).unwrap()).unwrap()
            } else {
                serde_yaml::from_str::<Value>(&source).unwrap()
            };
            assert!(VALIDATORS[scope].is_valid(&value));
            std::fs::write(directory.join(format!("over.{extension}")), source).unwrap();
        }
        let repo = Repository::new(temp.path().to_path_buf()).collecting_warnings();
        assert!(!repo.sync_config().unwrap().pull);
        let overlay = repo.get("app").unwrap();
        assert_eq!(overlay.target, "~/shared");
        assert_eq!(overlay.exclude, Some(vec!["*.tmp".to_string()]));
        // Local remotes resolve from the declaring descriptor, not the caller's working directory.
        assert_eq!(
            std::path::Path::new(&overlay.git.unwrap()["vendor"].url),
            temp.path().join("source")
        );
        assert_eq!(overlay.hooks.post_apply.len(), 1);
        assert!(repo.warnings.warnings().is_empty());
    }
}
