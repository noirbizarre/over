# ADR-002: An overlay is a directory and a cascading descriptor chain, not a git repository

**Status:** accepted

## Context

Many dotfile managers version the whole tracked tree as a single git
repository, and the tool's job is largely to keep the working copy and the
repository in sync. `over` could have modeled an overlay the same way — an
overlay *is* a git repository.

Overlays also need to inherit configuration: a subdirectory should be able
to add to, without repeating, the settings of its parents.

## Decision

`Repository` (`src/overlays/repository.rs`) is a plain filesystem root — no
git object anywhere in it. `Overlay::new` (`src/overlays/overlay.rs`) walks
up from the overlay's directory to the repository root, collecting one
config file (`over.toml`/`.yaml`/`.yml`) per ancestor directory, and merges
them with the lowest-priority (closest to the root) first via `config::
Config::builder()`. An overlay's identity is its filesystem path within the
repository, not a git ref.

Git is available *inside* an overlay, opt-in, via the `git:` field
(`src/actions/git/config.rs`): an overlay can declare repositories to clone
into its target as one of several apply steps. It is a resource an overlay
can use, not what an overlay is.

## Consequences

Nothing requires the overlay repository itself to be under version control
— `over` has no opinion on it. Most users do put `~/.dotfiles` under git
anyway, but that's outside `over`'s model entirely: there is no built-in
history, diffing, or sync for the overlay content.

Because identity is filesystem path, renaming or moving an overlay directory
renames the overlay — and breaks anything that referenced its old name
(`uses` entries elsewhere, `git over mount`'s stored `over.overlay` config).
There is no rename operation; only pick-a-new-name-and-fix-references.
