# `over lint`

Check overlays for configuration issues, without touching any target
directory.

```text
Usage: over lint [OPTIONS] [OVERLAY]

Arguments:
  [OVERLAY]  Check only this overlay and its uses dependencies

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
      --no-uses      Skip linting the selected overlay's uses dependencies
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

Without an overlay name, lint checks the whole repository. Supply a name to
check only that overlay and its transitive `uses` dependencies. Add `--no-uses`
to skip dependency contents and dependency-spanning cycle checks; direct
references to missing overlays are still reported. With no name, `--no-uses`
has no effect because every overlay is checked independently.
Relevant inherited and repository-root configuration is still validated when
linting a named overlay, including with `--no-uses`.

```sh
over lint
over lint hosts/laptop
over lint hosts/laptop --no-uses
```

Unlike `apply --dry-run`, `lint` never deserializes a descriptor blindly: it
pre-validates the raw keys first, so a typo (e.g. `targt` instead of
`target`) is reported as "did you mean `target`?" rather than as a raw
`serde` error. It also continues past a broken overlay to report on anything
that `uses` it, and flags cycles in the `uses` graph. See
[ADR-009](../adr/009-lint-is-a-separate-tolerant-pass.md).

## Mode and permission rules

Lint validates the shared `rules` list and reports obsolete configuration keys
with migration hints. A rule must set `mode`, `permissions`, or both. Literal
paths select a path and its descendants, respecting component boundaries.
Globs are accepted only for rules selecting `mode: symlink-directory`; they
select directory boundaries. Other modes and permissions-only rules cannot
use globs.

For example, this descendant override is unreachable:

```yaml
rules:
  - path: config
    mode: symlink-directory
  - path: config/editor
    mode: symlink
```

Over cannot apply rules inside a single directory symlink. Lint identifies
both declarations and suggests removing the descendant override or changing
the ancestor mode to `symlink`. Checkout also owns an atomic directory unit;
descendant mode overrides are reported unless they belong to an independently
mapped unit. A later same-path assignment or scope baseline that reopens the
ancestor is respected. Permission overrides for independently processed
partial files are checked separately from ordinary directory traversal.

Provably shadowed properties produce warnings; complementary mode-only and
permissions-only assignments are valid. Unreachable mode overrides are errors
and make lint exit unsuccessfully. Doctor includes the same lint findings.
