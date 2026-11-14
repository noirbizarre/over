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

Credential prompts require terminal stdin and stderr. For scripted fresh unlock,
supply `--identity`; Git filters and hooks never acquire credentials themselves.
Repeating `over unlock` with an already valid local key repairs filters and
unfinished ciphertext materialization without reauthentication. Existing
uncommitted plaintext edits are retained; a fresh unlock refuses ambiguous
dirty ciphertext/working-tree state before installing a key.

The unlocked key is intentionally persisted under the private Git **common**
administrative directory (`over-crypt/key`). It remains available after closing
terminals/editors and after reboot, until explicit lock. There is no daemon,
agent, keyring, session timeout, or shutdown hook. Filesystem permissions/ACLs and
host OS/full-disk encryption protect it while the OS session is locked.

On Unix the directory is private (`0700`) and the key is `0600`; unsafe owners,
permissions, symlinks, and hard links are rejected. Windows uses private ACLs,
native owner/DACL validation, and PowerShell for initial ACL setup. Unsupported
noncanonical ACE layouts fail closed. Unlocked keys are never stored in
XDG caches, tracked files, logs, or process arguments.

Filter configuration is published as one local update before granting new key
access. Failed preflight or configuration leaves the previous key state intact.
Recognized abandoned key-write temporaries are removed under exclusive lifecycle
ownership. On Unix, successful lock also synchronizes the administrative
directory after key removal. Filesystem snapshots, platform durability limits,
and physical storage erasure remain outside this guarantee.

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

`add --crypt` checks ownership, source drift, checkout freshness, and staged
bootstrap metadata before publication. Ordinary move/link ingestion retains its
encrypted rollback journal and index reservation until completion; failure
restores policy, index, and original input inodes. Independent existing source
files are refused even if their bytes match, because their access metadata may
differ. Use `crypt add` for files already stored in the source repository.

`crypt remove` warns and requires confirmation; `--yes` is deliberate
noninteractive approval. It stages plaintext with the removal policy, preserves
working content, and requires unlock. Future commits may expose that content.
Removing protection does not remove historical ciphertext or plaintext.

Bootstrap `.gitattributes` and `.over/crypt/` access metadata must remain readable
without the filter. Do not protect them with broad overriding rules.

## Transparent Git and explicit lock

Flat applications can share a virtual-checkout destination while retaining distinct
canonical source paths and encryption policy. Use `over add --crypt --overlay NAME PATH`
to acquire a new protected destination file explicitly. Its destination stays an
ordinary file; its staged and committed source blobs use native ciphertext. Existing
owned destination files can resolve their source policy through `over crypt add`.

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

Untracked protected plaintext, policy overrides, symlinked protected ancestry,
and unstaged protection declarations prevent lock. Representation changes retain
executability and supported native access metadata; read-only destinations are
not silently replaced. An inaccessible worktree blocks the common transition.

Lock cannot erase plaintext deployed to virtual destinations, copies held by
other programs, snapshots, backups, or keys already copied elsewhere. Delete does
not promise physical erasure from storage. Ordinary protected virtual diffs remain
in Git's ciphertext domain. Generated-checkout comparisons explicitly decode
protected snapshots while unlocked, and requested diffs can display rendered values.

## Protected generated snapshots

Generated checkout snapshots preserve each source entry's encryption policy,
including ordinary protected siblings of templates. Protected rendered bytes and
reverse-patch consumed renderings are encrypted before entering the Git object
database. Unlock makes their plaintext available transiently for rendering,
comparison, merging, restore, and reverse patching; it never decrypts the stored
Git blobs. These operations require the matching unlocked repository key.

Each generated commit carries a versioned provenance manifest in its message.
Source/destination names, protection policy, and Git metadata remain visible.
Unprotected templates still produce unprotected snapshots, even when their
rendering contains secrets obtained from context data. Protect the source to
protect its generated snapshot.

Lock and unlock also inspect older generated history and retained operational
snapshots. Known legacy snapshots are rewritten with encrypted protected entries
and updated checkout pointers. Interrupted migration resumes through unlock or
`over crypt recover`. Migration preserves pending/conflict roots under local
`refs/over/generated-retained/` refs so Git maintenance cannot prune needed bases.
Successful unapply removes an association's retained refs along with its main ref.

Legacy source mappings can change and source history can be replaced. Migration
therefore conservatively encrypts historical output unless its bytes are already
present in a known public source blob, recording the original exposed object IDs.
A public template today does not establish that an older rendering was public.
Missing ownership/source hints still require restored provenance or explicit cleanup.

Rewriting refs does **not** erase old plaintext objects. Over records known leaked
blob IDs in `over-crypt/generated-snapshots.json` under the common Git directory
and checks their actual availability, including packed and alternate object
stores. While known plaintext remains, lock/unlock reports incomplete cleanup
and preserves the previous source worktree/key access state; successfully
migrated snapshot refs remain migrated. Keep the ledger until cleanup is verified.

Inspect remaining refs, reflogs, and alternate object stores, stop repository
writers, and deliberately perform the appropriate Git history/reflog/object
maintenance. Over does not expire unrelated recovery history or automatically
prune your object database. Rerun lock/unlock to verify cleanup. If provenance is
missing or ambiguous, restore the matching XDG association/source history, or
explicitly retire the affected snapshots and their objects. The audit also finds
unreferenced legacy snapshot commits left by failed publication. Local cleanup
cannot establish erasure from clones, remotes, backups, or filesystem snapshots.

Repositories without protected source history can continue using their legacy
snapshots for ordinary and reverse commits without configuring encryption. These
comparisons do not certify old snapshot contents as public: reverse checkpoints
retain their legacy representation until sync publishes a new desired inventory.
An interrupted unencrypted sync can finish without a repository key.

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

Rotation and revocation refuse before publication if retained protected generated
snapshots would become unreadable with the new key. Resolve local edits and
unapply those generated checkouts first, then rotate and reapply. Historical
source templates encrypted with an unavailable generation cannot be reverse
patched; Over reports the missing matching key rather than processing ciphertext
as template text.

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
The journal records each participating worktree's `HEAD` and the initiating
worktree, so recovery from a sibling cannot overlook another branch advancing.
Changed ancestry, permissions, symlinks, or ingestion inode identity cause
refusal before rollback. Existing unrelated files and nonempty new scaffolds
are retained.
After a completed rotation with a stale local key, unlock through the new
envelopes. For migration rollback after verification, use the saved Age ciphertext
and a backed-up repository/index; do not discard new edits while restoring it.

## Storage checks and format

`over doctor` / `over crypt list` inspect effective policy, required local filter
configuration, and raw index/`HEAD` blobs. Working-tree plaintext after unlock is
expected, not a leak. Locked reports distinguish structural checks from
authentication unavailable without a key. Findings identify the storage location
and remediation; diagnosis never stages or rewrites encryption state.

The bounded source-history check examines at most 16 commits and 256 path/blob pairs.
Known plaintext exposure is reported; absence of a finding does not establish
clean history. A complete reachable-history audit is a separate future
capability. Encrypting current versions does not erase historical exposure.
Generated refs and operational roots are checked separately across their complete
history; the object database is also inspected for orphaned Over snapshot commits
and known leaked blob IDs. These checks do not mutate refs or encryption state.

Payload v1 uses RustCrypto RFC 5297 AES-256-SIV, authenticates its version/domain/
generation header, and authenticates the entire message before returning
plaintext. Identical plaintext/key/domain/generation produces identical bytes across
invocations and machines. Equality, length, names, and Git metadata are visible;
small changes replace the complete ciphertext. Files are processed in memory
with a 256 MiB limit. See [ADR-028](../adr/028-repository-encryption.md).

The hardened filter-input buffers, their retired growth allocations, prepared
refresh/transaction content, and serialized secret/journal buffers are wiped when
released. This is not complete memory
erasure: Age 0.12's private streaming-writer chunk, terminal/runtime libraries,
external processes, OS buffers, swap, dumps, and filesystem copies may retain
material beyond Over's public interfaces. Publication uses private filesystem
staging; process-kill cleanup and physical erasure are not guaranteed.
