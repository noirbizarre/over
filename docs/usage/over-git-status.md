# `over git status`

Show overlay status for the current git repository.

Repository paths honor the overlay's directory mappings. Source directories
mapped elsewhere are not listed as unapplied in the current repository.

```text
Usage: over git status [OPTIONS]

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```
