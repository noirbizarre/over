# `git over add`

Add files from the current git repository to an overlay — the `git-over`
equivalent of [`over add`](add.md), resolved from inside the repository
mounted with [`git over mount`](git-over-mount.md).

```
Usage: git-over add [OPTIONS] <FILES>...

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
