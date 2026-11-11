# `over doctor`

Summarize environment and overlay health. Doctor aggregates static diagnostics
from [`over lint`](lint.md) and the same runtime inspection used by
[`over status`](status.md), including missing deployments and managed-file problems.
Use status for detailed runtime diagnostics and lint for declaration errors.

```text
Usage: over doctor [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to check (every overlay otherwise)

Options:
  -H, --home <HOME>      Configuration and overlays root [env: OVER_HOME=]
  -r, --target <TARGET>  Set the target directory (defaults to ~) [env: OVER_TARGET=]
  -d, --debug            Toggle debug traces
      --no-uses          Skip dependency operations, retaining dependency data
      --fix              Repair malformed git-exclude blocks (over's own corrupted markers only)
  -v, --verbose          Toggle verbose output
  -h, --help             Print help
```

## Report

| Section | Source | What it covers |
|---|---|---|
| Environment | doctor | Git on `PATH` and XDG state directory writability. |
| Encryption security | repository encryption | Effective attributes, required local filters, raw index/HEAD blobs, and bounded historical exposure. |
| Overlay health | lint and status | Per-overlay warning/error summaries. |
| XDG state | doctor | Stale or corrupt sync/virtual-checkout bookkeeping. |

For example:

```text
📦 Overlay health
  workspaces/oxyde: ⚠ 2 warning(s)
```

The summary includes missing deployments, modified files, conflicts, checkout
state, Git prerequisites and inspection failures. Repository/worktree paths,
individual missing exclusions and configuration keys appear in status's
**Git configuration** section, rather than being repeated here. Use lint for static
details. Invalid overlays do not prevent checking other discoverable overlays.

Overlay names use the common overlay identity color. Nonzero errors use a red
cross and count; nonzero warnings use a yellow warning symbol and count. Zero
buckets and trailing command hints are omitted. Healthy rows use a green check.

Without `--verbose`, healthy rows are omitted; an empty section prints
"✔ No issues found". Verbose adds healthy checks, not duplicate detailed reports.
Several compositions can be affected by one dependency, but equivalent underlying
issues count once in the overall warning/error totals. Overlay summary rows do
not add findings to those totals.

## Selection and exit status

Encryption checks are repository-wide, including reserved `.over` data. They
identify working-tree/index/`HEAD` separately: decrypted working-tree plaintext
is expected after unlock. Stored plaintext, malformed/unsupported ciphertext,
attribute overrides, and incorrect filter configuration are security findings.
Authentication is performed only with an already available key, without
prompting; locked structure checks do not claim authenticated content. These
checks never stage/repair encryption data, including with `--fix`.

Known plaintext in the bounded history sample is reported with remediation and
the warning that encryption does not erase old commits, clones, backups, or
remotes. The sample covers at most 16 commits and 256 path/blob checks; a full
reachable-history audit remains a separate future capability. See
[storage checks](crypt.md#storage-checks-and-format).

Doctor checks all runtime overlays unless `NAME` is supplied. Static lint and
global XDG checks remain repository-wide. `default_overlay` does not narrow the
selection; its [data context](../configuration/overlay.md#consumer-metadata-data)
still participates in destination resolution. `--no-uses` skips dependency
runtime entries but retains their data.

Doctor exits nonzero for errors: broken deployments, content/ownership conflicts,
failed inspection, malformed exclusion markers, lint errors or global errors.
Missing/modified/locked entries, ahead/behind/diverged checkouts, missing or
incorrect Git settings, missing effective exclusions, tracked conflicts and stale
bookkeeping are warnings. Warning-only reports exit successfully, as lint does.
Status uses the same runtime severity but exits nonzero for any attention condition.

## Diagnostic writes

The environment check creates the XDG state directory if needed, then writes and
removes a probe file to verify writability, even without `--fix`. Created
directories remain. Probe creation or cleanup failures are errors; failed cleanup
can leave a probe behind.

Runtime and bookkeeping inspection does not write target files, Git config,
indexes or excludes, or state/lock files. It does not run hooks or fetch.

## `--fix`

`--fix` repairs **only** a malformed `.git/info/exclude` block: unmatched
`# >>> over: exclude:<name> >>>` / `# <<< over: exclude:<name> <<<` marker lines.
These do not self-heal on apply and remain errors until repaired.

Repair strips stray markers and appends a well-formed block for currently existing
managed artifacts, including dangling symlinks. When none remain it strips the
markers without adding a block. Contributions from linked worktrees are collected
before repairing their shared block. Inspection then reports the repaired state.

Repair never fills in ordinary missing exclusions, reconciles well-formed drift,
changes remotes/config, modifies managed content or prunes state records.
Handwritten content and other overlay blocks are preserved. See
[ADR-024](../adr/024-doctor-aggregates-diagnostics-and-repairs-only-its-own-corruption.md).
