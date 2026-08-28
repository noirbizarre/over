# ADR-008: `uses` composition is a flat union of independent trees, not a layered override

**Status:** accepted

## Context

An overlay can `uses` other overlays, pulling in their files and packages.
Two composition models fit: a layered override (later entries take priority
over earlier ones' *values*, like the ancestor-directory config cascade in
`Overlay::new` — see [ADR-002](002-overlay-is-a-directory-not-a-repository.md))
or a flat union (every used overlay applies to the same target, with no
declared priority between them).

## Decision

`Overlay::apply_inner` applies every `uses` entry, recursively, to the same
target directory as the parent overlay — not a separate namespace. A
`visited` set skips an overlay already fully applied (safe for diamond
dependencies) and a `stack` set turns a real cycle into an error rather than
infinite recursion. Package installs compose the same way, as a pure
`BTreeSet` union (`collect_packages`), with their own `visited` set so a
diamond-shared overlay's packages aren't installed twice.

`over lint`'s `check_cycles` treats graph validity (acyclic, every `uses`
target exists) as a static-analysis concern, separate from apply-time cycle
detection.

## Consequences

Diamond dependencies (two overlays both `uses`ing a common third one) work
correctly and cheaply — no double-application, no double-install. But there
is no way to declare that one used overlay's file should win over another's
identically-named file: the outcome depends on `uses` list order plus
whatever the user picks in the interactive conflict prompt. That is a
fragile, order-sensitive behavior for something that looks, from the
config, like a composition feature with defined priority.
