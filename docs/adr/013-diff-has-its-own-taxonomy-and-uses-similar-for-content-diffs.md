# ADR-013: `diff` has its own taxonomy and uses `similar` for content diffs, amending ADR-011

**Status:** accepted

## Context

ADR-011 introduced `Plan`/`ActualState` as the shared vocabulary `status`
and `diff` (#12/#109) would both build on, "without re-deriving their own
notion of what should be here and what already is". `status` (#12) landed
first and defined `status::Status` — a closed, 8-variant enum (`Applied`,
`Missing`, `Modified`, `Broken`, `Conflict`, `Ahead`, `Behind`, `Diverged`)
answering one question: does this entry need attention?

Issue #109 asks a different question: *what exactly* differs, not just
whether it does — "missing, modified, conflicting, and unexpected entries",
with real content diffs for regular files and target-path diffs (not
content) for symlinks. `status::Status::Conflict` alone can't carry that:
it's used today for two structurally different situations — a filesystem
type/content mismatch, and a git merge/rebase/cherry-pick in progress —
and `status::Status::Modified` is used exclusively for a dirty git
checkout, never for a filesystem entry.

## Decision

`crate::diff::Change` is its own enum, not a reuse of `status::Status`:
`Unchanged`, `Missing`, `Modified(ContentDiff)`, `Unexpected { actual:
ActualState }`, `Broken`, `Checkout(status::Status)`. It is derived
directly from the same `ActualState` × `MaterializationIntent`
combinations `SymlinkMaterializer::classify` already produces (no new
filesystem scanning):

- A file-symlink expected, a real file found: both are regular files, the
  overlay's source just isn't linked in yet → `Modified`, with a real
  content diff.
- Either symlink kind expected, a symlink pointing elsewhere found →
  `Modified`, with a diff of the target *paths*, never file content.
- Anything else structurally different (a directory expected, or a
  directory-symlink expected but a plain file/directory found) →
  `Unexpected` — #109's "unexpected entries", reinterpreted as a
  type-mismatch rather than an untracked-file scan (out of scope: no code
  exists to walk a target tree for files no `DesiredTree` describes, and
  adding it is a materially bigger feature than this issue asks for).
- Git checkouts: `status::git::inspect` reused verbatim, wrapped in
  `Change::Checkout(status::Status)` for the residual
  dirty/ahead/behind/diverged/conflict cases — commit/merge mechanics stay
  #110's job, exactly as ADR-011 already scoped for `Plan`.

For the two `Modified` cases, `crate::diff::content::ContentDiff` is a
plain data type (`Vec<DiffLine>`, each tagged `Equal`/`Delete`/`Insert`),
built with the `similar` crate (`TextDiff::from_lines` + `grouped_ops(3)`
for git-like context trimming) rather than shelling out to `git diff
--no-index` the way `actions::fs::show_diff` already does for interactive
conflict resolution. `similar` runs in-process (no `git` binary required
on `PATH`) and produces structured data with no formatting baked in — only
`ContentDiff`'s `Display` impl (used by the CLI) knows about colors. This
directly serves #109's explicit ask for "a machine-readable representation
later without coupling the core model to terminal formatting": today only
a colored terminal string is printed, but the diff itself is already a
plain `Vec<DiffLine>` a future `--json` flag could serialize as-is.

## Consequences

`diff` and `status` now derive genuinely independent classifications from
the same underlying `Plan`/`ActualState`/`status::git::inspect` primitives
— some duplication of the `(intent, operation)` match compared to
`status::Report::build`'s equivalent, accepted because the two commands
answer different questions and forcing one shared enum to serve both would
either weaken `status`'s existing meaning or bloat `diff`'s with
irrelevant variants. Binary files (either side not valid UTF-8) fall back
to a `None`-lines marker rather than erroring, mirroring git's own "binary
files differ" rather than diffing raw bytes as text. A future `unapply`
(#64) remains free to define its own third taxonomy the same way, per
ADR-011's original intent — `Plan`/`DesiredTree` is the shared
foundation, not a shared result type.
