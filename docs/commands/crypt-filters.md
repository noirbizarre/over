# Git encryption filters and diff helpers

Over installs these Git plumbing helpers in the selected repository's local Git
configuration. Usually invoke [`over unlock`](unlock.md) rather than run helpers
manually. See [Repository encryption](../usage/crypt.md) for attributes and setup.

```text
over crypt filter clean [PATH]
over crypt filter smudge [PATH]
over crypt filter-process
over crypt textconv <FILE>
over crypt diff -- <GIT-EXTERNAL-DIFF-ARGUMENTS>
```

All accept parser-level global diagnostic/home options, but repository discovery
uses Git's invocation environment (`GIT_DIR`/`GIT_WORK_TREE`) then cwd, not Over
home or overlay selection. There are no helper-specific options. Helpers never
prompt or acquire credentials. Stdout is exclusively payload/protocol bytes;
errors go to stderr with nonzero status.

The `.gitattributes` driver names are `filter=over`, `diff=over`, and
`diff=over-secret` for native entries: Git maps
those names to the configured helper commands. `over git` is the mounted-repository
namespace. Its former helper entry points remain hidden compatibility aliases;
`over unlock` refreshes existing local driver settings to use `over crypt`.

## Clean, smudge, and process

The one-shot `filter clean`/`filter smudge` commands read stdin to EOF and write
the transformed payload. Clean encrypts plaintext with the installed local key;
locked plaintext clean fails. Supported ciphertext may pass through while locked.
Unlocked smudge authenticates/decrypts file ciphertext, while locked smudge retains
ciphertext. Native-store ciphertext stays encrypted even during unlocked smudge.
Historical plaintext is preserved with an exposure warning, not destroyed.
Installed one-shot commands pass Git's `%f` logical pathname; process requests
use their `pathname=` record. New native ciphertext requires this context while
unlocked. Older context-free helper calls remain usable for older domains.

`filter-process` implements Git's v2 long-lived clean/smudge packet protocol;
it is not an ordinary stream-to-stream converter. Git prefers it when configured.
Every request rereads current key state. Malformed/unsupported payloads fail.
The process does not cache repository keys between requests. Lock waits for active
transformations; subsequent requests use locked behavior. Already transformed
output may still be buffered or held by Git or other processes after lock.

## Textconv

Git invokes `textconv` with a single filename, often a temporary blob outside the
repository. It writes exact authenticated plaintext for the current key generation,
including legacy aggregate metadata. New filename-bound native entries require
the path-aware external driver below; a temporary blob filename is insufficient
context for textconv to authenticate them.
Ordinary plaintext, including unlocked working files, passes through unchanged.

```sh
git diff
git diff --cached
git show HEAD
git diff --no-textconv
```

When the local key is absent, structurally valid ciphertext passes through
byte-for-byte without decryption or authentication, allowing locked Git diffs to
complete. With an installed key, conversion requires successful authentication.
Malformed/unsupported payloads, invalid installed keys, unavailable generations,
and interrupted transactions still fail; helpers never print partial decrypted
bytes. Other files may already have been printed before a later file fails in a
multi-file diff.

Generated attributes select `diff=over`; setup installs
`diff.over.textconv` and `diff.over.cachetextconv=false`. No persistent Git
textconv cache is created by this configuration. Already retained patches/caches
are not erased by lock. Old-generation history and staged rotations require
`--no-textconv` when that historical key is unavailable.

Git textconv output is for human review, not for applying patches. Git diff/log/show
normally enable it; format-patch does not. External difftools retain their own
behavior rather than automatically receiving decrypted files.

## Native secret diffs

Native-entry mutations select `diff=over-secret` and configure its external
command as `over crypt diff --`. Git supplies the logical path and both input
files, object IDs, and modes; rename/copy requests also supply the new logical
path and transformation metadata. Unmerged requests produce an opaque marker.
The helper authenticates both existing sides before emitting any value patch.
Legacy JSON-backed entries display decoded values, matching raw binary entries.

```sh
git diff --ext-diff --no-relative
git diff --cached --ext-diff --no-relative
git show --ext-diff --no-relative HEAD
git log -p --ext-diff --no-relative -- .over/secrets/
git diff --no-ext-diff --no-textconv
```

Use `--no-relative` so the logical path retains its native-store prefix. The
helper never guesses a name from a temporary filename or retries other names.
New-format ciphertext must be re-encrypted under a new name to rename it.

Text values produce uncolored unified review patches, including no-final-newline
markers. Binary values and locked ciphertext produce binary summaries. Git owns
the pager, but its external protocol does not forward every rendering option:
word diff, custom context, color, and other parent diff options are not inherited.
Single-blob `git show --textconv REV:PATH` cannot provide native name context;
use a commit patch as above. External difftools retain their existing behavior.

Rendering uses memory only: no plaintext temporary files, Git objects, or
persistent conversion cache. Owned plaintext/output buffers are zeroizing;
libgit2's transient diff allocations remain library-owned. Pair-level failure
emits no plaintext from either side, although Git may have printed earlier files.
Missing local keys permit opaque structural review; invalid installed keys,
unavailable generations, malformed formats, and authentication failures fail.
Use both `--no-ext-diff --no-textconv` to bypass readable conversion.

Setup atomically installs the driver with `cachetextconv=false` and
`trustExitCode=false`. Successful handling exits 0 even when a patch is printed;
Git determines its outer change status. Unlock repairs local commands without
rewriting tracked policy. Explicit secret mutations upgrade the native-entry
attribute. The helper never writes repository data or acquires credentials.
