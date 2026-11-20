# `over show` (compatibility alias)

`over show` is a compatibility alias for [`over status`](status.md). Prefer
`over status` in new workflows. Both names use one implementation and report
resolved configuration, reconciliation health and a final Git Changes section, including before
the first apply.

```sh
over show desktop
over show --all
over show desktop --verbose
over show desktop --short
```

Selection, `--target`, `OVER_TARGET`, `--no-uses`, `--all`, `--short`, verbosity, read-only behavior, and exit
statuses are identical. Successfully inspected overlays return `0` even when
missing or unhealthy; inspection and output failures return `1`. Like `status`,
`show` reports deployment health as well as configuration.
