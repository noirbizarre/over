# ADR-026: Git-aware diff delegates presentation to Git

**Status:** accepted. Supersedes ADR-013's command presentation and amends the
diff behavior in ADR-022/025.

## Context

A healthy deployment symlink can point to staged and unstaged source changes.
A virtual checkout instead has independent target files and a recorded source
baseline. A desired-versus-actual filesystem report cannot represent both as
Git changes. A custom patch renderer also bypasses user-configured pagers,
attributes, external diff drivers, and GUI difftools.

## Decision

Resolve comparisons separately from deployment diagnostics using the shared
desired tree, resolved overlay policy, directory mappings, and ownership
scopes. Retain source selection metadata for historical and index paths that
are absent from the current source filesystem.

Default to unstaged changes; expose `--staged`/`--cached` and `--all-changes`.
Symlink comparisons use source index/worktree and HEAD/index. Virtual checkout
comparisons use source index/target and recorded base/index. Label the recorded
revision explicitly because it can differ from source HEAD. Include owned new
target files as additions. Keep broken and misdirected links as diagnostics.

Delegate presentation to `git diff`, or explicitly to `git difftool` with
`--difftool`, behind `actions::git`. Preserve the source repository's Git
configuration context and terminal streams so Git handles pager precedence,
external drivers, attributes, text conversion, and tool selection/options.
Do not select a GUI merely because `diff.tool` is configured.

Virtual comparisons construct filtered trees in private temporary storage.
Project mapped targets back to logical source paths, apply source clean
filters, and preserve Git executable modes. Source objects are read-only
alternates; generated objects and index copies stay in scratch storage.
Disable textconv cache persistence without disabling text conversion. Read
recorded associations without creating XDG state or adopting deployments.

## Consequences

- Git must be available on `PATH` to present Git changes.
- A clean overlay health report does not imply an empty diff, or vice versa.
- Recorded-base staged views can differ from `git diff --cached` after HEAD
  advances; this is intentional and documented.
- Git configuration remains the only tool configuration. Temporary comparison
  inputs live until the selected Git command exits.
- Headers and path diagnostics use stderr; Git patches use stdout.
- Partial-block diagnostics keep `ContentDiff`/`similar`; declared Git entries
  retain ADR-021's provisioning boundary. The legacy reconciliation report
  remains available to library consumers, but no longer drives `over diff`.
