# `over commit`

For [protected virtual files](crypt.md), selected commits also include necessary
staged canonical `.gitattributes` and access metadata. The candidate tree is
checked against its own policy so plaintext cannot be committed beneath a
protected attribute. Resolve unstaged policy edits before committing; unrelated
ordinary staged paths remain outside the selected commit.

Record staged changes made directly in a
[virtual checkout](../adr/022-virtual-checkout-materialization.md) back
into the overlay's own source repository, as a normal git commit.

```text
Usage: over commit [OPTIONS] [PATH]...

Arguments:
  [PATH]...  Commit staged paths (defaults to all owned staged changes)

Options:
      --overlay <NAME>    Select the overlay explicitly
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME=]
  -r, --target <TARGET>    Set the target directory (defaults to ~) [env: OVER_TARGET=]
  -d, --debug              Toggle debug traces
  -m, --message <MESSAGE>  Commit message
      --no-prompt          Never prompt; use --message or an auto-generated message
  -v, --verbose            Toggle verbose output
  -h, --help               Print help
```

When `--overlay` is omitted, infer the current overlay from its source descriptor,
Git association, or recorded checkout, then use the repository's configured
`default_overlay`. Missing or ambiguous selection requires `--overlay NAME`.
Commit does not discover every overlay to infer ownership.

An explicit `--overlay NAME` retains the configured default's
[data context](../configuration/overlay.md#consumer-metadata-data) for path rendering.
Only the selected overlay's own checkout entries are committed.
This also applies when multiple applications project files into the same home:
their source snapshots and checkout checkpoints remain independent. Use an explicit
overlay when invoking commit from a shared destination directory.

Path operands filter **staged snapshots**, including staged additions and
deletions. They never stage current file contents. Without operands, all owned
staged changes are committed. Directory operands select staged descendants;
shell-expanded glob matches are accepted as multiple paths. Quoted wildcard
expressions are not expanded by `over commit`.

Relative paths resolve from the current directory when it is inside the selected
overlay's source or checkout target. Otherwise, including when selecting another
overlay with `--overlay`, they resolve from that overlay's source root. Absolute
source and target paths are also accepted. Unrelated untracked directories and
other overlays are not scanned.

## Example

After editing a file in an applied [virtual checkout](../configuration/materialization.md):

```sh
over add --overlay workspace/client ~/.config/client/settings.json
over diff workspace/client --staged
over commit --overlay workspace/client --message "Update client settings" \
  ~/.config/client/settings.json
```

See the [complete checkout example](../examples.md#edit-ordinary-files-with-checkout)
for source initialization and the mapped layout. Commit does not push a remote.

## What it does

For the named overlay's own virtual checkout entries (never a `uses`
dependency's — that content belongs to a different overlay):

1. Selects the added/modified/deleted files staged with [`over add`](add.md)
   in the source repository's Git index.
2. If a selected file changed both locally and in the source repository, to
   different content, refuses to commit at all — resolve the conflicting
   files manually first (`over status` lists file changes in its final Changes section).
3. Otherwise, with something to commit, asks for a commit message
   (`--message` skips the prompt; `--no-prompt` with no `--message` falls
   back to an auto-generated one).
4. Uses the staged snapshots, updates just those paths in the
   source repository's current tree (everything else in that repository —
   sibling overlays, unrelated files — is carried over untouched), and
   creates a normal, single-parent commit on top of the source
   repository's current `HEAD`.
5. Advances participating checkout checkpoints to the new commit.
   Unstaged target edits remain visible and unchanged. Selected source
   working-tree files are brought into agreement with the commit after
   checking that doing so preserves existing source edits.

Mapped and residual checkout units are committed together. Unrelated
staged files are preserved in the index and excluded from this commit.
A checkout whose own source content has advanced must be synced first.
Sync preserves independent staged and unstaged edits while reconciling
non-conflicting source advancement. Source updates are prepared before HEAD
advances; failures applying them or creating the commit attempt to restore
source backups. Failed restoration retains backups and reports their paths.
Existing source access permissions are retained; unstaged source content or
executable-bit edits are rejected rather than overwritten.

This is limited source-update recovery, not whole-command/crash rollback.
Checkpoint persistence happens after the commit: it can fail after HEAD has
advanced, and participating checkpoints may be only partly updated. That error
identifies the successful commit; inspect HEAD and checkout status before
retrying. Use `over diff NAME --staged` to inspect the selected changes;
commit has no dry-run flag. See [Safety and recovery](safety.md).

Unlike the earlier automatic-recording workflow, `over commit` does not
include unstaged target changes. Use `over add` to select changes first.

The resulting commit is entirely ordinary: inspect it with `git log`/`git
show`/`git diff` directly in the overlay's source repository, and push it
to a remote like any other commit.

## What it never does

- Commit anything in a *declared* `overlay.git` repository nested inside
  the overlay (a plugin manager's clone, etc.) — `over commit` only ever
  acts on virtual checkout entries, and refuses outright
  (`overlay is not checkout-materialized`) if the named overlay has none.
- Silently pick a side for a file that changed both locally and upstream —
  every selected conflicting path blocks the commit until resolved manually.
- Touch anything outside the managed path in the source repository.

See [ADR-022](../adr/022-virtual-checkout-materialization.md) for the full
design rationale.
