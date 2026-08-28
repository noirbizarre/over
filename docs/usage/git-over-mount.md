# `git over mount`

Mount the current git repository to an overlay: record the association in
the repository's `.git/config` (`over.overlay`), so later `git over add`/
`git over status` calls in this repository know which overlay to use.

```
Usage: git-over mount [OPTIONS]

Options:
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME]
  -o, --overlay <OVERLAY>  Name of the target overlay
  -d, --debug              Toggle debug traces
  -v, --verbose            Toggle verbose output
  -h, --help               Print help
```
