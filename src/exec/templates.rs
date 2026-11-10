use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    over: PathBuf,
}

impl<'a> TemplateContext<'a> {
    pub(crate) fn new(ctx: &'a crate::exec::Context) -> Result<Self> {
        // Context-free library renderers may use a default repository (an empty path).
        let root = if ctx.repository.root.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &ctx.repository.root
        };
        Ok(Self {
            ctx,
            env: std::env::vars().collect(),
            overlays: ctx.resolved_overlays.as_ref(),
            paths: TemplatePaths {
                over: std::path::absolute(root)?,
            },
        })
    }
}

/// Create a shared MiniJinja `Environment` with all registered filters and functions.
///
/// This is the single extension point for custom template capabilities
/// such as encryption (2.3) and secret manager integration (2.4).
pub fn create_env<'a>() -> Environment<'a> {
    // Register custom filters/functions here when needed
    Environment::new()
}

/// Render a template string against the given serializable context.
pub fn render_string(template: &str, ctx: &impl Serialize) -> Result<String> {
    let env = create_env();
    // MiniJinja 3 no longer converts `Serialize` types implicitly; without the
    // `Serde` wrapper the context fails to compile as `Into<Value>`.
    Ok(env.render_str(template, Serde(ctx))?)
}

pub(crate) fn render_path(template: &str, state: &impl Serialize, strict: bool) -> Result<String> {
    let mut env = create_env();
    if strict {
        // Only lint opts in: ordinary execution keeps its existing template semantics.
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    }
    Ok(env.render_str(template, Serde(state))?)
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
