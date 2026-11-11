# Native secrets and legacy Age files

Over's native key-value store shares the repository key and persistent unlock
state used by [Git encryption](crypt.md). Standard [Age](https://age-encryption.org/)
identities or passphrases protect that repository key through per-key envelopes.

## Unlock once

Create an initial user/key with `over user add`, then use `over unlock` after
cloning or explicitly locking. Unlock persists a private repository key under
Git's common administrative directory; it survives terminal closure and reboot.
`over lock` restores clean protected Git-worktree files to ciphertext and removes
the local key, refusing unsafe local edits. Filters and secret commands do not
prompt for a second credential. See [key management](crypt.md#users-and-keys).

Without that local key, native operations fail with unlock guidance. Provider
resolution treats a locked native store as a session-unavailable candidate;
corrupt ciphertext remains a fatal error. Plaintext values are never cached.

## Named secrets

```text
over secret add NAME [--replace]
over secret list
over secret get NAME
over secret remove NAME
```

The store belongs to the repository selected by `--home` / `OVER_HOME`, with
`~/.dotfiles` as the default. It does not depend on the selected overlay, the
current Git repository, or the target `--target`. The repository directory must
already exist.

The entire binary-safe map is one versionable ciphertext file:

```text
.over/
  secrets.yaml
```

Repository-root `.over` is reserved administration: overlay discovery and
materialization always exclude it, even if `.overignore` negations select it.
Secret commands do not automatically stage or commit this file. Despite its
filename, `secrets.yaml` contains a versioned binary encrypted payload, including
in the working tree while unlocked. Only secret operations decrypt its contents
in memory. Aggregate updates are serialized across processes and atomically
publish ciphertext, preventing lost updates or partial writes.

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

Unlock before using noninteractive secret commands. Avoid typing literal secret
values in shell commands; standard Age identity files support noninteractive unlock.

An existing secret requires explicit `--replace`; the flag is the replacement
authorization and does not cause another confirmation prompt. Failed encryption
leaves the previous ciphertext intact. Only ciphertext enters the atomic store
publication files.

### List, retrieve, or remove

`list` prints sorted names only. All operations require the shared unlock state,
including `list` and `remove`, because names reside inside the encrypted map.
Reading a missing store does not create one.

`get` intentionally writes the fully decrypted value to stdout, without adding
a newline. This is the explicit plaintext-output command; redirect or pipe it
only where that value is intended. Authentication failure emits no partial
plaintext. Other diagnostics and Debug output redact values and passphrases.

`remove` updates the aggregate ciphertext. As with any versioned data, removing
a value does not erase prior Git history.

## Legacy encrypted overlay files

Existing repositories without repository-key metadata retain the original
passphrase-based `.age` reader and per-name `.over/secrets/NAME.age` store.
For this legacy format only, a nonempty `OVER_SECRET` supplies the credential,
otherwise an allowed hidden terminal prompt acquires it for the invocation.
Legacy listing/removal do not require decryption. Migrate explicitly with
[`over crypt migrate`](crypt.md#migration-and-recovery); Over never silently
reinterprets or discards legacy ciphertext. Mixing both native store formats
requires migration before native secret operations continue.

Use a case-sensitive final `.age` suffix for passphrase-encrypted overlay files.
For example, `selected/config/token.age` deploys as `config/token` under the
overlay's resolved destination. Exactly one suffix is removed, target rules
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
templating is not implemented. Lifecycle hooks support native secret interpolation
through `secret("name")` in commands, environment values, working directories
and conditions. See [hook templates](../configuration/root.md#templates-and-conditions).

Fallback chains use a pipe inside the argument, for example
`secret("op://dev/github/token | github-token")`. Bare names address the active
repository's native store; provider-qualified references use `scheme://payload`.
External providers are not implemented yet and count as unavailable. Missing
secrets and unavailable providers allow fallback. Malformed expressions, locked
native credentials, wrong passphrases, corrupt ciphertext and store failures stop
resolution. Hook interpolation requires valid UTF-8, preserves whitespace, and
shares the invocation's cached Age credential without caching plaintext values.
Path templates cannot access secrets. Prefer passing values through hook `env`
rather than embedding them in shell command text.

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

### Legacy format scope

- Encrypted files require ordinary file-level materialization. Whole-directory
  symlinks, directory-valued link sidecars, and virtual checkouts reject encrypted
  descendants instead of deploying ciphertext silently.
- Encrypted control files (overlay descriptors, ignore files, link and partial
  sidecars) and encrypted symlink inputs are unsupported.
- Reverse-add of decrypted destinations, including a containing directory, is
  rejected. Update encrypted sources explicitly; encryption-aware checkout
  staging, synchronization, and restore are not supported.
