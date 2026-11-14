# ADR-028: Repository encryption shares one key and persistent local unlock state

## Status

Accepted for #245; evolves the original Age-file/native-secret implementation.

## Context

Git clean filters require stable output for unchanged content. Ordinary Age
payload encryption is randomized, and sharing an invocation's passphrase cannot
unlock subsequent Git subprocesses. Logical users must group independently
usable access methods without conflating them with Age recipients.

## Decision

Use RustCrypto `aes-siv`'s deterministic RFC 5297 AES-256-SIV implementation.
One random 64-byte repository key consists of independent AES-256 MAC/encryption
halves. Age remains the reviewed standard envelope/identity format, wrapping the
repository key separately per access method. Passphrase recipients remain
separate scrypt envelopes; never mix them into a multi-recipient envelope.

The binary payload begins with eight bytes `00 4f 56 45 52 43 52 59`, version
byte `01`, domain byte (`00` ordinary file, `01` native map/recovery), and a
16-byte random generation. The remaining bytes are the library's 16-byte SIV
authentication tag followed by encrypted content. The entire 26-byte header is
associated data. Paths are not associated data, so renames do not force
re-encryption. The secret store has a versioned binary-safe map representation
inside its payload. Repository identity is stable across rotations and checked
in both the public registry and local/enveloped key record.

SIV is deterministic authenticated encryption, not an ordinary AEAD with a
fixed nonce. The implementation controls all cryptographic processing; Over does
not implement a primitive/mode itself. Test the published RFC vector,
determinism, domain separation, corruption, and full authentication before
plaintext output. Reject unsupported/truncated payloads and impose a 256 MiB
whole-message bound. A future format changes its version explicitly and uses a
reviewed migration/rotation, never silent reinterpretation.

The presence of a valid repository- and generation-matched local key under Git's
private common administrative directory defines persistent unlocked state.
Standard Age identities and plugin stubs stay in their normal external files. No agent,
daemon, keyring, timeout, or XDG key cache is involved. Reboot/logout does not lock
Over. OS/filesystem security protects the key while the OS session is locked.
Filters never authenticate users or prompt; they read the key per request and
cooperate with shared/exclusive lifecycle guards. Explicit lock restores clean
protected Git-worktree files to ciphertext before deleting the key, preserving
unsafe edits by refusing the transition.

Canonical `.gitattributes` is the policy authority. Policy helpers use native Git
filtering and verify ciphertext before index publication. Virtual checkouts map
to source policy/index and compare in ciphertext space. Native secrets remain
direct ciphertext even in unlocked working trees, decrypting only in memory.
Recovery journals are authenticated ciphertext, record old/prepared states, and
refuse intervening edits. Age originals survive explicit legacy migration as
encrypted backups. Revocation rotates the key after trusted recovery/retained
passphrase preflight, never just deleting metadata.

## Consequences

- Equality and ciphertext length leak; names, history topology, commit messages,
  modes, and other Git metadata remain public. Identical plaintext under a
  different domain/generation does not share ciphertext. File substitution is
  not prevented by path binding; repository integrity remains a Git trust issue.
- Whole-file buffering limits large-file support and ciphertext does not provide
  useful Git deltas. Key/plaintext buffers are zeroized where owned; copies made
  by OS facilities or external processes cannot be revoked.
- A locked checkout retains supported ciphertext. Required clean rejects
  plaintext. Unconfigured third-party Git clients cannot be made fail-closed by
  versioned attributes alone; configure local filters before staging secrets.
- Removing a key envelope cannot erase learned keys. Rotation protects future
  state, never old commits/remotes/clones/backups or copied plaintext.

## Passage-compatible access methods (#246)

Standard Age plugin integration extends envelope wrapping/unwrapping without
changing v1 payloads or access metadata. Recipient-list imports create one named
key/envelope per recipient and publish atomically. Identity files are reused in
place; independently failing identity/plugin attempts cannot suppress available
recovery methods. Only a successful authenticated repository-key unwrap enters
the existing refresh/publication transaction. Filters never acquire credentials.

The Rust Age library handles ordinary software, SSH, scrypt and plugin operations.
The published library lacks native software-PQ identities; a narrow standard Age
CLI adapter handles only those envelopes. This is a library-capability exception,
not a second payload format or a proprietary hardware protocol. Device support
and lifecycle caveats are documented in the [encryption guide](../usage/crypt.md).
