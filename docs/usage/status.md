# `over status`

Get the current repository/directory overlays status.

```text
Usage: over status [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to check (default_overlay if configured, all overlays otherwise)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -d, --debug        Toggle debug traces
      --no-uses      Do not process uses
  -a, --all          Check every overlay, ignoring any configured default_overlay
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

When `NAME` is omitted, a configured `default_overlay` (see
[Default Overlay Selection](../configuration.md#default-overlay-selection))
narrows the report to just that overlay; pass `--all` to report on every
overlay regardless.

Each entry is classified into one of 8 statuses (referenced by `over diff`
and `over unapply` too):

| Status | Meaning |
|--------|---------|
| `Applied` | Target matches desired intent — nothing to do. |
| `Missing` | Target doesn't exist yet. |
| `Modified` | A git checkout has uncommitted changes. |
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

## Virtual checkouts (`materialization = "checkout"`)

A subtree (or the whole overlay, via `defaults.materialization =
"checkout"` or a `path = "."` rule) can be materialized as a **virtual
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
| `Modified` | One or more files were edited/added/deleted locally, and the source repository hasn't moved. Run `over commit`. |
| `Behind` | The source repository has new commits touching the managed path; nothing local to lose — `over sync` fast-forwards it automatically. |
| `Diverged` | Local edits *and* unrelated source-side changes, to different files — resolved by `over commit` (local) then `over sync` (source), in either order. |
| `Conflict` | The *same* file changed both locally and in the source repository, to different content — resolve manually, then re-run. |
| `Missing` / `Broken` | Not materialized yet, or its recorded association is gone — run `over apply`. |

`--verbose` also lists the specific added/modified/deleted files
underneath a virtual checkout's aggregate status line.

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
