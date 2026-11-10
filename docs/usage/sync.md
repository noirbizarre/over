# `over sync`

Reconcile editable checkouts with their source. Root declared Git repositories
can fetch, merge, and push upstream; virtual checkouts reconcile with the overlay
source repository locally, without network I/O.

```text
Usage: over sync [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to sync (default_overlay if configured, all overlays otherwise)

Options:
  -H, --home <HOME>      Configuration and overlays root [env: OVER_HOME=]
  -r, --target <TARGET>  Set the target directory (defaults to ~) [env: OVER_TARGET=]
  -d, --debug            Toggle debug traces
      --no-uses          Skip dependency operations, retaining dependency data
  -a, --all              Sync every overlay, retaining configured default_overlay data
  -v, --verbose          Toggle verbose output
      --pull-only        Only pull upstream changes into the checkout
      --push-only        Only push local commits to the overlay source
      --continue         Re-attempt a sync after resolving conflicts manually (alias for a plain re-run)
      --abort            Abort an in-progress merge, discarding uncommitted tracked/index changes
  -n, --dry-run          Report what would happen without syncing
      --no-prompt        Never prompt to commit a virtual checkout's local changes; report them as blocked instead
  -h, --help             Print help
```

When `NAME` is omitted, a configured `default_overlay` (see
[Default overlay selection](../configuration/root.md#default-overlay-selection))
narrows the sync to just that overlay; pass `--all` to sync every overlay
regardless.

Explicit `NAME` and `--all` retain the configured default's
[data context](../configuration/overlay.md#consumer-metadata-data). Each target resolves
its own metadata separately. `--no-uses` skips dependency operations, not their data.

## Example

```sh
over sync workspace/client --dry-run
over sync workspace/client --pull-only
```

Use a preview to inspect reconciliation first. For a virtual checkout, stage and
commit target edits with [`over add`](add.md) and [`over commit`](commit.md)
before syncing when they block source updates. See
[Materialization](../configuration/materialization.md#editing-and-lifecycle)
for the complete local editing workflow.

## Declared Git scope: root entries only

An overlay's `git` map can declare two conceptually different things:

- the overlay's **own** content, cloned to its target root (the `"."`
  entry, or the shorthand single-URL/single-object form) — this is "the
  overlay itself materialized as a checkout", and the declared-Git entry that
  `over sync` handles. Virtual checkouts are handled separately below.
- **nested repositories declared inside the overlay** (any other path key,
  e.g. a plugin-manager checkout at `.config/nvim/pack/plugins/foo`) — a
  separate concern. `over apply` ensures these exist and are configured as
  declared, but their content/history is never synchronized by `over
  sync`. Manage them with plain git yourself.

### What declared Git sync does

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
   discard the in-progress merge. Abort hard-resets tracked working files and
   the index to HEAD, including staged resolutions and edits made after the
   conflict. It is not Git's more protective `merge --abort`. Other Git
   operation states are refused; existing HEAD commits are not removed.
3. Pushes the branch to `origin` if it ended up ahead.

`--pull-only`/`--push-only` restrict this to one direction. `--dry-run`
never performs any network I/O or mutation; it reports what's already
known locally.
Previews distinguish prospective merge/fast-forward and list candidate paths;
a later fetch may reveal different upstream changes. For divergent branches,
incoming candidates compare the merge base to cached upstream, preserving
local-only additions in the preview; the eventual merge may still conflict.
`--abort --dry-run` lists tracked/index changes that would be discarded and
separately reports untracked/ignored obstructions that may be overwritten when
restoring HEAD. Unrelated untracked files are not listed as discarded. The
preview does not reset files or clear merge state.

### What declared Git sync never does

- Pull into a dirty working tree or overwrite ignored/untracked obstructions.
  Explicit `--abort` is the destructive exception for an in-progress merge.
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

## Virtual checkouts (`mode = "checkout"`) {#virtual-checkouts-mode-checkout}

A [virtual checkout](../adr/022-virtual-checkout-materialization.md) — no
`.git` at the target, backed by the overlay's own source repository — is
reconciled by the same `over sync` command, but entirely **locally**: no
network I/O, since the "source" is already the repository `over` reads the
overlay from.

- Nothing changed anywhere -> up to date.
- Only the source repository moved (nothing local to lose) -> the target
  is refreshed automatically (`--pull-only`/`--push-only` still apply:
  gated on the pull direction).
- Only the target has staged changes, with no unstaged edits -> asks for
  confirmation (and a commit message) before committing them into the source repository — the same
  as running `over commit` yourself. Declining, or passing `--no-prompt`,
  leaves both sides untouched and reports the checkout as blocked.
- Unstaged target edits -> reports the checkout as blocked. Select changes
  with `over add` and commit them with `over commit` first.
- Source advancement alongside independent local edits -> updates only
  source-changed paths after checking target and index snapshots. Local
  edits and staged snapshots remain intact. Already-converged content only
  needs its checkpoint advanced, including after a plain source Git commit.
- The *same* file changed on both sides, to different content -> reported
  as a conflict, never auto-resolved; resolve manually (edit the target
  and/or the source repository directly), then re-run.

`--dry-run` never prompts and never mutates anything for a virtual
checkout either, matching its root-`Checkout` behavior above.
Virtual refresh preserves existing access restrictions such as `0600`; Git's
executable bit is applied without resetting private permissions to `0644`.
See [Safety and recovery](safety.md) for the operation inventory and failure
limitations.
