# ADR-031: Encrypted Git diffs use authenticated textconv

## Status

Accepted. Extends [ADR-026](026-git-aware-diff-delegates-presentation-to-git.md)
and [ADR-028](028-repository-encryption.md).

## Context

Over's Git filters protect stored blobs, but clean/smudge alone do not give Git
readable diffs for encrypted content. Smudge cannot serve as the display boundary:
it intentionally retains locked ciphertext and keeps native-store working files
encrypted even while unlocked. Git's optional textconv cache could persist
decrypted content beyond the repository's explicit lock transition.

## Decision

Use Git's textconv interface, exposed as `over git textconv <FILE>`, and select it
with `diff=over` alongside `filter=over -text`. Continue delegating patch rendering
to Git. Install the absolute shell-quoted helper command and
`diff.over.cachetextconv=false` atomically with required filter configuration.
Reject effective conflicting worktree/include settings during setup.

The helper discovers the invoking repository, not the temporary input's directory.
It authenticates an entire ciphertext before emitting exact decrypted payload
bytes. Both supported domains can be displayed; ordinary plaintext passes through.
Owned buffers are zeroized under the same boundaries as encryption filters.
The helper never acquires credentials or changes content, index, objects, refs,
key material, or driver configuration. Existing administrative locking provides
coordination with lifecycle transitions.

Encrypted conversion requires the current valid local key. Lock state, corruption,
unsupported formats, unavailable generations, and interrupted transitions fail.
No old keys are retained to make historical diffs work. Missing presentation
attributes/configuration are diagnosed separately from broken encryption policy.
Unlock repairs local configuration without silently staging attribute upgrades.

## Consequences

Normal Git and Over diffs can display authenticated plaintext while unlocked,
including the native store's serialized payload without decrypting its working
file. Git supplies filenames and renders patches; no parallel diff engine or
friendly secret-map renderer is introduced.

Old-generation history and the old side of staged rotations may require
`--no-textconv`. Git's textconv patches are for review rather than application;
external difftools keep their existing semantics. Disabling caching prevents
normal persistent textconv cache writes but cannot erase prior patches/caches or
control user overrides, pagers, logs, and redirection. Multi-file diffs may have
printed earlier files before a later conversion fails.
