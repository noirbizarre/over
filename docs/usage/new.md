# `over new`

Create a new overlay.

```
Usage: over new [OPTIONS] [PATH] [TARGET]

Arguments:
  [PATH]    Overlay path within the repository (e.g. apps/myapp)
  [TARGET]  Target directory for the overlay

Options:
  -f, --format <FORMAT>  Overlay descriptor format [possible values: toml, yaml]
  -H, --home <HOME>      Configuration and overlays root [env: OVER_HOME]
  -d, --debug            Toggle debug traces
  -n, --dry-run          Run without applying changes
      --force            Overwrite without prompting
  -v, --verbose          Toggle verbose output
  -h, --help             Print help
```

`--format` picks the descriptor's extension for this one overlay. Absent a
flag, the root config's `format` field decides, and absent that, `toml` —
see [Configuration](../configuration.md#format-preference).
