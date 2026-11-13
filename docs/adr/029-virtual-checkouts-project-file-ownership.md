# 029 — Virtual checkouts project file ownership

## Status

Accepted. Amends ADR-022 and ADR-025 for fresh deployments; existing associations
retain their recorded exclusive ownership.

## Context

Flat application overlays commonly share a home target, nested fragment directories,
and independent root files. Directory-exclusive virtual checkouts reject this layout
despite disjoint payloads. Unique directory mappings cannot represent every fragment
or root file, and symlinks do not provide the same source/destination encryption lifecycle.

Allowing overlapping checkout roots without changing reverse operations would be
unsafe: target-keyed state would overwrite associations, and recursive target discovery
would import unrelated files into each source.

## Decision

Fresh `mode: checkout` units own concrete projected leaves. Ancestor directories are
shared scaffolds. Checkout units still determine reconciliation and checkpoints;
they no longer imply exclusive ownership of destination subtrees. Actual duplicate
files, file/ancestor conflicts, and atomic Git repository boundaries remain errors.
Composition remains a flat, order-independent union.

State schema v3 distinguishes associations by source repository, overlay, managed
source unit, and destination. Existing v1/v2 records keep exclusive scopes. Neither
upgrading nor reapplying converts them to projections. Symlink deployments require
explicit teardown before deploying virtual files.

One ownership inventory combines source selection, Git base/HEAD/index paths, and
durable admitted leaves. Static lint also works from uncommitted source files.
Operational projection discovery never recursively imports arbitrary target files.
Required directory traversal is distinct from leaf ownership.

New target files require explicit overlay selection. Recursive acquisition skips
sibling-owned paths; a directly selected foreign-owned leaf errors. Admissions
survive reset/unstage so one owner's temporary index state cannot transfer a file
to another. Unapply releases an association's claims and leaves shared roots intact.

Cooperating mutations serialize ownership decisions before index/state publication.
Admissions are reserved before publishing staged additions. First initialization
persists a pending association before target publication; interrupted initialization
reserves its paths and requires reapply before ordinary staging or sync.

Native encryption continues using canonical source-path attributes and ciphertext
Git snapshots. Destinations are ordinary files. Repository lock/unlock governs
source working trees and keys, not already deployed plaintext destinations.

## Consequences

- The existing flat home layout works without new configuration or file redirects.
- State and lifecycle consumers must identify source associations, not just targets.
- A new target file requires an explicit owner, even beside an application's files.
- Ownership is deliberately conservative after reset, ignore changes, or interrupted
  publication. Retained claims are released through association teardown.
- Legacy exclusive deployments retain their behavior and require explicit teardown
  to adopt projections. A lint-only overlap exception or symlink workaround would
  conceal the state/discovery hazards instead of resolving them.
