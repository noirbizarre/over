//! Validate root declarations before discovery can discard broken patterns.

use config::ValueKind;

use super::Diagnostic;
use crate::overlays::Repository;

pub(super) fn check(repo: &Repository) -> Vec<Diagnostic> {
    let Ok((values, descriptor)) = repo.root_source() else {
        // The shared lint pipeline already reports root loading failures.
        return Vec::new();
    };
    let Some(value) = values.get("overlays") else {
        return Vec::new();
    };
    let ValueKind::Array(entries) = &value.kind else {
        // Existing deserialization reports structural errors; inspect usable siblings below.
        return Vec::new();
    };
    let file = descriptor.file_name().unwrap_or_default().to_string_lossy();
    let mut diagnostics = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let ValueKind::String(path) = &entry.kind else {
            continue;
        };
        let declaration = format!("`overlays[{}]` ({path:?})", index + 1);
        if let Some(diagnostic) = check_path(repo, path, &declaration) {
            diagnostics.push(diagnostic.with_file(file.as_ref()));
        }
    }
    diagnostics
}

fn check_path(repo: &Repository, path: &str, declaration: &str) -> Option<Diagnostic> {
    let resolved = repo.root.join(path);
    let Some(pattern) = resolved.to_str() else {
        return Some(
            Diagnostic::error(
                "",
                format!("cannot expand {declaration}: path is not UTF-8"),
            )
            .with_hint("use a UTF-8 source repository path"),
        );
    };
    // Use discovery's glob language and joined path, not globset's different syntax.
    let paths = match glob::glob(pattern) {
        Ok(paths) => paths,
        Err(error) => {
            return Some(
                Diagnostic::error("", format!("invalid glob in {declaration}: {error}"))
                    .with_hint("correct the glob syntax, including balanced character classes"),
            );
        }
    };
    if !path.contains(['*', '?', '[']) {
        return match resolved.try_exists() {
            Ok(true) => None,
            Ok(false) => Some(
                Diagnostic::error(
                    "",
                    format!("{declaration} refers to a path that does not exist"),
                )
                .with_hint("create the path or correct it relative to the source repository root"),
            ),
            Err(error) => Some(
                Diagnostic::error("", format!("cannot inspect {declaration}: {error}"))
                    .with_hint("check source path permissions and accessibility"),
            ),
        };
    }

    let mut matched = false;
    for entry in paths {
        match entry {
            // Files and administration paths cannot be selected by discovery.
            Ok(entry) => matched |= entry.is_dir() && !entry.starts_with(repo.root.join(".over")),
            Err(error) => {
                return Some(
                    Diagnostic::error("", format!("cannot expand {declaration}: {error}"))
                        .with_hint("check source directory permissions and accessibility"),
                );
            }
        }
    }
    (!matched).then(|| {
        Diagnostic::warning("", format!("{declaration} matches no overlay directories"))
            .with_hint("correct the source-root-relative glob, create a matching directory, or keep it if intentionally optional")
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;
    use crate::lint::Severity;

    #[rstest]
    #[case("shared", None)]
    #[case("./shared/", None)]
    #[case("hosts/*", None)]
    #[case("hosts/[ld]*", None)]
    #[case("hosts/[", Some(Severity::Error))]
    #[case("missing", Some(Severity::Error))]
    #[case("optional/*", Some(Severity::Warning))]
    #[case("files/*", Some(Severity::Warning))]
    #[case(".over/*", Some(Severity::Warning))]
    #[case("files/plain", None)]
    fn declarations_distinguish_invalid_missing_and_optional_paths(
        #[case] path: &str,
        #[case] severity: Option<Severity>,
    ) {
        let temp = TempDir::new().unwrap();
        for directory in ["shared", "hosts/laptop", "files", ".over/admin"] {
            fs::create_dir_all(temp.path().join(directory)).unwrap();
        }
        fs::write(temp.path().join("files/plain"), "content").unwrap();
        fs::write(
            temp.path().join("over.toml"),
            format!("overlays = ['{path}']"),
        )
        .unwrap();
        let diagnostics = check(&Repository::new(temp.path().to_path_buf()));
        assert_eq!(diagnostics.first().map(|d| d.severity), severity);
        assert_eq!(diagnostics.len(), usize::from(severity.is_some()));
        if let Some(diagnostic) = diagnostics.first() {
            assert_eq!(diagnostic.overlay, "");
            assert_eq!(diagnostic.file.as_deref(), Some("over.toml"));
            assert!(diagnostic.message.contains("overlays[1]"));
            assert!(diagnostic.message.contains(path));
            assert!(diagnostic.hint.is_some());
        }
    }

    #[rstest]
    #[case("over.toml", "overlays = ['missing']")]
    #[case("over.yaml", "overlays:\n  - missing\n")]
    #[case("over.yml", "overlays:\n  - missing\n")]
    fn findings_identify_the_selected_source_descriptor(#[case] file: &str, #[case] content: &str) {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join(file), content).unwrap();
        let diagnostics = check(&Repository::new(temp.path().to_path_buf()));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].file.as_deref(), Some(file));
    }

    #[rstest]
    #[case(None)]
    #[case(Some("target = '~'"))]
    #[case(Some("overlays = []"))]
    fn absent_declarations_produce_no_findings(#[case] content: Option<&str>) {
        let temp = TempDir::new().unwrap();
        if let Some(content) = content {
            fs::write(temp.path().join("over.toml"), content).unwrap();
        }
        assert!(check(&Repository::new(temp.path().to_path_buf())).is_empty());
    }

    #[test]
    fn relative_repository_roots_resolve_declarations_from_the_source_directory() {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(&cwd).unwrap();
        fs::create_dir(temp.path().join("shared")).unwrap();
        fs::write(
            temp.path().join("over.toml"),
            "overlays = ['shared', 'missing']",
        )
        .unwrap();
        let root = temp.path().strip_prefix(cwd).unwrap().to_path_buf();
        let diagnostics = check(&Repository::new(root));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("overlays[2]"));
        assert_eq!(diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn invalid_fields_and_entries_do_not_hide_readable_declarations() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("over.yaml"),
            "repository: false\noverlays:\n  - missing\n  - false\n  - '[oops'\n  - missing\n  - 'optional/*'\n  - 42\n  - path: legacy\n",
        )
        .unwrap();
        let diagnostics = check(&Repository::new(temp.path().to_path_buf()));
        assert_eq!(diagnostics.len(), 4);
        for (diagnostic, index) in diagnostics.iter().zip([1, 3, 4, 5]) {
            assert!(diagnostic.message.contains(&format!("overlays[{index}]")));
        }
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[1].severity, Severity::Error);
        assert_eq!(diagnostics[2].severity, Severity::Error);
        assert_eq!(diagnostics[3].severity, Severity::Warning);
    }
}
