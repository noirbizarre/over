# ADR-023: Merge `git-over` into `over` as a nested `git` subcommand, superseding ADR-001

**Status:** accepted

## Context

ADR-001 shipped two `[[bin]]` targets — `over` and `git-over` — because Git
only resolves `git <verb>` to an executable named exactly `git-<verb>` on
`PATH`, and clap's derive API at the time had no way to make `git over
mount` dispatch into a subcommand of the `over` binary itself. That
mechanism was the *entire* reason a second binary existed; ADR-001 is
explicit that the two CLIs shared nothing but the library (`utils::
resolve_home`, the `Overlay`/`Repository` domain types), not an argument
tree.

That constraint no longer holds. `clap::Subcommand` supports nesting a
whole second `Subcommand` enum inside a variant of the first one
(`#[clap(subcommand)] Git(git::Commands)`), which is exactly the shape
needed to expose `mount`/`add`/`status` as `over git <verb>` without a
separate parser, a separate `main()`, or a separate binary on `PATH`. The
"different arguments" half of ADR-001's rationale was never really about
argument *shape* — `git-over`'s three commands only ever needed the same
three global flags (`--home`/`--debug`/`--verbose`) `over` already has —
it was about *not being forced through one `clap::Parser` tree*, which
nesting also solves without merging the trees.

## Decision

One `[[bin]]` target: `over` (`src/main.rs`). `src/cli/mod.rs`'s `Commands`
enum gains a `Git(git::Commands)` variant, and `src/cli/git/{mod,add,mount,
status}.rs` (moved from `src/cli/git_over/`) becomes a plain nested
subcommand module: no `CLI` struct, no `Parser` derive, no `main()`. Every
`execute()` in `add.rs`/`mount.rs`/`status.rs` now takes `&crate::cli::CLI`
— the single, shared struct — instead of its own parser type; the shared
helpers (`discover_repo`, `main_repo_root`, `get_overlay_config`/
`set_overlay_config`, `resolve_overlay`, `exclude_paths`,
`repo_relative_path`) move unchanged, since none of them ever depended on
which `CLI` type called them. `src/bin/git-over.rs` is deleted, and so is
the `git-over` `[[bin]]` entry in `Cargo.toml`.

The dispatch that used to be Git's own `git-<verb>` PATH resolution is now
`over`'s own `clap::Subcommand` match: `over git mount`, `over git add
<files>`, `over git status`.

## Consequences

The costs ADR-001's own Consequences section named are gone: one
`clap::Parser` tree means one `--help`/`--version` surface, one
`init_tracing` call, and a flag added to the shared `CLI` struct now
reaches `over git ...` for free instead of needing a second edit. Release
packaging (Homebrew, both AUR packages, the Docker image, the devcontainer
install script) now builds and ships exactly one binary.

The one real user-visible behavior change: **`git over mount` typed as a
literal Git-invoked command stops working.** Git resolved it by finding
`git-over` on `PATH`; with that executable gone, Git reports `git: 'over'
is not a git command`. Only `over git mount` (invoking `over` directly)
works now. This is accepted as the point of the change, not an oversight —
anyone scripting or documenting the old form needs `over git mount`
instead, which is why every doc, packaging template, and CI check
referencing the old invocation was updated alongside this ADR.

Nothing about *what* `mount`/`add`/`status` do changed — repository
discovery, `.git/config` reads/writes, and worktree handling are the exact
same code, just reached through a different dispatch path.
</content>
