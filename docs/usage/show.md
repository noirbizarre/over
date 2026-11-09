# `over show` (compatibility alias)

`over show` is a compatibility alias for [`over status`](status.md). Prefer
`over status` in new workflows. Both names use one implementation and report
resolved configuration together with reconciliation health, including before
the first apply.

```sh
over show desktop
over show --all
over show desktop --verbose
```

Selection, `--root`, `--no-uses`, `--all`, verbosity, read-only behavior, and exit
statuses are identical. In particular, `show` now returns `1` for missing,
unhealthy, or uncheckable overlays; it is no longer a configuration-only command.
