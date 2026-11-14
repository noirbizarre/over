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
    env: &'a std::collections::BTreeMap<String, String>,
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
            env: &ctx.path_inputs.environment,
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
    // Template locals and source snippets may contain resolved private values.
    env.set_debug(false);
    // An unavailable input must never become an unrelated empty destination.
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env
}

/// Secret capabilities are opt-in so destination templates cannot expose values in paths.
pub(crate) fn register_secrets(
    env: &mut Environment<'_>,
    resolver: crate::secrets::SecretResolver,
) {
    // Debug snapshots can retain plaintext locals even when the final error is sanitized.
    env.set_debug(false);
    env.add_function(
        "secret",
        move |expression: String| -> Result<String, minijinja::Error> {
            let value = resolver.resolve(&expression).map_err(|error| {
                minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, error.to_string())
                    .with_source(error)
            })?;
            let text = std::str::from_utf8(value.expose()).map_err(|_| {
                minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "secret is not valid UTF-8",
                )
            })?;
            Ok(text.to_owned())
        },
    );
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

pub(crate) fn render_content(template: &str, ctx: &crate::exec::Context) -> Result<String> {
    let state = TemplateContext::new(ctx)?;
    create_env()
        .render_str(template, Serde(&state))
        .map_err(|error| {
            // Neither Display nor the error chain is safe: user expressions can construct messages.
            anyhow::anyhow!(
                "template rendering failed ({:?}, line {})",
                error.kind(),
                error.line().unwrap_or(0)
            )
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn content_rendering_uses_the_captured_environment_instead_of_rediscovering_it() {
        let mut ctx = crate::exec::Context::default();
        ctx.path_inputs
            .environment
            .insert("OVER_RENDER_FROZEN".into(), "snapshot".into());
        assert_eq!(
            render_content("{{ env.OVER_RENDER_FROZEN }}", &ctx).unwrap(),
            "snapshot"
        );
        assert_eq!(
            render_content(
                "{{ env.OVER_RENDER_FROZEN }}",
                &ctx.with_progress(indicatif::ProgressBar::hidden())
            )
            .unwrap(),
            "snapshot"
        );
    }

    #[test]
    fn ordinary_template_environments_cannot_resolve_secrets() {
        assert!(render_path("{{ secret('token') }}", &HashMap::<String, String>::new()).is_err());
    }

    #[test]
    fn secret_text_is_preserved_and_template_errors_do_not_capture_plaintext() {
        use crate::secrets::{
            ProviderError, SecretBytes, SecretProvider, SecretReference, SecretResolver,
        };
        use std::sync::Arc;

        struct Text;
        impl SecretProvider for Text {
            fn resolve(&self, _: &SecretReference) -> Result<SecretBytes, ProviderError> {
                Ok(SecretBytes::new(b"  secret-value-sentinel\n".to_vec()))
            }
        }
        let mut env = create_env();
        register_secrets(&mut env, SecretResolver::new(Arc::new(Text)));
        assert_eq!(
            env.render_str("{{ secret('token') }}", ()).unwrap(),
            "  secret-value-sentinel\n"
        );
        let error = env
            .render_str("{% set token = secret('token') %}{{ missing }}", ())
            .unwrap_err();
        assert!(!format!("{error:#?}").contains("secret-value-sentinel"));
    }

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
