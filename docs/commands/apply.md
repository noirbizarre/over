# `over apply`

Apply overlays and their dependencies to their configured targets. Optionally
run install hooks (`--install`), provision declared Git repositories, and
materialize files as links, editable checkouts, or managed blocks according to
the [configuration](../configuration/materialization.md).

```text
Usage: over apply [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...  Names of overlays to compose (infer active context; host_overlay requires apply.default_to_host)

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
      --skip <NAME_OR_GLOB>  Skip overlay deployment by canonical name or glob (repeatable; quote 'workspace/*')
  -i, --install           Run install hooks after applying files
  -h, --help              Print help
```

- When `NAME` is omitted, [infer active context](../usage/selection.md), then use
  `host_overlay` only if root `apply.default_to_host` is true. The option defaults
  to false: configuring a host alone does not select it for deployment. Missing or
  ambiguous selection requires an explicit name. An explicit name always wins,
  including `over apply hosts/laptop` with the option disabled.
- Multiple explicit names compose as an ordered `uses` list, with dependencies
  before parents and shared participants included once. Each retains its target;
  overlapping ownership fails before any participant writes, regardless of order.
  For example, `over apply --no-prompt -- container/base container/rust`.
- `--no-uses` applies only the named overlays, skipping every overlay each
  `uses` operationally. Metadata still includes all transitive dependencies.
- The configured host's own `host` overrides remain available with an explicit
  `NAME`; this does not apply the host's files or include its unrelated dependencies.
  Each participant receives local `data`, resolved `host`, and its explicit dependency
  namespace from one loaded snapshot. See [data scopes](../configuration/overlay.md#consumer-metadata-data).
- `--install` additionally runs `hooks.install` (see
  [Install hooks](../install-config.md)). File materialization, declared
  Git provisioning, and post-apply hooks do not require `--install`.
- An ordinary source-link conflict at the target triggers an interactive prompt
  (skip, overwrite, absorb, diff) unless `--force` or `--no-prompt` is given.
- The same conflict policy applies to `.link.*` sidecars. No-prompt alone
  refuses conflicts; force explicitly authorizes replacing them, including
  conflicting directories. Checkout disposal requires separate confirmation
  or `--discard-checkout`; force alone is insufficient. Ownership and mapping
  boundaries cannot be overridden. See [Safety and recovery](../usage/safety.md).
- Generated symlink-mode files (templates or extension-stripped outputs) have no
  interactive conflict menu or Absorb/write-back choice. An unmanaged destination
  fails unless `--force` authorizes a non-directory replacement. Directory
  obstructions must be moved aside manually, even with force.
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
The `Plan:` summary includes only positive-count categories and is omitted for an
empty plan. Unchanged entries are green, conflicts and blocked migrations red,
and repairs, deferred entries and locked entries yellow; pending creates and
migrations use the default foreground.
See [Examples](../examples.md#compose-a-host) for the corresponding composition
and [`over unapply`](unapply.md) for reversing a selected overlay's own entries.

Apply has no whole-command rollback or permanent overwrite/absorb backup.
Absorb changes source content as well as the target. Reapply/unapply cannot
recover previously overwritten user data.

With `--install`, install hooks run after all overlays reconcile and before
post-apply hooks. Later failure does not undo file or command changes.
External commands retain their own
prompt policies; Over's force/no-prompt flags do not control child prompts.

## Skip overlays during staged provisioning

Repeat `--skip` to exclude overlay deployments from the normal explicit,
active-context, or opt-in host selection:

```sh
over apply --skip neovim --skip 'workspace/*'
over apply base desktop --skip 'workspace/**' --dry-run
```

This does not select every overlay in the repository. Quote patterns so your
shell passes them unchanged to Over.

Patterns match whole, case-sensitive canonical overlay names relative to the
collection root, with `/` separators (for example, `workspace/private`). Explicit
selection aliases such as `./workspace/private/` are normalized before matching.
An exact name excludes only that overlay, not its descendants. Glob syntax is:

- `*` matches zero or more characters within a path component; `?` matches one.
  Thus `'workspace/*'` matches `workspace/private`, but not `workspace/team/private`.
- `**` in a recursive component matches nested paths; use `'workspace/**'` for
  descendants at any depth.
- `[abc]`, `[a-z]`, and `[!abc]` select or negate character classes;
  `{base,desktop}` matches alternatives. Backslashes escape metacharacters,
  including on Windows; use `/` for path separators.

Invalid patterns fail before application. Each unmatched name or pattern emits
a warning and application continues. Matching diagnostics cover names encountered
in this invocation's selection (including inference candidates) and dependency
graph, rather than scanning every unrelated overlay. Overlapping patterns each
count as matched; skipped overlay lines are deduplicated and appear in dry-run
and redirected output too.

Skipped overlays do not deploy files, clone or check out Git repositories,
prepare their own content/secrets, or run their overlay hooks. Their metadata
can still be loaded: configured host overrides and dependency data/target values
remain available to retained overlays.

Normally, if a retained overlay requires a skipped overlay through `uses`, apply
fails before content preparation or mutation, identifying the dependency chain
and matching exclusion. Remove the exclusion or select different roots to resolve
the conflict. With `--no-uses`, dependencies supply metadata only, so a skipped
dependency does not cause a deployment conflict:

```sh
over apply base --no-uses --skip private
```

During target inference, a candidate with a skipped deployment dependency cannot
be discarded merely because its content was not prepared. If its routing matches
your current directory, Over reports the conflict and asks for an explicit name
rather than potentially applying a different overlay.

If all selected roots are excluded, apply succeeds without changes or repository
hooks. An excluded explicit, source-context, Git-associated, or default-host
selection never falls through to a replacement host. Repository hooks still run
normally when retained overlays are applied.

## Lifecycle hooks

For multiple distinct selected roots, repository hooks run once per event with
no selected `overlay` or `OVER_OVERLAY_*` environment variables. Participant hooks
retain their own overlay context. All hooks wait for every participant's files.

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
