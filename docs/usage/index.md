# Usage

`over` is the standalone CLI. `git-over` is a companion binary that
integrates the same overlay model with git workflows — Git resolves
`git over <command>` to it because it is named exactly `git-over` and sits on
`PATH`.

## `over`

| Command | Description |
|---|---|
| [`over add`](add.md) | Add files or directories to an overlay |
| [`over new`](new.md) | Create a new overlay |
| [`over list`](list.md) (`ls`) | List known overlays |
| [`over show`](show.md) | Display details about an overlay |
| [`over apply`](apply.md) | Apply a given overlay |
| [`over lint`](lint.md) | Check overlays for configuration issues |
| [`over completion`](completion.md) | Generate shell completion scripts |
| [`over status`](status.md) | Get the current repository/directory overlays status |

## `git-over`

| Command | Description |
|---|---|
| [`git over mount`](git-over-mount.md) | Mount the current git repository to an overlay |
| [`git over add`](git-over-add.md) | Add files from the current git repository to an overlay |
| [`git over status`](git-over-status.md) | Show overlay status for the current git repository |

Every `over` and `git-over` subcommand accepts `-H`/`--home` (or the
`OVER_HOME` environment variable) to point at the overlays root, and
`-d`/`--debug`, `-v`/`--verbose` for diagnostics.
