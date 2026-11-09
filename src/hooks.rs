//! Explicit lifecycle commands, dispatched only by mutating operations.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use tokio::process::Command;

/// Commands are deliberately hidden from Debug: overlays appear in debug traces.
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Hooks {
    pub post_clone: Vec<String>,
    pub post_init: Vec<String>,
    pub post_apply: Vec<String>,
}

impl fmt::Debug for Hooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hooks")
            .field("post_clone", &self.post_clone.len())
            .field("post_init", &self.post_init.len())
            .field("post_apply", &self.post_apply.len())
            .finish()
    }
}

impl<'de> Deserialize<'de> for Hooks {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Value-based validation avoids serde errors quoting secret-bearing values.
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_value(value).map_err(serde::de::Error::custom)
    }
}

impl Hooks {
    fn from_value(value: serde_json::Value) -> Result<Self> {
        let serde_json::Value::Object(events) = value else {
            bail!("hooks must be a mapping of lifecycle names to command lists");
        };
        let mut hooks = Self::default();
        for (event, commands) in events {
            let destination = match event.as_str() {
                "post-clone" => &mut hooks.post_clone,
                "post-init" => &mut hooks.post_init,
                "post-apply" => &mut hooks.post_apply,
                // Even an unknown key may contain a secret, so do not quote it.
                _ => bail!("unknown hook event; expected post-clone, post-init or post-apply"),
            };
            let serde_json::Value::Array(commands) = commands else {
                bail!("hook commands must be a list of strings");
            };
            for command in commands {
                let serde_json::Value::String(command) = command else {
                    bail!("hook commands must be strings");
                };
                if command.trim().is_empty() || command.contains('\0') {
                    bail!("hook commands must be nonempty strings without NUL bytes");
                }
                destination.push(command);
            }
        }
        Ok(hooks)
    }

    pub fn is_empty(&self) -> bool {
        self.post_clone.is_empty() && self.post_init.is_empty() && self.post_apply.is_empty()
    }

    fn commands(&self, event: Event) -> &[String] {
        match event {
            Event::PostClone => &self.post_clone,
            Event::PostInit => &self.post_init,
            Event::PostApply => &self.post_apply,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Event {
    PostClone,
    PostInit,
    PostApply,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PostClone => "post-clone",
            Self::PostInit => "post-init",
            Self::PostApply => "post-apply",
        })
    }
}

/// Child context is explicit so init never needs to discover an overlay.
pub struct HookContext<'a> {
    pub repository: &'a Path,
    pub cwd: &'a Path,
    pub scope: &'a str,
    pub overlay: Option<(&'a str, &'a Path, &'a Path)>,
    pub target_root: Option<&'a Path>,
}

#[derive(Debug)]
pub struct HookFailure {
    event: Event,
    scope: String,
    index: usize,
    pub status: ExitStatus,
}

impl fmt::Display for HookFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} hook #{} in {} failed ({}); completed filesystem changes are preserved; ",
            self.event, self.index, self.scope, self.status
        )?;
        match self.event {
            // A retry sees an existing checkout and cannot dispatch post-clone again.
            Event::PostClone => f.write_str(
                "recover this hook manually in the preserved repository, then rerun init (post-init only)",
            ),
            _ => f.write_str("fix the hook and retry the operation"),
        }
    }
}

impl std::error::Error for HookFailure {}

pub fn nested() -> bool {
    std::env::var_os("OVER_HOOK_ACTIVE").is_some()
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).context("failed to resolve hook context path")
}

pub async fn run(hooks: &Hooks, event: Event, context: HookContext<'_>) -> Result<()> {
    if nested() || hooks.commands(event).is_empty() {
        return Ok(());
    }
    for (index, script) in hooks.commands(event).iter().enumerate() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .current_dir(absolute(context.cwd)?)
            .env("OVER_HOOK_ACTIVE", "1")
            .env("OVER_HOOK_EVENT", event.to_string())
            .env("OVER_REPOSITORY_ROOT", absolute(context.repository)?);
        // Remove inherited context before supplying this lifecycle's own values.
        for key in [
            "OVER_TARGET_ROOT",
            "OVER_OVERLAY_NAME",
            "OVER_OVERLAY_ROOT",
            "OVER_OVERLAY_TARGET",
        ] {
            command.env_remove(key);
        }
        if let Some(root) = context.target_root {
            command.env("OVER_TARGET_ROOT", absolute(root)?);
        }
        if let Some((name, root, target)) = context.overlay {
            command
                .env("OVER_OVERLAY_NAME", name)
                .env("OVER_OVERLAY_ROOT", absolute(root)?)
                .env("OVER_OVERLAY_TARGET", absolute(target)?);
        }
        // Inherit stdio for interactive tools; never reproduce their output in errors.
        let status = command.status().await.with_context(|| {
            format!(
                "could not start {event} hook #{} in {}; ensure sh is available on PATH",
                index + 1,
                context.scope
            )
        })?;
        if !status.success() {
            return Err(HookFailure {
                event,
                scope: context.scope.to_owned(),
                index: index + 1,
                status,
            }
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use serde_json::json;

    #[rstest]
    #[case(json!(null))]
    #[case(json!({"post-apply": "secret-sentinel"}))]
    #[case(json!({"post-apply": [42]}))]
    #[case(json!({"post-apply": [{"secret-sentinel": true}]}))]
    #[case(json!({"post-apply": [""]}))]
    #[case(json!({"post-apply": ["\0secret-sentinel"]}))]
    #[case(json!({"secret-sentinel": []}))]
    fn invalid_hooks_fail_without_disclosing_values(#[case] value: serde_json::Value) {
        let error = serde_json::from_value::<Hooks>(value).unwrap_err();
        assert!(!error.to_string().contains("secret-sentinel"));
    }

    #[test]
    fn command_order_is_preserved_but_debug_output_never_contains_commands() {
        let hooks: Hooks = serde_json::from_value(json!({
            "post-clone": ["first-secret", "second-secret"],
            "post-init": ["init-secret"],
            "post-apply": ["apply-secret"]
        }))
        .unwrap();
        assert_eq!(
            hooks.commands(Event::PostClone),
            ["first-secret", "second-secret"]
        );
        assert_eq!(hooks.commands(Event::PostInit), ["init-secret"]);
        assert_eq!(hooks.commands(Event::PostApply), ["apply-secret"]);
        assert!(!format!("{hooks:?}").contains("secret"));
    }

    #[rstest]
    #[case(
        "toml",
        "[hooks]\npost-clone = ['clone']\npost-init = ['init']\npost-apply = ['apply']\n"
    )]
    #[case(
        "yaml",
        "hooks:\n  post-clone: [clone]\n  post-init: [init]\n  post-apply: [apply]\n"
    )]
    #[case(
        "yml",
        "hooks:\n  post-clone: [clone]\n  post-init: [init]\n  post-apply: [apply]\n"
    )]
    fn descriptors_keep_root_hooks_separate_from_overlay_hooks(
        #[case] extension: &str,
        #[case] content: &str,
    ) {
        let directory = tempfile::TempDir::new().unwrap();
        std::fs::write(directory.path().join(format!("over.{extension}")), content).unwrap();
        let child = directory.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let repository = crate::overlays::Repository::new(directory.path().to_owned());
        let root_hooks = repository.hooks().unwrap();
        assert_eq!(root_hooks.post_clone, ["clone"]);
        assert_eq!(root_hooks.post_init, ["init"]);
        assert_eq!(root_hooks.post_apply, ["apply"]);
        assert!(repository.get("").unwrap().hooks.is_empty());
        assert!(repository.get("child").unwrap().hooks.is_empty());
        std::fs::write(child.join(format!("over.{extension}")), content).unwrap();
        assert_eq!(repository.get("child").unwrap().hooks.post_apply, ["apply"]);
    }
}
