# ADR-020: File permissions are hierarchical rules scoped to `PartialFile` entries

**Status:** accepted

## Context

#107 deliberately left `DesiredEntry` without a `permissions` field,
anticipating this issue (#65) rather than guessing its shape. Two things
make permission metadata harder than it first looks, given ADR-006's
symlink-first architecture:

- Almost every `DesiredEntry` (`SymlinkFile`/`SymlinkDirectory`) is a
  symlink to a file still tracked in the overlay's own source tree. A
  symlink shares its target's inode: there is no independent "permission of
  the target" to set — the only real mode is the overlay source file's own,
  visible through the link. Managing it would mean `over` chmod'ing its own
  tracked source file as a side effect of `apply`/`status`/`diff`, which are
  otherwise all read-only-until-`execute` (`Plan::build`, `status::Report`,
  `diff::Report` never mutate anything) — a much bigger, more surprising
  commitment than this issue asks for.
- Git only tracks a single executable bit (`100644` vs `100755`), not
  arbitrary mode bits — a `0600` SSH key or credential file committed to an
  overlay repo will not keep that mode across a clone. Nothing in `over`
  today re-asserts a declared permission policy after materialization.
- #113/ADR-017 already generalized `link_dirs` into a `defaults`/`rules`
  hierarchical override model for materialization strategy, resolved
  path-by-path while walking the overlay tree.

## Decision

Mirror ADR-017's `defaults`/`rules` shape for permissions
(`src/overlays/permissions.rs`: `FileMode`, `PermissionRule`, `resolve`),
but scope it to the one intent where `over` writes real, target-owned
content directly today:
[`MaterializationIntent::PartialFile`](../../src/desired/entry.rs) (#66).
Every other intent (`Directory`, `Checkout`, `SymlinkFile`,
`SymlinkDirectory`) always carries `DesiredEntry.permissions: None` and is
never compared or enforced this way — see the regression test
`symlinked_file_never_carries_a_permission_even_with_a_matching_rule` in
`src/desired/tree.rs`.

- `Overlay::permissions: Option<Vec<PermissionRule>>` (`[[permissions]]
  path = "...", mode = "..."`) and `Overlay::defaults.mode: Option<FileMode>`
  cascade through the same descriptor chain as every other field
  (ADR-002), resolved with the same specificity precedence as
  `MaterializationRule` (most specific match, else `defaults.mode`, else
  `None` — unmanaged).
- Resolution matches against a `PartialFile` entry's own sidecar-relative
  stem (e.g. `ssh/agent` for `ssh/agent.partial.toml`), the same identity
  `.link.*`/`.partial.*` sidecars already use elsewhere — not the rendered,
  possibly-external `target` path, and not a path walked from the overlay's
  own file tree the way `MaterializationRule` is.
- A mismatch on an otherwise-correctly-materialized entry (content already
  matches, only the mode differs) is a new `Operation::Repair { current,
  desired }` — actionable like an unblocked `Migrate` (no prompt, no
  `--force`), never a `Conflict`: nothing structural is wrong, only a
  `chmod` is needed. `status` reports it as `Modified`; `diff` reuses the
  existing line-diff renderer for a trivial `644 -> 600` text diff, exactly
  like the symlink-target-path case already does — no new `Change` variant.
- Materialization is a plain `fs::set_permissions` (`actions::fs::
  SetPermissions`, `#[cfg(unix)]`-gated; a no-op on non-unix, where
  permission bits aren't a meaningful concept to enforce), applied either
  right after `EnsurePartialBlock` writes new/updated content, or on its
  own for a pure `Repair` step.

## Consequences

Permission tracking only ever affects the file `over` itself writes
content into — a symlinked file's permission remains entirely a function of
the overlay source file, completely unmanaged by this feature, exactly the
same as before #65. `#61`'s future rendered whole-file content is a natural
extension of the same mechanism (another intent that writes real content
`over` owns) with no redesign needed.

A `permissions`/`defaults.mode` rule whose `path` never actually matches
any `PartialFile` sidecar's stem silently has no effect — the same class of
"no-op override" already possible with an unrelated `rules` entry (ADR-017)
whose glob matches nothing on disk. `over lint` gains matching checks
(`check_empty_permissions`, `check_invalid_permission_globs`,
`check_duplicate_permission_rule_paths`), mirroring the existing `rules`
checks, but deliberately has no "paths exist" check the way
`check_rules_paths_exist` does: a permission rule's matching key is a
sidecar stem, not a real path in the walked tree, so "exists on disk"
doesn't apply the same way.

Because `permissions`/`defaults` are replaced wholesale by the nearest
config level that defines them, not merged element-wise (the same
limitation ADR-017 already documents for `rules`), an overlay that defines
its own `permissions` list loses the repository root's entries entirely
unless it repeats them.
