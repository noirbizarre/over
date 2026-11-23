# `over user`

Manage logical users and their independent Age envelopes for the repository key.

```text
over user list
over user show <USER>
over user add <USER> [NAME] [KEY-OPTIONS]
over user revoke <USER> [--yes]
over user delete <USER> [--yes]
over user key list <USER>
over user key add <USER> [NAME] [KEY-OPTIONS]
over user key detach <USER> <KEY> [--yes]
over user key revoke <USER> <KEY> [--yes]
```

Accepts [global options](index.md#shared-options-and-selection). Repository
selection follows [`over crypt`](crypt.md#repository-selection-and-options).
Names identify logical access records, not operating-system accounts.

## Add a user or key

`KEY-OPTIONS` are the same for `user add` and `user key add`:

| Option | Meaning |
| --- | --- |
| `--name <NAME>` | Alternative to the positional key name |
| `--name-prefix <PREFIX>` | Name imported recipients `PREFIX-1`, `PREFIX-2`, etc. |
| `-r`, `--recipient <RECIPIENT>` | Wrap for an existing public Age recipient; repeatable |
| `-R`, `--recipients-file <PATH>` | Import recipients from a standard Age list; repeatable |
| `--identity-output <PATH>` | Generate a software identity at a new private local path |
| `--passphrase` | Wrap with a passphrase entered twice through hidden input |

The positional `NAME` and `--name` are alternatives; both conflict with
`--name-prefix`. Recipient imports conflict with identity
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
over user key add alice recovery --recipient age1...
over user key list alice
```

Each enrollment, detach, deletion, or revocation creates an isolated commit,
honoring Git identity, hooks, signing, and message cleanup. Batch enrollment
commits all imported keys together. Unrelated staged work is preserved; pending
access-metadata edits must be committed or restored first. Configure
[access commit messages](../configuration/root.md#access-commit-messages) at the root.

New access methods live in `.over/users/<user>/<key>`, with a stable `user.json`
record preserving empty users. `.over/crypt.json` holds only the version,
repository ID, and active generation. Adding another key leaves the header and
existing key files unchanged. User/key names must be portable filesystem names;
case-colliding and platform-reserved names are rejected.
These naming requirements apply to access records, not unrelated Git paths;
Unix repositories may retain unrelated non-UTF-8 tracked filenames during access
mutations without renaming or committing their pending changes.

Each `user.json` contains `{"version":2}`: public existence metadata, not a
credential or profile. Git cannot represent an empty directory, so the record
retains the user after their last key is detached/revoked. `user delete` removes it.

Existing aggregate and nested split access metadata migrate automatically in the
next user/key mutation's commit. List/show/unlock remain compatible without migration.

## Inspect and remove access

Revocation, deletion, and detachment require confirmation, defaulting to No.
Interactive confirmation requires terminal stdin and stderr; pass `--yes` to
approve the selected operation in scripts. Approval precedes credential prompts
and mutations, and does not bypass unlock or recovery-key safeguards.

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
