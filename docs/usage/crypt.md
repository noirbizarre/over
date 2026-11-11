# Repository encryption

Over stores protected Git blobs as deterministic authenticated ciphertext while
presenting ordinary files in unlocked working trees. Git attributes are the
versioned policy; Over maintains them through `crypt` commands. The native
[key-value secret store](secrets.md) uses the same repository key.

## Set up and unlock

From the source repository:

```sh
over user add alice --identity-output ~/.config/age/over-alice.txt
over crypt add .env 'secrets/**'
git commit -m 'Protect repository files'
```

Create the identity's parent directory first. Identity files use standard Age
software identity syntax. Keep the private file outside repositories and back it
up securely. Generation refuses ordinary Git-working-tree paths, preventing an
identity from being staged as plaintext. Alternatively, import a public recipient with `--recipient age1...`
or choose `--passphrase`. `over user add alice` interactively configures the first
key; cancellation cannot persist an empty user.

After a manual clone, the working tree initially contains ciphertext:

```sh
over unlock --identity ~/.config/age/over-alice.txt
```

Unlock configures repository-local filters, unwraps the key, and refreshes clean
protected files. Without an identity option, Over reuses locally remembered
generated identities or asks which key/credential to use. It never takes a
passphrase in command arguments. `--home` / `OVER_HOME` explicitly selects a
repository; otherwise crypt/access commands discover the current Git repository,
falling back to the usual Over home for virtual destinations.

The unlocked key is intentionally persisted under the private Git **common**
administrative directory (`over-crypt/key`). It remains available after closing
terminals/editors and after reboot, until explicit lock. There is no daemon,
agent, keyring, session timeout, or shutdown hook. Filesystem permissions/ACLs and
host OS/full-disk encryption protect it while the OS session is locked.

On Unix the directory is private (`0700`) and the key is `0600`; unsafe owners,
permissions, symlinks, and hard links are rejected. Windows uses private ACLs and
requires PowerShell to establish/check them. Unlocked keys are never stored in
XDG caches, tracked files, logs, or process arguments.

## Policy commands

```text
over crypt add <path-or-pattern>...
over crypt remove <path-or-pattern>... [--yes]
over crypt list
over add --crypt <file>...
```

`crypt add` edits canonical `.gitattributes`, runs existing content through Git's
clean filter, verifies ciphertext, and publishes policy plus encrypted index
entries together. Existing plaintext commits are not rewritten. Literal files
are anchored/escaped; directory selections protect their descendants. Patterns
follow Git attributes syntax. Conflicting nested/local overrides cause failure
before plaintext can be published through the new policy.

For an overlay destination, `over add --crypt` performs policy declaration and
content addition through that same implementation. Select individual regular
files. Virtual paths resolve to their canonical source repository/path; ambiguous
ownership is an error. Virtual checkout commits include necessary source policy
and access metadata alongside encrypted blobs. No per-checkout policy is created.

`crypt remove` warns and requires confirmation; `--yes` is deliberate
noninteractive approval. It stages plaintext with the removal policy, preserves
working content, and requires unlock. Future commits may expose that content.
Removing protection does not remove historical ciphertext or plaintext.

Bootstrap `.gitattributes` and `.over/crypt/` access metadata must remain readable
without the filter. Do not protect them with broad overriding rules.

## Transparent Git and explicit lock

Over installs only local `filter.over.clean`, `filter.over.smudge`,
`filter.over.process`, and `filter.over.required=true`. Git uses its v2 long-lived
filter protocol; one-shot clean/smudge remain available as compatible fallback
commands. Plumbing stdout contains only payload/protocol bytes.

Once unlocked, ordinary Git add/commit/checkout/pull/merge/rebase invoke the
filters without credential prompts. Filters reread repository/generation-matched
local key state for every request, including long-lived processes. Required
filter errors never silently pass plaintext through. Without a key, clean rejects
plaintext and directs you to `over unlock`; supported ciphertext may pass through
without authentication. Locked smudge retains ciphertext. Malformed/unsupported
Over payloads fail. Historical plaintext is preserved by smudge with an exposure
warning, rather than destroying recoverable data.

```sh
over lock
```

Lock preflights all registered linked worktrees sharing the common key. It
refuses dirty, inaccessible, conflicting, or incompatible protected states,
restores clean protected working files from their index ciphertext, then deletes
the local key. It preserves staged content rather than resetting to `HEAD`.
Filters remain configured while locked. Unlock refreshes the linked worktrees
together. Different access metadata must be reconciled before a common rotation.

Lock cannot erase plaintext deployed to virtual destinations, copies held by
other programs, snapshots, backups, or keys already copied elsewhere. Delete does
not promise physical erasure from storage. Protected virtual diffs remain in
Git's ciphertext domain; explicit plaintext inspection is the user's choice.

## Users and keys

```text
over user list
over user show <user>
over user add <user> [--name <key>] [--recipient <age-recipient> | --identity-output <path> | --passphrase]
over user key list <user>
over user key add <user> [--name <key>] [--recipient <age-recipient> | --identity-output <path> | --passphrase]
over user key detach <user> <key>
over user key revoke <user> <key>
over user revoke <user>
over user delete <user>
over key rotate
```

A user is a logical grouping, not an Age identity. Each key independently wraps
the same repository key in a standard Age envelope. Software recipients and
passphrase/scrypt envelopes are supported; future Age plugin adapters can use the
same public recipient/envelope model without changing file encryption.

Duplicate software recipients are rejected so revocation cannot leave the same
private identity authorized through another key alias. Revocation also refuses
a retained passphrase that unlocks a revoked envelope; replace that reused
credential first.

Detaching removes a current envelope only: someone who learned the old key still
has it. Revoking a key/user rotates once for the remaining trusted keys and
re-encrypts current protected files plus native secrets. Deleting a user requires
no active keys. Add a trusted recovery key before revoking the last usable method.

Rotation preflights and validates **every retained passphrase again**, because
Age scrypt has no public recipient that can wrap a fresh key unattended. Missing
or wrong credentials abort without publication. Resolve unstaged protected edits
first. New metadata/content are staged for explicit review/commit; old commits
are not rewritten. A generation change on another branch/clone requires unlock
using that generation's trusted envelope.

Rotation protects new state, not previously published ciphertext, old commits,
clones, backups, caches, or plaintext already copied by recipients. It cannot
provide retroactive revocation. Rotate the underlying exposed credentials too.

## Migration and recovery

Back up the repository and identities before migration or rotation. Create the
initial user/key, then explicitly run:

```sh
over crypt migrate
```

Migration authenticates legacy `.age` overlay files and `.over/secrets/*.age`,
rejects differing destination collisions, stages suffix-free protected files and the
aggregate secret store, and preserves original Age ciphertext under private
Git administrative `over-crypt/legacy-backup/`. Supply the **legacy** credential
via its hidden prompt or `OVER_SECRET`; it is not a new repository unlock method.
Review the staged transition and commit explicitly. Keep backups until verified.
Retrying recognizes identical encrypted backups/prepared content; differing
legacy/index snapshots or new store values require explicit resolution first.

Interrupted publication leaves an encrypted recovery journal and blocks
cooperating encryption operations. Preserve the local key and journal, then run:

```sh
over crypt recover
```

Recovery rolls back recorded state only if current bytes match the old or
prepared version; intervening edits cause refusal rather than data loss. A
successfully published commit is recognized and preserved. Recovery snapshots
include encrypted source/target bytes and index metadata, never plaintext keys.
If a process died while owning Git's `index.lock`, first verify that process has
exited, inspect the lock, and remove only that stale lock before retrying recovery.
Over never guesses that another Git process's lock is stale. A changed unrelated
`HEAD` also requires inspection before rollback.
After a completed rotation with a stale local key, unlock through the new
envelopes. For migration rollback after verification, use the saved Age ciphertext
and a backed-up repository/index; do not discard new edits while restoring it.

## Storage checks and format

`over doctor` / `over crypt list` inspect effective policy, required local filter
configuration, and raw index/`HEAD` blobs. Working-tree plaintext after unlock is
expected, not a leak. Locked reports distinguish structural checks from
authentication unavailable without a key. Findings identify the storage location
and remediation; diagnosis never stages or rewrites encryption state.

The bounded history check examines at most 16 commits and 256 path/blob pairs.
Known plaintext exposure is reported; absence of a finding does not establish
clean history. A complete reachable-history audit is a separate future
capability. Encrypting current versions does not erase historical exposure.

Payload v1 uses RustCrypto RFC 5297 AES-256-SIV, authenticates its version/domain/
generation header, and authenticates the entire message before returning
plaintext. Identical plaintext/key/domain produces identical bytes across
invocations and machines. Equality, length, names, and Git metadata are visible;
small changes replace the complete ciphertext. Files are processed in memory
with a 256 MiB limit. See [ADR-028](../adr/028-repository-encryption.md).
