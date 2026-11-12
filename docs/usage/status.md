# `over status`

Inspect resolved overlay configuration and reconciliation health, even before
the overlay has ever been applied. `over show` is a compatibility alias with
the same options, output, and exit status.

```text
Usage: over status [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to check (infer context, then default_overlay, then all overlays)

Options:
  -H, --home <HOME>      Configuration and overlays root [env: OVER_HOME=]
  -r, --target <TARGET>  Set the target directory (defaults to ~) [env: OVER_TARGET=]
  -d, --debug            Toggle debug traces
      --no-uses          Skip dependency operations, retaining dependency data
  -a, --all              Check every overlay, retaining configured default_overlay data
  -v, --verbose          Toggle verbose output
  -h, --help             Print help
```

When `NAME` is omitted, [infer from context](selection.md), then use a configured
`default_overlay`. With neither, report on every overlay. Ambiguous context
requires an explicit name; pass `--all` to report on every overlay regardless
of context or default.

Explicit `NAME` and `--all` retain the configured default's
[data context](../configuration/overlay.md#consumer-metadata-data). Each target resolves
its own metadata separately. `--no-uses` skips dependency entries, not their data.

## Example

```sh
over status hosts/laptop
over status hosts/laptop --verbose
over status --all
```

Use verbose output to trace inherited values and individual entries. For actual
content changes, use [`over diff`](diff.md); to create missing entries, preview
[`over apply`](apply.md). See [Examples](../examples.md#compose-a-host) for the
host layout used here.

## Resolved configuration

Each overlay has separate **Configuration** and **Reconciliation** sections.
Configuration includes the source root, resolved target and its origin,
dependencies in composition order, target rules with resolved destinations,
materialization baseline and rule count, declared Git destinations, and
lifecycle-hook counts including install. These describe intent even when targets are missing.

Dependencies and composition are displayed vertically, one overlay per line;
composition retains dependency-first evaluation order. Git declarations show
configuration and contributing scope on indented lines. Entry provenance uses
separate overlay, source, scope and mode lines.

The headings are 📦 **Overlay**, 🛠️ **Configuration** and 🚦 **Reconciliation**.
Overlay names and spinners are cyan; paths and Git repository identities are blue.
Headings and keys are bold white. Other text uses the default foreground, with
dimmed scope annotations. Green, yellow and red are reserved for success, warning and
error semantics. Nonzero reconciliation counts and entry
labels share severity colors: applied is green, changes and missing entries
yellow, and broken entries or conflicts red. Only nonzero categories are shown;
an empty reconciliation summary reads `no entries`.

Unknown-key details are available through [`over lint`](lint.md);
[`over doctor`](doctor.md) summarizes static and runtime health rather than
duplicating status's detailed report.

Use `--verbose` to expand dependency configuration, policy scopes, selectors,
permissions and excludes in cascade order. Scopes name their declaring descriptor;
mapping origins name their declaration directory. Git summaries name
the latest contributing descriptor; individual merged settings can also inherit
from earlier scopes. Hooks are local to their overlay or repository, not inherited.

Unhealthy paths show their source or sidecar/declaration, owning overlay,
materialization intent, and responsible rule/scope where available. Healthy paths
are summarized by default and expanded with `--verbose`.

Scripts, consumer data values, remote URLs and arbitrary Git
configuration values are omitted. Hooks, including install, are described by
presence/counts without executing commands or consulting external providers.
Only positive hook-event counts are shown; scopes with no hooks are omitted.

## Exit status and read-only behavior

- `0`: every selected overlay is fully applied, including Git prerequisites,
  or the repository has no overlays.
- `1`: any selected overlay needs attention or cannot be checked, including
  unknown names and invalid configuration. An `Ahead` checkout also needs attention.
- Invalid command-line arguments use the argument parser's error exit status (`2`).

Multiple selections receive individual reports and an overall summary, including
failed checks. Entry counts are summed per overlay report; a shared dependency
may therefore contribute to several reports. A failure in one overlay does not
prevent checking other discoverable overlays.

Status never applies, repairs, installs, runs hooks, fetches, or writes target
files, Git objects/index/config/excludes, or XDG state/lock files. Virtual-checkout
association metadata is read as a snapshot; older state schemas migrate only in
memory. `NO_COLOR` and redirected stdout produce readable output without ANSI
color escapes, even if color is otherwise forced.

Use [`over diff`](diff.md) for changes and content diffs; status explains the
resolved configuration and whether it is healthy.

## Reconciliation states

Each entry is classified into one of these health statuses. `over diff` shares
desired-state selection but presents Git changes independently of health:

| Status | Meaning |
|--------|---------|
| `Applied` | Target matches desired intent — nothing to do. |
| `Missing` | Target doesn't exist yet. |
| `Modified` | A checkout has local/staged changes, or a managed file's permissions need repair. |
| `Broken` | A dangling soft symlink, or a checkout path that isn't a valid git repo. |
| `Conflict` | Target exists and doesn't match desired intent, or a git checkout has a merge/rebase/cherry-pick in progress. |
| `Ahead` | A git checkout is ahead of its upstream. |
| `Behind` | A git checkout is behind its upstream. |
| `Diverged` | A git checkout has diverged from its upstream. |
| `Locked` | Encrypted content requires unlocking before it can be verified. |

Without `--verbose`, only entries other than `Applied` are printed; pass
`--verbose` to also print unchanged entries.

## Declared `git` repositories

A `git` entry declared at the overlay's own root (`git = "<url>"`, or a
`"."` key) is a real checkout: its status reflects the checkout's own
working-tree/upstream state (dirty, ahead, behind, diverged, merge in
progress), same as any other git repository you work in directly.

Any other `git` entry (declared at a subpath, e.g. a plugin manager's
clone) is a **declared, provisioned resource** instead: its status only
reflects whether it's present and its declared configuration
(`url`/`tag`/`rev`/`remotes`/`config`) is applied. Uncommitted changes,
local commits, untracked files, or an in-progress merge inside it never
make it `Modified`/`Ahead`/`Behind`/`Diverged`/`Conflict` — that content
isn't something `over` manages. Configuration drift appears as individual warnings
in **Git configuration**, separately from content conflicts. Tag/revision drift
retains its reconciliation conflict classification. Re-running `over apply` reconciles managed
settings such as declared remotes and Git configuration, but does not rewrite an
existing checkout's `origin` URL to match `url`. For origin URL drift, inspect
the checkout and its intended repository identity, then explicitly repair it:

```sh
git -C /path/to/checkout remote set-url origin https://example.com/intended-repo.git
over status hosts/laptop
```

Replace the path and URL with the declared checkout and its intended `url`.
This changes the remote configuration, not the checkout's files or history. See
[ADR-021](../adr/021-declared-git-repositories-report-provisioning-not-content-status.md).

## Virtual checkouts (`mode = "checkout"`) {#virtual-checkouts-mode--checkout}

A subtree (or the whole overlay, via `mode = "checkout"`
or inherited policy) can be materialized as a **virtual
checkout**: ordinary, directly editable files at the target, with no
`.git` there at all, backed by the overlay's own source repository. See
[ADR-022](../adr/022-virtual-checkout-materialization.md) for how this
differs from a declared `git` repository above.

Its status is computed against the checkout's recorded base revision —
not by inspecting a real git working tree, since there isn't one at the
target:

Only paths tracked in the overlay's recorded base, current source HEAD or source
index are inspected. Status does not crawl the target for additions: unrelated
target files and mounted repositories' own files are ignored. Explicitly tracked
overlay files inside a mounted repository are still checked. Use `over add` to
select target additions for the overlay; `over diff` can preview them.

| Status | Meaning |
|--------|---------|
| `Applied` | No local edits, and the source repository hasn't moved since the checkout was last materialized/synced/committed. |
| `Modified` | Local or staged changes exist, and the managed source content hasn't moved. Select changes with `over add`, then run `over commit`. |
| `Behind` | The source repository has new commits touching the managed path; nothing local to lose — `over sync` fast-forwards it automatically. |
| `Diverged` | Local edits and source-side changes coexist. `over sync` advances non-conflicting source paths and preserves independent local/index edits; then stage and commit the remaining edits. |
| `Conflict` | The *same* file changed both locally and in the source repository, to different content — resolve manually, then re-run. |
| `Missing` / `Broken` | Target or recorded association is missing. Apply can create a missing target; see lost-association guidance below for existing files. |

Actionable modified, diverged, or conflicting virtual checkouts also list specific
added/modified/deleted files underneath their aggregate status line.
Files are labeled `(staged)` or `(unstaged)`; a file edited again after
staging can appear in both lists. Mapped checkout units are inspected
independently, so changes confined to a mapped sibling do not make the
residual checkout behind.

### Initialization into an existing directory

`over apply` can initialize a virtual checkout in an existing directory. It
preserves the root and unrelated files, creates missing committed files, and
adopts identical files without prompting. For actual path collisions it offers:

- **Skip:** keep the existing content as a local checkout change and continue.
- **Overwrite:** replace only the conflicting path with committed overlay content.
- **Absorb:** keep the existing content and copy it into the overlay's source
  working tree, without staging or committing it. Independent source edits must
  be resolved first.
- **Diff:** show differences against committed overlay content, then decide.

File/directory collisions are resolved as one group; keeping an obstructing file
also keeps it in place instead of creating files beneath it. Existing extra files
in the selected checkout scope can be selected as additions with `over add`;
they do not affect status until tracked in the source index. `--force` overwrites
eligible collisions; `--no-prompt` fails on unresolved collisions. Dry-run previews
without prompting. Mapping holes and protected checkout boundaries remain protected.

### Lost association metadata

The association is stored in `$XDG_STATE_HOME/over/virtual_checkout.toml`.
Losing it leaves target files intact, but loses the recorded base revision and
ownership scope needed to interpret those files. Reapplying can initialize the
existing directory against current source `HEAD`, using the collision choices
above. That creates a new association; it cannot reconstruct the old base.

Preserve the target files, source repository history, and any remaining state
before recovery. If available, restore a matching association from a state backup
for the same target, source and mapping scope. Without that backup, initialization
against current source content does not recover its previous base. Keep an
independent copy of existing target content for manual comparison and recovery.

## Git configuration

When an overlay materializes symlinks or checkouts into a target that's
itself inside a git repository, `over apply` keeps that repository's
`.git/info/exclude` in sync so its own `git status` doesn't list
overlay-managed paths as untracked (see [`over apply`](apply.md)). `over
status` reports actual missing protection, not whether a particular managed block
exists. Effective global ignores, `.gitignore` and handwritten exclusions count;
Git's ignore precedence and negations still apply. Absent artifacts need no
exclusion; dangling symlinks do. Tracked paths are reported independently and
are never removed from the index or hidden from `git status`.

Issues are grouped by owning overlay, shared repository and affected worktree:

```text
Git configuration:
  workspaces/oxyde
    ~/Workspaces/oxyde/press
      main
        ✘ exclude missing: opencode.jsonc
        🔒 tracked: mise.toml
      ✘ config missing: remote.upstream.url
```

Flat worktree layouts group under the shared repository, with exclusions stored
in `press/.git/info/exclude`, not `press/main/.git/info/exclude`. Worktree indexes,
ignore rules and private settings are checked in their own context. Healthy
repositories are omitted even with `--verbose`.

For existing declared repositories (including root declarations), status checks
origin and all declared remote properties, explicit local config and applicable
`config.worktree` settings, and local/worktree `over.overlay` associations.
Matching inherited config does not replace a missing explicitly managed scoped
entry. Extra undeclared remotes/properties are allowed; explicit fetch refspecs
must match their list, while additional push refspecs are allowed. Values and
URLs are omitted because they can contain secrets. Missing deployments are
reported by reconciliation, without fabricated missing-config entries.

Missing/incorrect settings, unprotected artifacts and tracked conflicts are
warnings. Malformed managed markers are errors; use `over doctor --fix` for that
limited repair. Well-formed block drift alone is not unhealthy when Git already
ignores the artifacts. Apply retains its managed-block reconciliation policy.
The overall Git issue count deduplicates shared dependency findings; entry counts
remain per selected overlay. Doctor consumes these same runtime findings in its
concise health summary.
