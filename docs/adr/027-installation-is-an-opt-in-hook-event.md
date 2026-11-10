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
engine only for `over apply --install` (or `-i`). Run it after every participating
overlay reconciles successfully and before any post-apply hook. Each event runs
repository-root hooks first, then overlay-local hooks in dependency-first order.
This lets installation consume deployed configuration and post-apply consume
installed tools. Ordinary apply does not prepare or execute install commands.

Retire top-level `install` configuration with explicit migration diagnostics.
Use generic hook conditions and shared context for platform selection; introduce
no package-manager integration, package deduplication, or persistent installation
state. Keep participant deduplication for diamond dependencies, but execute
identical commands in distinct participating overlays independently.

## Consequences

Users own command selection, manager flags, idempotency, and prompt policies.
Repeated/no-change applies with `--install` rerun hooks. Hooks do not inherit;
formerly inherited install configuration must move to root hooks or explicitly
composed overlays. Scripts relying on installation before deployment must adapt.

The existing engine supplies sequential execution, conditions before rendering,
secret-safe diagnostics, exit-status propagation, and recursion prevention. An
install failure stops remaining events and leaves deployed files and completed
command effects in place; retry starts hook execution again. Repository commands
remain arbitrary executable code, so `--install` requires trusting that code.
