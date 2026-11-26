# ADR-027: Installation is an opt-in hook event

**Status:** accepted; supersedes [ADR-004](004-install-config-is-declarative-with-a-shell-escape-hatch.md)
and amends the package-installation portion of [ADR-008](008-uses-composition-is-a-flat-union.md).

## Context

The dedicated installer models package managers, platform precedence, package
identities, and package-set composition. Those abstractions duplicate concerns
already owned by package managers and make Over maintain a second installation
model. Structured lifecycle hooks already supply conditional commands, cwd,
environment, shared machine/path context, and consistent failure handling.

## Decision

Replace the installer with `hooks.install`, dispatched by the existing hook
engine only for `over apply --install` (or `-i`). Overlay-local install hooks run
after every participating overlay reconciles successfully and before any
post-apply hook. This lets overlay installation consume deployed configuration
and post-apply consume installed tools. Ordinary apply does not prepare or
execute install commands.

Issue #360 amends the original root-after-deployment ordering: repository-root
install hooks run once before deployment, after resolving the complete graph,
data, and targets. Root-only `post-install` runs once after all overlays and
post-apply hooks succeed. Both phases use the shared `allowOverlays` sequence
of operational overlay objects in dependency-first application order, so users
can aggregate their own metadata without repeating work per overlay. Existing
overlay-local install and root/overlay post-apply ordering stays unchanged.

Retire top-level `install` configuration with explicit migration diagnostics.
Use generic hook conditions and shared context for platform selection; introduce
no package-manager integration, package deduplication, or persistent installation
state. Keep participant deduplication for diamond dependencies, but execute
identical commands in distinct participating overlays independently.

## Consequences

Users own command selection, manager flags, idempotency, and prompt policies.
Repeated/no-change applies with `--install` rerun hooks. Hooks do not inherit;
formerly inherited install configuration must move to root hooks or explicitly
composed overlays. Overlay scripts requiring pre-deployment installation must
move that setup to root install hooks.

The existing engine supplies sequential execution, conditions before rendering,
secret-safe diagnostics, exit-status propagation, and recursion prevention. An
install failure stops remaining events and leaves any deployed files and completed
command effects in place; retry starts hook execution again. Repository commands
remain arbitrary executable code, so `--install` requires trusting that code.
