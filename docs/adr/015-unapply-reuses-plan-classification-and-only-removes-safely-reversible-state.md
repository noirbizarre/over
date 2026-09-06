# ADR-015: `unapply` reuses `Plan`'s classification and only ever removes safely reversible state

**Status:** accepted

## Context

Issue #64 asks for `over unapply`: a reverse reconciliation from an
overlay's desired state and actual filesystem/git state, explicitly
*without* relying on an authoritative serialized `ApplyState` — "Respect
the desired-vs-actual model rather than introducing a separate imperative
removal mechanism." ADR-011 already anticipated this: `Plan`/`DesiredTree`
were built as the shared vocabulary `status`/`diff`/`unapply` (#12/#109/#64)
would each build on, and ADR-013 established the precedent that each of
these commands defines its own result taxonomy rather than forcing a
shared one.

Without a persisted apply-time record, `unapply` has no way to know
whether it created a given directory or whether it already existed (e.g.
the user's home directory, when `target = "~"`), nor whether a git
checkout has commits or edits that only exist locally. The issue is
explicit that this must never be guessed: "never implicitly discard
uncommitted changes or local commits", and destructive transitions must be
"explicit and previewable".

## Decision

**Scope: the named overlay's own entries only, no `uses` recursion.**
`DesiredTree::build_own` is used, mirroring `Overlay::apply_inner`'s
per-node scope. Unlike `apply`, `unapply` does not recurse into `uses`:
removing a shared dependency's entries here could break another,
still-applied overlay that also `uses` it (a diamond dependency). A used
overlay is unapplied by naming it explicitly.

**Classification reuses `Plan::build`'s existing `Operation`, unmodified.**
For `Directory`/`SymlinkFile`/`SymlinkDirectory` intents, `Operation::Noop`
already means exactly "the target still matches what this overlay would
materialize here" (including a dangling soft symlink whose source has
since vanished — still *our* link, just broken) and `Operation::Conflict`
already means "something else occupies the target". `crate::unapply::Outcome`
maps these directly: `Noop → Removed`, `Conflict → NotOwned`, `Create →
AlreadyAbsent`. No new read pass over the filesystem, and no changes to
`Plan`/`PlanStep`/`Operation` were needed — confirming ADR-011's original
promise. `Outcome` is its own enum, not a reuse of `status::Status` or
`diff::Change`, per ADR-013's "own taxonomy" precedent.

`Plan`'s classification is deliberately too coarse for `Checkout` entries
(`CheckoutMaterializer::classify` collapses every non-`Missing` git state
to `Operation::Noop` by design, ADR-014, to avoid routing git state through
filesystem conflict-resolution UI). `unapply` calls `status::git::inspect`
directly instead — exactly like `status`/`diff` already do — and only
removes a checkout when it reports `Status::Applied`: fully in sync with
its upstream, nothing uncommitted, nothing unpushed. Every other status
(`Modified`/`Ahead`/`Behind`/`Diverged`/`Conflict`/`Broken`) is left
untouched, unconditionally — there is no `--force` override, because there
is nothing here that should ever be forced. This directly reuses #110's
own safety primitive rather than re-deriving a notion of "clean enough to
delete".

**No persisted apply-state is needed for directories either.** A directory
is only ever removed with a non-recursive `fs::remove_dir`
(`unapply::remove_dir_if_empty`), never `remove_dir_all`, ignoring
`NotFound`/`DirectoryNotEmpty` as success rather than error. Combined with
bottom-up removal order (`DesiredTree` sorts entries ascending by target —
a directory's entry always precedes anything nested under it, so
`Report::execute` walks `.rev()` to remove children before parents), this
means a directory disappears exactly when, and only when, it turns out to
hold nothing but what this overlay put there. A user's home directory
(`target = "~"`) or any directory holding unrelated content is never at
risk: it is simply never empty, so removal is always silently skipped —
no ownership tracking, no special-casing the root target, required.

**No new interactive confirmation gate.** Because every removal is
already gated to exactly-matching, safe-to-lose state, every removal is
also reversible: symlinks/directories come back with a plain `over apply`
(overlay source content is never touched by `unapply`), and a checkout is
only ever removed when there was nothing local-only left to lose. `--dry-run`
is therefore sufficient preview, mirroring `apply`'s own UX — no `--force`/
`--no-prompt` equivalents exist, since there is nothing left to force
through once non-owned/dirty entries are always skipped.

**Kept out of the `Materializer` trait.** `crate::unapply` calls
`status::git::inspect`/`actions::fs`/the `symlink` crate directly for its
own removal primitives, the same way `status`/`diff`/`sync` already bypass
`Materializer::materialize` for their own concerns (git status inspection,
content diffing, fetch/merge/push) rather than growing that trait with
methods only one caller needs.

## Consequences

`Plan`/`PlanStep`/`Operation`/`ActualState`/`Materializer` are all
unchanged — `unapply` is a new top-level module (`crate::unapply`, plus
`crate::cli::unapply`) built entirely on existing read-only primitives.
Nothing here implements #113's rule-aware migrations (symlink ↔ checkout,
file-level ↔ directory-level symlink transitions): that remains a
`Plan`/`Operation` extension for a later issue, untouched by this one.
`uses`-aware/cascading unapply (removing a whole dependency graph safely,
accounting for shared use across overlays) is also left for a future
issue — the current scope is deliberately the single, explicitly named
overlay.

The main cost is conservatism: a directory containing even one unrelated
file, or a checkout with a single uncommitted line, is left exactly as-is
rather than cleaned up further. This is intentional — the alternative
requires either persisted apply-time provenance (the exact "authoritative
`ApplyState`" the issue asks to avoid) or destructive assumptions the issue
explicitly forbids.
