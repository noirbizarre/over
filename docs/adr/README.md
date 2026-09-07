# Architecture Decisions

Records of the decisions that shape this project, and — more usefully — the
reasons behind them. An ADR is written when a choice is hard to reverse or
likely to be re-proposed.

The point is not the decision; it is the alternatives that were rejected and
why. A record that only states the outcome saves nobody the argument.

A decision is changed by writing a new ADR that supersedes the old one, never
by editing the old one. The history is the value.

## Format

`NNN-kebab-case-title.md`, numbered in the order written, with the sections:

- **Status** — Proposed, Accepted, or Superseded by ADR-NNN
- **Context** — the forces in play, before any decision
- **Decision** — what was decided
- **Consequences** — what this costs, including what it makes harder

## Index

| # | Decision |
|---|---|
| [001](001-two-binaries-one-crate.md) | Two binaries, one crate |
| [002](002-overlay-is-a-directory-not-a-repository.md) | An overlay is a directory and a cascading descriptor chain, not a git repository |
| [003](003-overlay-descriptors-tolerate-any-format-on-read.md) | Overlay descriptors tolerate any format on read; `format` only governs what `over new` writes |
| [004](004-install-config-is-declarative-with-a-shell-escape-hatch.md) | Install configuration is declarative package sets with an explicit shell escape hatch |
| [005](005-async-is-scoped-to-io-concurrency.md) | Async is scoped to I/O concurrency, not a scheduled execution plan |
| [006](006-symlink-first-apply-with-escape-hatches.md) | Symlink-first apply, with two explicit widening escape hatches |
| [007](007-git2-is-not-confined-to-actions-git.md) | `git2` is a direct dependency of the CLI layer, not confined to `actions::git` |
| [008](008-uses-composition-is-a-flat-union.md) | `uses` composition is a flat union of independent trees, not a layered override |
| [009](009-lint-is-a-separate-tolerant-pass.md) | `over lint` is a separate, tolerant, pre-deserialization static-analysis pass |
| [010](010-templating-is-scoped-to-path-strings.md) | Templating is scoped to path-resolution strings, not file content |
| [011](011-plan-then-execute-reconciliation.md) | Plan-then-execute reconciliation, amending ADR-005 and ADR-006 |
| [012](012-materializer-backend-registry.md) | Materializer backend registry, amending ADR-011 |
| [013](013-diff-has-its-own-taxonomy-and-uses-similar-for-content-diffs.md) | `diff` has its own taxonomy and uses `similar` for content diffs, amending ADR-011 |
| [014](014-bidirectional-checkout-synchronization.md) | Bidirectional checkout synchronization is `over sync`, scoped to an overlay's root git entry |
| [015](015-unapply-reuses-plan-classification-and-only-removes-safely-reversible-state.md) | `unapply` reuses `Plan`'s classification and only ever removes safely reversible state |
| [016](016-partial-file-managed-blocks.md) | Partial files are managed blocks injected via a sidecar, not a stem-plus-sidecar pair |
| [017](017-materialization-rules-generalize-link-dirs.md) | Materialization rules generalize `link_dirs` into path/subtree overrides, amending ADR-006 |
| [018](018-root-declared-overlays-extend-descriptor-discovery.md) | Root-declared `overlays:` paths extend descriptor-glob discovery, amending ADR-002 |
