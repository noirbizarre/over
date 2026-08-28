# ADR-006: Symlink-first apply, with two explicit widening escape hatches

**Status:** accepted

## Context

Applying an overlay could copy its files into the target, or symlink them.
Copying is what many dotfile tools do; symlinking keeps the overlay-tracked
file and the target the same file, so an edit at either end takes effect
immediately, at the cost of the target filesystem now visibly containing
symlinks.

A single tree-wide choice (symlink every file, or symlink the whole overlay
as one directory) does not fit every case: some directories are cheaper to
link as a unit, and some targets legitimately live outside the overlay tree
entirely.

## Decision

Default behavior is per-file symlinking (`actions::fs::link` walks the
overlay tree and emits one link per file). Two explicit, opt-in widenings
exist on top:

1. `link_dirs` glob config lets a directory be symlinked as a single unit
   instead of walked file-by-file — avoiding thousands of individual links
   for something like a `node_modules`-shaped tree.
2. `*.link.{toml,yaml,yml}` sidecar files (`src/actions/symlink.rs`) declare
   arbitrary extra links (soft or hard) to paths outside the overlay tree,
   with the target templated via minijinja.

Conflicts are resolved lazily, action-by-action, during application
(`resolve_file_conflict`/`resolve_dir_conflict`: skip, overwrite, absorb,
diff) rather than computed up front as a whole-apply plan.

## Consequences

Live-editability is the main win: change a tracked file, the target sees it
without re-running `apply`. The cost is a filesystem that visibly contains
symlinks, which some tools and IDEs treat differently from a real file.
Because conflicts are resolved per-action rather than pre-flighted, a
multi-file apply can succeed partway and then stop on a conflict with
`--no-prompt`, leaving the target partially applied — there is no
whole-apply rollback, only the per-file rollback `add_file` already has. The
interactive "Absorb" choice also mutates the overlay *source* from live
target state, which is a surprising, hard-to-undo outcome if picked by
accident in a prompt loop.
