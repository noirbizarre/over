# ADR-024: `over doctor` aggregates existing diagnostics and repairs only its own corruption

**Status:** accepted

## Context

#142's own "Status and diagnostics" section floats an `over doctor` as a
possible follow-up, with one explicit constraint: "it must not silently
rewrite user-owned Git configuration". #149 narrows that into a concrete,
backlog-scoped ask: a `doctor`/repair command specifically for malformed
`.git/info/exclude` blocks — `over status` (#148) already detects and
reports a malformed block (a stray, unmatched begin/end marker line for
`over`'s own `exclude:<overlay>` marker), but never touches it; every write
path (`reconcile`/`unreconcile`, #146/#147) also leaves it strictly alone,
by design (see those issues' own acceptance criteria).

Two designs were on the table:

1. **Minimal**: `doctor` does nothing but repair malformed exclude blocks —
   the literal #149 ask, no environment or config checks.
2. **Aggregator**: `doctor` also surfaces overlay configuration issues
   (`over lint`'s domain) and environment/plumbing sanity (git on `PATH`,
   XDG state directory writable) alongside the exclude repair, in one
   report — closer to chezmoi's `doctor`, which is explicitly the shape a
   prior internal comparison against chezmoi/stow flagged as missing.

The minimal design is easy to reason about but produces a command whose
only job most users will never need (malformed blocks are rare — they
require external interference with `.git/info/exclude`, not normal `over`
usage). The aggregator design risks scope creep: reinventing diagnostic
taxonomies `lint`/`status` already own well.

## Decision

`over doctor` aggregates rather than reinvents. Each source keeps owning
its own diagnostic shape and severity:

- **Overlay configuration** — delegates entirely to
  `lint::lint_repository`; doctor never re-derives overlay validation.
- **Git-exclude drift** — delegates to `git_exclude::diagnose` per overlay,
  reusing `ExcludeDiagnosis`'s own `Display`.
- **Environment** and **XDG state** are the two checks doctor actually
  owns: `git` on `PATH` and the XDG state directory's writability (nothing
  else currently checked this), and stale `sync.toml`/`virtual_checkout.toml`
  records (a target that no longer exists, or an overlay that no longer
  resolves) — both non-authoritative bookkeeping per their own module docs,
  so a stale record is only ever a `Warning`.

`doctor::Finding` stores each source's *already rendered* display line
rather than a new structured format — `lint::Diagnostic` and
`ExcludeDiagnosis` each already have an established, tested human-facing
shape; doctor's job is aggregation, severity-based exit-code counting, and
`--verbose` gating, not reformatting.

Repair stays exactly as narrow as #149 asked: `--fix` calls
`git_exclude::repair`, which touches a (repository, overlay) group's
`.git/info/exclude` block **only** when it is currently
`BlockState::Malformed` for that overlay's own marker. Every other status
(`Ok`/`Missing`/`Modified`/`Orphaned`) is left completely untouched — repair
fixes corruption of `over`'s own marker lines, it never reconciles drift or
fills in a missing block (that stays `reconcile`'s job, run by `over
apply`). This is how #142's "must not silently rewrite user-owned Git
configuration" constraint is honored: the only bytes `--fix` ever writes
are `over`'s own stray marker lines and a freshly generated block using the
same `compute_expectation` the write path already uses — never a
user-authored line, and only on explicit `--fix`, never by default.

A malformed exclude block is promoted to `Error` severity (every other
exclude finding is a `Warning`): `Missing`/`Modified`/`Orphaned` all
self-heal on the very next `over apply` (`reconcile` recomputes the block
from scratch every run), but `Malformed` never does — it stays stuck until
someone runs `over doctor --fix`. That asymmetry is why plain `over doctor`
(no `--fix`) is the one case where exclude drift alone can fail the
process, matching `over lint`'s existing "errors fail, warnings don't"
exit-code contract.

## Consequences

`doctor` adds no new diagnostic vocabulary to learn — every finding traces
back to `over lint` or `over status`'s own already-documented output,
except the two checks doctor owns outright (environment, XDG state), which
follow the same `Ok`/`Warning`/`Error` shape.

The XDG-state check is detection-only in this first version: no `--fix`
support for pruning a stale record. That's an intentional, low-risk
omission (these records are explicitly non-authoritative — losing one only
loses "last synced N days ago"-style bookkeeping) rather than a limitation
that blocks anything; it can be added later without revisiting this
decision.

Because `doctor` calls into `lint`/`git_exclude` rather than owning their
logic, any future change to either module's diagnostics automatically
flows into `doctor`'s report — but it also means `doctor` inherits their
scope limits. It cannot detect anything `lint`/`status` themselves can't
(e.g. it does not walk `Plan`/`DesiredTree` reconciliation status the way
`over status`'s own entries do — only the git-exclude drift layered on top
of that).
