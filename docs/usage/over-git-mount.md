# `over git mount`

The repository path is reverse-routed through configured directory
mappings before being recorded in the overlay's logical namespace. Its
destination can be outside the overlay's base target.

Mount the current git repository to an overlay: record the association in
the repository's `.git/config` (`over.overlay`), so later `over git add`/
`over git status` calls in this repository know which overlay to use.

```text
Usage: over git mount [OPTIONS]

Options:
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME]
  -o, --overlay <OVERLAY>  Name of the target overlay
  -d, --debug              Toggle debug traces
  -v, --verbose            Toggle verbose output
  -h, --help               Print help
```
