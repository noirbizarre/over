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
deserialization, shared descriptor inspection checks top-level and nested keys,
offering a Levenshtein-distance typo suggestion (`targt` → "did you mean
`target`?") instead of letting `serde`'s error surface. Unknown and removed keys
are discarded with warnings in both runtime loading and lint; invalid recognized
settings still produce errors. Diagnostics carry a `Severity` (error/warning) with a defined sort
order, which `apply`'s plain `anyhow::Error` propagation has no equivalent
of.

## Consequences

Lint messages stay readable and specific — confirmed by tests that assert
no raw `serde`/`config` error text leaks through. Schema inspection lives in
`overlays::schema`, shared by runtime loading and lint. Its recognized-key lists
must be kept in sync with the deserialized models; no schema is generated from
the structs. Lint collects source-attributed warnings without duplicate runtime
output, and warnings alone do not invalidate dependencies. `discover_overlay_dirs`
also duplicates `Repository
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
renders alphabetical overlay and file sections, with errors before warnings
within each file. Overlay-wide findings appear before the file sections.
This presentation grouping does not change canonical diagnostic ordering,
including the flat list exposed by `over lint --json`.
The summary and exit status still count original errors and dependency warnings.
