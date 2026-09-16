# `over diff`

Show differences between desired overlay state and what actually exists on
disk (and, for `git` checkouts, in the repository), without touching
anything.

```
Usage: over diff [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to check (default_overlay if configured, all overlays otherwise)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -d, --debug        Toggle debug traces
      --no-uses      Do not process uses
  -a, --all          Diff every overlay, ignoring any configured default_overlay
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

When `NAME` is omitted, a configured `default_overlay` (see
[Default Overlay Selection](../configuration.md#default-overlay-selection))
narrows the report to just that overlay; pass `--all` to report on every
overlay regardless.

`diff` reuses the same `DesiredTree`/`Plan` reconciliation model as
[`over status`](status.md), but goes one step further: instead of just
labelling an entry as needing attention, it shows *what* differs —

- a real line-level content diff when an existing plain file hasn't been
  linked into the overlay yet;
- a target-path diff (not a content diff) when a symlink already exists but
  points somewhere other than the overlay's source;
- an "unexpected" notice when a fundamentally different kind of entity
  occupies a target (e.g. a real directory where a symlink was expected) —
  there's nothing meaningful to diff in that case;
- the same dirty/ahead/behind/diverged/merge-in-progress state
  `over status` reports for the overlay's own root `git` checkout (commit/
  merge mechanics themselves are out of scope — see #110). A declared
  (non-root) `git` entry instead only shows a `Conflict` when its declared
  configuration itself has drifted — see
  [ADR-021](../adr/021-declared-git-repositories-report-provisioning-not-content-status.md);
- for a **virtual checkout** (`materialization = "checkout"`, no `.git` at
  the target — see [ADR-022](../adr/022-virtual-checkout-materialization.md)),
  the same aggregate status `over status` reports, plus a real line-level
  content diff for each modified file (base content vs. current on-disk
  content) and a plain added/deleted marker for the rest — computed
  against the checkout's recorded base revision, not a real git index.

Without `--verbose`, only entries with something to show are printed; pass
`--verbose` to also print unchanged entries. See
[ADR-013](../adr/013-diff-has-its-own-taxonomy-and-uses-similar-for-content-diffs.md).
