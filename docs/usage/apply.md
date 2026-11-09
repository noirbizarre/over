# `over apply`

Apply a given overlay: optionally install packages first (`--install`), then
clone any declared git repositories and symlink its files into the target
root.

```text
Usage: over apply [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to apply (uses default_overlay if configured)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -d, --debug        Toggle debug traces
  -n, --dry-run      Run without applying changes
  -f, --force        Overwrite without prompting
  -v, --verbose      Toggle verbose output
      --no-prompt    Fail on conflict instead of prompting
      --no-uses      Skip dependency operations, retaining dependency data
  -i, --install      Install associated applications
  -h, --help         Print help
```

- When `NAME` is omitted, the repository's configured `default_overlay` (if
  any) is used instead of prompting interactively — see
  [Default Overlay Selection](../configuration.md#default-overlay-selection).
- `--no-uses` applies only the named overlay, skipping every overlay it
  `uses` operationally. Metadata still includes all transitive dependencies.
- The configured default's [data context](../configuration.md#consumer-metadata-data)
  remains available with an explicit `NAME`; this does not apply the default's files.
- `--install` additionally runs the `install` section (see
  [Install Configuration](../install-config.md)); without it, `apply` only
  links files.
- A file that already exists at the target triggers an interactive prompt
  (skip, overwrite, absorb, diff) unless `--force` or `--no-prompt` is given.
- `--dry-run`/`--verbose` print the structured reconciliation plan (one line
  per entry: create/no-op/conflict) before executing it, rather than just
  reporting success or failure. See
  [ADR-011](../adr/011-plan-then-execute-reconciliation.md).
- Symlinked files/directories and checkouts that land inside a git-tracked
  target are added to that repository's `.git/info/exclude`, in a block
  scoped to the applied overlay, so its own `git status` stops listing them
  as untracked. Skipped on `--dry-run`; a path already tracked by that
  repository is never excluded. See [`over status`](status.md#gitinfoexclude-diagnostics)
   for how to spot a missing, drifted, orphaned, or malformed block.

## Lifecycle hooks

Apply can execute repository-provided commands: **only apply repositories you
trust**. After all participating overlays materialize successfully, root
`hooks.post-apply` runs once, followed by each overlay's own post-apply hooks in
dependency-first order, without duplicates. These hooks also run on a successful
no-change apply. `--no-uses` includes only root and selected-overlay hooks;
`--dry-run` executes none.

If materialization fails, no post-apply hooks run. A hook failure stops later
hooks and fails the command, preserving completed filesystem changes. Retrying
apply reruns hooks, so commands must tolerate repeated execution. See
[Lifecycle Hooks](../configuration.md#lifecycle-hooks) for configuration, working
directories, environment variables, and exit-status behavior.
