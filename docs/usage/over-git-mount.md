# `over git mount`

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

## Example

Run from an existing repository within this overlay's target or mapped destination:

```sh
over git mount --overlay workspaces/demo
over git status
```

The repository path is reverse-routed through configured directory mappings
before being recorded in the overlay's logical namespace. Its destination can
be outside the base target. Mount offers to export selected Git properties and
stores `over.overlay` for later commands. See the [Git group overview](git.md)
and [`over git add`](over-git-add.md).
