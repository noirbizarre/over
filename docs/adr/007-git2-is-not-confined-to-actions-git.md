# ADR-007: `git2` is a direct dependency of the CLI layer, not confined to `actions::git`

**Status:** accepted

## Context

`actions::git` clones repositories declared by an overlay — a narrow,
resource-oriented use of `git2`. `git-over`'s entire purpose is different in
kind: `mount`/`add`/`status` read and write the *host* repository's own
state directly — `.git/config`'s `over.overlay` key, worktree
configuration, repository discovery from the current directory. That is not
"clone a resource"; it is "manipulate this repository", which is what
`git2` is for.

A stricter design would confine `git2::*` types entirely behind a narrow
trait in `actions::git` and have the CLI layer call through it — trading
directness for a swappable/mockable seam.

## Decision

`git2` is imported directly wherever the CLI layer actually needs it:
`src/cli/git_over/mod.rs` (`discover_repo`, `get_overlay_config`/
`set_overlay_config`, `resolve_overlay`), `src/cli/git_over/mount.rs`
(worktree config reads), and `src/cli/new.rs` (`git2::Repository::open`/
`init` when scaffolding a new overlay). No abstraction boundary was built
between these call sites and `actions::git`.

## Consequences

This is recorded as an accepted tradeoff, not a violation to fix. The
directness matches what `git-over` actually does — read and write the host
repository's own config, not a resource `over` owns. The cost: `git2` (and
its `vendored`/OpenSSL/`libz` feature flags) is now a dependency of the CLI
layer as well as `actions::git`, not swappable behind a seam — replacing or
upgrading the git backend would touch `cli/git_over/*` and `cli/new.rs` in
addition to `actions/git`. There is also duplicated worktree/config-reading
logic between `actions::git` and `cli/git_over/mount.rs`'s own
`read_worktree_config`, which can drift if one changes without the other.
