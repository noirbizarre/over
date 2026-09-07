# `over status`

Get the current repository/directory overlays status.

```
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
