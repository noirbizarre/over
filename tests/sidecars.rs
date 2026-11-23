use std::fs;

use assert_cmd::Command;
use rstest::rstest;
use serde_json::json;

#[rstest]
fn unknown_sidecar_attributes_warn_with_the_source_path_and_preserve_optional_defaults(
    #[values("toml", "yaml", "yml")] format: &str,
    #[values("partial", "link")] kind: &str,
    #[values("typo", "valid", "absent")] optional: &str,
) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(source.join("overlay/nested")).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(source.join("overlay/over.yaml"), "target: '~'\n").unwrap();
    let referent = temp.path().join("referent");
    fs::write(&referent, "linked").unwrap();
    let mut config = if kind == "partial" {
        json!({"target": "~/settings", "content": "managed"})
    } else {
        json!({"target": referent})
    };
    let (field, value) = if kind == "partial" {
        ("marker", "custom")
    } else {
        ("type", "hard")
    };
    let typo = if kind == "partial" { "makrer" } else { "types" };
    match optional {
        "valid" => config[field] = json!(value),
        "typo" => config[typo] = json!("unknown-value-sentinel"),
        _ => {}
    }
    let sidecar = source.join(format!("overlay/nested/entry.{kind}.{format}"));
    fs::write(
        &sidecar,
        if format == "toml" {
            toml::to_string(&config).unwrap()
        } else {
            // JSON is a YAML subset, keeping path escaping portable on Windows.
            serde_json::to_string(&config).unwrap()
        },
    )
    .unwrap();
    let output = Command::new(assert_cmd::cargo::cargo_bin!("over"))
        .env("NO_COLOR", "1")
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .env("XDG_CACHE_HOME", temp.path().join("cache"))
        .env_remove("OVER_HOOK_ACTIVE")
        .arg("--home")
        .arg(&source)
        .args(["apply", "overlay", "--no-prompt", "--target"])
        .arg(&target)
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    if optional == "typo" {
        assert!(
            stderr.contains(&sidecar.to_string_lossy().to_string()),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!("unknown key `{typo}` ignored")),
            "{stderr}"
        );
    } else {
        assert!(!stderr.contains("unknown key"), "{stderr}");
    }
    assert!(!stderr.contains("unknown-value-sentinel"));
    if kind == "partial" {
        let marker = if optional == "valid" {
            "custom"
        } else {
            "nested/entry"
        };
        assert_eq!(
            fs::read_to_string(target.join("settings")).unwrap(),
            format!("# >>> over: {marker} >>>\nmanaged\n# <<< over: {marker} <<<\n")
        );
    } else {
        let deployed = target.join("nested/entry");
        if optional == "valid" {
            assert!(!deployed.is_symlink());
            assert!(same_file::is_same_file(deployed, referent).unwrap());
        } else {
            assert_eq!(fs::read_link(deployed).unwrap(), referent);
        }
    }
}

#[rstest]
#[case("partial", "target")]
#[case("partial", "content")]
#[case("link", "target")]
fn malformed_required_sidecar_fields_still_fail_with_the_source_path(
    #[values("toml", "yaml", "yml")] format: &str,
    #[case] kind: &str,
    #[case] field: &str,
    #[values(false, true)] missing: bool,
) {
    let temp = tempfile::tempdir().unwrap();
    let mut config = if kind == "partial" {
        json!({"target": "~/settings", "content": "managed"})
    } else {
        json!({"target": "/referent"})
    };
    if missing {
        config.as_object_mut().unwrap().remove(field);
    } else {
        config[field] = json!([]);
    }
    let sidecar = temp.path().join(format!("entry.{kind}.{format}"));
    fs::write(
        &sidecar,
        if format == "toml" {
            toml::to_string(&config).unwrap()
        } else {
            serde_json::to_string(&config).unwrap()
        },
    )
    .unwrap();
    let error = if kind == "partial" {
        over::actions::partial::discover_partials(temp.path()).unwrap_err()
    } else {
        over::actions::symlink::discover_symlinks(temp.path()).unwrap_err()
    };
    let message = format!("{error:#}");
    assert!(
        message.contains(sidecar.file_name().unwrap().to_str().unwrap()),
        "{message}"
    );
    assert!(message.contains(field), "{message}");
}

#[rstest]
fn yaml_sidecars_retain_scalar_type_validation_and_source_locations(
    #[values("yaml", "yml")] format: &str,
    #[values("partial", "link")] kind: &str,
) {
    let temp = tempfile::tempdir().unwrap();
    let sidecar = temp.path().join(format!("entry.{kind}.{format}"));
    fs::write(&sidecar, "target: 123\ncontent: true\n").unwrap();
    let error = if kind == "partial" {
        over::actions::partial::discover_partials(temp.path()).unwrap_err()
    } else {
        over::actions::symlink::discover_symlinks(temp.path()).unwrap_err()
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("expected string, found integer"),
        "{message}"
    );
    assert!(message.contains("line 1 column 9"), "{message}");
}

#[rstest]
fn yaml_sidecars_still_reject_duplicate_known_fields(
    #[values("yaml", "yml")] format: &str,
    #[values("partial", "link")] kind: &str,
) {
    let temp = tempfile::tempdir().unwrap();
    let sidecar = temp.path().join(format!("entry.{kind}.{format}"));
    fs::write(
        &sidecar,
        "target: first\ntarget: second\ncontent: managed\n",
    )
    .unwrap();
    let error = if kind == "partial" {
        over::actions::partial::discover_partials(temp.path()).unwrap_err()
    } else {
        over::actions::symlink::discover_symlinks(temp.path()).unwrap_err()
    };
    assert!(format!("{error:#}").contains("duplicate field `target`"));
}
