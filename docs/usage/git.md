# `over git`

Use this command group while working inside a Git repository that should be
associated with an overlay. It discovers the Git repository from the current
directory, including when invoked in a subdirectory or worktree. This host
repository is distinct from the source overlay repository selected by `--home`.

| Command | When to use it |
| --- | --- |
| [`over git mount`](over-git-mount.md) | Associate the current repository with an overlay and export selected Git properties |
| [`over git add`](over-git-add.md) | Add host-repository files to that overlay |
| [`over git status`](over-git-status.md) | Inspect the associated overlay from the host repository |

Mount records the selected overlay in the host's Git config as `over.overlay`.
Mount and add accept `--overlay` to override this association and prompt when
neither is available. Status requires an existing association and reports an
error if none is configured. These commands do not use `default_overlay` as their
association-selection fallback.

From an existing host repository, with a matching overlay already defined:

```sh
over git mount --overlay workspaces/demo
over git status
```

The host repository must lie within the overlay's base target or a mapped
destination so Over can reverse-route its path. Mount offers an interactive
choice of Git properties to export. See the individual references for add
selection and worktree behavior. [`over apply`](apply.md) remains the command
for materializing an overlay; mounting records the host association.
