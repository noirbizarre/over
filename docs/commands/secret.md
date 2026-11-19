# `over secret`

Manage named binary-safe secrets in the repository's encrypted native store.

```text
over secret add <NAME> [--replace]
over secret list
over secret get <NAME>
over secret remove <NAME> [--yes]
```

Accepts [global options](index.md#shared-options-and-selection). Unlike crypt/access
commands, these always select the existing Over home through `--home`, then
`OVER_HOME`, then `~/.dotfiles`, independently of the current Git repository.
Storage requires the source Git repository root. There are no overlay,
target, JSON, or credential options.

All operations require an existing [`over unlock`](unlock.md), including
listing/removing names. Each value is ciphertext in `.over/secrets/<name>` even
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
