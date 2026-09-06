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

## Materialization Rules

By default, every file in an overlay is symlinked individually and every
directory is recursed into. Two fields, available both in the repository
root config and in any overlay's own descriptor (they inherit through the
same cascading chain as every other field — see
[ADR-002](adr/002-overlay-is-a-directory-not-a-repository.md)), let you
override this per overlay or per subtree:

```toml
[defaults]
materialization = "symlink"          # symlink | symlink-directory

[[rules]]
path = "some/directory"               # glob or literal, relative to the overlay root
materialization = "symlink-directory"
```

- `defaults.materialization` sets the materialization applied to every
  directory with no more specific `rules` match. `symlink` (the default)
  recurses into the directory and symlinks each file individually.
  `symlink-directory` symlinks the directory as a single unit — its
  contents are not separately enumerated or tracked.
- `rules` is a list of path/subtree overrides. `path` is a glob pattern (or
  a literal path) relative to the overlay root. When more than one rule
  matches the same path, the most specific one wins: a literal path always
  outranks a glob, and among glob patterns the longer one wins.
- A repository root `over.toml` setting `defaults`/`rules` becomes the
  baseline for every overlay; any overlay along the cascading chain can
  override either field entirely for its own tree (the nearest
  descriptor that defines the field wins, same as `exclude`/`link_dirs`/
  `git`/`uses`).
- A file added to a source subtree still governed by a `symlink`
  (file-level) rule is automatically picked up the next time `over`
  inspects the overlay — there is no separate step to "add" it to the
  rule.

`link_dirs` (a list of glob patterns symlinked as a whole directory instead
of walked file-by-file) still works and is equivalent to a `rules` entry
with `materialization = "symlink-directory"` — both resolve through the
same mechanism. See [ADR-017](adr/017-materialization-rules-generalize-link-dirs.md).

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
