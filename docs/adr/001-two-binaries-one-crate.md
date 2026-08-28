# ADR-001: Two binaries, one crate

**Status:** accepted

## Context

`over` needs to work two ways: as a standalone CLI (`over add`, `over
apply`, ...) and as a Git subcommand (`git over mount`, `git over add`, `git
over status`). Git only resolves `git <verb>` to an executable named exactly
`git-<verb>` on `PATH` — there is no way to make `git over mount` dispatch
into a subcommand of the `over` binary itself.

The two surfaces also don't need the same arguments. `git-over` runs from
inside an arbitrary git working directory and discovers the repository and
its mounted overlay from `.git/config` (`over.overlay`); `over` takes an
explicit overlay name or root. Forcing them through one `clap::Parser` tree
would mean every `over` flag also has to make sense for `git-over`, and vice
versa.

## Decision

One crate (`dot-over`), two `[[bin]]` targets: `over` (`src/main.rs`) and
`git-over` (`src/bin/git-over.rs`). Each is an independent `clap::Parser`
CLI with its own `Commands` enum (`src/cli/mod.rs` and
`src/cli/git_over/mod.rs`). What they share is the library: `utils::
resolve_home` and the `Overlay`/`Repository` domain types, not a common
argument tree.

## Consequences

Users get `git over mount` for free the moment `git-over` is on `PATH` —
Git's own subcommand mechanism does the dispatch, and nothing in `over`
itself has to know `git-over` exists.

The cost is duplication: both binaries repeat the `--home`/`--debug`/
`--verbose` global flags, their own `init_tracing` call, and their own help
text. Adding a flag to one CLI does not propagate to the other, and there is
no single "list every `over`/`git-over` command" help surface — a user has
to know to run `--help` on each. Release packaging (Homebrew, AUR) also has
to install and reference both binaries explicitly, rather than one.
