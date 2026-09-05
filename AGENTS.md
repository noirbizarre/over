# AGENTS.md

Notes for anyone — human or otherwise — changing this repository.

## What this project is

A git-based file overlay manager (dotfiles as overlays). `over` is the
standalone CLI; `git-over` is a companion binary that integrates with git
workflows (`git over mount`, `git over add`, `git over status`). Keep changes
minimal and focused.

## Layout

```
src/
├── lib.rs        the library surface
├── main.rs       the `over` binary
├── bin/git-over.rs  the `git-over` binary
├── actions/      filesystem, symlink, install and git side effects
├── cli/          argument types for both binaries
├── desired/      the DesiredTree/DesiredEntry desired-state model
├── exec/         execution context and templating
├── lint/         `over lint` diagnostics
├── materialize/  Materializer backends (registry, symlink)
├── overlays/     the Overlay/Repository domain model
├── plan/         the Plan/PlanStep reconciliation model
└── ui/           logging, styling, emojis
```

Dependencies point inward. Nothing in the library knows a command exists.

## Build/Test

- Full build: `cargo build` (or `mise run build`)
- Format: `cargo fmt --all` (or `mise run format`)
- Lint: `cargo clippy --all-targets --all-features -- -Dclippy::all` (or `mise run lint`)
- Test all: `cargo nextest run` (or `mise run test`)
- Single test: `cargo nextest run --test <file> -- <name::path>` or fallback `cargo test <name>`
- Coverage: `cargo llvm-cov nextest` (or `mise run cover`)

## Style

- Edition 2024; use `anyhow::Result` for fallible public fns; prefer `?` and propagate errors; avoid `.unwrap()` outside tests unless guaranteed.
- Imports: group std / external / crate; avoid wildcard; keep ordering lexical; re-export only intentional items (see `lib.rs`).
- Types: use explicit `PathBuf`, `Arc<Context>`; alias errors with `Result<T, anyhow::Error>`; prefer enums over strings for state.
- Naming: snake_case for functions/vars, PascalCase for types/traits; modules concise (`fs`, `git`); constants UPPER_SNAKE; avoid abbreviations except well-known (`ctx`).
- Async: traits with `#[async_trait]`; pass cloned `Arc` rather than &mut; avoid blocking in async (wrap with `spawn_blocking`).
- Error handling: never silence errors; use context via `anyhow!(...)` or `.with_context(...)`; return early on invalid state.
- CLI: derive `Parser`/`Subcommand`; keep help strings imperative; prefer explicit flags (`--dry-run`).
- Formatting enforced by `cargo fmt`; do not hand-align; trailing spaces removed (prek).
- Tests: use `rstest` for parametrization; assertions via `pretty_assertions` when readability matters; unit tests live beside code under `#[cfg(test)]`.

**Every non-obvious line carries a comment saying why.** Not what — the code
says what. Ideally naming the failure it prevents. A comment that restates the
code is worse than none.

**Test names are sentences.** `an_unchanged_input_produces_no_output`, not
`test_run_2`. The name should say what would be broken if it failed.

## Commits

Conventional Commits, enforced by commitlint on `commit-msg`. The type becomes
a changelog heading, so choose it as if someone will read it in release notes
— because they will.

## Releases

Driven by gh-ship. Never bump a version or push a tag by hand: `cliff.toml`
derives the version from the commit history, `prepare-release` applies it, and
`.github/ship.yml` is the contract between them. See CONTRIBUTING.md.

## Before you push

```sh
mise run ci
```

Formatting, Clippy, spelling, workflow linting, tests and the documentation
build. Same as CI.

## This repository is generated from a template

The toolchain, hooks, CI and release workflows come from
[rust.tpl](https://github.com/noirbizarre/rust.tpl) and are updated with
`git tpl update`. Files carrying template-owned content end with a
`# --- project-specific ---` marker: add below it, never above.

Changing template-owned content here fixes it in one repository. Changing it
in the template fixes it in all of them — prefer that.

General: Do not add new dependencies lightly; prefer existing patterns
(progress bars via `indicatif`, styles via `ui::style`). Update docs only if
user-facing behavior changes.
