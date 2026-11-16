# `over secret`

Manage named binary-safe secrets in the repository's encrypted native store.

```text
over secret add <NAME> [--replace]
over secret list
over secret get <NAME>
over secret remove <NAME>
```

Accepts [global options](index.md#shared-options-and-selection). Unlike crypt/access
commands, these always select the existing Over home through `--home`, then
`OVER_HOME`, then `~/.dotfiles`, independently of the current Git repository.
Storage requires the source Git repository root. There are no overlay,
target, JSON, or credential options.

All operations require an existing [`over unlock`](unlock.md), including
listing/removing names. The entire map, including names, is ciphertext in
`.over/secrets.yaml` even while unlocked. Each add, replacement, or removal
automatically commits the encrypted update, preserving unrelated staged changes.
Replacing a value with identical bytes creates no commit. Reads create no commit.
Commits honor Git identity, hooks, and signing configuration; failures prevent
publication. Recursive secret mutations from Git commit hooks are rejected.
Customize the default `chore(secrets): added/replaced/removed NAME` messages in
[root configuration](../configuration/root.md#secret-commit-messages).

| Subcommand | Input and output |
| --- | --- |
| `add NAME` | Read nonterminal stdin to EOF, preserving exact bytes/newlines; otherwise use hidden input |
| `add NAME --replace` | Explicitly authorize replacing an existing name without a second confirmation |
| `list` | Print sorted names, one per line, on stdout |
| `get NAME` | Write exact decrypted bytes on stdout, without adding a newline |
| `remove NAME` | Remove an existing entry; prior Git history is retained |

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
