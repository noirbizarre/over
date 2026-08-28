# ADR-004: Install configuration is declarative package sets with an explicit shell escape hatch

**Status:** accepted

## Context

`over apply --install` needs to express "these packages should be present"
across several package managers (`archlinux`, `apt`, `brew`, `winget`,
`cargo`, `python`, `node`). A fully declarative model — packages only, no
scripts — is safer and easier to audit and dedupe across `uses`. But it
cannot express manager bootstrapping: adding a Homebrew tap that needs
authentication, or a step that must run before or after a manager without
being itself a package.

## Decision

Packages are typed, serde-flexible values (flat string or full object,
`src/actions/install.rs`), collected into a `BTreeSet` and unioned across
`uses` by `collect_packages` — pure data, sorted, deduplicated, no shell
involved. But `pre`/`post` fields exist at every level (global, per-platform,
per-manager) and run as arbitrary shell via `run_cmd(ctx, "sh", &["-c",
script])`.

Manager *selection* is likewise a fixed rule, not user-authored: `decide_
linux_managers` picks one manager by hard-coded precedence (`archlinux` >
`brew` on Arch, `apt` > `brew` on Debian/Ubuntu) when only top-level configs
are set, but runs *every* configured manager once a platform-specific
override block exists.

## Consequences

The common case — "install these packages" — stays declarative, auditable,
and dedupable; the escape hatch covers what a closed package-list model
cannot. The cost: `--install` is not sandboxed and not idempotency-checked
by the framework — a `pre`/`post` script re-runs on every apply, and its
safety is entirely on the author. The platform-config-present-or-absent
branching in manager selection is also a subtle rule: adding a platform
block changes whether one manager runs or all of them do, which is easy to
get wrong without reading the source.
