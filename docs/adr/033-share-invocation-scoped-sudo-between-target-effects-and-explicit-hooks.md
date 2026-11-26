# ADR-033: Share invocation-scoped sudo between target effects and explicit hooks

## Status

Accepted.

## Context

System overlays need protected-target reads and writes without changing the
identity used for Git, templates, secrets, hooks by default, or XDG state.
Repeated subprocess-level sudo calls could reauthenticate midway through a run.
Mode-only permissions cannot describe system-file ownership.

## Decision

Resolve overlay/rule `sudo` independently from materialization and permissions.
Keep the octal permissions shorthand and add independently inherited `mode`,
`owner`, and `group` components. Explicit false disables inherited sudo; ownership
never implicitly enables it. Preserve existing mode-policy coverage.

Use one lazily launched, invocation-scoped helper through standard sudo. A
versioned private Unix-socket protocol carries typed target effects and prepared
content. Authentication keeps normal sudo terminal behavior. Noninteractive
invocations use `sudo -n`; cached authentication follows local sudo policy.
Read-only capabilities reject effects and hook execution. Failure is not retried.
Read-only commands and dry runs inspect targets as the invoking account and never
start or use sudo, even with cached credentials. Inaccessible targets produce
inspection diagnostics instead of triggering elevation. Mutating operations
retain configured elevation for both preparatory target reads and writes.
The helper terminates and is reaped when the invocation finishes. Launch it with
sudo's background mode and no inherited standard input: a persistent foreground
sudo PTY would otherwise consume keys meant for conflict prompts, crypto unlocks,
or ordinary hooks in the caller. Authentication still uses sudo's normal terminal
mechanism and credential cache. Wait for control-channel EOF during shutdown so
filesystem cleanup finishes before returning.

The parent performs selection, rendering, decryption, conflict consent, Git,
source transactions, and user-state updates. The helper performs filesystem
effects using pinned parent descriptors and guarded writes. Its single-threaded
scoped directory changes allow existing metadata-preserving transactions to work
with pinned parents without depending on Linux `/proc` paths. Hardlink isolation,
readonly-content refusals, and native metadata protections remain applicable.

Structured command hooks independently opt in with `sudo: true`. Preparation and
conditions stay in the parent. The helper spawns the prepared shell command with
sudo-established environment defaults, explicit hook overrides, lifecycle
context, and passed standard-stream descriptors. Transfer and restore terminal
foreground ownership when the hook shares the caller's terminal session. Each command has its own cwd
and environment; neither leaks into subsequent requests. Signals are forwarded
to the command's process group and actual exit status is returned.

## Consequences

- Symlink owner/group changes affect the destination link, not source referents.
- Declared hard links needing different ownership use their soft-link fallback.
  A detached unmarked regular copy would lose the identity needed for safe unapply;
  matching bytes alone must not become deletion authority.
- Shared partial contributors reconcile their blocks before final file metadata.
- Ordinary checkout modes retain Git executability semantics; ownership is
  independently configurable.
- Child policy incompatible with an atomic directory link is rejected.
- Git provisioning and arbitrary source-side processing are never elevated by
  overlay sudo. A hook explicitly authorized for sudo is an elevated shell
  program, not a filesystem-only operation.
- The helper is an invocation service for an authorized sudo user, not a
  restricted sudoers sandbox. Unix descriptor/account APIs are an intentional,
  narrowly scoped dependency.
