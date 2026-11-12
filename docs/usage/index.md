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
| [`over crypt`, `over unlock`, `over lock`](crypt.md) | Manage transparent repository encryption |
| [`over user`, `over key`](crypt.md#users-and-keys) | Manage logical users, Age keys, and rotation |
| [`over secret`](secrets.md) | Manage the repository-key-protected native secret store |
| [`over completion`](completion.md) | Generate shell completion scripts |

## Apply and inspect

| Command | When to use it |
| --- | --- |
| [`over apply`](apply.md) | Apply a given overlay |
| [`over lint`](lint.md) | Check overlays for configuration issues |
| [`over doctor`](doctor.md) | Check the environment and overlays for issues |
| [`over status`](status.md) | Inspect reconciliation health and staged/unstaged Git changes; `--short` prints file rows |
| [`over show`](show.md) | Compatibility alias for `over status` |
| [`over diff`](diff.md) | Show Git changes for managed overlay paths, with staged and difftool views |
| [`over unapply`](unapply.md) | Remove a given overlay's own entries from the target |

## Reconcile editable checkouts

| Command | When to use it |
| --- | --- |
| [`over sync`](sync.md) | Reconcile virtual checkouts locally, optionally pulling/pushing the Over source repository |
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

Long-running commands show a spinner on terminal stderr. Progress pauses for
prompts, hooks, and external tools; reports and warnings remain above it.
Redirected stderr contains no progress animation, and JSON reports stay on stdout.

Human-readable path labels and diagnostics abbreviate your user home directory
as `~` across commands, including when output is redirected. Paths outside your
home stay unchanged. This is only a display convention: JSON values, file and
patch contents, generated configuration, secrets, and external-tool output keep
their original values. The displayed `~` refers to your user home, independently
of the repository and target options below.

Every `over` subcommand, including the `git` group, accepts `-H`/`--home`
(or the `OVER_HOME` environment variable) to point at the overlays root, and
`-d`/`--debug`, `-v`/`--verbose` for diagnostics.

The default repository location is `~/.dotfiles`. Resolution priority is
`--home`, then `OVER_HOME`, then this default. Existing repositories previously
stored at `~/.over` are not moved automatically: set `OVER_HOME=~/.over` or use
`--home ~/.over` to continue using them. `over init` without a source validates
the resolved location, regardless of the current working directory.

[Crypt and access commands](crypt.md) are an exception: without `--home` or
`OVER_HOME`, they discover the current Git repository, falling back to the usual
Over home for virtual destinations. [`over secret`](secrets.md) instead uses the
home-based native store selection regardless of the current Git repository.

Commands that support `-r`/`--target` use it as the destination meaning of `~`.
Resolution priority is `--target`, then `OVER_TARGET`, then your home directory.
This does not select the source repository. Consult each page's syntax for supported flags.

An explicit overlay name selects that overlay. Otherwise commands first try
[contextual overlay selection](selection.md): Git associations, source context, destination ownership,
managed siblings, then target containment. Context precedes a configured default;
ambiguous contexts require an explicit overlay. Status/diff/sync retain their
all-overlay fallback outside an inferable/default context (`--all` forces all).
Lint and doctor retain their broader diagnostic scopes. See
[default selection](../configuration/root.md#default-overlay-selection).

For concepts and your first successful apply, start with
[Getting started](../index.md#getting-started). For supported modes and the
checkout lifecycle, see [Materialization](../configuration/materialization.md).
