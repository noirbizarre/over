# `over git status`

Show overlay status for the current git repository.

Repository paths honor the overlay's target rules. Source directories
mapped elsewhere are not listed as unapplied in the current repository.

```text
Usage: over git status [OPTIONS]

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

## Example

```sh
over git status
```

Run inside a repository previously associated with an overlay through
[`over git mount`](over-git-mount.md). This command requires the `over.overlay`
Git setting; it does not prompt for a missing association. For full overlay
configuration and reconciliation health, use [`over status NAME`](status.md).
