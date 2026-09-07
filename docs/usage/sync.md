# `over sync`

Reconcile a checkout-materialized overlay with its git source: pull
upstream changes into the checkout, and push local commits made directly in
the checkout back upstream.

```
Usage: over sync [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to sync (default_overlay if configured, all overlays otherwise)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -d, --debug        Toggle debug traces
      --no-uses      Do not process uses
  -a, --all          Sync every overlay, ignoring any configured default_overlay
  -v, --verbose      Toggle verbose output
      --pull-only    Only pull upstream changes into the checkout
      --push-only    Only push local commits to the overlay source
      --continue     Re-attempt a sync after resolving conflicts manually
      --abort        Abort an in-progress merge left by a conflicted sync
  -n, --dry-run      Report what would happen without syncing
  -h, --help         Print help
```

When `NAME` is omitted, a configured `default_overlay` (see
[Default Overlay Selection](../configuration.md#default-overlay-selection))
narrows the sync to just that overlay; pass `--all` to sync every overlay
regardless.

## Scope: the overlay's root git entry only

An overlay's `git` map can declare two conceptually different things:

- the overlay's **own** content, cloned to its target root (the `"."`
  entry, or the shorthand single-URL/single-object form) — this is "the
  overlay itself materialized as a checkout", and the only thing `over
  sync` ever touches.
- **nested repositories declared inside the overlay** (any other path key,
  e.g. a plugin-manager checkout at `.config/nvim/pack/plugins/foo`) — a
  separate concern. `over apply` ensures these exist and are configured as
  declared, but their content/history is never synchronized by `over
  sync`. Manage them with plain git yourself.

## What it does

For each overlay's root checkout (and, for a `worktree`/`worktrees`
bare-repo config, for every worktree directory that exists on disk —
synced independently):

1. Refuses to touch a checkout with uncommitted changes. Commit or stash
   first.
2. Fetches `origin` and fast-forwards or three-way-merges the current
   branch with its upstream — real git merge machinery, not a custom
   algorithm. A conflicting merge leaves real conflict markers in the
   working tree and stops there (no push): resolve with ordinary `git add`
   / `git commit`, then re-run `over sync`, or run `over sync --abort` to
   discard the in-progress merge (`git merge --abort` equivalent — your
   own commits are never touched, only the failed merge attempt).
3. Pushes the branch to `origin` if it ended up ahead.

`--pull-only`/`--push-only` restrict this to one direction. `--dry-run`
never performs any network I/O or mutation; it reports what's already
known locally.

## What it never does

- Touch a dirty working tree, even partially.
- Auto-resolve conflicts, rebase, or invent its own merge algorithm — git's
  own merge/rebase/conflict tooling is authoritative.
- Synchronize nested (non-root) `git` entries (see above).
- Discard local commits: `--abort` only resets an *in-progress, uncommitted*
  merge back to the pre-merge `HEAD`.

`over sync` persists a small, informational-only checkpoint under
`$XDG_STATE_HOME/over` (overlay/checkout association, last synced commit
and outcome). It is never read back to decide correctness — git's own
repository state remains authoritative — so deleting it is always
harmless.

See [ADR-014](../adr/014-bidirectional-checkout-synchronization.md) and
[the roadmap](https://github.com/noirbizarre/over/issues/112) for the full
design rationale.
