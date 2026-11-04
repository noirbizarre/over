# `over add`

Add files or directories to an overlay.

```text
Usage: over add [OPTIONS] <FILES>...

Arguments:
  <FILES>...  Files, directories, or glob patterns to add

Options:
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME]
  -o, --overlay <OVERLAY>  Name of the target overlay
  -d, --debug              Toggle debug traces
  -r, --root <ROOT>        The target root directory (~)
  -n, --dry-run            Run without applying changes
  -v, --verbose            Toggle verbose output
  -f, --force              Overwrite without prompting
  -h, --help               Print help
```

For symlink materialization, files are moved into the overlay and replaced
in place by a symlink pointing back at the overlay copy — the same
mechanism `over apply` uses in reverse.

## Mapped directories

Inputs are reverse-routed through the overlay's
[directory mappings](../configuration.md#directory-target-mappings).
For example, with `map.etc.target: /etc/client`:

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
