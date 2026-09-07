# `over apply`

Apply a given overlay: optionally install packages first (`--install`), then
clone any declared git repositories and symlink its files into the target
root.

```
Usage: over apply [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to apply (uses default_overlay if configured)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -d, --debug        Toggle debug traces
  -n, --dry-run      Run without applying changes
  -f, --force        Overwrite without prompting
  -v, --verbose      Toggle verbose output
      --no-prompt    Fail on conflict instead of prompting
      --no-uses      Do not process uses
  -i, --install      Install associated applications
  -h, --help         Print help
```

- When `NAME` is omitted, the repository's configured `default_overlay` (if
  any) is used instead of prompting interactively — see
  [Default Overlay Selection](../configuration.md#default-overlay-selection).
- `--no-uses` applies only the named overlay, skipping every overlay it
  `uses`.
- `--install` additionally runs the `install` section (see
  [Install Configuration](../install-config.md)); without it, `apply` only
  links files.
- A file that already exists at the target triggers an interactive prompt
  (skip, overwrite, absorb, diff) unless `--force` or `--no-prompt` is given.
- `--dry-run`/`--verbose` print the structured reconciliation plan (one line
  per entry: create/no-op/conflict) before executing it, rather than just
  reporting success or failure. See
  [ADR-011](../adr/011-plan-then-execute-reconciliation.md).
