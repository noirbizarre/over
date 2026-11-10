# Secrets and Age-encrypted files

Over uses passphrase-based [Age](https://age-encryption.org/) encryption for
named secrets and encrypted overlay files. Both use the same passphrase source.
No GPG installation, key pair, or external identity file is needed.

## Passphrase lifecycle

When encryption or decryption is needed, a nonempty `OVER_SECRET` supplies the
passphrase without prompting. Whitespace is significant. If the variable is
unset or empty, Over asks through a hidden terminal prompt. It never consumes
stdin as the passphrase. Without a usable terminal, or with `--no-prompt`, set
`OVER_SECRET`; otherwise the operation fails with an actionable error.

The credential is unlocked lazily and shared for the current invocation. Using
multiple secrets or encrypted files does not require multiple passphrase
prompts. Over never persists the passphrase or caches plaintext values. As with
other environment variables, child commands inherit `OVER_SECRET` when it is set.

Read-only reports and dry-run never prompt. Existing decrypted destinations are
reported as **locked** when no credential is available to verify their content.
Supplying `OVER_SECRET` enables in-memory verification in those reports.

## Named secrets

```text
over secret add NAME [--replace]
over secret list
over secret get NAME
over secret remove NAME
```

The store belongs to the repository selected by `--home` / `OVER_HOME`, with
`~/.dotfiles` as the default. It does not depend on the selected overlay, the
current Git repository, or the target `--root`. The repository directory must
already exist.

Each secret is a versionable ciphertext file:

```text
.over/
  secrets/
    github-token.age
    npm-token.age
```

Repository-root `.over` is reserved administration: overlay discovery and
materialization always exclude it, even if `.overignore` negations select it.
Secret commands do not automatically stage or commit these files.

Names contain 1–128 ASCII letters, digits, underscores, or hyphens, and start
with a letter or digit. Platform-reserved device names such as `CON` and `LPT1`
are rejected. Store directories and entries cannot be symlinks.

### Add or replace a value

`add` reads a single-line value without echoing when used interactively. Piped
stdin is detected automatically and read to EOF, preserving exact bytes,
including trailing newlines and binary data. When no value is supplied, Over
falls back to the hidden value prompt; without a terminal it fails. Values are
never accepted as command arguments.

For example, pipe the output of an existing credential provider:

```sh
credential-provider | over secret add github-token
```

Provide `OVER_SECRET` through your environment or credential manager for
noninteractive encryption. Avoid typing literal secret values in shell commands.

An existing secret requires explicit `--replace`; the flag is the replacement
authorization and does not cause another confirmation prompt. Failed encryption
leaves the previous ciphertext intact. Only ciphertext enters the atomic store
publication files.

### List, retrieve, or remove

`list` prints sorted names only. `list` and `remove` never decrypt or ask for a
passphrase. Reading a missing store does not create one.

`get` intentionally writes the fully decrypted value to stdout, without adding
a newline. This is the explicit plaintext-output command; redirect or pipe it
only where that value is intended. Authentication failure emits no partial
plaintext. Other diagnostics and Debug output redact values and passphrases.

`remove` deletes the current ciphertext file. As with any versioned file,
removing it does not erase prior Git history.

## Encrypted overlay files

Use a case-sensitive final `.age` suffix for passphrase-encrypted overlay files.
For example, `selected/config/token.age` deploys as `config/token` under the
overlay's resolved destination. Exactly one suffix is removed, directory mappings
apply to the logical output path, and collisions with ordinary files or partial
files are rejected before apply writes anything. Both binary and ASCII-armored
Age inputs are accepted.

You can prepare ciphertext with an Age-compatible tool:

```sh
age --passphrase --output selected/config/token.age token
over apply selected
```

The encrypted source remains unchanged. Decryption is a desired-state content
transformation, producing a real file rather than a source symlink. Content
templating and secret interpolation into templates/hooks are not implemented.

On Unix, decrypted files default to `0600`; explicit scalar/rule permissions
can override this, matched against the logical output filename. Plaintext is
written only to the final destination, never an Over cache or intermediate
file. Existing files are updated in place after complete authentication: an
I/O failure or crash during that write can leave partial destination content.
Hard-linked destinations and directory obstructions require manual resolution.

Apply preserves differing target content unless overwrite is explicitly
authorized interactively or with `--force`. Encrypted conflicts never offer
Absorb or a plaintext diff. Unapply removes only a destination whose bytes still
match the current decrypted source; it retains edits and cannot use force to
skip ownership verification. Encrypted diff diagnostics are metadata-only.

Decrypted destinations inside host Git repositories are excluded from ordinary
Git discovery before plaintext is written. Invalid exclusion blocks or conflicting
ignore rules stop publication; exclusions survive a later apply failure.
Already-tracked destinations must be untracked before apply;
excludes cannot protect tracked files. Retained encrypted destinations keep the
overlay's exclude block after unapply.

### Current scope

- Encrypted files require ordinary file-level materialization. Whole-directory
  symlinks, directory-valued link sidecars, and virtual checkouts reject encrypted
  descendants instead of deploying ciphertext silently.
- Encrypted control files (overlay descriptors, ignore files, link and partial
  sidecars) and encrypted symlink inputs are unsupported.
- Reverse-add of decrypted destinations, including a containing directory, is
  rejected. Update encrypted sources explicitly; encryption-aware checkout
  staging, synchronization, and restore are not supported.
