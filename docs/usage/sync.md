# `over sync`

Reconcile virtual checkouts with the local Over source repository. Optionally
pull that source repository before reconciliation and push its commits afterward.
By default, sync is entirely local and contacts no remote.

```text
Usage: over sync [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to sync (infer context, then default_overlay, then all overlays)

Options:
  -H, --home <HOME>      Configuration and overlays root [env: OVER_HOME=]
  -r, --target <TARGET>  Set the target directory (defaults to ~) [env: OVER_TARGET=]
  -d, --debug            Toggle debug traces
      --no-uses          Skip dependency operations, retaining dependency data
  -a, --all              Sync every overlay, retaining configured default_overlay data
  -v, --verbose          Toggle verbose output
      --pull             Fetch and rebase the Over source repository before local sync
      --no-pull          Disable source-repository pull for this invocation
      --push             Push existing source-repository commits after successful local sync
      --no-push          Disable source-repository push for this invocation
      --pull-only        Only refresh virtual checkouts from the local source
      --push-only        Only record virtual-checkout changes in the local source
      --continue         Re-attempt a sync after resolving conflicts manually (alias for a plain re-run)
      --abort            Report no abortable virtual-checkout operation; resolve source rebases with Git
  -n, --dry-run          Report what would happen without syncing
      --no-prompt        Never prompt to commit a virtual checkout's local changes; report them as blocked instead
  -h, --help             Print help
```

When `NAME` is omitted, [infer from context](selection.md), then use a configured
`default_overlay`. With neither, sync every overlay. Ambiguous context requires
an explicit name; `--all` syncs every overlay regardless of context or default.
Explicit `NAME` and `--all` retain the configured default's
[data context](../configuration/overlay.md#consumer-metadata-data).
`--no-uses` skips dependency operations, not their data.

## Source-repository remote synchronization

Enable operations per invocation or through root [`sync` settings](../configuration/root.md#source-synchronization):

```sh
over sync workspace/client --dry-run
over sync --pull --push
over sync --no-pull --no-push
```

`--pull` / `--no-pull` override only configured pull; `--push` / `--no-push`
override only push. These are independent of local `--pull-only` / `--push-only`.
There is no fetch-only flag: run ordinary `git fetch` in the source repository.

The order is:

1. Pull once for the Git worktree containing `--home` / `OVER_HOME`: fetch its
   current branch's configured upstream and rebase local commits onto it.
2. Reload root configuration and overlay discovery. Resolve selection, metadata,
   contexts and desired state from the updated source, including changed defaults
   or removed overlays.
3. Reconcile all selected virtual checkouts locally.
4. Push existing source commits to the configured upstream, without force, only
   after every local operation succeeds. An empty selection is also successful.

Both remote settings default to false. Invocation policy is resolved before pull;
changes to remote defaults fetched during this invocation apply next time. Pull
and push require an attached branch with a configured upstream; they do not assume
`origin` or matching local/remote branch names. They never stage or commit source
changes. The separate local virtual-checkout phase can explicitly ask to commit
target edits, as described below.

A dirty source worktree or index blocks pull before fetch. Over never auto-stashes
or discards source edits. An existing rebase stops sync before descriptor parsing.
A new rebase conflict leaves Git's native state and markers intact and prevents
target reconciliation and push. Resolve in the source worktree with ordinary Git:

```sh
git rebase --continue  # after resolving and staging conflicts
git rebase --abort    # to abandon the rebase instead
```

Then rerun `over sync`. The legacy `--continue` flag is only a rerun alias;
`--abort` does not abort source rebases or manage declared repositories.
It fails before any configured remote operations. Invalid descriptors in an
all-overlay selection also fail the command and prevent push, rather than being skipped.

Push failure is reported even though local reconciliation has completed. It does
not roll back local success; resolve non-fast-forward rejections with Git.
Dry-run never fetches, rebases, pushes, writes Git objects/refs, or contacts remotes.
It reports configured operations; remote state may have changed since the last fetch.

## Declared Git repositories

All repositories declared by an overlay's `git` map, including root entries and
worktrees, have independent Git lifecycles. `over apply` provisions them; manage
their fetch, merge, rebase, push, and conflict recovery with ordinary Git.
`over sync` does not perform Git lifecycle operations on them.

## Virtual checkouts (`mode = "checkout"`) {#virtual-checkouts-mode-checkout}

A [virtual checkout](../adr/022-virtual-checkout-materialization.md) has no `.git`
at its target and uses the overlay's source repository. Its reconciliation phase
is entirely local, after any optional source pull.

When targets are inside a host Git worktree, sync establishes managed
`.git/info/exclude` protection before publishing new leaves. It retains prior
protection if reconciliation fails, then reconciles the inventory after success.
Malformed managed blocks must be repaired before syncing. Dry-run does not change
exclusions, and unrelated neighboring files remain visible to host Git.

Reconciliation outcomes:

- Nothing changed anywhere → up to date.
- Only source content changed → refresh the target automatically. Local
  `--pull-only` / `--push-only` still gate this pull direction.
- Only the target has staged changes, with no unstaged edits → ask for
  confirmation and a commit message before committing into the source, as with
  `over commit`. Declining or `--no-prompt` reports blocked without changes.
- Unstaged target edits → report blocked. Select changes with `over add` and
  commit with `over commit` first.
- Independent source and local changes → refresh only source-changed paths after
  checking target/index snapshots. Preserve local edits and staged snapshots.
- Different changes to the same file → report conflict without auto-resolution;
  edit target/source as needed, then rerun.

Dry-run never prompts or mutates virtual checkouts. Refresh preserves private
access restrictions such as `0600`, while applying Git's executable bit.
See [Safety and recovery](safety.md) and
[Materialization](../configuration/materialization.md#editing-and-lifecycle).

### Generated checkouts

Checkouts containing [file templates](../configuration/overlay.md#file-templates)
use a generated desired snapshot rather than raw template text as their base.
Source template contents come from committed `HEAD`; newly resolved context data
can change the desired render without a source commit.

Sync compares three versions: the previous generated snapshot, the physical local
file, and the new desired render. Local-only edits are preserved and reported
with both destination and source-template paths. Non-overlapping textual edits
are merged automatically. Overlapping, binary, and delete/modify conflicts retain
the exact local file; sync reports the affected paths without printing values or
writing conflict markers. Conflicts stay visible on later sync/status runs.

The desired snapshot advances after reconciliation, including when local files
conflict. Merged/local content remains outside that desired snapshot. To accept
the generated desired version and clear a conflict, use `over restore PATH`.
Alternatively, edit the source template and sync a compatible result. Generated
destinations cannot be ingested through add, patch-add, or Absorb.
[`over commit`](commit.md#rendered-file-edits) can reverse-patch unambiguous
literal edits into source templates. Sync's offer to commit ordinary staged
files does not select rendered edits. After a reverse commit preserves independent
source changes, sync applies their committed rendering without replaying the
consumed local patch.
Ordinary non-template siblings retain their source staging workflow. A recursive
add containing generated destinations fails before staging anything.

Generated snapshots are complete Git trees/commits under local
`refs/over/generated/<checkout-id>` refs, chained to preserve internal history.
They do not move the user's `HEAD`, branches, or index. XDG state retains only
checkout/provenance/ref/conflict metadata, not duplicate snapshot contents.
Status and explicitly requested diffs compare generated output; diffs can display
the rendered values. Reporting and dry-run do not publish objects or refs.

Snapshots preserve their source entries' [encryption policy](crypt.md#protected-generated-snapshots).
Protected templates and ordinary protected siblings are encrypted in Git objects;
reconciliation decodes them transiently with the unlocked key. Unprotected
templates can still place locally rendered secrets in plaintext Git objects.
The refs themselves do not encrypt anything. Over never configures a push refspec for the generated
namespace. It is private by convention, not an access-control boundary: explicit
refspecs and `git push --mirror` can publish it. Removing a checkout/ref is not
secure erasure of Git objects or their history.
