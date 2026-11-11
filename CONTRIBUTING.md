# Contributing to over

## Setup

The toolchain is managed by [mise](https://mise.jdx.dev/); every tool is pinned
to an exact build in `mise.lock`, so a local run and a CI run use the same
versions.

```bash
mise install          # the tools and Git hooks
```

Mise's postinstall hook installs both `pre-commit` and `commit-msg` hooks.
Prek installs Commitizen and its Emotional plugin in an isolated Python environment
for commit-message validation.

Windows CI places build outputs and temporary files on a ReFS Dev Drive.
The shared setup action exports `TMP`, `TEMP` and `TMPDIR` for all subsequent
steps and child processes, with temporary storage outside the cached target directory.

Rust itself is not managed by mise: `rust-toolchain.toml` pins the channel, and
rustup installs it. CI uses `dtolnay/rust-toolchain` because it needs per-job
components and cross-compilation targets.

Dev and test profiles use `debug = "line-tables-only"` to retain line-number
backtraces with smaller outputs and less linker IO. Full debugger variable
information requires rebuilding with `CARGO_PROFILE_DEV_DEBUG=2` and/or
`CARGO_PROFILE_TEST_DEBUG=2`. Changing profiles causes a one-time rebuild;
old artifacts do not shrink in place.

Check/Clippy produces metadata, whereas nextest needs linked test executables.
Some compilation between them is expected. Features, compiler versions, coverage
instrumentation and release profiles also change build inputs. Keep these stable
within an edit loop and select Cargo packages/targets for focused checks; a
nextest runtime filter alone does not necessarily reduce compiled targets.

Before committing or opening a PR, choose checks relevant to your changes;
see [Before you push](#before-you-push) for guidance. Clippy runs through
`mise lint` and in CI, rather than in the commit hooks. `mise ci` is an
optional comprehensive check.

## Tasks

| Command | What it does |
|---|---|
| `mise build` | Build the project |
| `mise cli` | Run `over` from source (passes flags through) |
| `mise test` | Run the tests (accepts nextest selectors) |
| `mise cover` | Run the tests with coverage |
| `mise format` | Format |
| `mise lint` | Clippy, all Clippy lints denied |
| `mise lint:actions` | actionlint over the workflows |
| `mise lint:md` | rumdl over AGENTS.md, CONTRIBUTING.md, README.md and docs/ |
| `mise spell` | typos |
| `mise snapshots` | Review pending insta snapshots |
| `mise check` | Everything that does not modify the working tree |
| `mise ci` | `check` plus the documentation build |
| `mise docs` | Serve the documentation locally |
| `mise changelog` | Preview the changelog for unreleased commits |
| `mise release` | Show the version the next release would take |
| `mise tpl:update` | Bring the rendered template ref up to date |
| `mise tpl:diff` | Show what merging the template would change |

`mise <task>` is a shorthand for `mise run <task>`, but a builtin subcommand of
the same name wins it silently — which is why the format task is `format` and
not `fmt` (`mise fmt` formats `mise.toml`), and why running the binary is
`mise cli` and not `mise run` (`mise run` runs a task). Prefer the explicit
`mise run <task>` in scripts: mise can claim a new name in any release.

## Before you push

Choose checks based on the files changed and the risk. Run relevant tasks
individually rather than launching every check in parallel by default.

- **Rust code:** run `mise lint` (Clippy) and the relevant tests through
  `mise test`, which accepts nextest selectors. Target affected packages
  and tests instead of running the whole suite by default. For example,
  replacing the placeholders with the affected package and test name:

  ```sh
  mise test -p <package> -E 'test(<test_name>)'
  ```

- **Documentation, workflows or tooling:** run the relevant checks, such as
  `mise lint:md`, `mise spell` or `mise lint:actions`, and
  `mise docs:build` when documentation changes warrant a build.
  Documentation-only changes do not require Rust lint or tests.

Broaden validation when the change warrants it. `mise ci` is available as
an optional comprehensive check, not a prerequisite for committing, pushing or
opening a PR.

If a local check is unavailable or prohibitively expensive, proceed with the
commit or PR and report which checks ran, which were skipped and why in the
handoff or PR description. CI provides full validation.

### Configuration schemas

The documentation site publishes `schemas/root.json` and `schemas/overlay.json`,
generated from Rust configuration definitions with the optional `schema`
feature. The documentation generator lives in `docs/generate_schemas.rs`; its
Cargo example target makes it runnable without adding an installed CLI command.
When changing configuration fields or accepted input shapes, update their
schema derives/adapters and validate them:

```sh
mise run schemas:validate
mise run docs:build
```

The documentation workflow validates the schemas and supported/rejected
descriptor shapes before generating and publishing the site. It runs when Rust
schema inputs change, so published assets reflect the current definitions.
Both `mise run docs` and `mise run docs:build` generate the JSON files under
`docs/schemas/`; those files are ignored and must not be committed. To generate
only the schemas, run `mise run schemas:generate`. Validation uses temporary
files and does not require previously generated assets. The normal CLI build
does not enable schema generation.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/), enforced by
Commitizen with [Emotional](https://github.com/noirbizarre/emotional) on `commit-msg`.
The hook checks Emotional's Conventional Commit grammar; subject case and length
remain authoring conventions. The type selects the changelog section, and a `!`
or a `BREAKING CHANGE:` footer drives the version bump — so the message is part
of the release, not paperwork around it.

The official typos hooks check source files on `pre-commit` and commit messages
on `commit-msg`. Message spelling is checked without rewriting the message, so
fix any reported errors before retrying the commit.

## Releases

Releases are run by [gh-ship](https://github.com/noirbizarre/gh-ship).
**Never bump a version or push a tag by hand.**

1. A push to `main` triggers 🚢 Ship, which runs `gh ship run` and selects preparation.
2. `prepare` dispatches 🚀 Prepare Release, which asks git-cliff for the next
   version, writes `CHANGELOG.md`, updates Cargo and the Dev Container Feature's
   version and default binary version together, commits, and uploads a
   `ship.release.json` artifact describing what would ship.
3. gh-ship opens (or updates) the Release PR from `release/next`. Review it.
4. Merging it triggers 🚢 Ship again, where `gh ship run` selects publication: it tags the
   merge commit, creates a draft release, dispatches 📦 Publish Release to
   attach the binaries, then makes the
   release public.

The Docker workflow publishes the release's multi-architecture binary image
before calling the Dev Container Feature workflow. That workflow tests the
matching binary on amd64 and arm64 before publishing
`ghcr.io/noirbizarre/features/over`. PR/main Feature tests use an available image
override so release-preparation metadata can be tested before its image exists.
See [feature contributor instructions](devcontainer/README.md) for local testing
and the new GHCR packages' visibility/access requirements.

Nothing to release is the normal case for step 1, and costs one workflow run
reporting `changed: false`.

`gh ship validate` runs on every pull request, so a broken release contract
fails on the PR rather than mid-release.

### Repository requirements

The release jobs authenticate as a GitHub App, not with `GITHUB_TOKEN` — the
default token cannot trigger workflows, so a Release PR it authored would show
no CI results. That means the repository needs:

- a `release` environment holding the variable `APP_CLIENT_ID` and the secret
  `APP_PRIVATE_KEY`.
- the corresponding GitHub App installed on `noirbizarre/over` with
  Contents, Actions, Pull requests and Issues read/write permissions.

Preparation runs `gh ship sign` to re-create the release commit with GitHub's signature.

Also configure:

- squash-merge settings of `squash_merge_commit_title: PR_TITLE` and
  `squash_merge_commit_message: BLANK`, so the squash commit subject is the
  Conventional Commit title from `.github/ship.yml`.

### Documentation and coverage services

In GitHub Settings → Pages, set Source to **GitHub Actions** before the first
documentation deployment. The workflow uses the `github-pages` environment.

Onboard `noirbizarre/over` to Codecov for coverage and test results.

## This repository is generated from a template

The toolchain, hooks, CI and release workflows come from
[rust.tpl](https://github.com/noirbizarre/rust.tpl):

```bash
git tpl status         # is there a template update pending?
mise run tpl:update    # advance refs/tpl/<id>; may save migrated answers
mise run tpl:diff      # read what merging it would change
git tpl merge          # take it
```

`tpl:update` advances the rendered ref and may save migrated answers in
`.config/git.tpl.toml`. Rendered files reach your branch only through the merge.

`mise install` provides git-tpl through the declared `cargo:` tool. With
cargo-binstall available, mise downloads its release archive rather than
compiling the libgit2 dependency from source.

Files carrying template-owned content — `mise.toml`, `prek.toml`, `Cargo.toml` —
end with a `# --- project-specific ---` marker. Add below it, or put global mise
tools between the begin/end project-specific tools markers. Git's 3-way merge
then preserves your additions across updates. A fix that belongs to every
project belongs in the template, not here.
