//! Explicit lifecycle commands, dispatched only by mutating operations.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use tokio::process::Command;

/// Preserve the shorthand on serialization without exposing executable values in Debug.
#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum Hook {
    Command(String),
    Structured(HookDefinition),
}

#[derive(Clone, Serialize)]
pub struct HookDefinition {
    pub command: String,
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

impl fmt::Debug for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Hook { .. }")
    }
}

// These names describe authoritative lifecycle state, not user-provided environment.
const RESERVED_ENV: &[&str] = &[
    "OVER_HOOK_ACTIVE",
    "OVER_HOOK_EVENT",
    "OVER_REPOSITORY_ROOT",
    "OVER_TARGET_ROOT",
    "OVER_OVERLAY_NAME",
    "OVER_OVERLAY_ROOT",
    "OVER_OVERLAY_TARGET",
];

fn valid_command(command: &str) -> Result<()> {
    if command.trim().is_empty() || command.contains('\0') {
        bail!("hook command must be nonempty and contain no NUL bytes");
    }
    Ok(())
}

impl Hook {
    fn from_value(value: serde_json::Value) -> Result<Self> {
        if let serde_json::Value::String(command) = value {
            valid_command(&command)?;
            return Ok(Self::Command(command));
        }
        let serde_json::Value::Object(mut fields) = value else {
            bail!("hook entry must be a string or an object");
        };
        let Some(serde_json::Value::String(command)) = fields.remove("command") else {
            bail!("hook object requires a command string");
        };
        valid_command(&command)?;
        let mut optional_string = |key| -> Result<Option<String>> {
            match fields.remove(key) {
                None => Ok(None),
                Some(serde_json::Value::String(value)) if !value.contains('\0') => Ok(Some(value)),
                _ => bail!("hook {key} must be a string without NUL bytes"),
            }
        };
        let cwd = optional_string("cwd")?;
        let when = optional_string("when")?;
        let mut env = BTreeMap::new();
        if let Some(value) = fields.remove("env") {
            let serde_json::Value::Object(values) = value else {
                bail!("hook env must be a mapping of names to strings");
            };
            for (key, value) in values {
                if key.is_empty()
                    || key.contains(['=', '\0'])
                    // Windows environment names are case-insensitive, including recursion guards.
                    || RESERVED_ENV.iter().any(|reserved| key.eq_ignore_ascii_case(reserved))
                {
                    bail!("hook env name is invalid or reserved for lifecycle context");
                }
                let serde_json::Value::String(value) = value else {
                    bail!("hook env values must be strings");
                };
                if value.contains('\0') {
                    bail!("hook env values must not contain NUL bytes");
                }
                env.insert(key, value);
            }
        }
        if !fields.is_empty() {
            bail!("unknown hook field; expected command, env, cwd or when");
        }
        Ok(Self::Structured(HookDefinition {
            command,
            env,
            cwd,
            when,
        }))
    }
}

/// Commands are deliberately hidden from Debug: overlays appear in debug traces.
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Hooks {
    pub install: Vec<Hook>,
    pub post_clone: Vec<Hook>,
    pub post_init: Vec<Hook>,
    pub post_apply: Vec<Hook>,
}

impl fmt::Debug for Hooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hooks")
            .field("install", &self.install.len())
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
                "install" => &mut hooks.install,
                "post-clone" => &mut hooks.post_clone,
                "post-init" => &mut hooks.post_init,
                "post-apply" => &mut hooks.post_apply,
                // Even an unknown key may contain a secret, so do not quote it.
                _ => bail!(
                    "unknown hook event; expected install, post-clone, post-init or post-apply"
                ),
            };
            let serde_json::Value::Array(commands) = commands else {
                bail!("hook commands must be a list of strings or objects");
            };
            for (index, command) in commands.into_iter().enumerate() {
                // Serde only displays the outer error, so retain our value-free validation reason.
                destination.push(Hook::from_value(command).map_err(|error| {
                    anyhow::anyhow!("invalid {event} hook #{}: {error}", index + 1)
                })?);
            }
        }
        Ok(hooks)
    }

    pub fn is_empty(&self) -> bool {
        self.install.is_empty()
            && self.post_clone.is_empty()
            && self.post_init.is_empty()
            && self.post_apply.is_empty()
    }

    fn commands(&self, event: Event) -> &[Hook] {
        match event {
            Event::Install => &self.install,
            Event::PostClone => &self.post_clone,
            Event::PostInit => &self.post_init,
            Event::PostApply => &self.post_apply,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Event {
    Install,
    PostClone,
    PostInit,
    PostApply,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Install => "install",
            Self::PostClone => "post-clone",
            Self::PostInit => "post-init",
            Self::PostApply => "post-apply",
        })
    }
}

/// Child context is explicit so init never needs to discover an overlay.
pub struct HookContext<'a> {
    pub templates: crate::exec::Ctx,
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

// Rendered fields may contain secrets and must never acquire a value-bearing Debug.
struct PreparedHook {
    script: String,
    cwd: PathBuf,
    overrides: BTreeMap<String, String>,
}

fn template_failure_reason(error: &minijinja::Error) -> String {
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        // Only our closed, value-free taxonomy is safe to preserve from this chain.
        if let Some(reason) = cause.downcast_ref::<crate::secrets::ResolutionError>() {
            return reason.to_string();
        }
        source = cause.source();
    }
    format!(
        "template failed ({:?}); check syntax, variables, functions and secret UTF-8 encoding",
        error.kind()
    )
}

fn hook_failure(
    event: Event,
    index: usize,
    scope: &str,
    field: &str,
    reason: &str,
) -> anyhow::Error {
    let recovery = match event {
        Event::PostClone => {
            "recover this hook manually in the preserved repository, then rerun init (post-init only)"
        }
        _ => "fix the hook and retry the operation",
    };
    anyhow::anyhow!(
        "{event} hook #{} in {scope}: {field}: {reason}; completed filesystem changes are preserved; {recovery}",
        index + 1
    )
}

fn prepare(
    hook: &Hook,
    event: Event,
    index: usize,
    context: &HookContext<'_>,
) -> Result<Option<PreparedHook>> {
    // Subprocess locations and template paths share the same invocation anchor.
    let absolute = |path: &Path| context.templates.absolute_path(path);
    // Drop underlying errors: the UI prints entire chains, including template source.
    let failure =
        |field: &str, reason: &str| hook_failure(event, index, context.scope, field, reason);
    let state = crate::exec::templates::TemplateContext::new(&context.templates)
        .map_err(|_| failure("context", "could not resolve shared filesystem paths"))?;
    let mut environment = crate::exec::templates::create_env();
    use crate::secrets::{NativeProvider, SecretResolver};
    // Construction must not inspect the store or unlock a skipped hook's secrets.
    let repository = if context.templates.repository.root.as_os_str().is_empty() {
        PathBuf::new()
    } else {
        absolute(&context.templates.repository.root)
            .map_err(|_| failure("context", "could not resolve secret repository"))?
    };
    let resolver = SecretResolver::new(std::sync::Arc::new(NativeProvider::new(
        repository,
        context.templates.crypto.clone(),
        !context.templates.no_prompt,
    )));
    crate::exec::templates::register_secrets(&mut environment, resolver);
    let definition = match hook {
        Hook::Command(_) => None,
        Hook::Structured(definition) => Some(definition),
    };
    if let Some(expression) = definition.and_then(|value| value.when.as_deref()) {
        let value = environment
            .compile_expression(expression)
            .and_then(|expression| expression.eval(minijinja::value::Serde(&state)))
            .map_err(|error| failure("when", &template_failure_reason(&error)))?;
        let run = match value.kind() {
            minijinja::value::ValueKind::Bool => value.is_true(),
            minijinja::value::ValueKind::None => false,
            minijinja::value::ValueKind::String => !value.as_str().unwrap_or_default().is_empty(),
            _ => return Err(failure("when", "expected a boolean, none or string result")),
        };
        if !run {
            return Ok(None);
        }
    }
    let render = |source: &str, field: &str| -> Result<String> {
        let value = environment
            .render_str(source, minijinja::value::Serde(&state))
            .map_err(|error| failure(field, &template_failure_reason(&error)))?;
        if value.contains('\0') {
            return Err(failure(field, "rendered value contains a NUL byte"));
        }
        Ok(value)
    };
    let source = match hook {
        Hook::Command(command) => command,
        Hook::Structured(definition) => &definition.command,
    };
    let script = render(source, "command")?;
    valid_command(&script).map_err(|_| failure("command", "rendered command must be nonempty"))?;
    let mut cwd = absolute(context.cwd)?;
    let mut overrides = BTreeMap::new();
    if let Some(definition) = definition {
        for (key, value) in &definition.env {
            overrides.insert(key.clone(), render(value, "env")?);
        }
        if let Some(source) = &definition.cwd {
            let rendered = render(source, "cwd")?;
            if rendered.is_empty() {
                return Err(failure("cwd", "rendered directory must be nonempty"));
            }
            cwd = absolute(context.repository)?.join(rendered);
            let metadata = std::fs::metadata(&cwd).map_err(|error| {
                failure(
                    "cwd",
                    &format!("cannot inspect directory ({:?})", error.kind()),
                )
            })?;
            if !metadata.is_dir() {
                return Err(failure("cwd", "resolved path is not a directory"));
            }
        }
    }
    Ok(Some(PreparedHook {
        script,
        cwd,
        overrides,
    }))
}

pub async fn run(hooks: &Hooks, event: Event, context: HookContext<'_>) -> Result<()> {
    if nested() || hooks.commands(event).is_empty() {
        return Ok(());
    }
    let absolute = |path: &Path| context.templates.absolute_path(path);
    for (index, hook) in hooks.commands(event).iter().enumerate() {
        let hook = hook.clone();
        let templates = context.templates.clone();
        let repository = context.repository.to_owned();
        let cwd = context.cwd.to_owned();
        let scope = context.scope.to_owned();
        // One task per hook preserves visibility of changes made by earlier children.
        let prepared = tokio::task::spawn_blocking(move || {
            prepare(
                &hook,
                event,
                index,
                &HookContext {
                    templates,
                    repository: &repository,
                    cwd: &cwd,
                    scope: &scope,
                    overlay: None,
                    target_root: None,
                },
            )
        })
        .await
        .map_err(|_| hook_failure(event, index, context.scope, "preparation", "task failed"))??;
        let Some(PreparedHook {
            script,
            cwd,
            overrides,
        }) = prepared
        else {
            continue;
        };
        let failure =
            |field: &str, reason: &str| hook_failure(event, index, context.scope, field, reason);
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .current_dir(cwd)
            .envs(overrides)
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
        let status = command.status().await.map_err(|error|
            failure("process", &format!("could not start hook ({:?}); ensure sh is available on PATH and cwd can be entered", error.kind()))
        )?;
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

    #[tokio::test(flavor = "current_thread")]
    async fn secret_conditions_leave_the_runtime_responsive_and_share_one_unlock() {
        use crate::secrets::{CryptoService, SecretBytes, SecretName, SecretStore};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };

        let directory = tempfile::TempDir::new().unwrap();
        let writer = Arc::new(CryptoService::for_test(|_| Ok(Some("passphrase".into()))));
        let store = SecretStore::new(directory.path(), writer).unwrap();
        for name in ["first", "second"] {
            store
                .add(
                    &SecretName::parse(name).unwrap(),
                    &SecretBytes::new(Vec::new()),
                    false,
                    false,
                )
                .unwrap();
        }
        let (started, observed) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started));
        let (release, wait) = std::sync::mpsc::channel();
        let wait = Mutex::new(wait);
        let count = Arc::new(AtomicUsize::new(0));
        let acquired = count.clone();
        let crypto = Arc::new(CryptoService::for_test(move |_| {
            acquired.fetch_add(1, Ordering::SeqCst);
            started.lock().unwrap().take().unwrap().send(()).unwrap();
            // A blocked executor cannot release this credential acquisition in time.
            wait.lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))?;
            Ok(Some("passphrase".into()))
        }));
        let observer = tokio::spawn(async move {
            observed.await.unwrap();
            release.send(()).unwrap();
        });
        let mut templates = crate::exec::Context::builder()
            .repository(crate::overlays::Repository::new(
                directory.path().to_owned(),
            ))
            .no_prompt(true)
            .build();
        Arc::get_mut(&mut templates).unwrap().crypto = crypto;
        let hooks = serde_json::from_value(json!({"post-apply": [{
            "command": "must never execute", "when": "secret('first') == '' and secret('second') == '' and false"
        }]})).unwrap();
        run(
            &hooks,
            Event::PostApply,
            HookContext {
                templates,
                repository: directory.path(),
                cwd: directory.path(),
                scope: "test",
                overlay: None,
                target_root: None,
            },
        )
        .await
        .unwrap();
        observer.await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[rstest]
    #[case(json!(null))]
    #[case(json!({"post-apply": "secret-sentinel"}))]
    #[case(json!({"post-apply": [42]}))]
    #[case(json!({"post-apply": [{"secret-sentinel": true}]}))]
    #[case(json!({"post-apply": [""]}))]
    #[case(json!({"post-apply": ["\0secret-sentinel"]}))]
    #[case(json!({"secret-sentinel": []}))]
    #[case(json!({"post-apply": [{"command": "ok", "env": {"OVER_HOOK_ACTIVE": "secret-sentinel"}}]}))]
    #[case(json!({"post-apply": [{"command": "ok", "env": {"secret-sentinel=": "value"}}]}))]
    #[case(json!({"post-apply": [{"command": "ok", "env": {"TOKEN": 42}}]}))]
    #[case(json!({"post-apply": [{"command": "ok", "cwd": 42}]}))]
    #[case(json!({"post-apply": [{"command": "ok", "when": true}]}))]
    #[case(json!({"post-apply": [{"command": "ok", "secret-sentinel": "value"}]}))]
    fn invalid_hooks_fail_without_disclosing_values(#[case] value: serde_json::Value) {
        let error = serde_json::from_value::<Hooks>(value).unwrap_err();
        assert!(!error.to_string().contains("secret-sentinel"));
    }

    #[test]
    fn install_only_hooks_are_nonempty_and_hide_commands_in_debug_output() {
        let hooks: Hooks = serde_json::from_value(json!({"install": ["install-secret"]})).unwrap();
        assert!(!hooks.is_empty());
        assert_eq!(
            serde_json::to_value(hooks.commands(Event::Install)).unwrap(),
            json!(["install-secret"])
        );
        assert_eq!(Event::Install.to_string(), "install");
        assert!(!format!("{hooks:?}").contains("install-secret"));
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
            serde_json::to_value(hooks.commands(Event::PostClone)).unwrap(),
            json!(["first-secret", "second-secret"])
        );
        assert_eq!(
            serde_json::to_value(hooks.commands(Event::PostInit)).unwrap(),
            json!(["init-secret"])
        );
        assert_eq!(
            serde_json::to_value(hooks.commands(Event::PostApply)).unwrap(),
            json!(["apply-secret"])
        );
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
        assert_eq!(
            serde_json::to_value(root_hooks.post_clone).unwrap(),
            json!(["clone"])
        );
        assert_eq!(
            serde_json::to_value(root_hooks.post_init).unwrap(),
            json!(["init"])
        );
        assert_eq!(
            serde_json::to_value(root_hooks.post_apply).unwrap(),
            json!(["apply"])
        );
        assert!(repository.get("").unwrap().hooks.is_empty());
        assert!(repository.get("child").unwrap().hooks.is_empty());
        std::fs::write(child.join(format!("over.{extension}")), content).unwrap();
        assert_eq!(
            serde_json::to_value(repository.get("child").unwrap().hooks.post_apply).unwrap(),
            json!(["apply"])
        );
    }

    #[rstest]
    #[case(
        "toml",
        "[hooks]\npost-apply = ['plain', {command = 'secret-sentinel', env = {TOKEN = 'secret-sentinel'}, cwd = '.', when = 'true'}]\n"
    )]
    #[case(
        "yaml",
        "hooks:\n  post-apply:\n    - plain\n    - command: secret-sentinel\n      env: {TOKEN: secret-sentinel}\n      cwd: .\n      when: 'true'\n"
    )]
    #[case("yml", "hooks:\n  post-apply: [plain, {command: secret-sentinel}]\n")]
    fn mixed_hook_forms_parse_across_descriptor_formats_and_round_trip_safely(
        #[case] extension: &str,
        #[case] content: &str,
    ) {
        let directory = tempfile::TempDir::new().unwrap();
        std::fs::write(directory.path().join(format!("over.{extension}")), content).unwrap();
        let repository = crate::overlays::Repository::new(directory.path().to_owned());
        let hooks = repository.hooks().unwrap();
        assert_eq!(hooks.post_apply.len(), 2);
        assert!(!format!("{hooks:?} {:?}", hooks.post_apply).contains("secret-sentinel"));
        let serialized = serde_json::to_value(&hooks).unwrap();
        let restored: Hooks = serde_json::from_value(serialized.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), serialized);
    }
}
