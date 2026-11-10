use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use minijinja::Environment;
use minijinja::value::Serde;
use serde::Serialize;

/// Shared namespaces without rediscovering overlays during path rendering or bootstrap.
#[derive(Serialize)]
pub(crate) struct TemplateContext<'a> {
    #[serde(flatten)]
    ctx: &'a crate::exec::Context,
    env: HashMap<String, String>,
    overlays: &'a HashMap<String, String>,
    paths: TemplatePaths,
}

#[derive(Serialize)]
struct TemplatePaths {
    // Null is defined in MiniJinja; omitted fields let availability guards work.
    #[serde(skip_serializing_if = "Option::is_none")]
    home: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    over: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<PathBuf>,
}

impl<'a> TemplateContext<'a> {
    pub(crate) fn new(ctx: &'a crate::exec::Context) -> Result<Self> {
        let paths = TemplatePaths {
            home: ctx
                .path_inputs
                .home
                .as_deref()
                .map(|path| ctx.absolute_path(path))
                .transpose()?,
            over: (!ctx.repository.root.as_os_str().is_empty())
                .then(|| ctx.absolute_path(&ctx.repository.root))
                .transpose()?,
            target: ctx.current_target.clone(),
        };
        // Lossy strings could name a different destination than the actual filesystem path.
        for path in [&paths.home, &paths.over, &paths.target]
            .into_iter()
            .flatten()
        {
            anyhow::ensure!(path.to_str().is_some(), "template path is not valid UTF-8");
        }
        Ok(Self {
            ctx,
            env: std::env::vars().collect(),
            overlays: ctx.resolved_overlays.as_ref(),
            paths,
        })
    }
}

/// Create a shared MiniJinja `Environment` with all registered filters and functions.
///
/// This is the single extension point for custom template capabilities
/// such as encryption (2.3) and secret manager integration (2.4).
pub fn create_env<'a>() -> Environment<'a> {
    // Register custom filters/functions here when needed
    let mut env = Environment::new();
    // An unavailable input must never become an unrelated empty destination.
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env
}

/// Render a template string against the given serializable context.
pub fn render_string(template: &str, ctx: &impl Serialize) -> Result<String> {
    let env = create_env();
    // MiniJinja 3 no longer converts `Serialize` types implicitly; without the
    // `Serde` wrapper the context fails to compile as `Into<Value>`.
    Ok(env.render_str(template, Serde(ctx))?)
}

pub(crate) fn render_path(template: &str, state: &impl Serialize) -> Result<String> {
    render_string(template, state)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn render_string_basic() {
        let mut ctx = HashMap::new();
        ctx.insert("name", "world");
        let result = render_string("Hello, {{ name }}!", &ctx).unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn render_string_no_variables() {
        let ctx = HashMap::<String, String>::new();
        let result = render_string("plain text", &ctx).unwrap();
        assert_eq!(result, "plain text");
    }

    #[test]
    fn render_string_invalid_template() {
        let ctx = HashMap::<String, String>::new();
        let result = render_string("{{ invalid", &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn render_string_nested_context() {
        let mut inner = HashMap::new();
        inner.insert("key", "value");
        let mut ctx = HashMap::new();
        ctx.insert("nested", inner);
        let result = render_string("{{ nested.key }}", &ctx).unwrap();
        assert_eq!(result, "value");
    }

    #[test]
    fn create_env_returns_functional_environment() {
        let env = create_env();
        let mut ctx = HashMap::new();
        ctx.insert("x", "42");
        let result = env.render_str("val={{ x }}", Serde(&ctx)).unwrap();
        assert_eq!(result, "val=42");
    }
}
