# `over key`

Rotate the repository encryption key for remaining trusted access.

```text
over key rotate
```

Accepts [global options](index.md#shared-options-and-selection). Repository
selection follows [`over crypt`](crypt.md#repository-selection-and-options).
There are no command-specific options.

Rotation requires the installed current key and usable retained access envelopes.
It re-encrypts current protected files and native secrets into a new generation
and stages the transition. Retained passphrase keys require credential
reacquisition. Unsafe or incompatible linked-worktree state prevents publication.
Review the staged changes and commit explicitly; there is no machine-readable
success payload, and operational failures return nonzero with stderr diagnostics.

```sh
over key rotate
over crypt list
git diff --cached --no-textconv --stat
git commit -m 'Rotate repository encryption key'
```

Rotation does not erase previous keys learned by others, rewrite all historical
commits, or revoke the underlying credentials stored in protected files. To remove
access, use [`over user revoke` or `over user key revoke`](user.md).

The textconv driver uses only the current key. Old-generation ciphertext,
including the pre-rotation `HEAD` side of a staged rotation, cannot be converted;
use `--no-textconv` for that review. See [key rotation](../usage/crypt.md#users-and-keys)
for the full access lifecycle.
