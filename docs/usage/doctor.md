# `over doctor`

Check the environment and overlays for issues, aggregating diagnostics that
are each already computed by a focused, existing pass — closest to
chezmoi's `doctor`.

```
Usage: over doctor [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to check (every overlay otherwise)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -d, --debug        Toggle debug traces
      --no-uses      Do not process uses
      --fix          Repair malformed git-exclude blocks (over's own corrupted markers only)
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

The report is split into four sections:

| Section | Source | What it covers |
|---|---|---|
| Environment | doctor itself | `git` on `PATH`, the XDG state directory is writable. |
| Overlay configuration | `over lint` | Every overlay descriptor issue `over lint` would report. |
| Git-exclude | `over status` | `.git/info/exclude` drift for overlay-managed paths (the same diagnostics [`over status`](status.md) reports). |
| XDG state | doctor itself | Stale `over sync`/virtual-checkout bookkeeping records (a target that no longer exists, or an overlay that no longer resolves). |

Without `--verbose`, only findings that need attention are shown; a section
with nothing to report prints "No issues found". `over doctor` exits
non-zero only when a finding's severity is `Error` — a warning (drift that
self-heals on the next `over apply`, or a stale bookkeeping record) never
fails the process, mirroring `over lint`'s "errors fail, warnings don't"
contract.

Unlike [`over status`](status.md)/[`over diff`](diff.md)/[`over
sync`](sync.md), `NAME` here only narrows the Git-exclude section to a
single overlay's managed targets — Overlay configuration and XDG state
always cover every overlay in the repository. A configured
`default_overlay` is therefore never consulted, and there is no `--all`
flag: since only one of four sections could ever be narrowed, doctor always
reports on everything unless you name a specific overlay.

## `--fix`

`--fix` repairs **only** a malformed `.git/info/exclude` block: a stray,
unmatched `# >>> over: exclude:<name> >>>`/`# <<< over: exclude:<name> <<<`
marker line left behind by external corruption. Unlike every other drift
(`Missing`/`Modified`/`Orphaned`), a malformed block never self-heals on the
next `over apply` — it stays stuck until repaired, which is why it's the
one exclude finding severe enough to fail `over doctor` on its own.

Repair strips the stray marker line(s) and appends a fresh, well-formed
block with the current expected content — it never touches any other block,
any hand-written content, or any of `over`'s own drifted-but-well-formed
blocks. See [ADR-024](../adr/024-doctor-aggregates-diagnostics-and-repairs-only-its-own-corruption.md).
