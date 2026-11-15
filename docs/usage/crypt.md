# Repository encryption

Over stores protected Git blobs as deterministic authenticated ciphertext while
presenting ordinary files in unlocked working trees. Git attributes are the
versioned policy; manage them through `crypt` commands or edit them directly.
The native [key-value secret store](secrets.md) uses the same repository key.

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
protected files. Without an identity option, Over tries all applicable locally
remembered identities, then asks for remaining credentials. It never takes a
passphrase in command arguments. `--home` / `OVER_HOME` explicitly selects a
repository; otherwise crypt/access commands discover the current Git repository,
falling back to the usual Over home for virtual destinations.

Credential prompts require terminal stdin and stderr. For scripted fresh unlock,
supply an available unencrypted software identity with `--identity`; encrypted
identity files and hardware identities can still require terminal interaction.
Git filters and hooks never acquire credentials themselves.
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

Use the [`over crypt` reference](../commands/crypt.md) for exact arguments,
options, and diagnostic output, and [`over add --crypt`](../commands/add.md)
for ingestion of destination files.

`crypt list` inspects the canonical policy, effective attributes, filter setup,
and protected content in storage; it does not change policy or stage files.

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

## Manage attributes manually

Editing `.gitattributes` directly is also a first-class way to manage encryption
policy. It uses exactly the same Over Git filter as `crypt add`. For example,
add these rules to the source repository's root `.gitattributes`:

```gitattributes
*.secret filter=over diff=over -text
.env filter=over diff=over -text
.env.* filter=over diff=over -text
secrets/** filter=over diff=over -text
.gitattributes -filter !diff
.over/crypt/** -filter !diff -text
```

Patterns follow [Git attributes syntax](https://git-scm.com/docs/gitattributes),
including its directory and precedence rules. Check nested `.gitattributes`,
global attributes, and `.git/info/attributes` for overrides. Keep bootstrap
attributes and `.over/crypt/` outside protection.

`filter=over` enables encryption on clean (staging) and decryption on smudge
(unlocked checkout). `diff=over` selects Over's authenticated text conversion so
Git can display readable encrypted diffs. `-text` prevents Git's text/EOL
normalization from changing protected bytes. Slashless `*.secret` matches those
basenames at any depth; a directory rule alone is not recursive, so use `/**`.
Git attributes do not support negative patterns: use later attribute overrides.

### Configure the driver

`filter=over` selects a driver; it does **not** install or configure one. Initialize
[user access](#set-up-and-unlock) if the repository has none yet. On each clone,
use an authorized identity and run from that repository:

```sh
over unlock --identity ~/.config/age/over-alice.txt
```

This installs the repository-local filter and diff settings and unlocks the repository.
Repeating it repairs the driver. It does not require `over crypt add` or change
your manually managed rules.

The attribute names are independent of Over's CLI namespace: `filter=over` uses
the `filter.over.*` settings, and `diff=over` uses `diff.over.textconv`. These
settings invoke helpers under `over crypt`; `over git` manages mounted repositories.
Existing Git-prefixed helper commands remain hidden compatibility aliases for
installed configurations. Run `over unlock` to refresh them to the crypt namespace
without changing `.gitattributes`.

For direct Git configuration, the equivalent driver settings are below. Replace
`/home/alice/.cargo/bin/over` with your installed executable's absolute path,
retaining the inner shell quotes (escape any apostrophes in the path):

```sh
git config --local filter.over.clean "'/home/alice/.cargo/bin/over' crypt filter clean"
git config --local filter.over.smudge "'/home/alice/.cargo/bin/over' crypt filter smudge"
git config --local filter.over.process "'/home/alice/.cargo/bin/over' crypt filter-process"
git config --local filter.over.required true
git config --local diff.over.textconv "'/home/alice/.cargo/bin/over' crypt textconv --"
git config --local diff.over.cachetextconv false
```

Git evaluates these command strings through a shell. Over's setup records its
own absolute executable path with shell quoting. Using just `over` instead
requires it on every Git client's `PATH`, including editors and services;
storage diagnostics expect the settings installed by `over unlock`. These Git
settings alone neither create user/access metadata nor acquire a repository key.

Keep `required=true`: a failed required filter must stop the Git operation rather
than silently stage plaintext. Never disable or bypass filters to work around an
error; resolve the error and retry. Attributes alone cannot enforce this on an
unconfigured Git client: a clone without the driver configuration can stage
plaintext. Configure every writer before staging protected files.

### Re-stage already tracked plaintext

Declaring an attribute does not rewrite existing index blobs or history. Use this
procedure from the source repository root, with a backup and a reviewed clean
index/worktree. It stages the current content of the selected files, including
any edits you make before re-staging.

1. Set up the first user/key if needed and run `over unlock` **before** adding new
   protection rules. Commit/review the initial access metadata separately.
2. Edit `.gitattributes`, for example with the rules above. Check effective rules
   for each selected file, then stage the policy:

   ```sh
   git check-attr filter -- .env secrets/example.conf
   git add -- .gitattributes
   ```

   Each protected file should report `filter: over`. Resolve any conflicting
   overrides before continuing.
3. Force Git to run clean again on the specific already tracked files:

   ```sh
   git add --renormalize -- .env secrets/example.conf
   ```

   `--renormalize` only re-adds tracked files. Stage new files separately with
   `git add -- <new-path>` after verifying their effective attributes. Do not
   include unrelated files unless you intend to stage their changes too.
4. Verify storage while unlocked:

   ```sh
   over crypt list
   git diff --cached --stat
   ```

   Look for an authenticated **index** finding for every selected file, such as
   `.env index: ciphertext (authenticated)`. `crypt list` (or `over doctor`)
   reads the raw indexed blob and authenticates it with the local key; unlocked
   working-tree plaintext is expected. A policy line alone is insufficient.
   Do not print raw blobs to a terminal or logs to inspect them: a missed rule
   could expose plaintext. A ciphertext header alone would not authenticate it.

   Before the commit, findings about plaintext in `HEAD` are expected for files
   being converted; they are distinct from the index you will commit. Resolve
   index, policy, or driver errors before proceeding. Review the complete staged
   change, including `.gitattributes` and any necessary access metadata, then
   commit it. Recheck storage afterward: `HEAD` should now contain ciphertext,
   but historical exposure findings can remain.

This protects the newly staged versions only. Earlier plaintext commits, clones,
backups, and remotes remain exposed; rotate any credentials already disclosed.

## Transparent Git and explicit lock

Flat applications can share a virtual-checkout destination while retaining distinct
canonical source paths and encryption policy. Use `over add --crypt --overlay NAME PATH`
to acquire a new protected destination file explicitly. Its destination stays an
ordinary file; its staged and committed source blobs use native ciphertext. Existing
owned destination files can resolve their source policy through `over crypt add`.

Over installs local `filter.over.clean`, `filter.over.smudge`,
`filter.over.process`, `filter.over.required=true`, and the
[diff settings](#readable-encrypted-diffs). Git uses its v2 long-lived
filter protocol; one-shot clean/smudge remain available as compatible fallback
commands. Plumbing stdout contains only payload/protocol bytes.

Git runs clean when staging files (`git add`, including staging by `commit -a`),
and smudge when populating working files during checkout and relevant
pull/merge/rebase operations. An ordinary `git commit` records the existing index;
it does not re-clean unchanged index entries. The long-lived process driver takes
precedence over the one-shot commands. These filters never prompt for credentials.
Filters reread repository/generation-matched local key state for every request,
including long-lived processes. Required
filter errors never silently pass plaintext through. Without a key, clean rejects
plaintext and directs you to `over unlock`; supported ciphertext may pass through
without authentication. Locked smudge retains ciphertext. Malformed/unsupported
Over payloads fail. Historical plaintext is preserved by smudge with an exposure
warning, rather than destroying recoverable data.

### Locking and unlocking

Repository encryption and the native secret store share one persistent local key:

```sh
over unlock --identity ~/.config/age/over-alice.txt
# Work with protected files and native secrets.
over crypt list
over lock
```

Before locking, review and stage protected edits you want to keep. If lock
refuses a transition, resolve the reported problem and retry; failure does not
imply key removal. Lock requires an installed key, so it is not an unconditional
already-locked no-op. Closing a terminal, rebooting, or unplugging a hardware
device does not lock the repository.

Lock preflights all registered linked worktrees sharing the common key. It
refuses dirty, inaccessible, conflicting, or incompatible protected states,
restores clean protected working files from their index ciphertext, then deletes
the local key. It preserves staged content rather than resetting to `HEAD`.
Filters remain configured while locked. Unlock refreshes the linked worktrees
together. Different access metadata must be reconciled before a common rotation.

Native secret operations fail while locked. Smudge and textconv preserve supported
ciphertext byte-for-byte without decryption, so locked checkout and diffs remain
usable. Ciphertext passthrough cannot authenticate the encrypted payload.
Unlock restores access to the native store without converting its ciphertext into a
plaintext working-tree file. See [native secrets](secrets.md#unlock-once).

Unlock explicitly materializes tracked protected files from the index; installing
drivers alone would not rerun smudge on an existing checkout. Index entries and
`HEAD` are unchanged. Matching plaintext is left alone, untracked files are not
touched, and unrelated staged/unstaged edits are preserved. Fresh unlock refuses
ambiguous protected edits rather than overwriting them; save those edits and retry.
Repairing an already completed unlock also preserves ordinary protected plaintext
edits. File replacements are individually atomic and preserve supported metadata;
multi-file publication is journaled and recoverable.

Untracked protected plaintext, policy overrides, symlinked protected ancestry,
and unstaged protection declarations prevent lock. Representation changes retain
executability and supported native access metadata; read-only destinations are
not silently replaced. An inaccessible worktree blocks the common transition.

Lock cannot erase plaintext deployed to virtual destinations, copies held by
other programs, snapshots, backups, or keys already copied elsewhere. Delete does
not promise physical erasure from storage. Requested readable diffs display
plaintext while unlocked; protect their output like any other decrypted copy.
Long-lived filter processes keep no repository key between requests. Lock waits
for active transformations, and subsequent requests reread locked state. Already
transformed output may still be buffered by helpers or retained by Git, pagers,
and other processes after lock; removing the key does not promise memory erasure.

For opt-in systemd/Sway and macOS examples, see
[lock/unlock automation recipes](crypt-automation.md).

Exact flags are in [`over lock`](../commands/lock.md) and
[`over unlock`](../commands/unlock.md). For noninteractive startup, supply an
available unencrypted software identity; encrypted identities and hardware
plugins can still require interaction during unlock.

## Readable encrypted diffs

New encryption policies include `diff=over`. While unlocked, ordinary `git diff`,
`git diff --cached`, `git show`, and Git-backed Over comparisons use the local
textconv driver to authenticate and display protected plaintext. Clean/smudge
storage behavior is unchanged: index and committed protected blobs stay encrypted.
The native store also supports explicit diff conversion without decrypting its
working file; its presentation is the serialized map, including numeric byte arrays.

For an existing repository, run `over unlock` to repair/install local driver
settings, then add `diff=over` to older protection rules and review/commit the
attribute change. Alternatively, reapply selected policy through `over crypt add`.
Review `.over/.gitattributes` too: older `secrets.yaml filter=over -text` rules
need `diff=over` added explicitly. Unlock never rewrites or stages these rules.
Check effective selections with `git check-attr filter diff -- PATH`.

Conversion never prompts. Locked ciphertext passes through unchanged. With an
installed key, authentication errors and unavailable generations fail rather than
producing a misleading plaintext patch. Unsafe or corrupt installed keys and
interrupted transactions also remain errors.
Only the current repository key is retained: older generations, including the
old `HEAD` side of a staged rotation, cannot be converted. Use
`git diff --cached --no-textconv --stat` when reviewing a rotation.

Over sets `diff.over.cachetextconv=false`, preventing persistent Git conversion
caches with its installed configuration. Diffs can still disclose plaintext to
pagers, redirected output, and external tools; lock does not erase those copies
or previously created caches. Git textconv patches are for human review, not
applying; `git format-patch` does not generate them. External difftools retain
their own behavior. See [Git helper reference](../commands/crypt-filters.md#textconv).

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

Pending/conflict roots are retained under local `refs/over/generated-retained/`
refs so Git maintenance cannot prune needed bases. Successful unapply removes an
association's retained refs along with its main ref. Removing refs does not erase
objects from the Git database, clones, remotes, backups, or filesystem snapshots.

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
the same repository key in a separately addressable standard Age envelope.
Software, SSH, passphrase/scrypt and plugin-backed keys can belong to the same
user. Any one available key can unlock; possession of every device is not required.

Import standard recipient lists directly, including Passage's recipient files:

```sh
over user add alice --recipients-file ~/.passage/store/.age-recipients --name-prefix imported
over user key add alice --name laptop --recipient age1...
over unlock --identity ~/.passage/identities
over unlock --identity ~/.passage/identities --identity ~/.config/age/recovery.txt
```

`--recipient` (`-r`) and `--recipients-file` (`-R`) can be repeated. Each recipient
becomes a separate key/envelope. Multiple recipients require `--name-prefix`;
the example creates `imported-1`, `imported-2`, and so on. Inline recipients come
first in argument order, followed by files in argument order and their lines in
file order. Name collisions, duplicate recipients, and failures abort the entire
import. No partially configured new user is published. For one recipient, use
`--name` or the new user's default key name. Input paths must name files; `-`
stdin inputs are not supported.

Recipient files accept comments, empty lines, CRLF, and multiple recipients.
Unsupported SSH key types in public lists are skipped with a message; an empty
supported list is an error. Identity inputs accept standard multi-identity files,
binary or armored passphrase-encrypted identity files, SSH private keys (including
encrypted keys), and plugin stubs. An identity-file password protects that file;
a passphrase access key instead wraps the repository key directly with scrypt.
Over reads external identity files in place and remembers successful paths only
in private local hints. It does not copy them into the repository or depend on
Passage's store layout or environment variables.

Explicit `--identity` (`-i`) arguments restrict the identity-file sources to those
supplied, rather than silently adding remembered files. Over still tries selected
independent passphrase keys if necessary. `--user` and `--key` limit every attempt.
Missing plugins, unavailable devices, denied requests, wrong credentials, and
stale hints do not block another independently configured key. Successful unlock
reports unavailable attempts; if all fail, a bounded per-key/source summary is
returned without installing a key or publishing plaintext. A plugin-level denied
request skips that method; interrupting the command cancels the whole operation.

Ordinary Age, SSH, encrypted identities and hardware plugins use the Rust Age
library. Native software post-quantum identities (`AGE-SECRET-KEY-PQ-1...`) and
recipients (`age1pq1...`) require the standard `age` executable, version 1.3 or
later, on `PATH`: the published Rust library lacks this format. Only that envelope
operation uses the compatibility adapter; filters never run the Age CLI. Private
identity input goes through stdin, not arguments or temporary plaintext files.
A classical recovery envelope for the same repository key limits any claim of
post-quantum protection.

Duplicate canonical public recipients are rejected so revocation cannot leave the
same recipient authorized through another key alias. Distinct plugin recipient
strings can still represent related underlying credentials; Over cannot prove
their independence. Revocation also refuses
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

Rotation also needs each retained recipient's wrapping implementation, including
its plugin. Failure aborts rotation; retained keys are never silently dropped.

### Hardware keys: YubiKey and Ledger

Install and configure the upstream plugin, then import its public recipient and
use its normal identity stub. Over does not generate, import, or export hardware
private keys. Use the plugin's own setup tools for device generation, selection,
PIN and touch policy; no Over-specific device format is involved.

For [YubiKey](https://github.com/str4d/age-plugin-yubikey):

```sh
age-plugin-yubikey
age-plugin-yubikey --identity --slot SLOT > ~/.config/age/yubikey.txt
age-plugin-yubikey --list > ~/.config/age/yubikey-recipients.txt
over user key add alice --recipients-file ~/.config/age/yubikey-recipients.txt --name-prefix yubikey
over unlock --identity ~/.config/age/yubikey.txt
```

`age-plugin-yubikey` must be discoverable on `PATH`. Linux/BSD also need a running
PC/SC service (`pcscd`/pcsc-lite); source installation needs PC/SC development
headers. The plugin controls PIN/touch policy and any PIN caching. Over surfaces
its insertion/touch messages, confirmations and hidden PIN input in the terminal.

For [Ledger](https://github.com/Ledger-Donjon/age-plugin-ledger), install
`age-plugin-ledger` and the upstream [age app](https://github.com/Ledger-Donjon/app-age):

```sh
age-plugin-ledger --identity > ~/.config/age/ledger.txt
age-plugin-ledger --list > ~/.config/age/ledger-recipients.txt
over user key add alice --recipients-file ~/.config/age/ledger-recipients.txt --name-prefix ledger
over unlock --identity ~/.config/age/ledger.txt
```

Compatibility status checked against upstream documentation and repository status
on **2026-11-14**:

| Plugin | Upstream-listed devices | Availability and validation |
| --- | --- | --- |
| YubiKey | YubiKey 4/5 series, including Nano and USB-C variants | Upstream provides Linux/macOS/Windows binaries; Over's protocol fixture is tested, physical devices/platform combinations are not certified here. |
| Ledger | Nano S Plus and Nano X | App README describes a pre-release Ledger Live app using Developer mode/provider 4. **The app repository is archived**; current catalog availability and device/firmware compatibility require verification before relying on it. The plugin repository is not archived. No physical Ledger validation was performed for this integration. |

YubiKey NEO and the FIDO-only “Security Key by Yubico” are not supported by the
upstream PIV plugin. Other PIV devices and Ledger models are not claimed as tested
Over combinations. Do not equate protocol-fixture success with hardware/security
certification, particularly for the archived, pre-release Ledger app. Report
plugin/app defects upstream rather than adding device-specific workarounds.

Device interaction unwraps the repository key during `over unlock` (or explicit
key enrollment/rotation where a plugin requires it), never for each protected
file. The persisted local symmetric key has the lifecycle described above;
unplugging the device does not lock the repository. Use `over lock` explicitly.

Before relying on a hardware key, add and test an independently controlled recovery
key. With the repository locked, verify unlock with each device, PIN/touch and
confirmation handling, recovery with the device disconnected/plugin unavailable,
and ordinary Git operations without additional device prompts. Record the exact
model, firmware, plugin/app version and operating system. If a device is lost,
unlock with a recovery method and detach its envelope; if compromise is possible,
revoke/rotate and re-encrypt instead. Without any usable configured identity or
passphrase, Over cannot recover the repository key.

Rotation and revocation refuse before publication if retained protected generated
snapshots would become unreadable with the new key. Resolve local edits and
unapply those generated checkouts first, then rotate and reapply. Historical
source templates encrypted with an unavailable generation cannot be reverse
patched; Over reports the missing matching key rather than processing ciphertext
as template text.

Rotation protects new state, not previously published ciphertext, old commits,
clones, backups, caches, or plaintext already copied by recipients. It cannot
provide retroactive revocation. Rotate the underlying exposed credentials too.

## Recovery

Back up the repository and identities before changing protection policy or
rotating keys.

Interrupted publication leaves an encrypted recovery journal and blocks
cooperating encryption operations. Preserve the local key and journal. For an
interrupted unlock materialization, rerun `over unlock`: it safely restores the
recorded preparation state and retries, refusing intervening edits. This automatic
retry applies only to journals explicitly created for unlock; other transactions
and older journals require:

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
envelopes.

## Committed metadata and pre-checkout detection

Over can identify repository encryption directly from a committed Git tree,
including in a bare repository or immediately after `git clone --no-checkout`.
The existing `.over/crypt/access.json` is the versioned public marker; no extra
enablement file is required. Commit it, the applicable `.gitattributes` files,
and protected ciphertext together. A first-user setup without protected files
still needs its access metadata committed to be discoverable after clone.

### Access registry version 1

The registry is a regular tracked JSON blob, limited to 16 MiB:

| Field | Version 1 representation and meaning |
| --- | --- |
| `version` | Integer `1`; selects the registry schema. |
| `repository` | Array of 16 byte values; stable public repository identity. |
| `generation` | Array of 16 byte values; current public key-generation identity. |
| `users` | Object mapping logical user names to objects of named access methods. |
| `users.<user>.<key>.recipient` | Public Age recipient string, or `null` for a passphrase method. |
| `users.<user>.<key>.envelope` | Array of bytes containing an Age-encrypted repository key record, at most 16 KiB. |

User and access-method names contain 1–128 ASCII letters, digits, hyphens, or
underscores. Each independent envelope wraps the same 96-byte record: repository
identity, generation, and the 64-byte repository key. The public registry contains
no unencrypted repository key or private identity. Passphrase methods use separate
scrypt envelopes and have no reusable public recipient.

Readers accept unknown JSON fields in version 1 for additive compatibility;
writers need not preserve those fields. Changes to required field meanings or
representations require a new version. Unsupported versions are identified before
interpreting version-specific fields and require a compatible Over release; they
are never silently treated as ordinary unencrypted repositories. Malformed JSON,
invalid names/field shapes, excessive sizes, or a nonregular registry entry are
invalid metadata, not an absent marker.

### Detection contract

`over init` inspects the cloned committed tree before its first checkout. The
reusable detector also accepts an explicit tree. It distinguishes:

- **Configured encryption:** a supported registry, even with no protected files.
- **Active policy:** tracked paths whose committed attributes select `filter=over`.
  Nested overrides, macros, quoted patterns, and unset/reset rules follow Git
  semantics. An unmatched future-facing rule alone is not active protection.
- **Encrypted content:** raw Git blobs with the Over ciphertext magic, including
  the native secret store, independently of whether a policy or registry exists.
- **Unlock eligibility:** at least one configured method has a supported public
  descriptor and structurally parseable Age envelope with a key-sized payload.
  Passphrase envelopes count; one damaged alternative does not disable a healthy
  method. This does not authenticate the envelope, prove that a recipient matches
  it, or guarantee that credentials or a plugin are available locally.

Detection reads no working-tree files or private keys and runs no clean/smudge,
text conversion, identity lookup, or credential/plugin commands. Attribute
evaluation uses a disposable bare Git repository sharing read access to the
source objects, with local/global/system attribute overrides excluded. Scratch
administrative files are removed afterward; the inspected repository's config,
index, refs, objects, and private crypt state remain unchanged. The pinned tree
populates only the scratch index; `check-attr --cached` evaluates it without
requiring newer Git's `--source` option.

Empty repositories and repositories without crypt evidence need no crypt setup.
Policy/ciphertext without access metadata, a registry without any usable access
method, unsupported/malformed metadata, or inconsistent policy/ciphertext receive
deterministic diagnostics. Bootstrap attributes and `.over/crypt/` must stay
unencrypted. Fresh init refuses invalid bootstrap state before materialization
and preserves the clone for repair. It checks current committed objects, not
historical commits or authenticated plaintext; use storage diagnostics for a
broader audit.

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
