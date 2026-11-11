# Configuration

Over reads `over.toml`, `over.yaml`, or `over.yml` descriptors. Keep one per
directory. A repository-root descriptor supplies preferences and shared policy;
intermediate and overlay descriptors refine policy down the directory tree.
Composition through `uses` combines independent overlays rather than extending
that inheritance chain.

| Reference | When to use it |
| --- | --- |
| [Root configuration](configuration/root.md) | Repository preferences, overlay discovery, default selection, and lifecycle hooks |
| [Overlay configuration](configuration/overlay.md) | Targets, composition, inheritance, mappings, metadata, and sidecars |
| [Materialization](configuration/materialization.md) | File/directory symlinks, editable checkouts, precedence, and lifecycle |
| [Install hooks](install-config.md) | Opted-in installation commands and migration guidance |

For complete layouts, start with [Examples](examples.md). For your first applied
overlay, follow [Getting started](index.md#getting-started).

## Editor support

Published JSON Schemas provide completion, hover descriptions, and validation
in schema-aware TOML and YAML editors:

| Descriptor scope | Schema |
| --- | --- |
| Repository root | [root.json](schemas/root.json) |
| Intermediate or overlay directory | [overlay.json](schemas/overlay.json) |

Each schema supports `over.toml`, `over.yaml`, and `over.yml`. Select the schema
by descriptor scope, not its filename: root and child files have the same name
but accept different settings. Add the comment shown in the
[root](configuration/root.md#editor-support) or
[overlay](configuration/overlay.md#editor-support) reference to associate it
with the correct schema. YAML comments work with YAML Language Server; TOML
directives work with Taplo-compatible editors.

Schemas describe authored settings, so inherited fields may be omitted.
Unknown keys in documented objects are editor errors; Over still warns and
ignores unknown keys when loading. Consumer metadata, Git option names, remote
extras, and hook environment names remain open namespaces with their documented
value types. Schemas do not replace [`over lint`](usage/lint.md) or runtime
checks for filesystem state, dependency cycles, glob syntax, template evaluation,
and Git remote validity.

## Path resolution

Configured filesystem paths retain the root of their **declaring** descriptor:

- Repository-root fields are repository-root-relative.
- Intermediate descriptor fields are relative to that descriptor's directory.
- Overlay-local fields are relative to the overlay root.
- Inherited values keep their original base when other fields are overridden.

This applies to sources, destinations, rule paths, excludes, declared Git
placement/local remotes. Nested `.link.*`
referents and `.partial.*` destinations use their owning overlay root, not the
sidecar's containing directory. Absolute paths remain absolute. Destination
templates render before relative-path resolution. `~` means the execution root
(`--target`, then `OVER_TARGET`, otherwise the home directory).

For example, root rule `apps/editor/config` and overlay-local rule `config` in
`apps/editor/over.yaml` select the same source tree. Root rule `config` does not
implicitly select every overlay's `config` directory. Running Over from another
working directory does not change configured paths.

Overlay names in `uses` and `default_overlay` are repository-relative
identifiers. Git refs, worktree names, transport URLs, arbitrary Git config,
metadata, and shell commands retain their own semantics. CLI-relative `--home`
and `--target` refer to the invocation directory. `.overignore` patterns are
relative to their ignore file.

Shared template `paths` are anchored to captured invocation inputs, so later
evaluations and hook working-directory changes cannot reinterpret them against
another process directory. They are normalized without following symlinks.
See [Targets and templates](configuration/overlay.md#targets-and-templates) for
the meanings and availability of `paths.home`, `paths.over`, and `paths.target`.

## Root configuration

See [Root configuration](configuration/root.md) for root-only fields and shared
overlay policy.

### Format preference

See [Format preference](configuration/root.md#format-preference).

### Canonical Git repository

See [Canonical Git repository](configuration/root.md#canonical-git-repository).

### Default overlay selection

See [Default overlay selection](configuration/root.md#default-overlay-selection).

## Lifecycle hooks

See [Lifecycle hooks](configuration/root.md#lifecycle-hooks), including root events
and overlay-local install/post-apply hooks.

### Events and order

See [Events and order](configuration/root.md#events-and-order).

### Working directory and environment

See [Hook environment](configuration/root.md#working-directory-and-environment).

### Failures and retries

See [Failures and retries](configuration/root.md#failures-and-retries).

## Consumer metadata (`data`)

See [Consumer metadata](configuration/overlay.md#consumer-metadata-data).

### Resolution and precedence

See [Data resolution](configuration/overlay.md#resolution-and-precedence).

### Templates and external tools

See [Templates and external tools](configuration/overlay.md#templates-and-external-tools).

## Ignoring files with `.overignore`

See [Ignore files](configuration/overlay.md#ignoring-files-with-overignore).

## Directory target mappings

See [Directory target mappings](configuration/overlay.md#directory-target-mappings).

## Materialization rules

See [Materialization](configuration/materialization.md) for supported modes,
ordered precedence, selectors, and ownership boundaries.

## File permissions

See [File permissions](configuration/overlay.md#file-permissions).

## Partial files

See [Partial files](configuration/overlay.md#partial-files).
