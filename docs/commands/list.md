# `over list`

List known overlays. Aliased as `over ls`.

```text
Usage: over list [OPTIONS]

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -t, --tree         Display as tree
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

`--tree` (`-t`) groups overlays by their directory hierarchy:

```text
.dotfiles
├── git
├── shell
│   ├── bash
│   └── zsh
└── vim
```

```sh
over list --tree
```

Discovery combines descriptors and root-declared directories, preferring
more-specific overlays to ancestors. See
[Overlay discovery](../configuration/root.md#overlay-discovery) to include
directories without local descriptors, and [`over status`](status.md) to inspect
whether discovered overlays are applied.
