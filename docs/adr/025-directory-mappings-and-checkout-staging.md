# ADR-025: Directory mappings partition ownership; checkout adds stage snapshots

## Status

Accepted. Extends ADR-017 and amends ADR-022's commit workflow.

## Context

One overlay may contain workspace files and configuration destined for
different directory roots. Applying each source prefix at a different target
also requires an unambiguous inverse for adding files. A whole-overlay
virtual checkout must not materialize mapped source directories twice or
mistake their destinations for newly added residual files.

## Decision

Introduce literal directory-prefix `map` entries in overlay configuration.
One domain-level resolver supplies forward and reverse routing. Reject
nested source mappings and overlapping destination namespaces; relative
destinations resolve below the base target. Reverse routing must round-trip
through forward routing.

Partition desired state into residual content and independent mapped
subtrees. Preserve original logical paths for rule resolution. A virtual
checkout carries source exclusions and protected target namespaces; these
are different when a mapping changes the directory prefix. Use the same
scoped view for materialization, status, diff, sync, commit, and unapply.
Remove only owned files, never an entire directory containing protected
destinations. Persist scope/source identity and reject implicit changes to
an existing deployment's ownership. Whole-directory symlinks remain atomic.

For virtual checkouts, `over add` stages selected target snapshots in the
source repository's Git index. `over commit` commits only staged paths owned
by the selected overlay, combining its units while preserving unrelated
index entries and unstaged target edits. Initial materialization writes
owned blobs directly rather than using a Git checkout operation that can
rewrite the source index.

Validate both lexical namespaces and physical destination identities through
existing ancestors, so symlink aliases cannot give independent units the
same directory. Match deployed associations by source unit as well as target
to detect external-to-external remapping.

Prepare source-file updates transactionally before advancing HEAD, preserving
existing access permissions and rejecting unstaged source mode changes. Sync
can reconcile source-only changes and advance already-converged checkpoints
without consuming independent target edits or index snapshots.

## Consequences

- Mapping is orthogonal to strategy; no new materializer is required.
- Whole-overlay checkout defaults work with independently mapped directories.
- Existing virtual checkout layouts require unapply using their old config
  before changing their mapping boundaries.
- Checkout users explicitly stage with `over add` before `over commit`.
- Declared Git repositories and explicit partial-file targets retain their
  distinct ownership and reconciliation semantics.
