# Git encryption filters and textconv

Over installs these Git plumbing helpers in the selected repository's local Git
configuration. Usually invoke [`over unlock`](crypt.md#set-up-and-unlock) rather than run helpers
manually. See [Repository encryption](crypt.md) for attributes and setup.

```text
over git filter clean
over git filter smudge
over git filter-process
over git textconv <FILE>
```

All accept parser-level global diagnostic/home options, but repository discovery
uses Git's invocation environment (`GIT_DIR`/`GIT_WORK_TREE`) then cwd, not Over
home or overlay selection. There are no helper-specific options. Helpers never
prompt or acquire credentials. Stdout is exclusively payload/protocol bytes;
errors go to stderr with nonzero status.

## Clean, smudge, and process

The one-shot `filter clean`/`filter smudge` commands read stdin to EOF and write
the transformed payload. Clean encrypts plaintext with the installed local key;
locked plaintext clean fails. Supported ciphertext may pass through while locked.
Unlocked smudge authenticates/decrypts file ciphertext, while locked smudge retains
ciphertext. Native-store ciphertext stays encrypted even during unlocked smudge.
Historical plaintext is preserved with an exposure warning, not destroyed.

`filter-process` implements Git's v2 long-lived clean/smudge packet protocol;
it is not an ordinary stream-to-stream converter. Git prefers it when configured.
Every request rereads current key state. Malformed/unsupported payloads fail.

## Textconv

Git invokes `textconv` with a single filename, often a temporary blob outside the
repository. It writes exact authenticated plaintext for the current key generation,
including the native store's serialized map (initially JSON with byte arrays).
Ordinary plaintext, including unlocked working files, passes through unchanged.

```sh
git diff
git diff --cached
git show HEAD
git diff --no-textconv
```

Text conversion requires a valid current local key for ciphertext. Locked,
corrupt, truncated, unsupported, stale-generation, or interrupted-transaction
inputs fail; helpers do not print partial decrypted bytes. Other files may already
have been printed before a later file fails in a multi-file diff.

Generated attributes select `diff=over`; setup installs
`diff.over.textconv` and `diff.over.cachetextconv=false`. No persistent Git
textconv cache is created by this configuration. Already retained patches/caches
are not erased by lock. Old-generation history and staged rotations require
`--no-textconv` when that historical key is unavailable.

Git textconv output is for human review, not for applying patches. Git diff/log/show
normally enable it; format-patch does not. External difftools retain their own
behavior rather than automatically receiving decrypted files.
