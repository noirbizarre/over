# `over lock`

Restore protected source working files to ciphertext and remove the installed
local repository key.

```text
over lock
```

Accepts only [global options](index.md#shared-options-and-selection), including
`--home`, `--debug`, and `--verbose`. Repository selection follows
[`over crypt`](crypt.md#repository-selection-and-options). There is no force,
timeout, dry-run, or session option.

Lock requires the currently installed key. It preflights all registered linked
worktrees sharing that common key, refreshes protected files from **index**
ciphertext, then removes the key. Staged content is preserved. Unsafe edits,
untracked protected plaintext, conflicts, inaccessible worktrees, incompatible
generations, or unsafe filesystem state cause failure before key removal.
An already-locked repository is not an unconditional successful no-op.

Successful lock has no machine-readable payload. Errors are reported on stderr
with nonzero status. Required filters and the diff driver remain configured;
native secret operations and encrypted textconv conversion become unavailable.

```sh
over crypt list
over lock
```

Lock does not erase deployed virtual destinations, copies, memory, snapshots, or
backups. Follow the [locking workflow](../usage/crypt.md#locking-and-unlocking) and
[automation recipes](../usage/crypt-automation.md) for practical integration.
