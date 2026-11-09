# `over log`

An explicitly named overlay includes all its virtual checkout units,
including mapped directories. Current-directory inference selects the actual
owning unit, including destinations outside the base target. A residual
checkout's history excludes its independently mapped source directories.

Show the git history behind a
[virtual checkout](../adr/022-virtual-checkout-materialization.md)-materialized
overlay — including commits made via [`over commit`](commit.md) — without
needing to `cd` into the overlay's source repository and run `git log`
yourself.

```text
Usage: over log [OPTIONS] [NAME_OR_REVISION]

Arguments:
  [NAME_OR_REVISION]  Select an overlay or Git revision/range (overlay names take precedence)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
      --overlay <OVERLAY>  Select an overlay explicitly, interpreting the positional argument as a revision
  -r, --root <ROOT>  The target root directory (~)
  -n, --limit <LIMIT>  Maximum number of commits to show [default: 20]
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

## Scope

- With an overlay name or `--overlay NAME`: shows commits touching that
  overlay's managed paths. Errors if the named overlay isn't
  checkout-materialized.
- Without an overlay name: guesses the overlay from the current directory (the
  most specific virtual checkout containing it); if none matches, falls
  back to the repository containing the configured Over home.

Path filtering compares each commit's tree with its first parent (or an empty
tree for the initial commit). Commits changing only unrelated paths are skipped,
and the limit applies after filtering. This does not reproduce Git's path-history
simplification. Empty commits are omitted, including in whole-repository history.

## Revisions and ranges

```sh
over log                         # HEAD history, inferred scope or repository fallback
over log main                    # main..HEAD (branch shorthand)
over log main..HEAD               # Commits reachable from HEAD but not main
over log v1.0..HEAD               # Changes since a tag
over log HEAD~5..HEAD             # The last five commits on a linear history
over log main...feature           # Commits unique to either side
over log --overlay dotfiles main..HEAD
over log dotfiles                # All HEAD history touching this overlay
```

Existing overlay names take precedence over positional revisions. Use an explicit
range such as `main..HEAD` when a branch shares an overlay's name, or use
`--overlay NAME` to select a scope and interpret the positional argument strictly
as a revision. Existing but invalid overlay configurations report an overlay error.

Only standalone branch references (local or remote) are shorthand for
`BRANCH..HEAD`. `HEAD`, commit IDs, standalone tags, and expressions such as
`HEAD~5` show history reachable from the specified commit, following Git semantics.
Git's reference resolution precedence applies when branch and tag names collide.
Annotated and lightweight tags are both supported.

Two-dot ranges exclude everything reachable from the left side; three-dot ranges
show both sides with all shared merge-base history excluded. Omitted range
endpoints default to `HEAD`, as in Git. Revisions are resolved in the repository
backing the selected or inferred checkout scope, even when the checkout target
has no `.git` directory. Path filtering and `--limit` still apply to ranges.

The command accepts one commit-ish expression or two-/three-dot range supported
by libgit2, including ancestry expressions. Multiple revision arguments,
exclusion lists, and set-expanding suffixes (`^@`, `^!`, `^-`) are not supported.
Invalid revisions, unsupported expressions, and objects that cannot be peeled to
commits report an error.

## Presentation

Each compact commit line includes a colored short SHA, emphasized subject,
branch/tag decorations when present, and quieter author and absolute date
metadata. `HEAD -> branch`, detached `HEAD`, and `tag: NAME` labels remain readable
without color. Dates use the commit's recorded timestamp and UTC offset.

Styling follows terminal capabilities and is disabled for non-TTY output or a
nonempty `NO_COLOR` environment variable, even when `CLICOLOR_FORCE` is set.

Every commit is read directly via `git2` — no shelling out to `git`.

See [ADR-022](../adr/022-virtual-checkout-materialization.md) for the full
design rationale.
