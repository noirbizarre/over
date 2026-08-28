# `over apply`

Apply a given overlay: symlink its files into the target root, clone any
declared git repositories, and optionally install packages.

```
Usage: over apply [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to apply

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

- `--no-uses` applies only the named overlay, skipping every overlay it
  `uses`.
- `--install` additionally runs the `install` section (see
  [Install Configuration](../install-config.md)); without it, `apply` only
  links files.
- A file that already exists at the target triggers an interactive prompt
  (skip, overwrite, absorb, diff) unless `--force` or `--no-prompt` is given.
