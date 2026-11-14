# Materialization

Materialization is how an overlay's source entries become files at the target.
Choose it for the editing workflow you want, independently of which overlays
you compose with `uses`.

## Modes

Both root and overlay descriptors accept `mode` and ordered `rules`.

For missing root configuration, [`over init`](../usage/init.md#choose-a-repository-default)
recommends virtual checkout and persists the selected repository default. Existing
configuration is preserved, and the implicit fallback remains symlink when no
policy selects a mode. Initialization never migrates existing deployments.

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

Fresh checkouts own projected files, sharing destination directories with other
overlays. Initial materialization reads committed source `HEAD`, so commit source
content before applying it. A virtual checkout is distinct from a declared `git` clone.

### Compose flat application overlays

The following root configuration supports applications targeting the same home:

```yaml
mode: checkout
default_overlay: hosts/laptop
overlays: [apps/*, hosts/*]
```

```yaml
# hosts/laptop/over.yaml
uses: [apps/git, apps/ssh]
```

`apps/git/.config/git/config` and `apps/ssh/.ssh/config` become ordinary files at
their corresponding home paths. Applications can also contribute distinct files
to `.env.d`, `.alias.d`, `.config/systemd/user`, and `.local/bin`, or independent
root files such as `.gitconfig`. These directories are shared containers.
Two overlays claiming the same file still conflict, even with identical content.
File/directory conflicts and another overlay's atomic Git repository remain errors.

Unrelated destination files are not discovered as checkout additions. Use
`over add --overlay apps/git ~/.env.d/new-fragment` to assign a new file explicitly.
Existing owned files can infer their overlay. Each application retains its own
recorded base, staged snapshots, and source lifecycle at a shared destination.

Older deployed checkout associations retain their exclusive directory ownership;
upgrading does not narrow them automatically. To adopt projections, resolve edits,
unapply using the old layout, and apply again. Deployed symlinks and recursive
symlink mirrors also require unapplying the old layout before checkout deployment;
`--force` does not convert them.

## Rule configuration

[Overlay Rules](overlay.md#overlay-rules) is the shared reference for selectors,
declaration order, independent target/mode/permission cascading, defaults, and
inference. For example:

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

## Atomic boundaries and mappings

An effective directory link owns its entire subtree; descendant rules cannot
change what is inside it. A checkout remains a source reconciliation unit:
descendant mode rules do not split it, even though fresh destination ownership is
per file. Lint reports unreachable overrides with
the blocking declaration and a repair hint. Reopen a boundary with a later
same-path assignment or a nearer baseline selecting `symlink`, then select the
desired child units.

[Rule targets](overlay.md#overlay-rules) create explicit
ownership boundaries with their own effective mode. With checkout mode, the
residual overlay and each mapped directory are separate checkout units. A
mapping cannot hide inside a whole-directory link that already owns its subtree.
Single-file checkout redirects are unsupported; a file redirect inheriting checkout
mode must redirect a containing directory instead or explicitly select symlink mode.
Independent root files already work inside a root checkout projection without redirects.

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

For ordinary symlinks, editing the target edits the linked source. Template
symlinks instead expose reconstructible rendered artifacts: edit their tracked
source templates to make lasting changes. For ordinary virtual checkout files,
the source and target are separate snapshots:

1. Edit ordinary target files.
2. [`over add --overlay NAME PATH`](../usage/add.md) stages selected target
   snapshots in the source index.
3. [`over commit --overlay NAME`](../usage/commit.md) commits staged changes only; later
   unstaged target edits remain outside that commit.
4. [`over sync NAME`](../usage/sync.md#virtual-checkouts-mode-checkout)
   reconciles source advancement and target changes locally. Applying an existing
   checkout does not replace this workflow.

Use [`over restore PATH`](../usage/restore.md) to discard selected unstaged target
changes, `over restore --staged PATH` to unstage without changing target content,
or `over restore --source REV PATH` to recover historical content as an unstaged
change. Restore overwrites selected content; preview with `--dry-run` first.

Virtual checkout reconciliation performs no network I/O. `over sync` can optionally
pull/rebase and push the Over source repository; all overlay-declared Git repositories
remain independent working repositories managed with Git. Use [`status`](../usage/status.md) and
[`diff`](../usage/diff.md) to inspect state before recording or reconciling it.

Before changing mappings on an existing checkout, commit or resolve changes,
unapply with the old configuration, change the mapping, and apply again.
Ownership associations are not silently reused for different layouts. Review
the plan with `over apply NAME --dry-run` when changing modes; `--force` does not
bypass checkout disposal or ownership checks. Real checkout disposal requires
confirmation or `--discard-checkout`, even when status is clean. Dirty virtual
checkouts and independent mapped destinations remain protected under every flag.
Checkout-to-recursive-link changes require unapplying the old layout first.
See [Safety and recovery](../usage/safety.md) for the full contract.

Template-containing checkouts use complete generated Git snapshots and preserve
local edits through three-way sync. Generated output cannot be staged with
`over add`; [`over commit`](../usage/commit.md#rendered-file-edits) can translate
unambiguous literal edits into source-template patches. See
[Generated checkouts](../usage/sync.md#generated-checkouts)
for snapshot storage, conflict resolution, and local-data security.

## Migrating older descriptors

Removed `default`, `defaults`, and `materialization` settings warn and are ignored; use
`mode`, scalar `permissions`, and `rules`. Replace `link_dirs` with rules using
`mode: symlink-directory`. Move old permission-rule arrays into the shared
`rules` list. Relative mapping destinations follow declaration-relative path
resolution, just like the base target.
