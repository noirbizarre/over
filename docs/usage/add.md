# `over add`

Add files or directories to an overlay.

```text
Usage: over add [OPTIONS] <FILES>...

Arguments:
  <FILES>...  Files, directories, or glob patterns to add

Options:
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME=]
  -o, --overlay <OVERLAY>  Select the target overlay (otherwise infer from file context)
  -d, --debug              Toggle debug traces
  -r, --target <TARGET>    Set the target directory (defaults to ~) [env: OVER_TARGET=]
  -n, --dry-run            Run without applying changes
  -v, --verbose            Toggle verbose output
  -f, --force              Overwrite without prompting
  -p, --patch              Interactively stage virtual-checkout changes by hunk
  -h, --help               Print help
```

Without `--overlay`, [infer from the input paths](selection.md). Managed siblings
can identify an overlay for a new file. Ambiguous or missing context requires an
explicit overlay; `default_overlay` does not select an ingestion destination.

For symlink materialization, files are moved into the overlay and replaced
in place by a symlink pointing back at the overlay copy — the same
mechanism `over apply` uses in reverse.

## Mapped directories

Inputs are reverse-routed through the overlay's
[rule targets](../configuration/overlay.md#overlay-rules).
For example, with a rule `{path: etc, target: /etc/client}`:

```sh
over add --overlay workspace/client /etc/client/other.yaml
```

This adds `etc/other.yaml` to that overlay. Inputs outside the base target
and mapped destinations are rejected, as are base-target paths that would
reapply somewhere else because their source prefix was mapped away.

## Virtual checkout staging

Inside a virtual checkout, `over add` acts like `git add`: selected new,
modified, or deleted files are staged in the overlay source repository's
Git index at their reverse-mapped source paths. Target files remain ordinary
files. A directory selection includes tracked deletions beneath it; a
missing literal path can select a deleted tracked file.

```sh
over add --overlay workspace/client /etc/client/config.toml
over commit workspace/client --message "Update client configuration"
```

Staging records a snapshot. Further edits stay unstaged, and `over commit`
commits only staged changes. Status and diff distinguish staged and unstaged
content. Unrelated index entries are preserved. Sync a checkout that is
behind its source before staging, and resolve conflicting source edits
first. `--dry-run` leaves both the index and target files unchanged.

Use [`over restore --staged PATH`](restore.md) to unstage a file while preserving
target edits, or `over restore PATH` to discard its unstaged target changes.

### Interactive partial staging

Use `--patch` (`-p`) to stage selected changes instead of entire files:

```sh
over add -p --overlay workspace/client /etc/client/config.toml
over add --patch --overlay workspace/client /etc/client/
```

Each text hunk includes surrounding context. Choose **Accept** to stage it
or **Skip** to leave it unstaged. **Finish** applies the accepted choices and
skips all remaining changes. **Cancel** (including Escape) discards every
choice in the session. Finishing the last choice applies accepted changes.
An interrupted or failed prompt does not stage any choices.

Hunks show canonical Git content after clean filters. External diff drivers
and text conversion tools are not used to generate applicable patches.

Already-staged changes and unrelated index entries are preserved. All target
and source working-tree files remain unchanged. If another process changes
the index while you select hunks, the operation fails: rerun it against the
new index. Index updates are atomic per repository; selections spanning
multiple repositories are not a single transaction.

Added and deleted text files, binary files, and empty-file changes are offered
as whole-file choices. Executable-mode changes are separate whole-change
choices. Manual hunk splitting/editing and file/directory replacements are
not supported in patch mode.

Patch mode only stages virtual-checkout paths owned by the selected overlay.
Directory selections skip other materializations; explicitly selecting an
unmanaged file fails. No files are moved or replaced with symlinks.
`--force` does not accept choices automatically. Interactive selection needs
a terminal; `--patch --dry-run` displays candidate changes without prompting
or updating the index.
