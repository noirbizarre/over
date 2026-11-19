# Native secrets

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

Use [`over secret`](../commands/secret.md) for command syntax and output contracts.
The examples below explain how to use the native store in workflows.

The store belongs to the repository selected by `--home` / `OVER_HOME`, with
`~/.dotfiles` as the default. It does not depend on the selected overlay, the
current Git repository, or the target `--target`. The repository directory must
already exist.

Each binary-safe value has its own versionable ciphertext file:

```text
.over/
  secrets/
    github-token
    database-password
```

Repository-root `.over` is reserved administration: overlay discovery and
materialization always exclude it, even if `.overignore` negations select it.
Each mutation automatically commits only its entry and required initial encryption
metadata, preserving unrelated staged work. Entries remain encrypted in unlocked
working trees. New entries encrypt the value bytes directly, without a JSON or
Base64 wrapper. The ciphertext header carries the format version; separate
authenticated context binds the exact secret name to the value. Renaming or
swapping ciphertext between names fails authentication. Secret commands and the
path-aware Git diff driver decrypt in memory. Independent secret
changes touch independent Git paths and can merge without an aggregate-file conflict.
Writers serialize locally and publish through a recoverable isolated commit.

Automatic commits honor Git identity, hooks, and signing configuration, using
the same isolated commit behavior as [`over commit`](../commands/commit.md).
Secret names are public in filenames and Git history, even while locked.
The default messages include names too; configure
[message templates](../configuration/root.md#secret-commit-messages) to customize
them or omit names from messages. Secret values are never available to templates.
Commit pending changes to the selected entry or encryption metadata before
mutating secrets. Reads do not stage or commit anything, and identical-value
replacements create no empty commit. Git commit hooks may read secrets but
cannot recursively mutate them.

Older aggregate files at `.over/secrets` or `.over/secrets.yaml` are split
automatically on the next mutation. Legacy `.over/secrets/<name>.age` directories
also migrate in that same commit, after decrypting all entries with their original
Age passphrase. The original encrypted file/directory is retained under Git's
private `over-crypt/legacy-state-backup-*` path. Access metadata migrates separately
on a user/key mutation; secret changes do not force an access-format migration.
Read-only commands do not migrate or commit. Resolve competing stores before
continuing; Over does not silently merge them.
Secret mutations validate all migration destination names before publication.
Aggregates with case-colliding names (or mutations introducing a case variant)
are rejected; the failed secret mutation leaves the original store, index, and
HEAD unchanged.

Existing JSON-backed native entries remain readable alongside binary entries.
They convert to the binary representation only when actually rewritten, including
key rotation. Reads and identical-value replacements leave their bytes unchanged;
rewriting one entry does not migrate unrelated entries. Older Over versions that
do not recognize the new ciphertext domain cannot read newly written entries.

For readable value diffs, secret mutations register `diff=over-secret`:

```sh
git diff --ext-diff --no-relative
git diff --cached --ext-diff --no-relative
git show --ext-diff --no-relative HEAD
```

Both sides authenticate under their own logical names before any value patch is
printed. Legacy entries display their decoded values too. Text values use unified
patches; binary values and locked ciphertext use opaque summaries. The helper
keeps plaintext out of temporary files, Git objects, and persistent caches.
Use `--no-ext-diff --no-textconv` for ciphertext-only review, including history
whose key generation is unavailable. See the [helper reference](../commands/crypt-filters.md#native-secret-diffs)
for presentation limitations.

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
values in shell commands; unencrypted software Age identity files support
noninteractive unlock. Encrypted identity files and hardware plugins may require
terminal interaction during unlock, never for each subsequent native-secret read.

An existing secret requires explicit `--replace`; the flag is the replacement
authorization and does not cause another confirmation prompt. Failed encryption
leaves the previous ciphertext intact. Only ciphertext enters the atomic store
publication files.

### List, retrieve, or remove

`list` prints sorted names only. All operations require the shared unlock state,
including `list` and `remove`. Listing validates filenames without reading every
value. Getting one native entry does not authenticate unrelated entries, so a
corrupt value does not prevent retrieving another secret. Reading a missing
store does not create one.

`get` intentionally writes the fully decrypted value to stdout, without adding
a newline. This is the explicit plaintext-output command; redirect or pipe it
only where that value is intended. Authentication failure emits no partial
plaintext. Other diagnostics and Debug output redact values and passphrases.

`remove` asks for confirmation, defaulting to No, before deleting the selected
entry. Interactive approval requires terminal stdin and stderr; use
`over secret remove NAME --yes` for explicit approval in scripts.
As with any versioned data, removing
a value does not erase prior Git history.

## Use secrets in hooks

Lifecycle hooks support native secret interpolation
through `secret("name")` in commands, environment values, working directories
and conditions. See [hook templates](../configuration/root.md#templates-and-conditions).

Pass values through the hook environment rather than embedding them in shell
command text:

```yaml
hooks:
  post-apply:
    - command: ./deploy --token "$TOKEN"
      env:
        TOKEN: "{{ secret('github-token') }}"
```

Fallback chains use a pipe inside the argument, for example
`secret("op://dev/github/token | github-token")`. Bare names address the active
repository's native store; provider-qualified references use `scheme://payload`.
External providers are not implemented yet and count as unavailable. Missing
secrets, unavailable providers, and a missing local repository key allow
fallback. Malformed expressions, corrupt ciphertext and store failures stop
resolution. Hook interpolation requires valid UTF-8 and preserves whitespace
without caching plaintext values. Native stores require a prior
`over unlock`; hook lookup does not prompt to acquire their local repository key.
Path templates cannot access secrets.
