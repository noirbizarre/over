# `over unapply`

Remove a given overlay's own entries from the target: unlink files and
directories `over apply` created there, and remove a fully-synced git
checkout. The overlay source itself is never touched, so a plain `over
apply` afterwards fully restores everything.

```
Usage: over unapply [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to unapply (uses default_overlay if configured)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -d, --debug        Toggle debug traces
  -n, --dry-run      Report what would be removed without removing anything
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

When `NAME` is omitted, the repository's configured `default_overlay` (if
any) is used instead of prompting interactively — see
[Default Overlay Selection](../configuration.md#default-overlay-selection).

## What it does

`unapply` never trusts a persisted record of what a previous `apply` did.
Instead it re-walks the overlay's current desired state and compares it,
read-only, against actual filesystem/git state — the same
`DesiredTree`/`Plan` model `apply`/`status`/`diff` already share — and only
removes an entry that *still* matches exactly what the overlay would
materialize there right now:

- A file or directory symlink is removed only if it still points at the
  overlay's source. Anything else there (a real file, a symlink pointing
  elsewhere, a plain directory where a symlink was expected) is left
  untouched and reported.
- A directory is removed only if it's already empty (a non-recursive
  removal) — never `rm -rf`. A directory that still holds anything else
  (unrelated user files, another overlay's content) is silently left in
  place.
- A git checkout (the overlay's own root checkout, or any nested `git`
  entry) is removed only when it's fully in sync with its upstream: no
  uncommitted changes, no unpushed commits, no merge/rebase in progress.
  Anything else is left untouched — resolve with `over sync` or plain git
  first.

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
- Discard uncommitted changes or local commits in a git checkout —
  reusing the exact same read-only inspection `over sync`/`over status`
  rely on (see
  [ADR-014](../adr/014-bidirectional-checkout-synchronization.md)).

Because unapply only ever removes exactly-matching, safe state, every
removal is reversible: symlinks and directories come back with a plain
`over apply`, and a checkout is only ever removed when there was nothing
local-only left to lose.

See
[ADR-015](../adr/015-unapply-reuses-plan-classification-and-only-removes-safely-reversible-state.md)
for the full design rationale.
