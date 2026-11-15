# ADR-032: Host context is distinct from deployment selection

## Status

Accepted. Supersedes ADR-019's configuration naming and command selection policy.

## Context

The former default overlay both supplied machine values and selected deployment
targets. Configuring host identity could therefore authorize mutations unrelated
to the invocation's active context. Data inspection also selected the host before
active context, unlike other reporting commands.

## Decision

Use root `host_overlay` for optional machine context. Active overlay context is
inferred from invocation paths; applied overlays describe deployed state. Neither
host identity nor active context asserts that an overlay has been applied.

Inspection selects explicit names, then active context, then the host. Without
either context, status/diff report all overlays and data requires a name. Lint and
doctor retain repository-wide/global defaults.

Apply and sync select explicit names, then active context. Each may fall back to
the host only with its own root `apply.default_to_host` or `sync.default_to_host`
enabled, both disabled by default. Command-specific settings live in root
`apply` and `sync` tables. Other mutating commands require explicit or active
selection; sync also accepts explicit `--all`.
Ambiguous or invalid context errors never activate host fallback.

Host data remains independent of deployment selection: recursively merge the
selected overlay's own host defaults with the configured host's own values taking
precedence. Lists are replaced, not combined, and no implicit data inheritance
or deployment of host dependencies is introduced.

## Consequences

The removed `default_overlay` key is ignored with a migration hint. Users rename
it to `host_overlay` and separately opt into host apply/sync defaults if desired.
Scripts relying on implicit mutation targets must opt in, name an overlay, run in
active context, or use explicit sync `--all`. Sync validates intent before source pulls and loads
fresh descriptors afterward, rejecting changed inferred identity.
