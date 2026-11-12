# `over restore`

Discard selected virtual-checkout changes, unstage files, or recover content from
an earlier Git revision. **Restoring overwrites or removes selected content
without a confirmation prompt.** Inspect changes with [`over diff`](diff.md) or
preview the destinations with `--dry-run` first.

```text
Usage: over restore [OPTIONS] <FILES>...

Arguments:
  <FILES>...  Explicit file paths to restore (destructive)

Options:
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME=]
  -o, --overlay <OVERLAY>  Select an overlay (otherwise infer from context, then use default_overlay)
  -d, --debug              Toggle debug traces
  -r, --target <TARGET>    Set the target directory (defaults to ~) [env: OVER_TARGET=]
      --source <REV>       Restore from a Git revision instead of the default source
  -v, --verbose            Toggle verbose output
      --staged             Restore the source repository index (default source: HEAD)
      --worktree           Restore materialized working-tree files
  -n, --dry-run            Show destructive effects without applying changes
  -h, --help               Print help

Discard selected changes permanently. By default, restore the worktree from the index.
With --staged, default to HEAD. With --source REV, default to worktree only;
historical recovery remains unstaged unless --staged is requested.
Paths are literal files, including deleted tracked files; directories and globs are not expanded.
```

## Sources and destinations

The index is the backing source repository's existing Git index, shared with
[`over add`](add.md) and [`over commit`](commit.md). Each linked source worktree
uses its own index. The worktree destination is the materialized target, not the
source repository's working-tree files.

| Options | Read from | Restore to |
| --- | --- | --- |
| None, or `--worktree` | Index | Target worktree |
| `--staged` | `HEAD` | Index |
| `--staged --worktree` | `HEAD` | Index and target worktree |
| `--source REV` | Revision tree | Target worktree |
| `--source REV --staged` | Revision tree | Index |
| `--source REV --worktree` | Revision tree | Target worktree |
| `--source REV --staged --worktree` | Revision tree | Both |

`REV` accepts Git revision syntax, such as a commit ID, tag, or `HEAD~2`, resolved
against the selected checkout's source repository. Invalid or ambiguous revisions
fail before any changes are applied.

Historical recovery without `--staged` changes only the target file. Its content
remains unstaged: explicitly run `over add` and `over commit` to record it.
Restore never creates a commit, moves a ref, writes source working-tree files, or
advances the checkout's recorded base. Unrelated staged entries and target edits
are preserved. Source working-tree edits do not prevent restore.

For [natively protected paths](crypt.md), direct restoration into the index accepts
only ciphertext from the current encryption generation. Historical plaintext and
old-generation ciphertext are rejected, even when `REV` is otherwise valid.
Restore historical plaintext into the worktree, then use `over add` to stage it
under the current protection policy. Recovering old-generation ciphertext first
requires that generation's trusted key; the current key alone cannot decrypt it.

## Select files and an overlay

Supply one or more literal file paths, relative to the current directory or
absolute. Missing paths can select deleted tracked files. Directories and globs
are not expanded; quote names containing shell metacharacters.

`--overlay NAME` takes priority, followed by [contextual inference](selection.md)
from all selected paths, including mapped destinations outside the base target.
With no contextual candidates, use the configured `default_overlay`. Ambiguous
selection requires an explicit `--overlay`, even with a default. `--target` sets
the destination meaning of `~`; it does not change how relative file arguments
are resolved.

Only the selected overlay's own virtual-checkout files are eligible. Ignored and
control files, other materializations, nested declared repositories, and paths
outside the managed scope are rejected. Symlinks, gitlinks, directories, special
files, and conflicted index entries are not supported restore targets. Resolve
selected conflicts or intent-to-add entries before restoring them.

## Deletions and untracked files

Restore recreates deleted tracked files when the chosen source contains them.
When a tracked path is absent from a selected revision, it is removed only from
the requested destinations. For example, unstaging a newly added file removes
its index entry but leaves its target content; adding `--worktree` removes both.

Default index-source restore requires a path still present in the index. After a
staged deletion, use `--source HEAD` to recover the file from the current commit.

Untracked files are not clean targets. Restore rejects untracked selections and
refuses historical recovery over an unrelated untracked replacement. All paths
are validated before writing. A changed index or target during preparation causes
failure rather than silently discarding another writer's changes. Index
publication is atomic per repository; operations spanning multiple source
repositories are not a single crash-atomic transaction.

## Examples

From inside a virtual checkout:

```sh
# Discard unstaged edits, restoring from the index
over restore file.txt

# Unstage the file, preserving target edits
over restore --staged file.txt

# Recover historical content as an unstaged change
over restore --source abc123 file.txt

# Discard staged and unstaged changes, restoring from HEAD
over restore --staged --worktree file.txt

# Restore historical content into both destinations
over restore --source abc123 --staged --worktree file.txt other.txt

# Preview an explicit mapped selection
over restore --overlay workspace/client --dry-run /etc/client/config.toml
```

## Current limitations

Restore operates on whole files. Interactive `-p`/`--patch` restoration is deferred
to a follow-up sharing the existing partial-staging engine.

Ordinary worktree writes use stored blob content; restore does not introduce Git
checkout smudge-filter or line-ending conversion. Over's native encryption is an
exception: protected blobs undergo its materialization transform, producing
decrypted worktree content with the matching local repository key.

Existing files retain private access permissions and native metadata. On Linux,
shared hard links are detached using a metadata-preserving replacement. On macOS,
shared hard links require filesystem support for native cloning (for example,
APFS); the replacement retains extended attributes, resource forks, and the exact
ACL, and unexpected ownership/mode changes are rejected. A cloning or metadata failure leaves
the live file and its other links unchanged. Other platforms without native
metadata cloning refuse shared-file updates. Existing single-link files use
in-place writes on non-Linux platforms, preserving their native metadata.
