# ADR-012: Materializer backend registry, amending ADR-011

**Status:** accepted

## Context

ADR-011 built `Plan`/`PlanStep`/`Operation` between `DesiredTree` and actual
filesystem state, but left `Plan::execute`'s dispatch hardcoded: a pair of
free functions, `classify()` and `build_action()`, pattern-matched directly
on `MaterializationIntent`/`Provenance` and called into the concrete
`actions::fs`/`actions::symlink` types. ADR-011 named this explicitly as
acceptable debt: "the seam #108 needs (replacing intent-based dispatch with
a `Materializer` registry) is documented but not built... given there is
exactly one materializer today."

Issue #108 asks to make symlink and git-checkout materialization two
backends of the same `DesiredTree` → `Plan` → materialize pipeline, without
prematurely building the bidirectional git-checkout workflow itself (#110)
or rule-migration semantics (#113).

## Decision

`src/materialize/` introduces:

- `Materializer` — a trait owning both read-only actual-state inspection
  (`classify`) and execution (`materialize`) for the intents it
  (`handles`). Backends decide for themselves what "inspect actual state"
  means (plain `symlink_metadata` for symlinks/directories today; a
  git-aware equivalent for #110's checkout backend later), rather than
  `Plan` assuming one universal notion of "actual state".
- `MaterializerRegistry` — a small ordered list of backends. `find(intent)`
  returns the one that handles it, or `None`. `Plan::build` treats `None`
  as `Operation::Deferred`, and `Plan::execute` skips `Deferred` steps —
  both exactly as before this issue.
- `SymlinkMaterializer` — a straight move of the previous `classify()`/
  `build_action()` bodies (`Directory`/`SymlinkFile`/`SymlinkDirectory`
  intents; sidecar-vs-regular `Provenance` dispatch to `EnsureSymlink` vs
  `EnsureLink`/`EnsureDirLink`/`EnsureDir`). No behavior change: the same
  conflict semantics (force/no_prompt/interactive skip-overwrite-absorb-diff
  for regular entries, unconditional overwrite for `.link.*` sidecars) still
  live in those unchanged `Action` impls.

`MaterializationIntent::Checkout` has no registered backend yet —
`MaterializerRegistry::find` returns `None` for it, so it still classifies
as `Operation::Deferred`, identical to the previous hardcoded match arm.
#110 adds a `CheckoutMaterializer` to the registry's backend list to change
that, without `Plan` or any other backend changing.

`Materializer::materialize` is declared `#[async_trait(?Send)]`: it
dispatches to `Box<dyn Action>` internally, and `Action` (`src/exec/
action.rs`) itself isn't `Send`-bound. Like `Plan::execute` before this
issue, materialization is only ever awaited inline — never spawned onto
another task — so this costs nothing today and doesn't preclude a backend
spawning its own concurrent work internally (as `actions::git::
clone_repositories` already does with `join_all`).

## Consequences

`Plan` no longer knows about concrete `actions::fs`/`actions::symlink`
types, or about Git, at all — `plan::reconcile` now only imports
`MaterializerRegistry`. Adding checkout materialization (#110) or
materialization-rule migrations (#113) means adding a `Materializer` impl
and one line in `MaterializerRegistry::default`, not touching `Plan::build`/
`Plan::execute`. This is a pure refactor: existing `Plan`-level tests
(idempotency, dry-run, force/no_prompt conflict handling) pass unchanged,
confirming the registry-mediated path behaves identically to the removed
hardcoded dispatch.

This does not implement bidirectional Git checkout sync (#110) — `Checkout`
entries remain `Operation::Deferred` and unmaterializable until that issue
registers a real backend. It also does not add materialization-rule
migration detection (#113); `Operation` still has no `Migrate`-style variant.
</content>
