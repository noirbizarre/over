# `over commit`

Record local changes made directly in a
[virtual checkout](../adr/022-virtual-checkout-materialization.md) back
into the overlay's own source repository, as a normal git commit.

```text
Usage: over commit [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to commit (uses default_overlay if configured)

Options:
  -H, --home <HOME>        Configuration and overlays root [env: OVER_HOME]
  -r, --root <ROOT>        The target root directory (~)
  -m, --message <MESSAGE>  Commit message
      --no-prompt          Never prompt; use --message or an auto-generated message
  -d, --debug              Toggle debug traces
  -v, --verbose            Toggle verbose output
  -h, --help               Print help
```

When `NAME` is omitted, the repository's configured `default_overlay` (if
any) is used instead of prompting interactively — see
[Default Overlay Selection](../configuration.md#default-overlay-selection).

## What it does

For each of the named overlay's own virtual checkout entries (never a
`uses` dependency's — that content belongs to a different overlay):

1. Computes the specific added/modified/deleted files since the
   checkout's recorded base revision (the same comparison `over status`/
   `over diff` use).
2. If any file changed both locally and in the source repository, to
   different content, refuses to commit at all — resolve the conflicting
   files manually first (`over status --verbose` lists them).
3. Otherwise, with something to commit, asks for a commit message
   (`--message` skips the prompt; `--no-prompt` with no `--message` falls
   back to an auto-generated one).
4. Builds new blobs for the changed files, updates just those paths in the
   source repository's current tree (everything else in that repository —
   sibling overlays, unrelated files — is carried over untouched), and
   creates a normal, single-parent commit on top of the source
   repository's current `HEAD`.
5. Advances the checkout's recorded base revision to the new commit, so a
   subsequent `over status`/`over commit`/`over sync` sees a clean
   checkout again.

The resulting commit is entirely ordinary: inspect it with `git log`/`git
show`/`git diff` directly in the overlay's source repository, and push it
to a remote like any other commit.

## What it never does

- Commit anything in a *declared* `overlay.git` repository nested inside
  the overlay (a plugin manager's clone, etc.) — `over commit` only ever
  acts on virtual checkout entries, and refuses outright
  (`overlay is not checkout-materialized`) if the named overlay has none.
- Silently pick a side for a file that changed both locally and upstream —
  every conflicting path blocks the whole commit until resolved manually.
- Touch anything outside the managed path in the source repository.

See [ADR-022](../adr/022-virtual-checkout-materialization.md) for the full
design rationale.
