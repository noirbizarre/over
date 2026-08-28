# `over add`

Add files or directories to an overlay.

```
Usage: over add [OPTIONS] <FILES>...

Arguments:
  <FILES>...  Files, directories, or glob patterns to add

Options:
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME]
  -o, --overlay <OVERLAY>  Name of the target overlay
  -d, --debug              Toggle debug traces
  -r, --root <ROOT>        The target root directory (~)
  -n, --dry-run            Run without applying changes
  -v, --verbose            Toggle verbose output
  -f, --force              Overwrite without prompting
  -h, --help               Print help
```

Files are moved into the overlay and replaced in place by a symlink pointing
back at the overlay copy — the same mechanism `over apply` uses in reverse.
