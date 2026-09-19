# `over git add`

Add files from the current git repository to an overlay — the `over git`
equivalent of [`over add`](add.md), resolved from inside the repository
mounted with [`over git mount`](over-git-mount.md).

```
Usage: over git add [OPTIONS] <FILES>...

Arguments:
  <FILES>...  Files, directories, or glob patterns to add

Options:
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME]
  -o, --overlay <OVERLAY>  Name of the target overlay
  -d, --debug              Toggle debug traces
  -n, --dry-run            Run without applying changes
  -f, --force              Overwrite without prompting
  -v, --verbose            Toggle verbose output
  -h, --help               Print help
```
