# Materialization

Materialization is how an overlay's source entries become files at the target.
Choose it for the editing workflow you want, independently of which overlays
you compose with `uses`.

## Modes

Both root and overlay descriptors accept `mode` and ordered `rules`.

| Mode | When to use it | Behavior |
| --- | --- | --- |
| `symlink` | Share source files while retaining real target directories | Recurse and link individual files; implicit fallback |
| `symlink-directory` | Share an entire directory as one unit | Link the directory without separately traversing its children |
| `checkout` | Edit ordinary target files and record selected changes back to the source | Materialize tracked source content as a virtual checkout, without a target `.git` |

The overlay root remains a directory scaffold under either symlink mode.
`symlink-directory` links child directories as units; ordinary files encountered
during traversal are still individually symlinked. Modes select directory units,
not arbitrary single-file checkouts. New source files in a linked directory are
visible through the link; new individually linked files require another apply.

Root `checkout` owns the whole overlay except independently mapped directories.
Initial checkout materialization reads committed source `HEAD`, so commit source
content before applying it. It is distinct from a declared `git` clone.

## Ordered precedence

For each source path and each property (`mode` and `permissions`), Over resolves:

1. The implicit mode `symlink`, with permissions unmanaged.
2. The repository root's explicit baseline, then its rules in declaration order.
3. Each intermediate descriptor down to the overlay: its explicit baseline,
   then its rules in declaration order.

The **last matching assignment wins per property**. Specificity does not rank
rules. A mode-only rule leaves permissions unchanged, and vice versa. Omitted
baselines preserve earlier decisions. Child rule lists append to the cascade;
`rules: []` does not clear ancestor rules.

```yaml
# ~/.dotfiles/over.yaml
mode: symlink
rules:
  - path: apps/editor/config
    mode: symlink-directory
```

```yaml
# ~/.dotfiles/apps/editor/over.yaml
mode: symlink
rules:
  - path: config/private
    mode: checkout
  - path: config
    mode: symlink
  - path: tools/*
    mode: symlink-directory
```

The child baseline resets the root directory-link decision. Its later `config`
rule wins over `config/private`, so both use individual-file symlinks. Immediate
child directories of `tools` are whole-directory links. Reversing the two
`config` rules would select a checkout for `config/private` instead.

`uses` composes independent overlays; its order does not make conflicting file
ownership safe or add descriptors to another overlay's inheritance chain.
[`over lint`](../usage/lint.md) diagnoses composed target conflicts and explains
overlapping rules with the winning declaration for each property.

## Rule selectors

Literal `path` selectors are component-aware source prefixes: `foo` selects
`foo` and `foo/bar`, never `foobar`. `.` selects the declaring directory and
descendants. Paths retain their [declaring scope](../configuration.md#path-resolution),
even when policy is inherited or destinations are mapped elsewhere.

Globs are supported **only** when a rule explicitly sets
`mode: symlink-directory`. They select directory boundaries: `tools/*` selects
immediate child directories, not files or grandchildren. Rules selecting
`symlink`, `checkout`, or permissions alone use literal prefixes. Every rule
must set mode, permissions, or both. Excludes and discovery use their own glob
languages.

## Atomic boundaries and mappings

An effective directory link owns its entire subtree; descendant rules cannot
change what is inside it. A checkout is also an atomic directory unit:
descendant mode rules do not split it. Lint reports unreachable overrides with
the blocking declaration and a repair hint. Reopen a boundary with a later
same-path assignment or a nearer baseline selecting `symlink`, then select the
desired child units.

[Directory mappings](overlay.md#directory-target-mappings) create explicit
ownership boundaries with their own effective mode. With checkout mode, the
residual overlay and each mapped directory are separate checkout units. A
mapping cannot hide inside a whole-directory link that already owns its subtree.

A whole-directory link is rejected before changes if it would expose ignored
children or control metadata. Use file-level symlinks or checkout instead.

## Other materialization mechanisms

These declarations complement modes; they are not additional `mode` values:

| Mechanism | Purpose | Reference |
| --- | --- | --- |
| Explicit soft/hard link | Link a named target entry to a configured referent | [Link sidecars](overlay.md#link-sidecars) |
| Managed block | Write a literal block inside an otherwise independent file | [Partial files](overlay.md#partial-files) |
| Declared Git repository/worktree | Provision a real repository with its own history and configuration | [Git declarations](overlay.md#git-declarations) |
| Directory scaffold | Create target directories needed for independently managed children | [Modes](#modes) |

Sidecars and descriptors are control metadata, never ordinary deployed files.

## Editing and lifecycle

For symlinks, editing the target edits the linked source. For virtual checkouts,
the source and target are separate snapshots:

1. Edit ordinary target files.
2. [`over add --overlay NAME PATH`](../usage/add.md) stages selected target
   snapshots in the source index.
3. [`over commit NAME`](../usage/commit.md) commits staged changes only; later
   unstaged target edits remain outside that commit.
4. [`over sync NAME`](../usage/sync.md#virtual-checkouts-mode-checkout)
   reconciles source advancement and target changes locally. Applying an existing
   checkout does not replace this workflow.

Virtual checkout sync performs no network I/O. Syncing a root declared Git
repository can fetch, merge, and push; nested declared repositories are provisioned
but not synchronized by Over. Use [`status`](../usage/status.md) and
[`diff`](../usage/diff.md) to inspect state before recording or reconciling it.

Before changing mappings on an existing checkout, commit or resolve changes,
unapply with the old configuration, change the mapping, and apply again.
Ownership associations are not silently reused for different layouts. Review
the plan with `over apply NAME --dry-run` when changing modes; `--force` does not
bypass all ownership and data-preservation checks.

## Migrating older descriptors

Removed `default`, `defaults`, and `materialization` settings are rejected; use
`mode`, scalar `permissions`, and `rules`. Replace `link_dirs` with rules using
`mode: symlink-directory`. Move old permission-rule arrays into the shared
`rules` list. Relative mapping destinations follow declaration-relative path
resolution, just like the base target.
