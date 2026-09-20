# `over log`

Show the git history behind a
[virtual checkout](../adr/022-virtual-checkout-materialization.md)-materialized
overlay — including commits made via [`over commit`](commit.md) — without
needing to `cd` into the overlay's source repository and run `git log`
yourself.

```text
Usage: over log [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay whose history to show (guessed from the current directory if omitted, falling back to the whole repository's history)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -n, --limit <LIMIT>  Maximum number of commits to show [default: 20]
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

## Scope

- With `NAME`: shows commits touching that overlay's managed path,
  restricted to it exactly like `git log -- <path>` would (a commit
  that only changed unrelated files elsewhere in the same repository is
  skipped). Errors if the named overlay isn't checkout-materialized.
- Without `NAME`: guesses the overlay from the current directory (the
  most specific virtual checkout containing it); if none matches, falls
  back to the whole source repository's history, unfiltered.

Every commit is read directly via `git2` — no shelling out to `git`.

See [ADR-022](../adr/022-virtual-checkout-materialization.md) for the full
design rationale.
