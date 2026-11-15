# `over new`

Create a new overlay.

```text
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
see [Format preference](../configuration/root.md#format-preference).

## Example

```sh
over new workspaces/demo '~/Workspaces/demo'
```

Use an empty or nonexistent destination to start with a new overlay. If the
destination already contains files or repositories, new offers to absorb selected
entries. To preview descriptor creation and ingestion, add `--dry-run`.
Creating a descriptor is distinct from applying its future contents; see
[`over apply`](apply.md) and [Examples](../examples.md).
