# ADR-017: Materialization rules generalize `link_dirs` into path/subtree overrides, amending ADR-006

**Status:** accepted

## Context

ADR-006 established symlink-first apply with two escape hatches:
`link_dirs` (a directory symlinked as a unit instead of walked file-by-file)
and `.link.*` sidecars (arbitrary extra links). `link_dirs` is a single flat
list of glob patterns, always resolving to the same directory-level symlink
outcome, with no way to declare a repository-wide default independent of any
particular pattern, and no way to override that default for a specific
subtree without also going back to enumerating `link_dirs` globs.

#113 asks for a general `defaults`/`rules` model: a default materialization
for an overlay, plus more specific path/subtree overrides, resolved
hierarchically. `checkout` materialization (`overlay.git`) and rule-driven
migration between materializations are both out of scope here — see #129 for
migration and #130 for legacy detection.

## Decision

Add `defaults: { materialization }` and `rules: [{ path, materialization }]`
to the overlay descriptor schema (`src/overlays/rules.rs`:
`MaterializationKind`, `Defaults`, `MaterializationRule`). `materialization`
is one of `symlink` (recurse and symlink files individually — today's
implicit default) or `symlink-directory` (symlink the directory as a single
unit). `checkout` is deliberately not a valid value: that intent stays
driven by `overlay.git`, unrelated to this resolution.

Resolution (`rules::resolve`), for a directory at a given relative path:

1. the most specific matching `rules` entry (a literal path always outranks
   a glob; longer patterns are considered more specific within the same
   tier — see `MaterializationRule::specificity`);
2. `defaults.materialization`;
3. the hardcoded fallback, `symlink`.

Both fields are inherited through the *same* cascading descriptor chain
ADR-002 already established (root `over.toml` down to the overlay's own
directory) — no separate root-only configuration mechanism was introduced.
A repository root `over.toml` setting `defaults.materialization` becomes the
baseline for every overlay; any overlay along the chain can override it
entirely (config-rs replaces a scalar/list value wholesale at the nearest
level that defines it — the same "closest wins entirely, not merged
element-wise" semantics `exclude`, `link_dirs`, `git`, and `uses` already
have).

`link_dirs` is kept, unchanged in the config file, but reimplemented as
sugar: `Overlay::effective_rules` (`rules::effective_rules`) translates each
`link_dirs` glob into an equivalent `MaterializationRule` with
`materialization: symlink-directory`, placed *before* explicit `rules`
entries so an explicit rule wins a specificity tie against a legacy
`link_dirs` pattern for the same path. `Overlay::is_link_dir` — the two
existing call sites (`desired::tree::walk_overlay_tree`,
`actions::fs::add_dir`) — now delegates to `rules::resolve`, so both
mechanisms flow through one resolution path.

## Consequences

A single new mechanism replaces what would otherwise be two independent
special cases (`link_dirs`, and any future default-materialization field).
`over lint` gains matching checks for `rules` (invalid globs, empty list,
duplicate paths, paths matching nothing on disk), mirroring the existing
`link_dirs` checks.

Because `rules`/`defaults` — like every other overlay field — are replaced
wholesale by the nearest config level that defines them rather than merged
element-wise across levels, an overlay that defines its own `rules` list
loses the repository root's `rules` entries entirely unless it repeats them.
This is consistent with the rest of the schema, but is a real limitation
worth documenting for anyone expecting per-level list merging.

Specificity ranking (`MaterializationRule::specificity`) is a heuristic —
literal-beats-glob, then longer-pattern-wins — not a full partial order over
arbitrary glob pairs. It resolves the case #113 asks for (a specific path
overriding a broader glob) but can still surprise for two overlapping globs
of unrelated shapes; `over lint`'s duplicate-path check only catches exact
string duplicates, not general shadowing.
