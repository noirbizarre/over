//! Path provenance is retained before descriptor merging can erase it.

use std::path::{Path, PathBuf};

use anyhow::Result;
use config::{Map, Source, Value, ValueKind};

#[derive(Debug, Clone)]
pub(super) struct ScopedSource(pub Map<String, Value>);

impl Source for ScopedSource {
    fn clone_into_box(&self) -> Box<dyn Source + Send + Sync> {
        Box::new(self.clone())
    }

    fn collect(&self) -> std::result::Result<Map<String, Value>, config::ConfigError> {
        Ok(self.0.clone())
    }
}

pub(super) fn logical_path(raw: &str, origin: &Path, overlay: &Path) -> Result<Option<String>> {
    // Preserve forbidden traversal for the mapping/Git validators instead of silently dropping it.
    if Path::new(raw)
        .components()
        .any(|part| part == std::path::Component::ParentDir)
    {
        return Ok(Some(raw.into()));
    }
    let path = super::mapping::absolute(&origin.join(raw))?;
    let overlay = super::mapping::absolute(overlay)?;
    Ok(path.strip_prefix(overlay).ok().map(|path| {
        if path.as_os_str().is_empty() {
            ".".into()
        } else {
            path.to_string_lossy().replace('\\', "/")
        }
    }))
}

/// Only typed install paths are rewritten; package options and scripts remain opaque.
pub(super) fn resolve_install_paths(value: &mut Value, origin: &Path) {
    match &mut value.kind {
        ValueKind::Table(table) => {
            for (key, value) in table {
                if key == "path" {
                    if let ValueKind::String(path) = &mut value.kind {
                        // Tilde is execution-root-relative; resolving it here loses that distinction
                        // before the caller supplies a custom root.
                        if path == "~" || path.starts_with("~/") {
                            continue;
                        }
                        *path = super::mapping::lexical(&origin.join(&*path))
                            .to_string_lossy()
                            .into_owned();
                    }
                } else {
                    resolve_install_paths(value, origin);
                }
            }
        }
        ValueKind::Array(values) => {
            for value in values {
                resolve_install_paths(value, origin);
            }
        }
        _ => {}
    }
}

pub(super) fn resolve_remote(raw: &mut String, origin: &Path) {
    // Git's transport URLs and scp-style addresses are identifiers, not local paths.
    if !raw.is_empty()
        && !raw.contains("://")
        && (!raw.contains(':') || Path::new(raw).is_absolute())
    {
        *raw = super::mapping::lexical(&origin.join(&*raw))
            .to_string_lossy()
            .into_owned();
    }
}

pub(super) fn resolve_git_paths(value: &mut Value, origin: &Path, overlay: &Path) -> Result<()> {
    let entries = match &value.kind {
        ValueKind::String(_) => Map::from([(".".into(), value.clone())]),
        ValueKind::Table(table) if table.contains_key("url") => {
            Map::from([(".".into(), value.clone())])
        }
        ValueKind::Table(table) => table.clone(),
        _ => return Ok(()),
    };
    let mut normalized = Map::new();
    for (key, mut config) in entries {
        let Some(logical) = logical_path(&key, origin, overlay)? else {
            continue;
        };
        match &mut config.kind {
            ValueKind::String(remote) => resolve_remote(remote, origin),
            ValueKind::Table(table) => {
                if let Some(Value {
                    kind: ValueKind::String(remote),
                    ..
                }) = table.get_mut("url")
                {
                    resolve_remote(remote, origin);
                }
                if let Some(Value {
                    kind: ValueKind::Table(remotes),
                    ..
                }) = table.get_mut("remotes")
                {
                    for remote in remotes.values_mut() {
                        match &mut remote.kind {
                            ValueKind::String(url) => resolve_remote(url, origin),
                            ValueKind::Table(table) => {
                                if let Some(Value {
                                    kind: ValueKind::String(url),
                                    ..
                                }) = table.get_mut("url")
                                {
                                    resolve_remote(url, origin);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        normalized.insert(logical, config);
    }
    value.kind = ValueKind::Table(normalized);
    Ok(())
}

pub(super) fn absolute_origin(path: &Path) -> Result<PathBuf> {
    super::mapping::absolute(path)
}
