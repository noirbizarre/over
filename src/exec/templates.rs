use anyhow::Result;
use minijinja::Environment;
use serde::Serialize;

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
    Ok(env.render_str(template, ctx)?)
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
        let result = env.render_str("val={{ x }}", &ctx).unwrap();
        assert_eq!(result, "val=42");
    }
}
