# Destructive operations and recovery

Over checks ownership before changing deployed content. Use `apply --dry-run`,
`unapply --dry-run`, and `sync --dry-run` to inspect affected paths. A preview
never prompts, executes hooks, updates checkpoints, or writes Git
objects, indexes, or refs. It is a snapshot, not a reservation of those paths.

## Authorization contract

- **Ordinary apply conflicts**, including `.link.*` sidecars, require a choice:
  skip, overwrite, absorb, or inspect a diff where applicable. Scaffold
  replacement uses a yes/no confirmation. Declining a scaffold replacement
  stops apply before children can traverse it.
- **`--no-prompt` alone fails on unresolved apply conflicts.** It never means
  overwrite. **`--force` explicitly authorizes ordinary conflict replacement**,
  including recursive removal of conflicting directories. Force takes precedence
  when combined with no-prompt.
- **Real checkout disposal has separate authorization.** Interactive apply
  migrations and unapply disclose the path and local-data/history risks, with
  confirmation defaulting to No. `--discard-checkout` explicitly authorizes
  that loss without a prompt, including with `--no-prompt`. `--force` alone
  cannot authorize checkout disposal.
- **Encrypted files** offer only skip or explicitly authorized overwrite, never
  Absorb or plaintext diff. Authentication completes before target writes, and
  unapply retains changed or unverifiable plaintext. See [Age encryption](secrets.md).
- A clean Git working tree does **not** prove a repository is backed up.
  Ignored files, other branches, tags, stashes, reflogs, hooks and local
  configuration may be its only copy. Consequently, real checkout disposal
  currently always requires confirmation or `--discard-checkout`, even when
  status says applied. Over does not contact remotes to prove recovery.
- Ownership mismatches, independent mapped/checkout units, source overlap, and
  shared/external Git layouts cannot be overridden. Ambiguous legacy layouts
  require restoring their old configuration or moving them aside manually.
  The discard flag does not apply to dirty virtual checkouts.

Changing a mode authorizes automatic replacement only for reconstructible owned
state: matching source links, exact source-link mirrors, or clean virtual
checkouts without independent destinations. Foreign legacy links are preserved.
To change a checkout to recursive per-file links, unapply its old layout first.
Blocked migrations fail apply and suppress install and post-apply hooks.

## Operation inventory

| Operation | Affected data and authorization | Safeguards and preview |
| --- | --- | --- |
| Apply ordinary/sidecar links | Conflicting target file, link, or whole directory; interactive overwrite/absorb or `--force` | No-prompt refuses; checkout-specific checks precede generic recursive replacement; preview names target and replacement policy. |
| Apply encrypted files | Final plaintext destination; explicit overwrite or `--force` for differing files | Authenticate in memory before writes; default Unix mode `0600`; no plaintext staging files. Existing destinations update in place. Hard links and directory obstructions refuse. |
| Apply scaffold replacement | Conflicting node at a required directory; confirmation or `--force` | Unlinks a conflicting directory link rather than changing its referent; refusal stops descendants. |
| Absorb | Copies target content over overlay/sidecar source, then replaces target with a link; explicit Absorb choice | Rejects overlapping trees, prepares copies before replacing the source, and copies links without traversing their referents. Diff/Skip do not modify either input. |
| Managed partial files/permissions | Managed block or declared mode; applying the configured block/mode is opt-in; structural/block conflicts follow their conflict policy | Unapply removes only an unchanged block, preserving surrounding bytes; changed types are rejected. Preview names the physical file and marker or mode. |
| Link/layout migration | Old target node/tree, replaced with the configured mode | Validates source-link ownership and rechecks transition predicates before removal. Unknown links/content are preserved. Preview identifies old/new shapes and target removal. |
| Real checkout migration/unapply | Entire repository directory, including ignored files and Git metadata/history | Separate consent/discard flag; exact repository and ownership checks; nested independent units and shared/external Git layouts refuse. |
| Virtual checkout initialization/refresh | Owned projected files and executable bits; explicit mode configuration or sync | Checks association/scope, selected paths and source/index/target compatibility; preserves unrelated mapped files, staging and existing private access permissions. Sync preview lists changed physical paths. |
| Virtual checkout unapply | Only unchanged owned files and empty directories | Dirty content/executable-mode edits are retained; independent mappings and ignored files survive; association removed after successful teardown. |
| Ordinary unapply | Matching links (including verified hard links), unchanged blocks, empty scaffolds; explicit `unapply` | Rechecks before removal, never recursively clears ordinary directories; reports retained directories and changed entries. No dependency recursion. |
| Declared Git provisioning | Clone destination, configured revisions, remotes, worktrees and repository config; explicit apply/configuration | Legacy deletion requires expected source-link ownership; existing revisions use safe Git checkout. Preview identifies the destination. Git configuration is not an index reset. |
| Declared Git sync pull/merge | Root checkout worktree/index and local branch; explicit sync/pull | Dirty checkouts and ignored/untracked obstructions refuse; safe checkout precedes fast-forward ref advancement; conflicts leave normal merge state and stop push. Preview uses cached upstream state, identifies candidate paths and distinguishes merge/fast-forward. |
| Sync push | Remote branch; explicit sync/push | Ordinary non-force push; no push after unresolved pull conflicts. Dry-run performs no network I/O. |
| Sync abort | Uncommitted tracked/index changes in an in-progress merge; explicit `--abort` | Rejects other Git operation states; dry-run lists prospective discarded paths and never resets. HEAD commits remain, but post-conflict edits may be lost. |
| Virtual checkout commit | Selected source working-tree paths and HEAD; explicit commit, or confirmed sync commit | Preflight, prepared source updates and limited backup/restore; unrelated index entries and target edits survive. Inspect selection with `diff --staged`; commit has no dry-run flag. |
| Reverse add/staging | Selected deployment/source paths or selected source-index entries; explicit `add`/`git add` | Source selection, checkout association and mapping checks apply. Virtual staging preserves source working files and unrelated staging. Ordinary reverse-add moves content into the source: inspect paths before running. |
| Host Git excludes | Only the overlay's marker block in `.git/info/exclude` | Does not alter the host index or tracked files; malformed blocks remain intact. Unapply removes its own block unless encrypted destinations are retained. Dry-run leaves excludes untouched. |
| Lifecycle hooks/external diff tools | Arbitrary effects of trusted repository commands | Lifecycle hooks run only at their documented events, stop on failure and are suppressed under dry-run; their effects are outside Over's filesystem ownership model. External inspection tools can have their own side effects. |
| Install hooks | Arbitrary repository-provided commands, including package-manager/system changes; explicit `apply --install` | Run only after all reconciliation succeeds, before post-apply. Dry-run skips execution. Child commands have their own prompt/privilege policies and must be safe to repeat. |

## Recovery limitations

There is **no general backup, whole-command rollback, or crash-recovery journal**.
Keep your own backups and commit/push important content before destructive
choices. Overwrite and absorb do not preserve a permanent previous version.
If creating a link fails after removing its target, that removal is not undone.
Completed earlier overlay steps remain when a later step fails.

Unapply does not restore content previously overwritten/absorbed by apply, original
metadata, repository-local history/configuration, packages, or hook effects.
Reapply reconstructs the current configured projection, not historical user data.

Source-commit updates have a **limited** recovery mechanism: preparation happens
before replacement, and apply/commit failures attempt to restore source backups.
Rollback itself can fail; diagnostics identify retained backups. After a commit
succeeds, checkpoint persistence can still fail: the error identifies the commit
and affected checkpoint. Inspect source HEAD and checkout status before retrying;
an error does not imply the commit was undone.

Plans recheck ownership/transition conditions and detect changed physical parents.
Established ancestor aliases are supported. These checks do not make concurrent
filesystem edits atomic; avoid changing deployment paths while a command runs.
Managed-block writes and virtual refreshes on existing single-link files retain their inode, preserving current
ownership, ACLs, extended attributes and native metadata. These writes are
in-place: I/O failure after writing begins may leave partial content.
On Linux, shared files are detached through a prepared replacement that preserves
owner/group, mode, ACLs and extended attributes visible to the process; failed
metadata copying or verification refuses replacement. Invisible privileged
attributes are outside that supported metadata scope. Platforms without supported
metadata cloning refuse shared-file updates rather than changing unrelated hard
links or silently losing metadata. This is per-file handling, not multi-file
rollback.

Virtual-checkout commit/restore transactions also detach shared files on macOS
when native filesystem cloning is supported (for example, APFS). They retain
xattrs/resource forks and install the exact original ACL rather than merging
inherited access; unexpected ownership or mode changes and cloning failures refuse the
replacement. Existing single-link files retain the in-place behavior.

## Regression coverage

`tests/safeguards.rs` exercises actual CLI flag combinations, sidecar/scaffold
conflicts, ignored/staged/detached/branch/stash disposal, identity refusal,
checkout mode changes, private permissions, object/index/state-preserving sync
previews, and merge-abort boundaries. Unit tests beside actions/materializers,
plan, unapply, sync and commit cover interactive decisions and stale execution.
`tests/mappings.rs`, `tests/ignore.rs`, `tests/mapping_validation.rs`,
`tests/checkout_commit.rs` and `tests/hooks.rs` cover independent ownership,
staging, failure preservation and external-command lifecycle behavior.
