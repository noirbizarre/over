# ADR-016: Partial files are managed blocks injected via a sidecar, not a stem-plus-sidecar pair

**Status:** accepted

## Context

Issue #66 asks for partial file management: modifying part of an existing
(possibly foreign) file rather than symlinking the whole thing, with
conflict detection against actual content and a safe, explicit unapply.
Its dependencies (#107 desired state, #13 plan/reconcile, #64 unapply) are
closed; it is explicitly scoped independent of #113 (hierarchical
rules/migration) and doesn't depend on #61 (full file templating) — content
is used verbatim, the same way `.link.*` sidecar `target` strings were
before #61 renders anything (ADR-010).

`.link.*` sidecars (ADR-006) are the only existing per-path configuration
mechanism: a `<stem>.link.{toml,yaml,yml}` file declares an arbitrary extra
symlink, with the payload (a `target` string) living inline in the config,
not in a separate same-stem file — there is no expectation that a file
named `<stem>` exists anywhere in the overlay tree.

## Decision

A new `*.partial.{toml,yaml,yml}` sidecar convention, discovered and parsed
exactly like `.link.*` (`actions::partial::discover_partials` mirrors
`actions::symlink::discover_symlinks` field-for-field: same glob, same stem
derivation, same TOML > YAML > YML precedence-with-warning on duplicates).
Its payload is inline, like `.link.*`'s `target`:

```toml
target = "~/.zshrc"
content = "alias ll='ls -la'\n"
# marker = "aliases"   # optional; defaults to the sidecar's own stem
```

This is a deliberate rejection of a stem-file-plus-sidecar pair (where a
same-named file's content would be the block body): it would need a new
walk-exclusion rule to keep that stem file from *also* becoming its own
`SymlinkFile` entry, and would only pay for itself once #61 needs to render
that content — which is explicitly out of scope here. Inline `content`
needs no new exclusion beyond the sidecar file itself (mirroring
`walk_overlay_tree`'s existing `symlink_config` glob), and is trivially
extensible later: #61 can add a `content_file`/render step without
breaking this shape.

The block is delimited by fixed marker lines:

```text
# >>> over: <marker> >>>
<content>
# <<< over: <marker> <<<
```

Fixed to `#`-comments, not configurable — the dominant dotfile case (shell
rc files, ini-style configs, gitconfig, ssh config). A target file that
can't contain `#` comments (JSON, for instance) isn't a good fit for this
feature yet; a configurable comment style is left for a future issue if
demand appears, rather than speculatively building it now.

**New model surface**, mirroring every existing intent/provenance pair:

- `MaterializationIntent::PartialFile { content, marker }` — the first
  intent to ever produce `EntryKind::File` (reserved since #107/#61, never
  produced until now).
- `Provenance::PartialSidecar { overlay, config }`.
- `DesiredTree::build`'s target for a partial entry is the **rendered path
  itself** (`symlink::render_symlink_target` reused as-is — it's already a
  generic path-template renderer, not symlink-specific), not
  `target_root.join(stem)`: a managed block's target is an arbitrary
  external file, not something living under the overlay's own target root
  by naming convention (unlike `.link.*`, whose target root placement
  mirrors a real symlink's expected location).

**Classification is content-aware**, the one materializer backend for
which "actual state" means more than `symlink_metadata`
(`PartialFileMaterializer::classify` reads the target file when it's a
plain file): missing target or missing block → `Operation::Create`
(purely additive — inserting a block never touches existing content);
block present and matching → `Operation::Noop`; block present but drifted,
or markers malformed (a begin without a matching end, or vice versa) →
`Operation::Conflict`, resolved at execute time through the same
force/no_prompt/interactive gate every other conflict uses, with its own
choice set (Skip/Overwrite/Diff — no "Absorb": there's no single overlay
source file to adopt hand-edited content into, only a `content` string in
the sidecar). A directory or symlink occupying the target is a *structural*
conflict, resolved separately (Skip/Overwrite only), reusing
`actions::fs::remove_target` (promoted to `pub(crate)`) rather than
duplicating it.

**Unapply strips only the block.** `unapply::dematerialize`'s new
`PartialFile` arm re-reads the target immediately before acting (same
classify→execute race-safety every other removal path already has) and
only removes the block if it *still* matches exactly what this entry would
write. If stripping leaves nothing behind, the file itself is removed
(mirroring "a directory disappears exactly when it turns out to hold
nothing but what this overlay put there," ADR-015) — otherwise the file is
rewritten with just the block gone, every other byte preserved. No new
`Outcome` variant needed: `Operation::Noop` on a `PartialFile` entry already
falls through the existing generic `(_, Operation::Noop) => Outcome::Removed`
arm.

## Consequences

`EntryKind`/`MaterializationIntent`/`Provenance` gain one new variant each,
which is a breaking change for every exhaustive match over them —
`desired::entry::kind()`, `materialize::symlink`'s `classify`/`build_action`
(both gain an `unreachable!()` arm, `handles()` excludes the new intent),
and `plan::step::PlanStep`'s `Display` (three new arms) all had to be
updated for the crate to keep compiling; `plan::reconcile::Plan` itself
needed no change (it dispatches through `MaterializerRegistry`, never
matches on `MaterializationIntent` directly, confirming ADR-012's registry
promise). `unapply::Report::build`'s classification needed no change either
(its match is not exhaustive over intent) — only `dematerialize` gained an
arm.

The main cost is the fixed `#`-comment marker format: files that can't
carry `#` comments aren't supported. A hand-edited block inside the
markers is always caught as drift (never silently overwritten without
`--force`/a prompt) — the same conservatism ADR-015 chose for unapply
applies here to apply/materialize too. This is independent of #113: there
is no rule/hierarchy resolution here, the sidecar is, like `.link.*`,
today's only per-path configuration mechanism; #113 may later let a block's
`marker`/`content` be resolved from inherited configuration instead of a
literal sidecar field, without needing this ADR's model to change shape.
