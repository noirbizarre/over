# Usage

`over` is the standalone CLI. Its `git` subcommand group (`over git
mount`/`add`/`status`) integrates the same overlay model with git
workflows.

## `over`

| Command | Description |
|---|---|
| [`over add`](add.md) | Add files or directories to an overlay |
| [`over new`](new.md) | Create a new overlay |
| [`over list`](list.md) (`ls`) | List known overlays |
| [`over show`](show.md) | Display details about an overlay |
| [`over apply`](apply.md) | Apply a given overlay |
| [`over lint`](lint.md) | Check overlays for configuration issues |
| [`over doctor`](doctor.md) | Check the environment and overlays for issues |
| [`over completion`](completion.md) | Generate shell completion scripts |
| [`over status`](status.md) | Get the current repository/directory overlays status |
| [`over diff`](diff.md) | Show differences between desired and actual overlay state |
| [`over sync`](sync.md) | Reconcile a checkout-materialized overlay with its git source |
| [`over unapply`](unapply.md) | Remove a given overlay's own entries from the target |
| [`over commit`](commit.md) | Commit local changes in a checkout-materialized overlay back to its source repository |
| [`over log`](log.md) | Show the git history behind a checkout-materialized overlay |

## `over git`

| Command | Description |
|---|---|
| [`over git mount`](over-git-mount.md) | Mount the current git repository to an overlay |
| [`over git add`](over-git-add.md) | Add files from the current git repository to an overlay |
| [`over git status`](over-git-status.md) | Show overlay status for the current git repository |

Every `over` subcommand, including the `git` group, accepts `-H`/`--home`
(or the `OVER_HOME` environment variable) to point at the overlays root, and
`-d`/`--debug`, `-v`/`--verbose` for diagnostics.
