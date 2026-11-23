# Select an overlay from context

Commands accepting an existing overlay can usually omit its name when the
current context identifies exactly one candidate. An explicit `NAME` or
`--overlay NAME` always takes precedence.

Most commands check evidence in this order, stopping at the first matching tier
(`commit` uses the bounded inference described below):

1. **Git association:** the containing repository's explicit `over.overlay`
   setting. Repository-local settings apply; an enabled worktree configuration
   overrides shared settings. Global Git settings, repository names, branches,
   and remote URLs do not establish ownership.
2. **Source context:** the path lies within exactly one discovered overlay source
   directory. This wins even when another overlay targets the same directory,
   allowing commands to load the source overlay without resolving unrelated targets.
3. **Destination ownership:** the path belongs to one of its managed destinations.
   Checkout ownership respects exclusions;
   directory scaffolding and dependency consumers are not additional owners.
4. **Immediate siblings:** a file's parent contains managed entries from an
   overlay. For a directory or the current working directory, Over examines
   immediate managed children. Configured entries count even before application
   or after deletion. Over does not search recursively or climb parent directories.
5. **Target containment:** the path lies inside an overlay's resolved target or
   [rule target](../configuration/overlay.md#overlay-rules). Paths routed away
   by rules are excluded. A unique match preserves the target rule and source
   directory correspondence, including the remaining relative suffix.

The first matching tier must identify one overlay. Multiple candidates are an
error (except for `log`'s repository-history fallback), even when one target is
deeper or a default is configured. The error lists
the candidates and asks for an explicit overlay. Conflicting Git association
values and stale associations also require correction or an explicit override.

Multiple valid source explanations within one overlay are also ambiguous.
Neither an explicit overlay nor a configured host chooses between its rules.

`add`, `git add`, and `restore` infer from their operands, including missing literal paths
that represent tracked deletions. Inferred operands must agree on one overlay;
an ambiguous operand is not resolved by another operand. Selection does not
bypass the operation's path, materialization, or ownership validation. Commands
without file operands use the invocation directory.

[`commit`](../commands/commit.md) selects from `--overlay` or current-directory
context before resolving its path filters. File operands do not select its overlay.
Its bounded inference checks the containing Git repository's association first,
then recorded checkout ownership (source context takes precedence over target
context), then ancestor source descriptors within the configured Over home.
It does not discover every overlay or use the sibling and target-containment tiers
above. Missing or ambiguous context requires `--overlay NAME`; use it when
committing from a shared destination directory. The configured host is never a
fallback for commit.

## Fallbacks when no context matches

| Commands | Fallback |
| --- | --- |
| `apply` | Configured `host_overlay` only with root `apply.default_to_host` enabled (false by default), otherwise an actionable missing-overlay error |
| `sync` | Configured host only with root `sync.default_to_host` enabled (false by default), otherwise require an explicit overlay or `--all` |
| `data` | Configured host, otherwise require an explicit overlay |
| `add`, `git add`, `git mount`, `unapply`, `commit`, `restore` | Require an explicit overlay |
| `status` / `show`, `diff` | Configured host, otherwise all overlays |
| `log` | Whole history of the repository containing the configured Over home; also used for ambiguous context or an inferred non-checkout overlay |

Active context precedes the host. Except for `log`, ambiguity never invokes a
fallback or an overlay selection prompt. `log` never selects the host implicitly;
name the host overlay explicitly to scope its history. `--all` on status/diff/sync explicitly bypasses inference and
host selection. The configured host's own `host` overrides still apply
when another overlay is selected; its data and unrelated dependencies do not.
Invalid configured hosts remain errors during data resolution.

`lint` and `doctor` retain their repository-wide diagnostic policies.
`git status` requires an explicit repository association; it does not prompt.

For example, if only `workspaces/client` manages files in `/etc/client`:

```sh
cd /etc/client
over status
over add new-config.toml
```

Both commands can select `workspaces/client` without spelling its name.
