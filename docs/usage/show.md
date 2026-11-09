# `over show`

Display details about an overlay.

Includes configured directory mappings as source-prefix/target pairs, along
with the base target, scalar `mode` and `permissions` baselines, and shared
rules in cascade order. Each policy scope names its declaring descriptor so
inherited selectors can be interpreted relative to the correct root.

```text
Usage: over show [OPTIONS] <NAME>

Arguments:
  <NAME>  Name of the overlay to display

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```
