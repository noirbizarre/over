# Locking and unlocking

Repository encryption and the native secret store share one persistent local key.
After [setting up access](crypt.md#set-up-and-unlock), use:

```sh
over unlock --identity ~/.config/age/over-alice.txt
# Work with protected files and native secrets.
over crypt list
over lock
```

Unlock persists the key in Git's private common administrative directory and
refreshes protected files across registered linked worktrees. Closing a terminal,
rebooting, or unplugging a hardware device does not lock the repository. Repeating
an unlock with an already valid key repairs driver configuration and unfinished
materialization without discarding plaintext working edits.

Before locking, review and stage protected edits you want to keep. Lock restores
protected working files from the encrypted index, preserving staged content.
Dirty, conflicting, inaccessible, or incompatible worktrees stop the transition;
resolve the reported problem and retry. A failed lock does not imply key removal.
Lock requires an installed key; it is not an unconditional already-locked no-op.

While locked, native secret operations and encrypted textconv diffs fail; clean
rejects plaintext and smudge retains ciphertext. Lock does not erase deployed
destinations, copies, program memory, or backups. See the detailed
[encryption lifecycle](crypt.md#transparent-git-and-explicit-lock).

For noninteractive startup, supply an available unencrypted software identity;
encrypted identities and hardware plugins may still need interaction during
unlock. Git helpers never acquire credentials. To integrate with your session,
use the [user-managed automation recipes](crypt-automation.md).

Exact arguments and selection behavior are in [`over unlock`](../commands/unlock.md)
and [`over lock`](../commands/lock.md).
