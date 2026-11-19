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

Use Git's textconv interface, exposed as `over crypt textconv <FILE>`, and select it
with `diff=over` alongside `filter=over -text`. Continue delegating patch rendering
to Git. Install the absolute shell-quoted helper command and
`diff.over.cachetextconv=false` atomically with required filter configuration.
Reject effective conflicting worktree/include settings during setup.

Encryption plumbing belongs under `over crypt`, alongside policy management;
`over git` remains the mounted-repository namespace. Driver identifiers in
`.gitattributes` are independent of the configured CLI commands, so `filter=over`
and `diff=over` remain unchanged. Hidden Git-prefixed compatibility aliases keep
installed configurations usable until unlock refreshes them.

The helper discovers the invoking repository, not the temporary input's directory.
It authenticates an entire ciphertext before emitting exact decrypted payload
bytes. Both supported domains can be displayed; ordinary plaintext passes through.
Owned buffers are zeroized under the same boundaries as encryption filters.
The helper never acquires credentials or changes content, index, objects, refs,
key material, or driver configuration. Existing administrative locking provides
coordination with lifecycle transitions.

Plaintext conversion of ciphertext requires the current valid local key. When the
key is absent, structurally valid ciphertext passes through unchanged so locked
diffs complete without decryption or authentication. Invalid installed keys,
malformed/unsupported formats, unavailable generations with an installed key, and
interrupted transitions still fail.
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
Long-lived filters reload keys for every request. Lock coordinates active
transformations, but already transformed output may remain buffered outside that
boundary; it cannot be recalled or securely erased by removing the local key.

## Amendment: filename-bound binary native entries (#334)

Native entries now encrypt raw value bytes in ciphertext domain 2, authenticating
the exact name as a separate AES-SIV associated-data component alongside the
header and a fixed purpose label. Domains 0/1 and ciphertext version 1 retain
their original wire contract. Legacy JSON entries remain readable and migrate
only on rewrite; no read performs migration.

Textconv receives temporary blob filenames without reliable logical paths.
Native entries therefore select a dedicated `diff=over-secret` external driver,
exposed as `over crypt diff --`. Git supplies old/new logical paths; the helper
authenticates both sides before rendering a unified value patch in memory with
libgit2. Legacy entry display normalizes to value bytes. Ordinary protected
files retain textconv and Git's builtin renderer.

This is a scoped exception to delegating all patch rendering to Git. It preserves
readable secret diffs without weakening filename authentication or storing a
redundant name in ciphertext. Binary/locked inputs use opaque summaries. No
plaintext files, objects, or persistent caches are introduced; owned buffers are
zeroizing, while libgit2's internal transient allocations remain library-owned.
The external protocol does not forward all parent rendering flags; the helper
uses uncolored unified output, and callers use `--no-relative` for stable paths.
Raw bypass requires `--no-ext-diff --no-textconv`. See the
[helper contract](../commands/crypt-filters.md#native-secret-diffs).
