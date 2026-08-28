# ADR-010: Templating is scoped to path-resolution strings, not file content

**Status:** accepted

## Context

Some dotfile tools (chezmoi, for instance) template file *content* at apply
time — injecting a machine-specific value into a tracked config file. `over`
already uses minijinja for two path-shaped strings: an overlay's `target`
field (`Overlay::resolve_target`) and a `.link.toml` sidecar's `target`
field (`render_symlink_target`), rendered against `env`/`overlays` maps.
Extending the same engine to file bodies was a natural next step, and
`src/actions/templates.rs` exists as a named, currently-empty module —
scaffolding for it.

## Decision

For now, templating stays scoped to those two path fields.
`exec::templates::create_env()` documents itself as "the single extension
point for custom template capabilities such as encryption and secret
manager integration" and deliberately registers nothing beyond a bare
`Environment` — no custom filters, no loaders, no includes. `src::actions::
templates` remains an empty stub rather than a half-built content-templating
path.

## Consequences

The blast radius of a templating bug is small: at worst a path resolves
wrong, never a tracked file's *content* silently rendering incorrectly. The
cost is a real feature gap — there is no way today to keep one tracked file
that varies per machine; users must either use per-platform overlay
sections or maintain separate files. The empty `actions/templates.rs` stub
is also dead code that can mislead a contributor into thinking
content-templating already has a home; implementing it later will need to
answer a new question the current design ducks — a templated file can no
longer just be symlinked, it must be rendered and written, which changes
what the `.link` exclusion globs need to skip.
