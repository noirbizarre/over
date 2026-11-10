# `over unapply`

Remove a given overlay's own entries from the target: unlink files and
directories `over apply` created there, and remove owned checkouts subject to
their disposal safeguards. The overlay source itself is never touched.
Reapply reconstructs the current configured projection, not historical user data.

```text
Usage: over unapply [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to unapply (otherwise infer context, then use default_overlay)

Options:
  -H, --home <HOME>       Configuration and overlays root [env: OVER_HOME=]
  -r, --target <TARGET>   Set the target directory (defaults to ~) [env: OVER_TARGET=]
  -d, --debug             Toggle debug traces
  -n, --dry-run           Report what would be removed without removing anything
      --discard-checkout  Discard owned real checkouts, including local files and Git history
  -v, --verbose           Toggle verbose output
      --no-prompt         Leave risky checkouts untouched instead of prompting
  -h, --help              Print help
```

When `NAME` is omitted, [infer from context](selection.md), then use the
repository's configured `default_overlay`. Missing or ambiguous selection
requires an explicit name.

An explicit `NAME` retains the configured default's
[data context](../configuration/overlay.md#consumer-metadata-data) for path rendering.
Only the selected overlay's own entries are removed.

## Example

```sh
over unapply apps/git --dry-run
over unapply apps/git
```

Select the app itself to remove its files. Unapplying a host composition does
not recursively remove its shared app dependencies. See
[composition](../configuration/overlay.md#composition) and
[`over status`](status.md) before removing checkout entries with local changes.

## What it does

`unapply` does not use a historical apply log as removal authority.
It re-walks the overlay's current desired state and compares it,
read-only, against actual filesystem/git state using the reconciliation model
shared with `apply` and `status`. `diff` shares desired-state selection but
presents Git changes independently. Virtual checkouts also use their persisted
XDG association, verified against the current overlay, source and scope before
removal. `unapply` only
removes an entry that *still* matches exactly what the overlay would
materialize there right now:

- A file or directory symlink is removed only if it still points at the
  overlay's source. Anything else there (a real file, a symlink pointing
  elsewhere, a plain directory where a symlink was expected) is left
  untouched and reported.
- A directory is removed only if it's already empty (a non-recursive
  removal) — never `rm -rf`. A directory that still holds anything else
  (unrelated user files, another overlay's content) is retained and reported
  as such.
- A real Git checkout (root or nested `git` entry) requires explicit disposal
  confirmation, default No, or `--discard-checkout`. This can discard local
  edits, ignored files, branches, stashes and Git metadata/history. Even clean
  status does not prove those are backed up. With `--no-prompt` alone it is
  retained and reported. Foreign identities, independent checkout units and
  shared/external Git layouts cannot be overridden.
- A [virtual checkout](../adr/022-virtual-checkout-materialization.md) is
  removed only when it's fully applied: no local edits, and not behind the
  source repository. Anything else is left untouched — resolve with `over
  commit`/`over sync` first. Its recorded XDG association is dropped
  alongside it, so a virtual checkout later re-materialized at the same
  path starts fresh rather than being misread against stale history.
  For mapped layouts, only owned files and empty directories are removed;
  independently mapped destinations are protected. A dirty mapped checkout
  survives removal of a clean residual checkout around it.
  When protected contents keep the container directory in place, the report
  says the projection was removed and the directory retained. This counts as
  one successfully removed projection, not removal of the directory itself.

Removal proceeds deepest-target-first, so a directory that only ever held
overlay-managed content becomes empty (and is then removed) once its
contents are gone.

## What it never does

- Recurse into `uses`. Only the named overlay's own entries are
  considered — unapplying a shared dependency could otherwise break
  another overlay that still `uses` it. Unapply a used overlay explicitly
  by name if that's what you want.
- Remove anything not recognizably its own: a foreign file, a changed
  symlink target, or a non-empty directory are always left alone, with no
  `--force` escape hatch (there's nothing to force through).
- Discard real-checkout data/history without separate explicit confirmation
  or `--discard-checkout`, or discard dirty virtual-checkout content at all.

There is no general backup or rollback. Symlinks and configured directories
can be recreated with apply, but discarded checkout-local data/history,
previous overwrite/absorb content, packages and hook effects are not restored.
See the [operation inventory and recovery limitations](safety.md).

See
[ADR-015](../adr/015-unapply-reuses-plan-classification-and-only-removes-safely-reversible-state.md)
for the full design rationale.
