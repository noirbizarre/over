# `over diff`

Show Git changes for an overlay's managed paths. The default view shows
**unstaged changes**. Over leaves source files, target files, the source index,
and the virtual checkout's recorded base unchanged.

For virtual checkouts, use [`over restore`](restore.md) after inspection to discard
selected unstaged changes, unstage files, or recover a historical version.

```text
Usage: over diff [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to check (default_overlay if configured, all overlays otherwise)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>  The target root directory (~)
  -d, --debug        Toggle debug traces
      --no-uses      Skip dependency operations, retaining dependency data
  -a, --all          Diff every overlay, retaining configured default_overlay data
      --staged       Show staged changes instead of unstaged changes (alias: --cached)
      --all-changes  Show both staged and unstaged changes
      --difftool     Use Git's configured difftool
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

When `NAME` is omitted, a configured `default_overlay` (see
[Default overlay selection](../configuration/root.md#default-overlay-selection))
narrows the report to that overlay; pass `--all` to select every overlay.
Explicit `NAME` and `--all` retain the configured default's
[data context](../configuration/overlay.md#consumer-metadata-data). Each target resolves
its own metadata separately. `--no-uses` skips dependency entries, not their data.

`--all` selects overlays; `--all-changes` selects both change views. The
`--staged` and `--all-changes` options are mutually exclusive.

## Symlink materialization

For file symlinks and whole-directory symlinks, editing the deployed content
already edits the source repository. Over runs Git's diff in that repository,
scoped to the selected overlay's managed source paths:

- `over diff NAME`: source index → source working tree, like `git diff`.
- `over diff NAME --staged`: source `HEAD` → source index, like `git diff --cached`.

Deleted source paths remain in scope. Unrelated repository changes, ignored
paths, and Over's control files are excluded. Untracked source files follow
normal Git diff behavior and do not appear until staged.

Missing, broken, or misdirected deployment links are path diagnostics, rather
than patches comparing linked content with an independent target copy. Source
changes can still be shown when deployment needs repair. Sources outside Git
receive a diagnostic explaining that no Git comparison is available.

## Virtual checkout materialization

For `mode = "checkout"`, target files are ordinary files with no `.git` directory.
Over constructs private temporary Git snapshots and asks Git to compare them:

| View | Comparison |
|---|---|
| Default (unstaged) | Source index → materialized target |
| `--staged` / `--cached` | Recorded base revision → source index |

The recorded base is the revision saved when the virtual checkout was created
or synchronized. If source `HEAD` has advanced, the staged view intentionally
shows changes since that **recorded revision**, even when `git diff --cached`
in the source would be empty. The comparison header identifies that baseline.
Use [`over status`](status.md) to inspect source drift and checkout health.

Both views preserve staged content independently of subsequent edits. Git
renders text, binary, added/deleted files, and executable-mode changes. Owned
new target files appear as additions even before staging. Source attributes
and clean filters apply to the target's logical source paths.

Directory mappings are projected back to source-repository paths. Each unit
respects its managed ownership boundaries and protected target namespaces;
unrelated index entries and foreign files are excluded. A missing recorded
association is reported without adopting the checkout or updating its base.

## Git presentation and tools

Git must be available on `PATH`. Over delegates patches and tool selection to
Git, using your existing Git configuration:

```sh
# Default terminal diff, including core.pager / pager.diff configuration
over diff workstation

# Show both views with the configured pager or external diff driver
over diff workstation --all-changes

# Explicitly request the configured diff.tool and difftool.<tool> options
over diff workstation --difftool
over diff workstation --staged --difftool
```

Configured external diff drivers, attributes, text conversion, colors, pagers,
and pager-specific settings remain Git's responsibility. A configured
`diff.tool` alone does not launch a GUI; `--difftool` explicitly requests it.
There is no separate Over tool configuration. Git's ordinary difftool prompts
and tool options apply. Temporary inputs remain available until Git exits.

Repository/scope headers and deployment diagnostics go to stderr; Git patches
go to stdout. Finding changes is successful; Git/tool failures are errors.
Partial-file blocks retain their existing block-level diagnostics. Declared
non-root Git repositories retain their provisioning-only boundary from
[ADR-021](../adr/021-declared-git-repositories-report-provisioning-not-content-status.md).
