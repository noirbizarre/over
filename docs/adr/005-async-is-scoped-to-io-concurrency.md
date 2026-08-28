# ADR-005: Async is scoped to I/O concurrency, not a scheduled execution plan

**Status:** accepted

## Context

Applying an overlay touches many independent resources: files to symlink,
git repositories to clone, packages to install. A scheduler that built a
plan of independent actions and ran them with bounded parallelism was one
option (`src/exec/plan.rs` exists as a named module for exactly this). A
simpler sequential walk, parallelizing only where ordering genuinely doesn't
matter, was the other.

## Decision

`Overlay::apply_inner` is a sequential, recursively-async function: `uses`
entries apply before the overlay's own files, and each one is awaited before
moving on. `visited`/`stack` sets make diamond dependencies safe and cycles
an error — logic that is easier to reason about single-threaded than across
a concurrent scheduler.

The one place that genuinely runs in parallel is `actions::git::
clone_repositories`, which `join_all`s a `tokio::spawn`ed task per declared
git repository, because independent clones have no ordering constraint
between them. Elsewhere, `async`/`#[async_trait] Action` exists mainly so
that *blocking* filesystem syscalls and interactive `dialoguer` prompts can
run inside `tokio::task::spawn_blocking` without stalling the runtime — not
because those operations benefit from concurrency.

`src/exec/plan.rs` remains an empty, unused file: scaffolding for a real
execution plan that was never built.

## Consequences

Ordering-sensitive behavior (conflict prompts, `uses` applying fully before
its dependents write on top) stays simple to follow. The cost: applying an
overlay with many files or many `uses` entries is not parallelized beyond
the one git-clone hot path — large trees still apply file-by-file,
sequentially. The dead `plan.rs` file is also a standing source of confusion
about where execution-planning logic is meant to live; it should either be
implemented or removed.
