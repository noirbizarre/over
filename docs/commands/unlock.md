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

Explicit identities replace remembered identity-file sources. Otherwise applicable
private local identity hints are tried before interactive credential acquisition.
Independent selected passphrase envelopes can still require hidden input.
Credential prompts require terminal stdin and stderr. Software identities can
support unattended unlock; encrypted identity files and plugins may still prompt.

```sh
over unlock --identity ~/.config/age/over-alice.txt
over unlock --user alice --key laptop
```

The key persists privately in Git's common administrative directory until
[`over lock`](lock.md). Closing a terminal or rebooting does not remove it.
Success has no machine-readable payload; errors go to stderr with nonzero status.

A no-option unlock with a valid installed key repairs filters, textconv settings,
and unfinished materialization without reauthentication. Existing plaintext edits
are retained. Fresh unlock refuses ambiguous dirty protected state before
installing a key. Driver repair does not rewrite tracked attributes: existing
repositories must [select `diff=over`](../usage/crypt.md#readable-encrypted-diffs)
explicitly on older rules.

See [locking and unlocking](../usage/crypt.md#locking-and-unlocking) for the lifecycle and
[user keys](user.md) for supported access methods.
