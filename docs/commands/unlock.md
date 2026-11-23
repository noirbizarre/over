# `over unlock`

Unlock repository files and native secrets, configure local Git drivers, and
refresh protected source working files across registered linked worktrees.

```text
over unlock [-i <IDENTITY>]... [--user <USER>] [--key <KEY>]
```

| Option | Meaning |
| --- | --- |
| `-i`, `--identity <PATH>` | Read a standard Age identity file; repeat for several files |
| `--user <USER>` | Restrict candidate access envelopes to this user |
| `--key <KEY>` | Restrict candidate access envelopes to this key name |

Also accepts [global options](index.md#shared-options-and-selection).
Repository selection follows [`over crypt`](crypt.md#repository-selection-and-options).
There is no passphrase argument, force option, or dry-run mode.

Without explicit identities, interactive acquisition first offers a `USER/KEY`
chooser when multiple keys match. `--user` and `--key` independently filter the
choices; a single match skips the menu. Selection precedes identity hints,
passphrase prompts, and device interaction. Cancellation or failure of the chosen
key ends acquisition without trying another key. Prompts and menus go to stderr.

Explicit identities replace remembered identity-file sources and skip the chooser,
preserving automatic matching and independent credential recovery. Otherwise only
applicable private local identity hints are used. Independent selected passphrase
envelopes can still require hidden input.
Credential prompts require terminal stdin and stderr. Software identities can
support unattended unlock; encrypted identity files and plugins may still prompt.

```sh
over unlock --identity ~/.config/age/over-alice.txt
over unlock --user alice --key laptop
```

The key persists privately in Git's common administrative directory until
[`over lock`](lock.md). Closing a terminal or rebooting does not remove it.
Success has no machine-readable payload; errors go to stderr with nonzero status.

## Decryption failures

Errors identify the selected user/key and distinguish malformed or unsupported
Age input, authentication failure, unavailable or cancelled interaction, and
plugin installation or interaction failure. Authentication failure can mean an
incorrect credential **or damaged ciphertext**; it does not prove a wrong password.
Repository/generation mismatches remain separate access-metadata errors.

A scrypt work-limit rejection reports `required`, `target`, and `limit` as
`log_n` factors (`N = 2^log_n`). **Password verification was not attempted.**
Age calibrates passphrase encryption on the creation machine; a substantially
slower machine or VM can reject that cost before checking the password. Use
another configured access method or a trusted environment capable of reading the
envelope, and investigate performance/resource differences. Over does not
automatically raise resource limits or retry with a higher limit.

If interaction is unavailable, use terminal stdin and stderr or an unencrypted
software identity with `--identity`. For plugin failures, check installation on
`PATH`, device connection, PIN, and confirmation. Plugin error bodies are withheld;
bounded device instructions can still appear on stderr.

## Materialization and recovery

A no-option unlock with a valid installed key repairs filters, textconv settings,
and unfinished materialization without reauthentication. Existing plaintext edits
are retained. Fresh unlock refuses ambiguous dirty protected state before
installing a key. Driver repair does not rewrite tracked attributes: existing
repositories must [select `diff=over`](../usage/crypt.md#readable-encrypted-diffs)
explicitly on older rules.

Unlock materializes tracked protected files from their indexed ciphertext without
rewriting the index or `HEAD`. Already matching plaintext is left unchanged;
untracked files and unrelated local edits are preserved. Save ambiguous protected
edits before retrying a refused fresh unlock. Each replacement is atomic and
preserves supported native metadata; multi-file publication uses an encrypted
recovery journal.

If unlock materialization was interrupted, another unlock restores the recorded
preparation state and retries safely using the retained recovery key. Intervening
edits cause refusal and leave the journal intact. Other interrupted crypt
transactions require `over crypt recover`. If an exited process left `index.lock`,
inspect it and remove only that stale lock before retrying; Over never deletes
unknown Git locks automatically.

See [locking and unlocking](../usage/crypt.md#locking-and-unlocking) for the lifecycle and
[user keys](user.md) for supported access methods.
