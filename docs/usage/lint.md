# `over lint`

Check overlays for configuration issues without applying or repairing anything.
Lint reads source files and relevant destination metadata to resolve ownership;
it never changes target directories, Git data, or state, and never runs hooks.

```text
Usage: over lint [OPTIONS] [OVERLAY]

Arguments:
  [OVERLAY]  Check only this overlay and its uses dependencies

Options:
  -r, --root <ROOT>  Set the target root directory (~)
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

Destinations resolve using the current machine, consumer metadata, and your home
directory as `~`. Use `--root` to inspect a different deployment root; `--home`
selects the source repository. Unavailable template variables are errors unless
the template handles them with a default or guard. Dependency metadata still
participates with `--no-uses`, so broken metadata dependencies can prevent
destination resolution even when their files are skipped.

```sh
over lint
over lint hosts/laptop
over lint hosts/laptop --no-uses
over lint hosts/laptop --root /tmp/over-preview
```

Both `lint` and `apply` inspect descriptor keys before deserialization. Unknown
keys, including nested and removed settings, are ignored with warnings; a typo
such as `targt` can include a "did you mean `target`?" hint. Lint reports these
warnings once per declaration and continues checking recognized settings, so a
warning does not hide a genuine configuration error. It also continues past a
broken overlay to report on anything that `uses` it, and flags dependency cycles. See
[ADR-009](../adr/009-lint-is-a-separate-tolerant-pass.md).

## Diagnostic output

Findings are grouped under overlay headings in alphabetical order, with errors
before warnings within each overlay. Only overlays with findings are shown; the
repository-root overlay is displayed as `.`.

Configuration and graph errors stay on the overlay responsible for them. Every
transitive consumer receives a warning identifying the invalid overlay, once per
consumer and invalid dependency even when multiple dependency paths reach it.
For example, if `alpha` uses `middle`, which uses a broken `zulu`:

```text
Overlay alpha
  warning: depends on overlay "zulu" which has configuration errors
    = fix the errors in "zulu" first

Overlay middle
  warning: depends on overlay "zulu" which has configuration errors
    = fix the errors in "zulu" first

Overlay zulu
  error: ...
```

A consumer's own errors remain errors. Ownership conflicts and resolution
failures that depend on a composition's metadata remain errors on that
composition. Warnings alone do not make a dependency invalid. Lint exits
unsuccessfully while any original errors remain; warnings alone do not fail lint.

## Composed target ownership

Each selected overlay is checked together with its transitive `uses` dependencies.
Two unrelated overlays are not implicitly composed during repository-wide lint.
Diamond dependencies contribute their entries once.

Distinct owners claiming the same target or overlapping managed namespaces are
errors. Diagnostics identify both overlays, source paths or sidecar declarations,
the resolved destination, and a suggested resolution. Rename, remap, exclude, or
remove a conflicting claim: `uses` is a flat union, so reordering it cannot choose
a winner. Lint does not change apply's existing conflict prompts or force behavior.

Shared directory scaffolds are valid. Directory symlinks and checkouts establish
managed namespace boundaries; checkout mapping holes and exclusions are respected.
Partial-file blocks with distinct markers may share a file, provided their explicit
file permissions agree. Duplicate markers and incompatible file-wide permissions
are errors.

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

Shadowed or overlapping properties within a scope produce warnings identifying
the final effective winner for each property. Later matching assignments win;
nearer scope baselines can reset inherited values. Literal-prefix overlaps are
checked statically, while glob overlaps include a matching source directory as
evidence. Complementary mode-only and permissions-only assignments are valid,
as are intentional overrides between scopes. Unreachable mode overrides are errors
and make lint exit unsuccessfully. Doctor includes the same lint findings.
