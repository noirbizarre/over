# ADR-009: `over lint` is a separate, tolerant, pre-deserialization static-analysis pass

**Status:** accepted

## Context

`over lint` could have been implemented as "try to construct every overlay
via `Overlay::new` and report the errors" — reusing the same path `apply`
and `Repository::overlays()` already use. That path is not built for
diagnostics: `Repository::overlays()` silently warns and *drops* a
malformed overlay, and `config`/`serde` error messages are technical
("expected one of `name`, `root`, ...", raw "caused by:" chains) rather than
actionable.

## Decision

`lint_repository` never modifies a target directory and never calls `Overlay
::apply`. It re-implements overlay discovery so it can continue past a
parse failure — reporting, for instance, "overlay X `uses` overlay Y, which
failed to parse" instead of silently treating Y as nonexistent. Before
deserializing at all, `prevalidate_descriptor` parses the raw TOML/YAML as a
generic table and checks top-level keys against a hand-maintained `VALID_
OVERLAY_KEYS` list, offering a Levenshtein-distance typo suggestion
(`targt` → "did you mean `target`?") instead of letting `serde`'s error
surface. Diagnostics carry a `Severity` (error/warning) with a defined sort
order, which `apply`'s plain `anyhow::Error` propagation has no equivalent
of.

## Consequences

Lint messages stay readable and specific — confirmed by tests that assert
no raw `serde`/`config` error text leaks through. The cost is duplication:
`VALID_OVERLAY_KEYS` must be kept in sync by hand with `Overlay`'s actual
`#[derive(Deserialize)]` fields — nothing generates it from the struct — so
a new field added without updating the constant makes `lint` misreport it
as an unknown-key typo. `discover_overlay_dirs` also duplicates `Repository
::overlays()`'s glob-walk logic rather than sharing it, which can drift if
one changes without the other.

## Amendment: resolved ownership (#194)

After tolerant static checks, lint resolves each selected composition through the
shared desired-state builder. It reads relevant destination metadata to preserve
runtime path-routing and materialization semantics, but never applies, repairs,
executes hooks, writes Git objects, or persists state. The current machine and home
directory provide the default context; `--root` selects another target root.
Resolution failures become diagnostics and do not stop unrelated compositions.
Ownership errors follow ADR-008's flat union rather than traversal-order precedence.

## Amendment: dependency impact and grouped output

Configuration and graph errors belong to the overlay responsible for them.
Every transitive `uses` consumer receives one warning per invalid dependency,
regardless of how many errors or paths lead to that dependency. These warnings
do not replace a consumer's independent errors. Context-dependent resolution
failures and composition ownership conflicts remain errors on the composition;
they do not establish that a dependency is invalid in every consumer's context.

Diagnostics sort by overlay name first, then severity and message. The lint CLI
renders alphabetical overlay sections with errors before warnings inside each
section, keeping an overlay's findings together even when severities differ.
The summary and exit status still count original errors and dependency warnings.
