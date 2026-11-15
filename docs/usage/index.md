# Getting started

Over's normal lifecycle is **set up → preview → apply → inspect → edit and
record → sync → unapply**. Start with the [first-overlay walkthrough](../index.md#getting-started)
for installation and a complete example in temporary directories.

## Set up a source repository

Already have an Over repository? [`over init`](../commands/init.md) clones or
validates it. Initialization does not apply files. For a new setup, create a
Git repository and [discover overlays](../configuration/root.md#overlay-discovery)
or use [`over new`](../commands/new.md). Commit source files before initializing
a checkout: its starting content comes from committed source `HEAD`.

## Preview, apply, and inspect

Select an overlay explicitly while learning:

```sh
over apply hosts/laptop --dry-run
over apply hosts/laptop
over status hosts/laptop
```

[`--home` and `--target`](../commands/index.md#shared-options-and-selection)
select the source repository and destination. Review the preview before applying;
see [selection](selection.md), [materialization](../configuration/materialization.md),
and [safety and recovery](safety.md) for detailed behavior.

## Edit and record changes

With default source symlinks, editing a deployed file edits its source directly:
review and commit through Git. With ordinary editable checkouts, stage destination
edits through Over before recording them:

```sh
over diff hosts/laptop
over add --overlay hosts/laptop ~/.config/example/config
over diff hosts/laptop --staged
over commit --overlay hosts/laptop -m 'Update configuration'
over sync hosts/laptop
```

The paths above are illustrative: use a file actually managed by your overlay.
Follow the [checkout example](../examples.md#edit-ordinary-files-with-checkout)
for a complete setup. Generated/template files have additional
[reverse-patching rules](../configuration/materialization.md#editing-and-lifecycle).
[`over sync`](../commands/sync.md) reconciles checkout changes; source pull/push
are explicit options or configured policy.

## Reverse and extend the setup

[`over unapply`](../commands/unapply.md) removes an overlay's own safely reversible
entries, retaining local edits and independently applied dependencies. Review
status before reversing a deployment.

Next, [compose reusable overlays](../examples.md),
[protect repository files](crypt.md), [manage secrets](secrets.md),
[lock and unlock](locking.md), or [use a Dev Container](../devcontainer.md).
Use [Configuration](../configuration.md) for descriptor fields and
[Commands](../commands/index.md) for exact CLI contracts.

<!-- Preserve published fragments from the former command overview. -->
<span id="commands"></span>
<span id="shared-options-and-selection"></span>
<span id="set-up-and-select-overlays"></span>
<span id="apply-and-inspect"></span>
<span id="reconcile-editable-checkouts"></span>
<span id="over-git"></span>

Looking for the former command overview? Its tables and
[shared options](../commands/index.md#shared-options-and-selection) now live in
[Commands](../commands/index.md), including the [Git group](../commands/git.md).
