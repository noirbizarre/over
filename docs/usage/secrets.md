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

The entire binary-safe map is one versionable ciphertext file:

```text
.over/
  secrets.yaml
```

Repository-root `.over` is reserved administration: overlay discovery and
materialization always exclude it, even if `.overignore` negations select it.
Each mutation automatically commits this file and required initial encryption
metadata, preserving unrelated staged work. Despite its
filename, `secrets.yaml` contains a versioned binary encrypted payload, including
in the working tree while unlocked. Secret operations and explicit Git textconv
display decrypt its contents in memory. Aggregate updates are serialized across processes and atomically
publish ciphertext, preventing lost updates or partial writes.

Automatic commits honor Git identity, hooks, and signing configuration, using
the same isolated commit behavior as [`over commit`](../commands/commit.md).
The default messages include secret names in plaintext Git metadata; configure
[message templates](../configuration/root.md#secret-commit-messages) to customize
them or omit names. Secret values are never available to message templates.
Commit pending changes to the tracked store or encryption metadata before
mutating secrets. Reads do not stage or commit anything, and identical-value
replacements create no empty commit. Git commit hooks may read secrets but
cannot recursively mutate them.

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
including `list` and `remove`, because names reside inside the encrypted map.
Reading a missing store does not create one.

`get` intentionally writes the fully decrypted value to stdout, without adding
a newline. This is the explicit plaintext-output command; redirect or pipe it
only where that value is intended. Authentication failure emits no partial
plaintext. Other diagnostics and Debug output redact values and passphrases.

`remove` updates the aggregate ciphertext. As with any versioned data, removing
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
