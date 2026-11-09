# `over lint`

Check overlays for configuration issues, without touching any target
directory.

```text
Usage: over lint [OPTIONS] [OVERLAY]

Arguments:
  [OVERLAY]  Check only this overlay and its uses dependencies

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
      --no-uses      Skip linting the selected overlay's uses dependencies
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

Without an overlay name, lint checks the whole repository. Supply a name to
check only that overlay and its transitive `uses` dependencies. Add `--no-uses`
to skip dependency contents and dependency-spanning cycle checks; direct
references to missing overlays are still reported. With no name, `--no-uses`
has no effect because every overlay is checked independently.
Relevant inherited and repository-root configuration is still validated when
linting a named overlay, including with `--no-uses`.

```sh
over lint
over lint hosts/laptop
over lint hosts/laptop --no-uses
```

Unlike `apply --dry-run`, `lint` never deserializes a descriptor blindly: it
pre-validates the raw keys first, so a typo (e.g. `targt` instead of
`target`) is reported as "did you mean `target`?" rather than as a raw
`serde` error. It also continues past a broken overlay to report on anything
that `uses` it, and flags cycles in the `uses` graph. See
[ADR-009](../adr/009-lint-is-a-separate-tolerant-pass.md).
