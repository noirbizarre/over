# `over lint`

Check overlays for configuration issues without applying or repairing anything.
Lint reads source files and relevant destination metadata to resolve ownership;
it never changes target directories, Git data, or state, and never runs hooks.

A spinner on terminal stderr shows that checks are running. Diagnostics and
`--json` output remain on stdout, with no animation in redirected stderr.

```text
Usage: over lint [OPTIONS] [OVERLAY]

Arguments:
  [OVERLAY]  Check only this overlay and its uses dependencies

Options:
  -H, --home <HOME>      Configuration and overlays root [env: OVER_HOME=]
  -r, --target <TARGET>  Set the target directory (defaults to ~) [env: OVER_TARGET=]
  -d, --debug            Toggle debug traces
      --no-uses          Skip linting the selected overlay's uses dependencies
      --json             Output diagnostics as JSON
  -v, --verbose          Toggle verbose output
  -h, --help             Print help
```

Without an overlay name, lint checks the whole repository. Supply a name to
check only that overlay and its transitive `uses` dependencies. Add `--no-uses`
to skip dependency contents and dependency-spanning cycle checks; direct
references to missing overlays are still reported. With no name, `--no-uses`
has no effect because every overlay is checked independently.
Relevant inherited and repository-root configuration is still validated when
linting a named overlay, including with `--no-uses`.

Root `overlays` declarations are checked independently of discovered overlays.
Malformed globs and nonexistent literal paths are errors. Globs matching no
usable overlay directories produce warnings, so intentionally optional patterns
do not fail lint. Files and the `.over` administration directory do not count as
overlay matches. Paths resolve relative to the source repository root, regardless
of `--target`. Findings identify the root descriptor, the exact path or pattern,
and its one-based `overlays[N].path` entry, with a hint for fixing it.

Destinations resolve using the current machine, consumer metadata, and your home
directory as `~`. Use `--target` or `OVER_TARGET` to inspect a different deployment root; `--home`
selects the source repository. Unavailable template variables are errors unless
the template handles them with a default or guard. Dependency metadata still
participates with `--no-uses`, so broken metadata dependencies can prevent
destination resolution even when their files are skipped.

```sh
over lint
over lint hosts/laptop
over lint hosts/laptop --no-uses
over lint hosts/laptop --target /tmp/over-preview
```

Both `lint` and `apply` inspect descriptor keys before deserialization. Unknown
keys, including nested and removed settings, are ignored with warnings; a typo
such as `targt` can include a "did you mean `target`?" hint. Lint reports these
warnings once per declaration and continues checking recognized settings, so a
warning does not hide a genuine configuration error. It also continues past a
broken overlay to report on anything that `uses` it, and flags dependency cycles. See
[ADR-009](../adr/009-lint-is-a-separate-tolerant-pass.md).

## Diagnostic output

Findings are grouped by overlay, then file, in alphabetical order, with errors
before warnings within each file. Findings without an associated file appear
directly beneath the overlay, before file groups. Only overlays with findings
are shown; the repository-root overlay is displayed as `.`. Overlay names are
cyan and file paths blue. Inline configuration references and commands use
bold emphasis with the default foreground, without backticks in human output.
Hints and multiline details appear beneath their finding.

Configuration and graph errors stay on the overlay responsible for them. Every
transitive consumer receives a warning identifying the invalid overlay, once per
consumer and invalid dependency even when multiple dependency paths reach it.
For example, if `alpha` uses `middle`, which uses a broken `zulu`:

```text
alpha
  warning: depends on overlay "zulu" which has configuration errors
    = fix the errors in "zulu" first

middle
  warning: depends on overlay "zulu" which has configuration errors
    = fix the errors in "zulu" first

zulu
  error: ...
```

A consumer's own errors remain errors. Ownership conflicts and resolution
failures that depend on a composition's metadata remain errors on that
composition. Warnings alone do not make a dependency invalid. Lint exits
unsuccessfully while any original errors remain; warnings alone do not fail lint.

For a finding associated with a file, the file heading appears above it:

```text
desktop
  over.toml
    warning: unknown key targt ignored
      = did you mean target?
```

### JSON output

Use `over lint --json` for agents and scripts. Stdout contains only a JSON report,
including when there are no findings:

```json
{
  "diagnostics": [
    {
      "severity": "warning",
      "overlay": "desktop",
      "message": "unknown key `targt` ignored",
      "hint": "did you mean `target`?",
      "file": "over.toml"
    }
  ],
  "error_count": 0,
  "warning_count": 1
}
```

Diagnostics form a flat list in canonical overlay/severity/message order.
Severity is `error` or `warning`; absent `hint` and `file` values are `null`.
Messages and hints retain their original text, including backticks. The root
overlay's identity is an empty string, displayed as `.` only in human output.
A clean result has an empty `diagnostics` array and zero counts.

Terminal output is pretty-printed using the default foreground. Piped or redirected
output is compact JSON with a trailing newline and no ANSI colors, even when
color is forced through the environment.

Exit status is unchanged: warnings succeed, errors fail after emitting the
report. Error summaries and operational failures are reported on stderr;
operational failures before lint results are available do not emit a JSON report.

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
