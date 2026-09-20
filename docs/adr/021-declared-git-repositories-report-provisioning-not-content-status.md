# ADR-021: Declared (non-root) `git` repositories report provisioning status, not content status, amending ADR-014

**Status:** accepted

## Context

ADR-014 already drew a conceptual line through `overlay.git`: the root
entry (`repo_key ==` [`ROOT_PATH`](../../src/actions/git/config.rs), `"."`)
is the overlay's own content, a genuine bidirectional sync surface; any
other entry is a declared, opaque resource — e.g. a plugin manager's clone
at a subpath — whose content `over` never synchronizes. That ADR only
implemented the split for `over sync` (`src/sync/mod.rs` skips non-root
entries outright).

Every other consumer of git-checkout state — `status::git::inspect` (used
by `over status`, `over diff`, and `over unapply`) — never made that
distinction. It always computed `Applied`/`Modified`/`Broken`/`Conflict`/
`Ahead`/`Behind`/`Diverged` from the checkout's own `repo.state()`,
`repo.statuses()`, and `repo.graph_ahead_behind()` against its upstream
tracking branch — the right question for the root checkout, the wrong one
for a declared resource. A tmux plugin manager writing state files, or
checking out branches inside its own clone, made `over status` report that
clone as dirty/ahead/behind exactly as if it were the user's own dotfiles
checkout (#140). Issue #140 asks for a **provisioning/configuration**
status instead: present, in the right structural form, with its declared
configuration (url/tag/rev/remotes/git config) applied — regardless of any
other content, history, or branch state inside it.

## Decision

**No new config field, no new `Provenance`/`DesiredEntry` field.**
`Provenance::Git`'s existing `repo_key` already carries the exact
distinction needed — the same one `src/sync/mod.rs` already branches on.
`status::git::inspect` gets the same branch:

- `repo_key == ROOT_PATH`: unchanged behavior, renamed `inspect_content` —
  the full dirty/ahead/behind/diverged/merge-in-progress computation this
  module always did.
- any other `repo_key`: a new `inspect_declared`, which never calls
  `is_dirty`/`ahead_behind`/reads `repo.state()`. It only checks:
  1. **Presence** — `Missing` if absent (or, for a declared bare+worktree
     repository, if any explicitly named `worktrees` directory, or the
     auto-created default-branch worktree, doesn't exist yet —
     `actions::git::detect_default_branch` made `pub(crate)` for this,
     read-only, without re-deriving its HEAD/`origin/HEAD`/main/master
     fallback chain).
  2. **Declared configuration** — `declared_config_status` returns
     `Conflict` ("configuration drift") on the first mismatch: `url`
     (compared against the `origin` remote), `tag`/`rev` (non-bare only —
     `actions::git::checkout_ref` never applies these to a bare repo
     either), extra `remotes`' urls, and arbitrary `config` entries.
     Otherwise `Applied`.

  Deliberately **not** checked: `config.branch`. `EnsureGitRepository`
  itself only ever applies a branch at clone time (`RepoBuilder::branch`),
  never reconciling it on an existing repository — switching branches
  post-clone risks discarding local work, so `apply` never does it either.
  Reporting a "drift" here that `apply` can never fix would be misleading.
  Also deliberately coarse about `remotes` (url only, not
  `push`/`tagopt`/extras) and `worktree_config` — a natural follow-up, not
  required by #140's acceptance criteria.

`Status::Applied`/`Missing`/`Conflict`'s doc comments gain a
declared-repo-specific meaning alongside their existing root-checkout one
— the same pattern the codebase already uses for `Status::Modified` (dirty
checkout *or* permission-repair) and `Status::Conflict` (type mismatch *or*
merge-in-progress): one closed enum, context-dependent meaning, rather than
growing the variant set for every new intent.

**`unapply`'s destructive removal gate is a deliberate, explicit opt-out —
not a side effect.** Tracing every caller of the function being changed
matters here: `materialize/symlink.rs`'s checkout→symlink migration safety
check calls `status::git::inspect_checkout` (the single-directory
primitive) directly, never the function this ADR changes, so it's
unaffected either way. `crate::unapply`, however, called the exact
top-level `status::git::inspect` this ADR just made root/declared-aware,
at both its classification (`Report::build`) and its actual
`fs::remove_dir_all` gate (`remove_checkout_if_clean`). Left alone, a
declared repository with real uncommitted/unpushed local content would
newly classify `Applied` and get silently deleted whole — exactly the data
loss ADR-015 built `unapply` to never cause. Both call sites are changed to
call `inspect_content` explicitly instead, with a comment pointing back
here, so a declared repository is removed only when it is *fully* clean —
precisely as strict as the root checkout, unaffected by #140's reporting
change. `docs/usage/unapply.md`'s existing wording ("a git checkout — the
overlay's own root checkout, or any nested `git` entry — is removed only
when it's fully in sync with its upstream") already matches this and
needed no edit.

## Consequences

`over status`/`over diff` stop surfacing unrelated content, local commits,
uncommitted changes, or ahead/behind/diverged state for a declared
repository once its declared invariants are satisfied — the behavior #140
asks for — while `over apply`'s idempotent reconciliation
(`EnsureGitRepository`), `over sync`'s scoping, and `over unapply`'s
removal safety are all unchanged. `CheckoutMaterializer::classify` is
unaffected: it only ever branches on `Missing` vs. "anything else → Noop",
and that split is preserved for both root and declared entries.

The cost is the same conservatism ADR-014 already accepted for `remotes`/
`worktree_config`: `declared_config_status` reports *that* configuration
has drifted, not a itemized diff of which field, and deliberately never
verifies `branch`. Deeper per-field remote/worktree-config verification and
a more detailed drift description are natural, backward-compatible
follow-ups, not required here.
