# ADR-010: Templating is scoped to path-resolution strings, not file content

**Status:** accepted

## Age amendment (#62)

Age-encrypted ordinary files now provide a whole-file transformation and real-file
materializer. Their plaintext remains short-lived and is not rendered as a
template. Named secret resolution is available through the reusable secret store;
it is not exposed to path templates. Future content rendering must run after
decryption. See [Secrets and Age-encrypted files](../usage/secrets.md).

## Context

Some dotfile tools (chezmoi, for instance) template file *content* at apply
time — injecting a machine-specific value into a tracked config file. `over`
already uses minijinja for two path-shaped strings: an overlay's `target`
field (`Overlay::resolve_target`) and a `.link.toml` sidecar's `target`
field (`render_symlink_target`), rendered against `env`/`overlays` maps
(later joined by a `machine` map — see
[ADR-019](019-default-overlay-selection-extends-templating-to-overlay-choice.md)).
Extending the same engine to file bodies was a natural next step, and
`src/actions/templates.rs` exists as a named, currently-empty module —
scaffolding for it.

## Decision

For now, templating stays scoped to those two path fields.

**Extension (#212):** lifecycle hook commands, environment values, working
directories, and conditions also use the shared MiniJinja environment at their
configured lifecycle point. This extends execution configuration, not file
content rendering. See [Lifecycle hooks](../configuration/root.md#lifecycle-hooks)
for context availability and failure semantics.

**Secret resolution extension (#63):** hook environments explicitly register
`secret()` using the shared context's source repository and credential service.
Conditions and executable fields use the same capability, with execution-only
lookups deferred until the condition succeeds. Destination/default-selection
environments remain secret-free. `exec::templates::create_env()` supplies strict
undefined handling; hook-only capability registration does not enable file
content rendering. `src::actions::templates` remains an empty stub.

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
