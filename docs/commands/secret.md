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
Modern storage requires the source Git repository root. There are no overlay,
target, JSON, or credential options.

All modern operations require an existing [`over unlock`](unlock.md), including
listing/removing names. The entire map, including names, is ciphertext in
`.over/secrets.yaml` even while unlocked. Commands do not stage or commit it.

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

## Resolution and legacy compatibility

Resolution uses the hook-template function `secret("name")`, not a CLI
`resolve` subcommand. See [native secret usage](../usage/secrets.md) and
[hook templates](../configuration/root.md#templates-and-conditions) for fallback
chains and provider availability.

Without repository-key metadata, commands retain legacy per-name Age storage.
Legacy add/get use a passphrase (`OVER_SECRET` or hidden input); list/remove need
no decryption. Once modern encryption is configured, mixed stores require
[`over crypt migrate`](crypt.md#recover-and-migrate). `OVER_SECRET` does not unlock
modern repository encryption. Follow the [legacy migration guide](../usage/secrets.md#legacy-encrypted-overlay-files).
