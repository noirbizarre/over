# `over git add`

Add files from the current git repository to an overlay — the `over git`
equivalent of [`over add`](add.md), resolved from inside the repository
mounted with [`over git mount`](over-git-mount.md).

```text
Usage: over git add [OPTIONS] <FILES>...

Arguments:
  <FILES>...  Files, directories, or glob patterns to add

Options:
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME]
  -o, --overlay <OVERLAY>  Select the target overlay (otherwise infer from file context)
  -d, --debug              Toggle debug traces
  -n, --dry-run            Run without applying changes
  -f, --force              Overwrite without prompting
  -v, --verbose            Toggle verbose output
  -h, --help               Print help
```

Without `--overlay`, [infer from the input paths](selection.md), preferring an
explicit repository `over.overlay` association. All inferred operands must agree;
ambiguous or missing selection requires an explicit overlay. The configured
default does not choose an ingestion destination.

Added files are also excluded from this repository's `git status` via the
same per-overlay `.git/info/exclude` block that `over apply` maintains.

## Example

From a mounted repository with an existing `.editorconfig`:

```sh
over git add .editorconfig --dry-run
over git add .editorconfig
```

Rule targets are reversed just as for [`over add`](add.md), including
repositories outside the overlay's base target. Virtual-checkout inputs stage
selected snapshots in the overlay source index; symlink inputs retain
move-and-link ingestion. Use [`over git status`](over-git-status.md) to inspect
the host association and [`over diff`](diff.md) for staged checkout changes.
