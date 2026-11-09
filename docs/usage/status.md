# `over status`

Inspect resolved overlay configuration and reconciliation health, even before
the overlay has ever been applied. `over show` is a compatibility alias with
the same options, output, and exit status.

```text
Usage: over status [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to check (default_overlay if configured, all overlays otherwise)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -d, --debug        Toggle debug traces
      --no-uses      Skip dependency operations, retaining dependency data
  -a, --all          Check every overlay, retaining configured default_overlay data
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

When `NAME` is omitted, a configured `default_overlay` (see
[Default Overlay Selection](../configuration.md#default-overlay-selection))
narrows the report to just that overlay; pass `--all` to report on every
overlay regardless.
Without a configured default, omitting `NAME` selects every overlay.

Explicit `NAME` and `--all` retain the configured default's
[data context](../configuration.md#consumer-metadata-data). Each target resolves
its own metadata separately. `--no-uses` skips dependency entries, not their data.

## Resolved configuration

Each overlay has separate **Configuration** and **Reconciliation** sections.
Configuration includes the source root, resolved target and its origin,
dependencies in composition order, directory mappings with resolved destinations,
materialization baseline and rule count, declared Git destinations, and install
and lifecycle-hook summaries. These describe intent even when targets are missing.

Use `--verbose` to expand dependency configuration, policy scopes, selectors,
permissions and excludes in cascade order. Scopes name their declaring descriptor;
mapping origins name their declaration directory. Install and Git summaries name
the latest contributing descriptor; individual merged settings can also inherit
from earlier scopes. Hooks are local to their overlay or repository, not inherited.

Unhealthy paths show their source or sidecar/declaration, owning overlay,
materialization intent, and responsible rule/scope where available. Healthy paths
are summarized by default and expanded with `--verbose`.

Scripts, consumer data values, package options, remote URLs and arbitrary Git
configuration values are omitted. Install sections and hooks are described by
presence/counts without executing commands or consulting external providers.

## Exit status and read-only behavior

- `0`: every selected overlay is fully applied, including its exclude blocks,
  or the repository has no overlays.
- `1`: any selected overlay needs attention or cannot be checked, including
  unknown names and invalid configuration. An `Ahead` checkout also needs attention.
- Invalid command-line arguments use the argument parser's error exit status (`2`).

Multiple selections receive individual reports and an overall summary, including
failed checks. Entry counts are summed per overlay report; a shared dependency
may therefore contribute to several reports. A failure in one overlay does not
prevent checking other discoverable overlays.

Status never applies, repairs, installs, runs hooks, fetches, or writes target
files, Git objects/index/config/excludes, or XDG state/lock files. Virtual-checkout
association metadata is read as a snapshot; older state schemas migrate only in
memory. `NO_COLOR` and redirected stdout produce readable output without ANSI
color escapes, even if color is otherwise forced.

Use [`over diff`](diff.md) for changes and content diffs; status explains the
resolved configuration and whether it is healthy.

## Reconciliation states

Each entry is classified into one of 8 statuses (referenced by `over diff`
and `over unapply` too):

| Status | Meaning |
|--------|---------|
| `Applied` | Target matches desired intent — nothing to do. |
| `Missing` | Target doesn't exist yet. |
| `Modified` | A checkout has local/staged changes, or a managed file's permissions need repair. |
| `Broken` | A dangling soft symlink, or a checkout path that isn't a valid git repo. |
| `Conflict` | Target exists and doesn't match desired intent, or a git checkout has a merge/rebase/cherry-pick in progress. |
| `Ahead` | A git checkout is ahead of its upstream. |
| `Behind` | A git checkout is behind its upstream. |
| `Diverged` | A git checkout has diverged from its upstream. |

Without `--verbose`, only entries other than `Applied` are printed; pass
`--verbose` to also print unchanged entries.

## Declared `git` repositories

A `git` entry declared at the overlay's own root (`git = "<url>"`, or a
`"."` key) is a real checkout: its status reflects the checkout's own
working-tree/upstream state (dirty, ahead, behind, diverged, merge in
progress), same as any other git repository you work in directly.

Any other `git` entry (declared at a subpath, e.g. a plugin manager's
clone) is a **declared, provisioned resource** instead: its status only
reflects whether it's present and its declared configuration
(`url`/`tag`/`rev`/`remotes`/`config`) is applied. Uncommitted changes,
local commits, untracked files, or an in-progress merge inside it never
make it `Modified`/`Ahead`/`Behind`/`Diverged`/`Conflict` — that content
isn't something `over` manages. If its declared configuration itself
changes (e.g. its `url`), it's reported `Conflict` until the next `over
apply` reconciles it. See
[ADR-021](../adr/021-declared-git-repositories-report-provisioning-not-content-status.md).

## Virtual checkouts (`mode = "checkout"`) {#virtual-checkouts-mode--checkout}

A subtree (or the whole overlay, via `mode = "checkout"`
or a `path = "."` rule) can be materialized as a **virtual
checkout**: ordinary, directly editable files at the target, with no
`.git` there at all, backed by the overlay's own source repository. See
[ADR-022](../adr/022-virtual-checkout-materialization.md) for how this
differs from a declared `git` repository above.

Its status is computed against the checkout's recorded base revision —
not by inspecting a real git working tree, since there isn't one at the
target:

| Status | Meaning |
|--------|---------|
| `Applied` | No local edits, and the source repository hasn't moved since the checkout was last materialized/synced/committed. |
| `Modified` | Local or staged changes exist, and the managed source content hasn't moved. Select changes with `over add`, then run `over commit`. |
| `Behind` | The source repository has new commits touching the managed path; nothing local to lose — `over sync` fast-forwards it automatically. |
| `Diverged` | Local edits and source-side changes coexist. `over sync` advances non-conflicting source paths and preserves independent local/index edits; then stage and commit the remaining edits. |
| `Conflict` | The *same* file changed both locally and in the source repository, to different content — resolve manually, then re-run. |
| `Missing` / `Broken` | Not materialized yet, or its recorded association is gone — run `over apply`. |

Actionable modified, diverged, or conflicting virtual checkouts also list specific
added/modified/deleted files underneath their aggregate status line.
Files are labeled `(staged)` or `(unstaged)`; a file edited again after
staging can appear in both lists. Mapped checkout units are inspected
independently, so changes confined to a mapped sibling do not make the
residual checkout behind.

## `.git/info/exclude` diagnostics

When an overlay materializes symlinks or checkouts into a target that's
itself inside a git repository, `over apply` keeps that repository's
`.git/info/exclude` in sync so its own `git status` doesn't list
overlay-managed paths as untracked (see [`over apply`](apply.md)). `over
status` reports the state of that block, one line per (repository,
overlay) pair, read-only — it never creates or writes to
`.git/info/exclude`:

| Status | Meaning |
|--------|---------|
| `exclude ok` | The block already matches what `over apply` would write — nothing to do. Only shown with `--verbose`. |
| `exclude missing` | Managed paths exist here, but no block has been written yet — run `over apply`. |
| `exclude modified` | The block exists but its content differs from what `over apply` would write (manual edit, or drift) — re-running `over apply` reconciles it. |
| `exclude orphaned` | The block exists but nothing is left to exclude in it (e.g. every managed path it used to cover is now tracked by git) — `over apply` will shrink or remove it. |
| `exclude malformed` | A begin/end marker mismatch for this overlay's block — never auto-repaired; fix it by hand. |

A managed path that's already tracked by the repository is reported as a
`tracked:` line underneath its group, independently of the block's own
status above — `over` never removes a tracked path from the index or
hides it from `git status`.
