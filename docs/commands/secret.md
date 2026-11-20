# `over secret`

Manage named binary-safe secrets in the repository's encrypted native store.

```text
over secret add <NAME> [--replace]
over secret list
over secret get <NAME> [--identity <PATH>]... [--user <USER>] [--key <KEY>]
over secret remove <NAME> [--yes]
```

Accepts [global options](index.md#shared-options-and-selection). Unlike crypt/access
commands, these always select the existing Over home through `--home`, then
`OVER_HOME`, then `~/.dotfiles`, independently of the current Git repository.
Storage requires the source Git repository root. There are no overlay,
target or JSON options. Credential options apply only to `get`.

Adding, listing, and removing require an existing [`over unlock`](unlock.md).
Getting a native secret uses a matching local key when available; otherwise it
acquires a key for this invocation only. Each value is ciphertext in `.over/secrets/<name>` even
while unlocked. Names are public filenames in Git history. Each add, replacement, or removal
automatically commits the encrypted update, preserving unrelated staged changes.
Replacing a value with identical bytes creates no commit. Reads create no commit.
Commits honor Git identity, hooks, and signing configuration; failures prevent
publication. Recursive secret mutations from Git commit hooks are rejected.
Customize the default `chore(secrets): added/replaced/removed NAME` messages in
[root configuration](../configuration/root.md#secret-commit-messages).

Existing `.over/secrets` / `.over/secrets.yaml` aggregate files and legacy Age
directories migrate automatically on the next mutation, in that same commit.
Reads retain compatibility without migrating. Legacy Age entries additionally
require their original passphrase. Conflicting stores are rejected for explicit
reconciliation. Directory migrations retain an encrypted backup under Git's
private `over-crypt/legacy-state-backup-*` path. Getting an entry reads only that
value; listing reads names while retaining the unlock requirement.

## One-shot reads

Native `get` reads the **HEAD commit captured at invocation**, whether locked or
unlocked. Staged and unstaged changes are ignored. Access metadata, credential
envelopes, and ciphertext come from that same snapshot, even if HEAD changes
during a prompt. Commit changes before reading them. Historical standalone Age
stores retain their filesystem-based behavior.

`get` also accepts a bare Git repository selected with `--home` or `OVER_HOME`.
Its current HEAD (including detached HEAD) selects the commit; there is no
separate revision option. Both access metadata and the requested secret must
exist in that commit. An unborn or missing HEAD, missing entry/object, corrupt
Git data, unsupported format, or unavailable credential fails with a nonzero
status and no plaintext. Bare reads never fall back to files in the repository
directory. See the [provisioning example](../usage/secrets.md#read-from-a-bare-repository).

On a locked repository, provide a repeatable `--identity` (`-i`) file, reuse an
existing identity hint, or answer the identity/passphrase prompts. `--user` and
`--key` restrict the credentials tried. A matching local repository key takes
precedence over these options and avoids all credential prompts.

Prompts require terminal stdin and stderr and never appear on stdout, so piping
the value remains supported. Without a terminal, a usable identity must already
be available, for example an explicit unencrypted software identity:

```sh
over secret get github-token --identity /run/credentials/identity | consuming-program
```

Missing identities, cancelled prompts, or invalid credentials fail without
printing plaintext. `OVER_SECRET` is not a repository-key passphrase source.
Encrypted identities and hardware plugins may require terminal interaction.

Over creates no lock files, temporary files, keys, identity hints, or other
filesystem artifacts during native reads, and does not configure Git filters.
Bare reads create no working tree and do not update the index, refs, Git
configuration, or object database. Bare support is limited to `get`.
External identity plugins may manage their own state. The native-PQ adapter
requires a temporary ciphertext file and is unavailable for one-shot acquisition;
use another credential or normal `over unlock` for that identity. Temporary
repository keys and plaintext use zeroizing buffers. A successful one-shot read
does not unlock mutations, hooks, or Git filters.

New native entries encrypt raw value bytes and authenticate the exact filename.
Existing JSON-backed native entries remain readable and convert only on actual
rewrite, including key rotation. Reads and identical-value replacements do not
convert them. New writes require an Over version supporting the binary-entry
ciphertext domain. Secret mutations install the path-aware `diff=over-secret`
attribute for [readable Git value diffs](../usage/secrets.md#named-secrets).

| Subcommand | Input and output |
| --- | --- |
| `add NAME` | Read nonterminal stdin to EOF, preserving exact bytes/newlines; otherwise use hidden input |
| `add NAME --replace` | Explicitly authorize replacing an existing name without a second confirmation |
| `list` | Print sorted names, one per line, on stdout |
| `get NAME` | Write exact decrypted bytes on stdout, without adding a newline |
| `remove NAME` | Confirm removal of an existing entry; prior Git history is retained |

Removal confirmation defaults to No and requires terminal stdin and stderr.
Use `secret remove NAME --yes` for explicit approval in scripts. Refusal leaves
the store unchanged, including pending legacy migrations; approval does not
bypass unlock or authentication requirements.

Empty/no supplied input falls back to hidden input and requires a terminal.
Missing names, invalid input, lock state, and authentication/storage errors fail
with stderr diagnostics and nonzero status. Authentication failure emits no
partial value. Missing-store reads do not create it.

Names are 1–128 ASCII letters/digits/underscores/hyphens, start alphanumeric, and
cannot be platform-reserved device aliases such as `CON`.

```sh
credential-provider | over --home ~/.dotfiles secret add github-token
over --home ~/.dotfiles secret list
over --home ~/.dotfiles secret get github-token | consuming-program
over --home ~/.dotfiles secret remove github-token
```

## Resolution

Resolution uses the hook-template function `secret("name")`, not a CLI
`resolve` subcommand. See [native secret usage](../usage/secrets.md) and
[hook templates](../configuration/root.md#templates-and-conditions) for fallback
chains and provider availability.
