# `over user`

Manage logical users and their independent Age envelopes for the repository key.

```text
over user list
over user show <USER>
over user add <USER> [KEY-OPTIONS]
over user revoke <USER>
over user delete <USER>
over user key list <USER>
over user key add <USER> [KEY-OPTIONS]
over user key detach <USER> <KEY>
over user key revoke <USER> <KEY>
```

Accepts [global options](index.md#shared-options-and-selection). Repository
selection follows [`over crypt`](crypt.md#repository-selection-and-options).
Names identify logical access records, not operating-system accounts.

## Add a user or key

`KEY-OPTIONS` are the same for `user add` and `user key add`:

| Option | Meaning |
| --- | --- |
| `--name <NAME>` | Name the new access key |
| `--name-prefix <PREFIX>` | Name imported recipients `PREFIX-1`, `PREFIX-2`, etc. |
| `-r`, `--recipient <RECIPIENT>` | Wrap for an existing public Age recipient; repeatable |
| `-R`, `--recipients-file <PATH>` | Import recipients from a standard Age list; repeatable |
| `--identity-output <PATH>` | Generate a software identity at a new private local path |
| `--passphrase` | Wrap with a passphrase entered twice through hidden input |

`--name` conflicts with `--name-prefix`. Recipient imports conflict with identity
generation and passphrase wrapping; generation and passphrase are alternatives.
Multiple recipients require a prefix, with inline recipients preceding file
recipients. Without a method, an interactive chooser is required. The first key
for a user defaults to `default`; an additional key without a name prompts, so
name it explicitly in noninteractive invocations.

New passphrases require matching confirmation. If the entries differ, both
hidden prompts repeat before the user or key is added.

The first `user add` initializes repository encryption, installs Git drivers, and
persists an unlocked key. Subsequent access changes need the unlocked repository
key. Generated identities require an existing parent and nonexistent output path;
ordinary Git-working-tree storage is rejected. Keep private identities outside
the working tree and back them up.

```sh
over user add alice --identity-output ~/.config/age/over-alice.txt
over user key add alice --name recovery --recipient age1...
over user key list alice
```

Each enrollment, detach, deletion, or revocation creates an isolated commit,
honoring Git identity, hooks, signing, and message cleanup. Batch enrollment
commits all imported keys together. Unrelated staged work is preserved; pending
access-metadata edits must be committed or restored first. Configure
[access commit messages](../configuration/root.md#access-commit-messages) at the root.

New access methods live in `.over/crypt/users/<user>/<key>`, with a stable `.user`
marker preserving empty users. `.over/crypt/repository.json` holds only the version,
repository ID, and active generation. Adding another key leaves the header and
existing key files unchanged. User/key names must be portable filesystem names;
case-colliding and platform-reserved names are rejected.

Existing aggregate access metadata migrates automatically in the next user/key
mutation's commit. List/show/unlock remain compatible without migration.

## Inspect and remove access

`user list` prints usernames only, one per line, on stdout. `user show` and
`user key list` print the username followed by indented `KEY: RECIPIENT` lines;
passphrase entries display `Age passphrase`. They never print private key bytes.

`key detach` removes an envelope without revoking a repository key already learned
through it. The only recovery key cannot be detached. `user delete` requires an
existing user with no remaining keys.

`user revoke` and `user key revoke` rotate current protected content for remaining
trusted access. Retained passphrase keys require credential reacquisition.
Protected files and policy must be clean. The initiating branch is committed;
generation updates in linked worktrees remain staged and are reported.
Mutations have no machine-readable success
payload; errors are stderr/nonzero.

See [users, hardware compatibility and access recovery](../usage/crypt.md#users-and-keys)
and [`over key rotate`](key.md) for rotation and history limits.
