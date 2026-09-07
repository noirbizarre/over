# ADR-019: `default_overlay` extends templating to overlay selection, amending ADR-010

**Status:** accepted

## Context

#113 asks for a repository root that can designate one overlay as the
default target for commands accepting an optional `NAME` argument, e.g.:

```toml
default_overlay = "hosts/{{ machine.hostname }}"
```

Two existing decisions shape how this can be implemented:

- ADR-010 says templating "stays scoped to those two path fields" — an
  overlay's `target` and a `.link.toml` sidecar's `target`
  (`exec::templates::render_string` against `exec::Context`). A
  `default_overlay` value isn't a path at all; it's an overlay *name*
  used to look one up via `Repository::get`.
- `apply`/`unapply` (`src/cli/apply.rs`, `src/cli/unapply.rs`) already
  have a `NAME` omitted → interactive `FuzzySelect` prompt fallback.
  `status`/`diff`/`sync` (`src/cli/status.rs`, `diff.rs`, `sync.rs`)
  instead treat an omitted `NAME` as "every overlay" — a different
  contract from apply/unapply's, and one that a configured
  `default_overlay` necessarily changes: reporting/syncing "just the
  default" is the whole point of configuring one, but this narrows what
  used to be a fan-out over every overlay.

## Decision

`default_overlay: Option<String>` is added to `RootConfig`
(`src/overlays/repository.rs`), read through the existing one-shot,
non-cascading `root_config()` helper — the same mechanism `format` and
`overlays` already use (ADR-018). It is root-scoped only, never cascaded
per-overlay, since "which overlay is the default" has no meaning at any
other level of the descriptor chain.

`Repository::default_overlay(&self, ctx: &exec::Context) -> Result<Option<Overlay>>`
renders the raw string via `exec::templates::render_string` against the
same `exec::Context` `Overlay::resolve_target` already uses — extending
ADR-010's templating scope to this third field. Unlike `target`, the
rendered string isn't used as a path: it's passed straight to
`Repository::get`, so validation (does this overlay exist?) is folded
into the same call. Absence is silent (`Ok(None)`, matching
`preferred_format()`'s contract); a configured-but-broken value (bad
template syntax, or a name that doesn't resolve to a real overlay) is a
hard `Err`, never a silent fallback to another selection method — this
mirrors how a bad `uses` entry already fails loudly rather than being
skipped.

CLI wiring keeps explicit CLI selection strictly first, per #113's ask:

- `apply`/`unapply`: a new shared `cli::common::select_overlay` helper
  (deduplicating the previously copy-pasted `Some(name) => repo.get /
  None => FuzzySelect` block) tries `Repository::default_overlay` between
  the explicit-name check and the interactive prompt.
- `status`/`diff`/`sync`: an omitted `NAME` now resolves to
  `Repository::default_overlay`'s result when one is configured,
  otherwise falls back unchanged to today's "every overlay"
  (`Repository::overlays()`). A new `-a`/`--all` flag (conflicting with
  the positional `NAME`) is the explicit opt-out, always producing
  "every overlay" regardless of a configured default.

In all five commands, an explicit `NAME` short-circuits before
`default_overlay` is even resolved — a misconfigured default never
breaks an invocation that didn't need it.

## Consequences

Templating's blast radius (ADR-010's stated benefit: "at worst a path
resolves wrong") now also covers overlay *selection*, not only path
resolution — a broken `default_overlay` template can send commands to
the wrong overlay or fail outright, not just place files somewhere
unexpected. The scope is still narrow (one field, resolved once per
invocation, always validated against `Repository::get` before use).

`status`/`diff`/`sync` gain a real, user-visible behavior change: once
`default_overlay` is configured, a bare `over status` (etc.) reports on
one overlay instead of every overlay it did before. This is opt-in
(nothing changes until a user adds `default_overlay` to their config) but
is still a breaking change in effect for anyone who configures it
expecting today's fan-out to keep working — `--all` is the escape hatch,
and this is called out in `docs/configuration.md` and each command's
usage doc.

`over add` is deliberately not wired to `default_overlay` — its
`NAME`-omitted prompt selects a *target to add files into*, a related but
distinct concept from choosing which existing overlay a command acts on,
and #128 doesn't ask for it.
