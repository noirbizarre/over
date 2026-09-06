# ADR-014: Bidirectional checkout synchronization is `over sync`, scoped to an overlay's root git entry

**Status:** accepted

## Context

ADR-011/012 both explicitly deferred "turning `Checkout` entries into real
Git worktrees/checkouts" to #110, leaving `MaterializerRegistry::find`
return `None` for `MaterializationIntent::Checkout` and `Plan::build`
classify every such entry as `Operation::Deferred`. Issue #110 asks for
more than materialization, though: a checkout, once it exists, must be
editable with normal git tooling at its target and reconciled back to its
source — "the overlay itself materialized as a checkout is a bidirectional
synchronization surface" — while explicitly treating "git repositories
declared inside an overlay" (the same `overlay.git` map, used for e.g.
plugin-manager checkouts at a subpath) as an opaque resource whose content
is never synchronized.

Today, `overlay.git` is a single, undifferentiated `HashMap<String,
GitRepoConfig>`: every entry — whether it's the overlay's own root content
or a nested plugin repo — is cloned/configured identically by
`actions::git::clone_repositories`/`EnsureGitRepository`, called directly
and unconditionally by `Overlay::apply_inner`, entirely outside
`Plan`/`Materializer`.

## Decision

**Scope: only the root entry (`ROOT_PATH`, `"."`) is a bidirectional sync
surface.** `ROOT_PATH` already exists as the sentinel for "this overlay's
own content, cloned to its target root" (the shorthand single-URL/single-
object `git` form resolves to it). No new config field is introduced to
distinguish "sync surface" from "opaque resource": the existing key
already carries that meaning, and it maps exactly onto the issue's own
two-concern split. Any non-root `git` entry keeps today's behavior
unchanged — ensured present/configured, never content-synced.

**`CheckoutMaterializer` fills the registry seam, but only for presence,
not sync.** `src/materialize/checkout.rs` registers a real `Materializer`
for `MaterializationIntent::Checkout`: `classify` reuses
`status::git::inspect` (no duplicated git-state logic) and maps
`Status::Missing → Operation::Create`, every other status (`Applied`,
`Modified`, `Ahead`, `Behind`, `Diverged`, `Broken`, `Conflict`) →
`Operation::Noop`. Mapping non-`Missing` states to `Operation::Conflict`
was rejected: that would route a git checkout through the filesystem
conflict-resolution UI (`--force`/`--no-prompt`/interactive
skip-overwrite-absorb-diff) built for symlinks and files, which could
overwrite or prompt to overwrite a checkout's real content — precisely
what the issue asks never to do implicitly. `materialize` delegates to the
existing, unchanged `EnsureGitRepository` action.

`Overlay::apply_inner`'s direct, parallel `actions::git::clone_repositories`
call is deliberately **left in place**. By the time `Plan::build`
classifies a `Checkout` entry during a normal `apply`, the repository
already exists (clone happened first, synchronously), so
`CheckoutMaterializer::materialize` is naturally a no-op fallback there —
it only does real work for any other caller of `Plan::execute` (e.g.
tests, or a future direct caller). This keeps `apply`'s existing
parallel-clone performance for overlays with multiple `git` entries,
rather than serializing them through `Plan::execute`'s sequential step
loop for no behavioral benefit today.

**Bidirectional sync is a new, separate `over sync` command — never part
of `apply`.** `apply`'s job stays "ensure presence/configuration"; it never
pulls or pushes checkout content. `src/actions/git/sync.rs` adds the
mutating primitives (`fetch_origin`, `pull`, `push_branch`,
`abort_merge`), all built on git2's own merge machinery
(`merge_analysis`/`merge`/`cleanup_state`) rather than a custom merge
algorithm — real conflicts are left as real conflict markers in the
working tree and index, exactly as `git merge` would leave them, for the
user to resolve with ordinary git tooling. `src/sync/mod.rs` orchestrates:
it finds every root `Checkout` entry in a `DesiredTree`, expands a
`worktree`/`worktrees` bare-repo config into one independently-synced unit
per worktree directory that exists on disk (satisfying "define behavior
for multiple materialized checkouts from the same overlay source" without
needing per-worktree `DesiredEntry`s — a documented future refinement,
same as the existing aggregation-by-severity in `status::git::inspect`),
and refuses to touch a dirty working tree at the pull step.

**Merge only, no rebase, no auto-stash, for this iteration.** The pull
direction always produces a fast-forward or a real merge commit
(`git pull --no-rebase` semantics) — rebase-based sync is a documented
future option, not built now. A dirty checkout blocks the pull step with a
clear error rather than being auto-stashed: "never discard uncommitted
changes implicitly" is read literally.

**`over sync --abort` never needs a persisted "pre-merge HEAD".** A
conflicted `git2::Repository::merge` never moves `HEAD` (confirmed against
git2's own docs) — only the index/working directory and `MERGE_HEAD`
change. Aborting is therefore just `reset(HEAD, Hard)` +
`cleanup_state()`, using whatever `HEAD` already is.

**XDG state (`src/sync/state.rs`) is informational only, never
authoritative.** It persists a `SyncState { checkouts: HashMap<path,
CheckoutRecord> }` — overlay/repo-key/worktree association, last synced
commit, last outcome — read back only to enrich `over sync`'s own output.
Nothing in the pull/push/abort decision tree reads it: conflict/resume
correctness comes entirely from git's own `repo.state()`/`MERGE_HEAD`/
index, per the issue's own constraint ("state is operational metadata...
not a second Git database"). Deleting the file is always harmless.

## Consequences

`over apply`, `over status`, and `over diff` are unchanged in behavior:
`status::git`/`diff` already classified git checkouts independently of the
`Materializer` registry and continue to do so. `Plan`/`PlanStep`/
`Operation`/`ActualState` are unchanged — no new variants were needed,
confirming ADR-012's registry design absorbs #110 without touching the
shared `Plan` vocabulary. The cost of leaving `apply_inner`'s parallel
clone path untouched is that `CheckoutMaterializer::materialize`'s
"ensure repository presence" logic is exercised twice in slightly
different code paths (the direct call, and the registry-mediated
fallback) — accepted as low risk since both ultimately call the same,
unchanged `EnsureGitRepository` action.

This does not implement rebase-based sync, auto-stash, per-worktree
`DesiredEntry`s, or content synchronization for non-root `git` entries —
all documented, deliberate non-goals left for later issues. `unapply`
(#64) and materialization-rule migrations (#113) are untouched.
