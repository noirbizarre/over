# Commands

`over` is the standalone CLI. Its `git` subcommand group (`over git
mount`/`add`/`status`) integrates the same overlay model with git
workflows.

## Set up and select overlays

| Command | Description |
|---|---|
| [`over init`](init.md) | Clone or validate an Over repository and configure its canonical origin |
| [`over add`](add.md) | Add files or directories to an overlay |
| [`over new`](new.md) | Create a new overlay |
| [`over list`](list.md) (`ls`) | List known overlays |
| [`over data`](data.md) | Export resolved overlay data as JSON |
| [`over secret`](secrets.md) | Manage repository-local Age-encrypted secrets |
| [`over completion`](completion.md) | Generate shell completion scripts |

## Apply and inspect

| Command | When to use it |
| --- | --- |
| [`over apply`](apply.md) | Apply a given overlay |
| [`over lint`](lint.md) | Check overlays for configuration issues |
| [`over doctor`](doctor.md) | Check the environment and overlays for issues |
| [`over status`](status.md) | Inspect resolved configuration and reconciliation health |
| [`over show`](show.md) | Compatibility alias for `over status` |
| [`over diff`](diff.md) | Show Git changes for managed overlay paths, with staged and difftool views |
| [`over unapply`](unapply.md) | Remove a given overlay's own entries from the target |

## Reconcile editable checkouts

| Command | When to use it |
| --- | --- |
| [`over sync`](sync.md) | Reconcile a checkout-materialized overlay with its git source |
| [`over commit`](commit.md) | Record staged checkout changes in the source repository |
| [`over restore`](restore.md) | Discard selected changes, unstage files, or recover historical content |
| [`over log`](log.md) | Show the git history behind a checkout-materialized overlay |

## `over git`

See the [Git group overview](git.md) for host-repository discovery and association.

| Command | Description |
|---|---|
| [`over git mount`](over-git-mount.md) | Mount the current git repository to an overlay |
| [`over git add`](over-git-add.md) | Add files from the current git repository to an overlay |
| [`over git status`](over-git-status.md) | Show overlay status for the current git repository |

## Shared options and selection

Every `over` subcommand, including the `git` group, accepts `-H`/`--home`
(or the `OVER_HOME` environment variable) to point at the overlays root, and
`-d`/`--debug`, `-v`/`--verbose` for diagnostics.

The default repository location is `~/.dotfiles`. Resolution priority is
`--home`, then `OVER_HOME`, then this default. Existing repositories previously
stored at `~/.over` are not moved automatically: set `OVER_HOME=~/.over` or use
`--home ~/.over` to continue using them. `over init` without a source validates
the resolved location, regardless of the current working directory.

Commands that support `--root` use it as the destination meaning of `~`; it does
not select the source repository. Consult each page's syntax for supported flags.

An explicit overlay name selects that overlay. Omitted-name behavior differs:
apply/unapply/commit use the configured default or prompt; status/diff/sync use
the default or all overlays (`--all` forces all). Data requires a name or default;
restore uses an explicit `--overlay`, the default, or inferred path ownership;
lint and doctor have broader diagnostic scopes. See
[default selection](../configuration/root.md#default-overlay-selection).

For concepts and your first successful apply, start with
[Getting started](../index.md#getting-started). For supported modes and the
checkout lifecycle, see [Materialization](../configuration/materialization.md).
