# Select an overlay from context

Commands accepting an existing overlay can usually omit its name when the
current context identifies exactly one candidate. An explicit `NAME` or
`--overlay NAME` always takes precedence.

Over checks evidence in this order, stopping at the first matching tier:

1. **Git association:** the containing repository's explicit `over.overlay`
   setting. Repository-local settings apply; an enabled worktree configuration
   overrides shared settings. Global Git settings, repository names, branches,
   and remote URLs do not establish ownership.
2. **Ownership:** the path lies within an overlay's source directory, or belongs
   to one of its managed destinations. Checkout ownership respects exclusions;
   directory scaffolding and dependency consumers are not additional owners.
3. **Immediate siblings:** a file's parent contains managed entries from an
   overlay. For a directory or the current working directory, Over examines
   immediate managed children. Configured entries count even before application
   or after deletion. Over does not search recursively or climb parent directories.
4. **Target containment:** the path lies inside an overlay's resolved target or
   mapped destination. Paths routed away by mappings are excluded.

The first matching tier must identify one overlay. Multiple candidates are an
error, even when one target is deeper or a default is configured. The error lists
the candidates and asks for an explicit overlay. Conflicting Git association
values and stale associations also require correction or an explicit override.

File-taking commands infer from their operands, including missing literal paths
that represent tracked deletions. Inferred operands must agree on one overlay;
an ambiguous operand is not resolved by another operand. Selection does not
bypass the operation's path, materialization, or ownership validation. Commands
without file operands use the invocation directory.

## Fallbacks when no context matches

| Commands | Fallback |
| --- | --- |
| `apply`, `unapply`, `commit`, `restore`, `data` | Configured `default_overlay`, otherwise an actionable missing-overlay error |
| `add`, `git add`, `git mount` | Require an explicit overlay |
| `status` / `show`, `diff`, `sync` | Configured default, otherwise all overlays |
| `log` | History of the source repository |

Context precedes the default. Ambiguity never invokes a fallback or an overlay
selection prompt. `--all` on status/diff/sync explicitly bypasses inference and
the default's selection. Defaults still contribute their usual metadata even
when another overlay is selected; invalid default metadata remains an error.

`lint` and `doctor` retain their repository-wide diagnostic policies.
`git status` requires an explicit repository association; it does not prompt.

For example, if only `workspaces/client` manages files in `/etc/client`:

```sh
cd /etc/client
over status
over add new-config.toml
```

Both commands can select `workspaces/client` without spelling its name.
