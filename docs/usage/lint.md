# `over lint`

Check overlays for configuration issues, without touching any target
directory.

```
Usage: over lint [OPTIONS]

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

Unlike `apply --dry-run`, `lint` never deserializes a descriptor blindly: it
pre-validates the raw keys first, so a typo (e.g. `targt` instead of
`target`) is reported as "did you mean `target`?" rather than as a raw
`serde` error. It also continues past a broken overlay to report on anything
that `uses` it, and flags cycles in the `uses` graph. See
[ADR-009](../adr/009-lint-is-a-separate-tolerant-pass.md).
