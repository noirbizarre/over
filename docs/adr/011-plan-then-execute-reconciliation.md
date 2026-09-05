# ADR-011: Plan-then-execute reconciliation, amending ADR-005 and ADR-006

**Status:** accepted

## Context

ADR-006 documented that conflicts during `apply` are "resolved lazily,
action-by-action, during application... rather than computed up front as a
whole-apply plan". ADR-005 flagged `src/exec/plan.rs` as an empty, unused
file — scaffolding for a real execution plan that was considered and
deliberately not built, "either be implemented or removed".

`Overlay::apply` walked the overlay tree once, building `EnsureDir`/
`EnsureLink`/`EnsureDirLink`/`EnsureSymlink` and executing each immediately.
Every `Action::execute` started with `if ctx.dry_run { return Ok(()) }`, so
`--dry-run` walked the exact mutating code path and could never distinguish
"already applied" from "about to create" — it printed one line per entry
unconditionally.

Issue #13 asks for a typed `Plan` sitting between `DesiredTree` (#107, what
we want) and actual filesystem state, so `apply`'s preview is a real diff and
so `status`/`diff`/`unapply` (#12/#109/#64) can be built on the same type
later.

## Decision

`src/plan/` (not `src/exec/plan.rs`, which is deleted) introduces:

- `plan::actual::inspect` — read-only inspection of what currently exists at
  a path (missing / directory / file / symlink-to-X).
- `plan::PlanStep` / `plan::Operation` — one step per `DesiredEntry`,
  classified as `Create` / `Noop` / `Conflict` / `Deferred` by comparing
  desired intent to actual state.
- `plan::Plan::build` — read-only, produces every step up front.
- `plan::Plan::execute` — walks the steps and, for each `Create`/`Conflict`,
  builds and runs the *existing, unchanged* `EnsureDir`/`EnsureLink`/
  `EnsureDirLink`/`EnsureSymlink` actions. Conflict resolution itself
  (force/no_prompt/interactive skip-overwrite-absorb-diff for regular
  entries; unconditional overwrite for `.link.*` sidecars) is not
  reimplemented — it still lives in those actions, invoked with the same
  `Ctx` flags as before.

`Overlay::apply_inner` now builds a `DesiredTree::build_own` (this overlay's
entries only, no `uses` recursion) and a `Plan` from it, prints the plan as a
structured preview under `--verbose`/`--dry-run`, then executes it. `uses`
recursion, cycle detection, per-overlay banners, and git repository cloning
(`actions::git::clone_repositories`) are untouched — they stay outside the
plan, one overlay node at a time, exactly as before. `actions::fs::link` and
`Overlay::apply_symlinks` are removed: both are fully superseded.

This **amends** ADR-006: conflicts are now classified up front, during plan
build, so a preview can show them — but final resolution (the prompt, the
`--force` removal, the absorb/diff choice) still happens at execute time,
using the same code ADR-006 described. It does not reverse ADR-006's
symlink-first default or its two escape hatches (`link_dirs`, `.link.*`
sidecars), only the "no whole-apply plan" clause.

This does **not** reverse ADR-005: no scheduler or parallel execution was
added. `Plan::build` is a sequential read-only pass; `Plan::execute` is a
sequential walk, same as the code it replaces. Async remains scoped to I/O
concurrency (`spawn_blocking` for filesystem syscalls, `join_all` for
independent git clones) — `Plan` doesn't change that. `src/exec/plan.rs` —
the file ADR-005 flagged as needing to be implemented or removed — is
removed; the plan is implemented as its own top-level `src/plan/` module
instead, mirroring `src/desired/`, since it is a domain concept read by
future `status`/`diff` (#12/#109), not execution-context/templating
plumbing that belongs under `exec/`.

Git checkout materialization stays entirely outside `Plan::execute`:
`MaterializationIntent::Checkout` entries are classified as
`Operation::Deferred` and are visible in a plan's preview, but only
informational — #108/#110 own turning them into real checkouts/worktrees
and will replace `Plan::execute`'s current direct dispatch on
`MaterializationIntent` with a registered `Materializer` lookup.

## Consequences

`--dry-run` now reflects true reconciliation state (create/unchanged/
conflict/deferred counts) instead of an unconditional action list — a
genuine improvement, at the cost of one more read pass over the target
filesystem before executing (negligible next to the syscalls apply already
performs). `Plan`/`DesiredTree` are now the shared vocabulary `status`/
`diff`/`unapply` (#12/#109/#64) can build on without re-deriving their own
notion of "what should be here" and "what already is". The seam #108 needs
(replacing intent-based dispatch with a `Materializer` registry) is
documented but not built — `Plan::execute` still hardcodes the
`actions::fs`/`actions::symlink` mapping, which is acceptable debt given
there is exactly one materializer today.
