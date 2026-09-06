# Configuration

## Root Configuration

`over` supports a root configuration file at `~/.over/over.toml` (or
`over.yaml`/`over.yml`). This file controls global preferences that apply
across all overlays.

### Format Preference

The `format` field sets the default descriptor format used by `over new`
when creating new overlays. Accepted values are `toml` (default) and `yaml`.

TOML (`~/.over/over.toml`):

```toml
format = "yaml"
```

YAML (`~/.over/over.yaml`):

```yaml
format: yaml
```

Format resolution priority (highest to lowest):

1. `--format` / `-f` CLI flag
2. Root config `format` field
3. Default (`toml`)

This preference only governs what `over new` **writes**. Reading an overlay
tolerates any of `.toml`/`.yaml`/`.yml` regardless of this setting — see
[ADR-003](adr/003-overlay-descriptors-tolerate-any-format-on-read.md).

## Partial Files

A `<name>.partial.{toml,yaml,yml}` sidecar file, placed anywhere in an
overlay, injects a managed block into an existing (possibly foreign) file
instead of symlinking it whole — useful for a single alias, export, or
config stanza that must coexist with content `over` doesn't otherwise
manage.

```toml
# aliases.partial.toml
target = "~/.zshrc"
content = "alias ll='ls -la'\n"
# marker = "aliases"   # optional; defaults to the sidecar's own stem
```

- `target`: the file to modify. Templated the same way an overlay's own
  `target` field is (`{{ env.* }}`, `{{ machine.* }}`, `{{ overlays[...] }}`).
- `content`: the literal block body, used verbatim (not templated).
- `marker`: identifies the block, so more than one sidecar can manage
  distinct blocks in the same target file. Defaults to the sidecar's stem.

The block is delimited by fixed `#`-comment marker lines:

```text
# >>> over: aliases >>>
alias ll='ls -la'
# <<< over: aliases <<<
```

A hand-edited block (content between the markers no longer matching what
the overlay declares) is always reported as a conflict, never silently
overwritten, exactly like any other `apply` conflict — see
[ADR-016](adr/016-partial-file-managed-blocks.md). `over unapply` removes
only the block, leaving the rest of the target file untouched.
