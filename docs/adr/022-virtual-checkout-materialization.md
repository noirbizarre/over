# ADR-022: Virtual checkout materialization, distinct from `overlay.git`'s `Checkout`, amending ADR-017

> Schema amended by #190: select virtual checkouts with `mode: checkout`,
> at descriptor level or in shared ordered rules. Checkout units remain
> atomic; ineffective descendant mode overrides are lint errors.

**Status:** accepted

The ownership and automatic-recording decisions below are amended by
[ADR-025](025-directory-mappings-and-checkout-staging.md): mappings partition
checkouts, and `over add` stages snapshots before a staged-only `over commit`.

Diff views are amended by
[ADR-026](026-git-aware-diff-delegates-presentation-to-git.md): recorded
base/index and index/target comparisons use private snapshots and Git's
configured presentation, independently of aggregate checkout status.

## Context

Issue #141 asks for a second, distinct materialization mode: an overlay's
own tracked files, materialized as ordinary, directly editable files at the
target — with **no** `.git` directory or nested repository ever visible
there — backed by the overlay's own source repository (the repository
`over` already reads the overlay from, not a separately declared/cloned
one). Git bookkeeping needed to track the materialized view (an
overlay-specific base revision and managed-path association) must live
under `$XDG_STATE_HOME/over`, separately from target content. It is operational
metadata, not a content backup; losing it prevents automatic re-adoption of a
populated ordinary-file checkout.

This is easy to conflate with existing concepts, so the boundary matters:

- `overlay.git` + `MaterializationIntent::Checkout` +
  `CheckoutMaterializer` (#110, ADR-014) is a **real `git clone`** of a
  (possibly unrelated) declared repository, with a normal, visible `.git`
  at the target. Its Git lifecycle is user-managed; #231 supersedes ADR-014's
  original declared-checkout `over sync` workflow. Optional remote sync now
  operates only on the Over source repository, around local reconciliation.
  This is a resource an overlay *declares*, whether at its root or a
  subpath (ADR-021 further splits root vs. declared for status/diff).
- Issue #141's "virtual checkout" has neither of those: no `.git` ever
  appears at the target, there is no separate clone, and the "source" is
  always the overlay's own already-on-disk repository. It needs its own
  intent, materializer, and reconciliation model.

`overlays::rules::MaterializationKind` (ADR-017) deliberately excluded a
`checkout` value, with an explicit note that the exclusion was about
`overlay.git`'s concept. #141 asks for exactly the rule-selectable
`checkout` value that note anticipated adding later, for this unrelated
concept.

## Decision

**Naming.** A new `MaterializationKind::Checkout` rule value (config:
`materialization = "checkout"`) resolves, through the existing
`rules::resolve` precedence chain (rule > `defaults` > `Symlink`
fallback), to a new `MaterializationIntent::VirtualCheckout` — kept
distinct from the existing `MaterializationIntent::Checkout` in both name
and Rust type, so the two can never be confused in a `match`. A `path =
"."` rule (a new, narrow special case in `MaterializationRule::matches`)
targets the overlay's own root, since globs can't otherwise express "the
empty relative path" — the common case from the issue ("the overlay
itself materialized as a virtual checkout").

**Mechanism: private git plumbing, no `.git` at target.** Confirmed
against a small git2 spike before implementation:

- `git2::Repository::discover(source)` finds the overlay's own repository
  (its `Repository::root`, or a git root further up, canonicalized) —
  never assumed to equal `over`'s `Repository::root` field, so an overlay
  nested inside a larger repository still works.
- `tree.get_path(managed_path).to_object(repo).into_tree()` retrieves the
  *subtree* for the managed path (empty path -> the whole tree, for a
  root-rooted virtual checkout); checking out *that* tree with
  `git2::build::CheckoutBuilder::target_dir(target)` writes its entries
  directly under `target` with no path prefix and — critically — no
  `.git` anywhere, because it's a checkout of a tree object into an
  arbitrary directory, not a repository operation on that directory.
- Status/diff have no real git index at `target` to diff against, so
  content comparison is done by hashing on-disk files as git blobs
  (`Oid::hash_object(Blob, bytes)`) and comparing against the tracked
  tree's recorded blob `Oid`s (`Tree::walk`). This is a genuine 3-way
  comparison (`base` = the tree at the checkout's recorded `base_oid`,
  `local` = on-disk content, `source` = the repository's current `HEAD`),
  computed independently per file — see `status::virtual_checkout`.
- `over commit` builds new blobs (`repo.blob(bytes)`) for changed paths and
  replaces just those paths in the repository's current `HEAD` tree via
  `git2::build::TreeUpdateBuilder` (upsert/remove, `create_updated`),
  which shares every untouched tree object rather than reconstructing the
  whole tree by hand — everything outside the managed path (sibling
  overlays, other files in the same repository) is carried over
  unchanged. The resulting commit is a normal, single-parent
  `repo.commit(Some("HEAD"), ...)`, using the same signature-resolution
  helper (`actions::git::sync::signature`, promoted to `pub(crate)`) `over
  sync`'s merge commits already use — fully inspectable/revertable with
  plain `git log`/`git show`/`git revert` in the source repository.
- Persisted state (`materialize::virtual_checkout::state`, mirroring
  `sync::state`'s own contract exactly): `{overlay, managed_path, base_oid,
  created_at, last_commit_at}` per target path, under
  `$XDG_STATE_HOME/over/virtual_checkout.toml`. No persisted git index —
  the "index" the issue mentions is entirely ephemeral, recomputed from
  `base_oid` + a fresh tree walk whenever needed. Losing this file leaves
  target content intact but loses the recorded base and ownership association.
  Reapply can initialize populated targets against current source `HEAD`, but
  cannot reconstruct the lost historical base. See
  [lost association metadata](../usage/status.md#lost-association-metadata)
  for preservation and recovery limitations.

**`Materializer::classify` is sync; XDG reads inside it are too.**
Existing `classify()` implementations already do blocking I/O
(`symlink_metadata`, `git2` calls) directly, since `Plan::build` itself is
a plain sync function. `VirtualCheckoutMaterializer::classify` needs to
know whether a target already has a recorded association, so
`xdg::state::StateFile` gained `load_blocking` (the same locked read,
without the `spawn_blocking`/`.await` wrapper) rather than making
`classify` — and therefore the whole `Materializer` trait — `async`.

**`VirtualCheckoutMaterializer` leaves associated checkouts unchanged**:
an existing,
known virtual checkout (one with a recorded association) is never routed
through filesystem conflict-resolution semantics built for symlinks —
dirty/behind/conflict states are `over status`/`over diff`/`over
sync`/`over commit`'s job, never `apply`'s. First initialization into an
unassociated directory preserves its root and unrelated content. Missing files
are created and identical files adopted. Actual path collisions offer Skip,
Overwrite, Absorb, and Diff. Skip retains local content and continues; Absorb
also adopts that content into the source working tree without staging or
committing it. Both retain local differences against the captured `HEAD` base.
Overwrite replaces only the approved conflicting path or structural group.
Legacy symlink-only installations retain their existing migration checks.

**Migration both ways** follows the existing `Operation::Migrate`
pattern: a stale symlink migrating *to* a virtual checkout is always safe
(removing a symlink never loses source content, mirrors
`CheckoutMaterializer`); a virtual checkout migrating *back* to a symlink
(`SymlinkMaterializer::classify`, a new `virtual_checkout_migration`
alongside the existing `checkout_migration`) is only unblocked when
`status::virtual_checkout::inspect` reports `Status::Applied` — otherwise
`blocked: Some(reason)`, never executed even under `--force`, exactly
like the existing checkout migration gate.

**`over sync`'s reconciliation for a virtual checkout is entirely
local — no network I/O**, unlike the root `Checkout` case in the same
module: the "source" is already the on-disk repository `over` reads the
overlay from. `SyncOptions` gains `no_prompt`; `SyncOutcome` gains
`Committed` (kept distinct from `Pushed`, which implies a remote).
Deterministic per `Status`: `Applied` -> `UpToDate`; `Behind` (source-only
changes, confirmed nothing local to lose) -> re-checkout and advance
`base_oid` -> `FastForwarded`, gated on `opts.pull`; `Modified`
(local-only changes) -> gated on `opts.push`, prompts
(`dialoguer::Confirm` + `Input`, unless `no_prompt`) before calling
`crate::commit::commit` — declining, or `no_prompt`, leaves everything
untouched and reports `Blocked`; `Diverged`/`Conflict` -> `Conflict`,
never auto-resolved in either direction, per-file.

**`over commit` (`crate::commit`) is a small, narrow module** reused by
both `cli::commit` and `over sync`'s commit-prompt path, so the two never
duplicate the blob/tree/commit logic or the conflict-refusal policy. It
refuses anything that isn't a `VirtualCheckout` entry
(`CommitOutcome::NotVirtualCheckout`) rather than silently no-op'ing —
satisfying the issue's "must refuse or clearly report when the selected
overlay is not checkout-materialized" without the caller needing its own
check, and guaranteeing a declared `overlay.git` repository can never be
committed by this path even by accident.

**`over log`** resolves an overlay's virtual checkout and walks history
with a `Revwalk` + per-commit `diff_tree_to_tree` pathspec filter scoped
to the managed path (empty path -> unfiltered, the whole repository's
history) — no shelling out to `git`. With no overlay named, it guesses
from the current directory (the most specific virtual-checkout target
containing it), falling back to the whole source repository's log.
Formats commit timestamps with a small, dependency-free
days-since-epoch calendar conversion (Howard Hinnant's
`civil_from_days`) rather than adding a date/time crate for one display
string.

## Consequences

Four `Materializer` backends are now registered (`PartialFileMaterializer`,
`SymlinkMaterializer`, `CheckoutMaterializer`, `VirtualCheckoutMaterializer`),
each still routed by `MaterializerRegistry::find` on `MaterializationIntent`
alone — the registry/trait shape from ADR-012 needed no change. `Status`/
`Change`/`SyncOutcome` are reused as-is for the aggregate view (`Behind`'s
"ahead" meaning is reinterpreted for `Diverged` as "N locally-changed
files", not a commit count — documented on
`status::virtual_checkout::inspect`, since a virtual checkout has no
notion of local *commits* ahead the way a root `Checkout` does).

A known, accepted limitation: nothing detects or prevents an overlay
resolving to both `overlay.git`'s `Checkout` and this issue's
`VirtualCheckout` at the *same* target path — `Plan::build` would produce
two entries for it, mirroring the pre-existing root-`Checkout`-plus-base-
`Directory` coexistence gap `desired::tree`'s own comments already
document. Detecting and erroring on that overlap cheaply (e.g. a
`Plan::build` post-check) is a natural, backward-compatible follow-up, not
required by #141's acceptance criteria.

Symlinks (`git2::FileMode::Link`) and submodules
(`git2::FileMode::Commit`) inside a managed path are excluded from
tracked-blob comparison (`materialize::virtual_checkout::git::tracked_entries`)
— a symlink's on-disk "content" (what it points to) isn't meaningfully
comparable to a blob's recorded content the same way a regular file's is.
Precise symlink/submodule tracking inside a virtual checkout is a
documented follow-up, not required here.
