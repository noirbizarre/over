# ADR-018: Root-declared `overlays:` paths extend descriptor-glob discovery, amending ADR-002

**Status:** accepted

## Context

`Repository::overlays()` (`src/overlays/repository.rs`) discovers overlays
purely by globbing for `over.{yml,yaml,toml}` (`GLOB_PATTERN`) — a directory
with no local descriptor is invisible, even though `Overlay::new`'s
cascading config merge (ADR-002) already tolerates a missing descriptor at
every *ancestor* level except the overlay's own directory
(`.required(dir == root)`).

#113 asks for a repository root that can declare overlays/workspaces
directly, e.g. `overlays: [{ path: "hosts/*" }]`, so a directory doesn't
need its own descriptor purely to exist as an overlay.

## Decision

Add `OverlayDeclaration { path: String }` (`src/overlays/discovery.rs`),
read once from the repository root's own descriptor via a new
`RootConfig { format, overlays }` struct (`src/overlays/repository.rs`) —
the same one-shot, non-cascading read `preferred_format()` already used
for `format`, now shared between both fields. `path` is a glob pattern or
literal path, relative to the repository root.

`resolve_declared_dirs` expands declarations into concrete directories
using the `glob` crate, not `globset`: `globset` only tests an
already-known path against a pattern (`GLOB_PATTERN`, `exclude`, `rules`
all work this way — enumerate the tree with `WalkDir`, then filter), while
a root declaration describes what to enumerate directly. This mirrors
`cli::common::resolve_inputs`, the existing precedent for expanding a
user-supplied glob against the real filesystem. A malformed glob, or one
matching nothing, is silently skipped — consistent with how
`MaterializationRule::matches` treats a bad `rules` glob: `over lint` is
where diagnostics belong, not a hard error at discovery time. This issue
does not add such a lint check; a future one can (see Consequences).

`Repository::overlays()` unions descriptor-glob discovery with
`declared_overlay_dirs()`, then dedups by resolved path before the
existing "more specific overlay wins" parent-skip pass, which is otherwise
unchanged. `lint::discover_overlay_dirs` (`src/lint/mod.rs`) does the same
union so `over lint` isn't blind to root-declared overlays.

`Overlay::new`'s `.required(dir == root)` becomes unconditional
`.required(false)`: an overlay directory can now have zero descriptor
anywhere in its own file, resolving entirely from ancestor config
(including the repository root) plus hardcoded defaults (`target`'s
`set_default`).

Two scope decisions, made explicitly narrow for this issue:

- **No nested declarations.** Only the repository root's own descriptor's
  `overlays:` key is read. A directory matched by a root declaration
  cannot itself declare further `overlays:` entries — avoids
  recursion/cycle-safety concerns not requested by #113's core scenarios.
- **Naming stays path-derived**, unchanged from today (ADR-002: identity
  is the filesystem path, not a git ref) — a root-declared overlay's name
  is still computed the same way in `Overlay::new`.

## Consequences

`Repository::get()`/`Overlay::new` now succeed for *any* existing
directory under the repository root, even one with zero descriptor
anywhere in its ancestor chain and not declared via `overlays:` at all —
this was a deliberate, explicit part of #113/#127's scope, not limited to
declared directories. Anyone relying on a missing descriptor as an
implicit "this is not an overlay" guard (there was no such guard enforced
elsewhere) will see previously-erroring paths now resolve successfully.

`Repository::overlays()` and `lint::discover_overlay_dirs` still duplicate
the descriptor-glob walk (pre-existing, not introduced here) — now each
also duplicates the declared-dirs union call. Unifying the two discovery
functions into one shared implementation is a reasonable follow-up, out of
scope here.

An empty-match root glob and a malformed root glob are both silent by
design (see Decision) — no `over lint` check was added for either in this
issue. A future issue can add one, mirroring `check_invalid_rules_globs`/
`check_rules_paths_exist`.
