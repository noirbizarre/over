# `over list`

List known overlays. Aliased as `over ls`.

```
Usage: over list [OPTIONS]

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -t, --tree         Display as tree
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

`--tree` (`-t`) groups overlays by their directory hierarchy:

```
.over
├── git
├── shell
│   ├── bash
│   └── zsh
└── vim
```
