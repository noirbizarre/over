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
