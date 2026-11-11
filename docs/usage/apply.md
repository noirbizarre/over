# `over apply`

Apply an overlay and its dependencies to their configured targets. Optionally
run install hooks (`--install`), provision declared Git repositories, and
materialize files as links, editable checkouts, or managed blocks according to
the [configuration](../configuration/materialization.md).

```text
Usage: over apply [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to apply (otherwise infer context, then use default_overlay)

Options:
  -H, --home <HOME>       Configuration and overlays root [env: OVER_HOME=]
  -r, --target <TARGET>   Set the target directory (defaults to ~) [env: OVER_TARGET=]
  -d, --debug             Toggle debug traces
  -n, --dry-run           Run without applying changes
  -f, --force             Overwrite ordinary filesystem conflicts without prompting
  -v, --verbose           Toggle verbose output
      --discard-checkout  Discard owned real checkouts, including local files and Git history
      --no-prompt         Fail on conflict instead of prompting
      --no-uses           Skip dependency operations, retaining dependency data
  -i, --install           Run install hooks after applying files
  -h, --help              Print help
```

- When `NAME` is omitted, [infer from context](selection.md), then use the
  repository's configured `default_overlay`. Missing or ambiguous selection
  requires an explicit name.
- `--no-uses` applies only the named overlay, skipping every overlay it
  `uses` operationally. Metadata still includes all transitive dependencies.
- The configured default's [data context](../configuration/overlay.md#consumer-metadata-data)
  remains available with an explicit `NAME`; this does not apply the default's files.
- `--install` additionally runs `hooks.install` (see
  [Install hooks](../install-config.md)). File materialization, declared
  Git provisioning, and post-apply hooks do not require `--install`.
- A file that already exists at the target triggers an interactive prompt
  (skip, overwrite, absorb, diff) unless `--force` or `--no-prompt` is given.
- The same conflict policy applies to `.link.*` sidecars. No-prompt alone
  refuses conflicts; force explicitly authorizes replacing them, including
  conflicting directories. Checkout disposal requires separate confirmation
  or `--discard-checkout`; force alone is insufficient. Ownership and mapping
  boundaries cannot be overridden. See [Safety and recovery](safety.md).
- `--dry-run`/`--verbose` print the structured reconciliation plan (one line
  per entry: create/no-op/conflict) before executing it, rather than just
  reporting success or failure. See
  [ADR-011](../adr/011-plan-then-execute-reconciliation.md).
- Symlinked files/directories and checkouts that land inside a git-tracked
  target are added to that repository's `.git/info/exclude`, in a block
  scoped to the applied overlay, so its own `git status` stops listing them
  as untracked. Skipped on `--dry-run`; a path already tracked by that
  repository is never excluded. See [`over status`](status.md#git-configuration)
  for missing effective protection, tracked conflicts and malformed markers.

## Example

On a terminal, `Applying overlay …` appears in the live progress display alongside
provisioning and reconciliation details. Each completed overlay leaves an
`Applied overlay …` line above progress, in dependency-first order. These lines
remain in redirected stdout too. They describe that overlay's file application;
later overlays or install/post-apply hooks can still fail.

```sh
over apply hosts/laptop --dry-run
over apply hosts/laptop
over status hosts/laptop
```

Use the preview to inspect conflicts and planned changes before applying.
See [Examples](../examples.md#compose-a-host) for the corresponding composition
and [`over unapply`](unapply.md) for reversing a selected overlay's own entries.

Apply has no whole-command rollback or permanent overwrite/absorb backup.
Absorb changes source content as well as the target. Reapply/unapply cannot
recover previously overwritten user data.

With `--install`, install hooks run after all overlays reconcile and before
post-apply hooks. Later failure does not undo file or command changes.
External commands retain their own
prompt policies; Over's force/no-prompt flags do not control child prompts.

## Lifecycle hooks

Apply can execute repository-provided commands: **only apply repositories you
trust**. With `--install`, applying an untrusted repository executes its arbitrary
installation commands. After all participating overlays reconcile successfully,
opted-in `hooks.install` runs first, then `hooks.post-apply`. Each event runs root
hooks once, followed by each overlay's own hooks in dependency-first order,
without duplicate participants. These hooks also run on a successful
no-change apply. `--no-uses` includes only root and selected-overlay hooks;
`--dry-run` executes none.

Ordinary apply does not prepare or execute install hooks. If materialization
fails, neither install nor post-apply hooks run. A hook failure stops later
hooks and fails the command, preserving completed filesystem changes. Retrying
apply reruns hooks, so commands must tolerate repeated execution. See
[Lifecycle hooks](../configuration/root.md#lifecycle-hooks) for configuration, working
directories, environment variables, and exit-status behavior.
