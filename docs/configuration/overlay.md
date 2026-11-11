# Overlay configuration

An overlay is a directory of source files plus configuration inherited from
repository-root, intermediate, and local descriptors. It need not be a separate
Git repository. Use a local `over.toml`, `over.yaml`, or `over.yml` to override
inherited policy. Root-declared overlays may omit a local descriptor entirely.

```yaml
# ~/.dotfiles/hosts/laptop/over.yaml
description: Laptop configuration
target: "~"
uses:
  - apps/git
  - apps/editor
data:
  theme: dark
```

## Fields and inheritance

Configuration cascades from the repository root down the selected directory's
ancestor chain. `uses` is different: it includes independently configured
overlays, not another configuration layer. Dependencies do not override each
other's files; overlapping ownership is a conflict.

| Field | Purpose | Inheritance |
| --- | --- | --- |
| `description` | Human-readable summary | Nearest value wins |
| `target` | Base destination; defaults to `~` | Nearest value wins, retaining its declaring root |
| `uses` | List of repository-relative overlay names | Nearest list replaces the inherited list; `[]` clears it |
| `exclude` | Source exclusion glob or list of globs | Nearest declaration wins with its original base; `[]` clears it |
| `map` | Redirect source directories | Merge by source key; nearer entries override the same key |
| `mode` | Directory materialization baseline | Ordered policy cascade |
| `permissions` | Octal baseline for partial-file targets | Ordered policy cascade, independent of mode |
| `rules` | Ordered path-specific mode/permission assignments | All scopes contribute; `[]` does not clear ancestor rules |
| `data` | Consumer-owned metadata mapping | Recursive mapping merge; other values replace |
| `git` | Repositories/worktrees to provision | Tables merge by key; nearer conflicting values replace |
| `hooks` | Overlay-local install and post-apply commands | Never inherited |

Root-only preferences are documented in [Root configuration](root.md). Runtime
fields such as the overlay's name and source root are not descriptor settings.

Unknown keys are ignored with a warning identifying the declaring descriptor.
This includes extra fields in rules, mappings, Git repository/worktree objects,
and hooks. Removed settings such as `install`, `default`, `defaults`,
`materialization`, and `link_dirs` also warn and are ignored, with migration hints.
Warnings do not prevent application. Invalid values for recognized settings,
missing required fields, and malformed descriptors remain errors. Arbitrary
consumer keys inside `data` and supported Git configuration namespaces are allowed.

## Targets and templates

`target` is the base destination for ordinary entries. An overlay with
`target: "~"` and `.config/editor/config.toml` manages
`~/.config/editor/config.toml`. `~` means `--target` when supplied, then
`OVER_TARGET` when set, otherwise the user's home directory; `--home` selects the
source repository instead.

Destination templates can use `machine` facts (`os`, `arch`, `hostname`,
`username`, `distro`, `distro_id`), shared `paths`, the current `overlay`, and
resolved `data`:

```toml
# ~/.dotfiles/workspaces/over.toml — inherited by workspaces/*
target = "~/Workspaces/{{ overlay.name | split('/') | last }}"
```

The same context is available to path templates and every lifecycle hook field
(`when`, `command`, `env` values, and `cwd`):

| Field | Meaning |
| --- | --- |
| `paths.home` | The current user's detected home directory, independent of `--target`, `OVER_TARGET`, and `--home` |
| `paths.over` | The active Over source repository root |
| `paths.target` | The current overlay's resolved base destination, independent of directory mappings |

These values are filesystem paths serialized as absolute native path strings.
They retain symlink spelling and do not require the destination to exist. Paths
that cannot be represented as UTF-8 produce an error rather than a lossy string.
Each dependency and its hooks see its own target; repository install/post-apply hooks
see the selected overlay's target.

Unavailable fields are undefined, not null. `paths.target` is unavailable during
init, default-overlay selection, and evaluation of the target definition itself.
`paths.home` is unavailable if home detection fails; `paths.over` is unavailable
for library contexts with no repository. No unrelated directory is substituted.
Guard optional inputs with `paths.target is defined`, or provide an explicit
fallback with `default`. Unguarded undefined values fail template evaluation,
including in ordinary destination templates.

For example, a sidecar can refer to `{{ paths.target }}/shared`, while a target
definition can use `{{ paths.home }}/Workspaces`. Unlike `~`, `paths.home` does
not follow an execution-root override.

Templates apply to supported path fields and lifecycle hook execution fields,
not arbitrary strings. File bodies, partial content, and metadata strings
remain literal. See
[Path resolution](../configuration.md#path-resolution) for relative paths and
[Default overlay selection](root.md#default-overlay-selection) for bootstrap
template limitations.

## Composition

Use `uses` to assemble an overlay from reusable pieces:

```yaml
# ~/.dotfiles/hosts/laptop/over.yaml
uses: [apps/git, apps/editor]
```

Names are literal repository-relative identifiers, not globs. Dependencies are
processed transitively, dependency-first, without repeating shared dependencies.
Each keeps its own target and configuration. `--no-uses` skips dependency
operations but retains dependency metadata. Missing dependencies and cycles are
errors. See [Examples](../examples.md#compose-a-host) for a complete layout.

## Excludes and ignore files

`exclude` selects source paths using globs relative to the declaring descriptor:

```yaml
# ~/.dotfiles/apps/editor/over.yaml
exclude: ["cache/**", "notes.txt"]
```

### Ignoring files with `.overignore`

Place `.overignore` at the repository root for shared exclusions, or inside an
overlay/nested directory for subtree-local rules. Patterns use Git's ignore
syntax, relative to the containing ignore file:

```gitignore
target/
dist/
.env
.env.*
*.pem
!.env.example
```

- `/secrets` anchors to that ignore file's directory. Basenames such as `*.pem`
  match at any depth below it.
- `*` and `?` match within a component; `**` spans directories. Trailing `/`
  matches directories, not regular files or symlinks.
- Blank lines and `#` comments are ignored. Escape initial `#` or `!` and
  significant trailing spaces with `\`.
- Last matching rule wins within a file; deeper ignore files take precedence.
  `!` re-includes a path only if its parents are not excluded. Reopen `cache/`
  before trying to re-include `cache/keep`.

Selection uses source paths before sidecar parsing and destination mapping.
Ignoring a sidecar suppresses its declaration; negation cannot deploy control
metadata as ordinary files. Ignores do not unregister overlays or disable their
configuration. There is no `ignore` descriptor field.

Source-tree selection always excludes Git administration paths (`.git`
directories, their contents, and worktree pointer files) from deployment and
reverse add. Ignore negations cannot re-include them, and Over does not parse
sidecars or ignore rules inside them. Whole-directory links to content inside
the overlay source tree are rejected if they would expose Git administration
data.

This selection policy does not constrain explicit `.link.*` referents outside
the overlay source tree. Such a declaration can link an external repository
directory, including its `.git`, or an individual Git administration file.

`exclude` applies independently and cannot be undone by `.overignore` negation.
Over does not read `.gitignore`, `.ignore`, global Git excludes, ignore files
above its repository root, or symlinked `.overignore` files.

Current source working-tree rules govern apply, status, diff, add, commit, sync,
and unapply, including tracked checkout content. Newly ignored deployed files
become unmanaged and stay in place; Over stops reporting, staging, committing,
syncing, or removing them. Already staged ignored changes stay in the source
index. Removing the ignore brings paths back under normal conflict checks.

A whole-directory link cannot hide ignored children or metadata and is rejected
if it would expose them. Reverse add performs the same check; recursive
file-level add skips ignored children, and explicitly adding an ignored file
fails.

## Directory target mappings

Use `map` when one overlay should own several destination directories:

```yaml
# ~/.dotfiles/workspace/client/over.yaml
target: ~/Workspace/Client
map:
  config:
    target: ~/.config/client
  state:
    target: ~/.local/share/client
```

`config/settings.json` lands at `~/.config/client/settings.json`, not below
`~/Workspace/Client/config`. Unmapped `main/app` stays at
`~/Workspace/Client/main/app`. [`over add`](../usage/add.md) routes the other way:
`~/.config/client/other.yaml` belongs to `workspace/client/config/other.yaml`.

- Keys are literal relative directory names without `.` or `..` components.
  Overlapping keys such as `config` and `config/editor` are rejected. A source
  directory may be absent until files are added.
- Targets use the same templates and path resolution as the base target.
  Inherited mappings retain their declaring origin.
- Destinations cannot overlap each other, contain the base target, overlap the
  overlay source, or collide with residual managed content. A destination below
  the base target is supported.
- Rules/excludes select sources before routing. Each mapped root has its own
  effective mode. A mapping inside a whole-directory symlink is rejected.
- Link placement and declared Git placement follow mappings. Partial files keep
  their explicit destinations.

In checkout mode, mapped directories and the residual overlay are separate
ownership units. Before changing mappings on an existing checkout, resolve its
changes and unapply using the old configuration, then edit and apply again.
See [Materialization](materialization.md#atomic-boundaries-and-mappings).

## Materialization rules

`mode` accepts `symlink`, `symlink-directory`, or `checkout`. `rules` assigns mode
and/or permissions to source paths. See the complete
[materialization and precedence reference](materialization.md), including
directory boundaries, the directory-link glob exception, and lifecycle guidance.

## File permissions

Use an octal string for a baseline or path-specific assignment:

```toml
# ~/.dotfiles/apps/shell/over.toml
permissions = "644"

[[rules]]
path = "ssh/agent"
permissions = "600"
```

Permissions only manage **partial-file targets**. Symlinks share the source
inode, so applying permissions there would mutate source files. This setting
does not independently chmod virtual checkout content.

The selector matches a sidecar's own stem (`ssh/agent` for
`ssh/agent.partial.toml`), not its rendered destination. Mode and permissions
cascade independently, last matching assignment per property wins. Without an
assignment, permissions remain unmanaged. Permission drift is a pending change
fixed by apply, not a blocking content conflict.

## Link sidecars

Use `<name>.link.toml`, `.link.yaml`, or `.link.yml` to link a target entry to a
different referent. For an overlay targeting `~`, this example places a link at
`~/.config/editor`:

```toml
# ~/.dotfiles/apps/editor/.config/editor.link.toml
target = "~/shared/editor"
type = "soft"
```

Here `target` is the **referent**, while the sidecar's relative path without its
suffix determines link placement. Mapping rules can relocate that placement.
Relative referents use the owning overlay root, even for nested sidecars;
destination templates are supported.

`type` is `soft` (default) or `hard`. Directories fall back to soft links with a
warning; failed file hard links also fall back to soft links. The sidecar itself
is not deployed.

## Partial files

Use a `<name>.partial.toml`, `.partial.yaml`, or `.partial.yml` sidecar when a
managed block must coexist with content Over does not own:

```toml
# ~/.dotfiles/apps/shell/aliases.partial.toml
target = "~/.zshrc"
content = "alias ll='ls -la'\n"
# marker = "aliases" # Optional; defaults to the sidecar's stem.
```

`target` is a templated destination path; relative paths use the owning overlay
root. `content` is literal. `marker` distinguishes blocks in the same file:

```text
# >>> over: aliases >>>
alias ll='ls -la'
# <<< over: aliases <<<
```

One optional trailing newline in `content` is the separator before the end
marker: `"alias x=y"` and `"alias x=y\n"` describe the same block. Additional
trailing newlines preserve blank lines and are significant. Existing marker
layouts remain readable; applying an unchanged block does not rewrite it.
Empty `content` sidecars are skipped. Block operations also support empty blocks.

Hand-edited managed content is a conflict, never silently overwritten. Unapply
removes only the block, preserving unrelated content. Use
[permissions](#file-permissions) to manage the destination's mode.

## Git declarations

Use `git` to provision real repositories, such as a plugin checkout. This is
different from `mode: checkout`, which exposes the overlay source's own files.

```yaml
# ~/.dotfiles/apps/tmux/over.yaml
target: "~"
git:
  .tmux/plugins/tpm: https://github.com/tmux-plugins/tpm.git
```

A string or detailed repository object supplied directly as `git` is shorthand
for the `"."` entry at the overlay's target root. Other map keys name placement
paths; relative paths use the declaring descriptor's source scope before target
routing. A detailed entry requires `url` and supports:

| Field | Purpose / default |
| --- | --- |
| `branch`, `tag`, `rev` | Select a branch, tag, or commit; choose the intended revision form |
| `recurse_submodules` | Clone submodules; default `false` |
| `worktree` | Bare repository with worktrees; implied by a `worktrees` map unless explicitly set |
| `per_worktree_config` | Enable worktree config; defaults to the effective worktree setting |
| `worktrees` | Map names to branch strings or `{branch, config}` objects |
| `remotes` | Extra named remotes with required `url`, optional `fetch`, `push`, `tagopt`, and extra string settings |
| `config` | Repository Git configuration; dotted keys or nested sections |
| `worktree_config` | Bare repository's worktree-level configuration |

For worktree mode, the default branch worktree is created automatically.
Per-worktree config manages `core.bare = false` and `extensions.worktreeConfig`
in `.git/config`, and `core.bare = true` in `.git/config.worktree`.

```yaml
# ~/.dotfiles/workspaces/project/over.yaml — replace URL before applying
target: ~/Workspaces/project
git:
  url: https://github.com/your-org/project.git
  worktree: true
  worktrees:
    review: main
  config:
    fetch.prune: "true"
```

Apply provisions repositories and their configuration. Manage all declared
repositories, including root `"."` entries and worktrees, with ordinary Git.
[`over sync`](../usage/sync.md) reconciles virtual checkouts and optionally
synchronizes the Over source repository instead.

## Install configuration

Declare installation commands under `hooks.install`, executed only with
`over apply --install`, after reconciliation and before post-apply hooks.
The former top-level `install` field is ignored with a migration warning; see
[Install hooks](../install-config.md) for examples and migration guidance.

## Overlay hooks

Declare local commands under `hooks.install` and `hooks.post-apply`:

```yaml
# ~/.dotfiles/apps/editor/over.yaml
hooks:
  install:
    - command: brew install neovim
      when: machine.os == "macos"
  post-apply:
    - './scripts/refresh-cache "$OVER_OVERLAY_TARGET"'
    - command: ./scripts/report
      when: machine.os == "linux"
      env:
        REPORT_HOST: "{{ machine.hostname }}"
```

By default the script must exist in this overlay's source directory. Commands
run there after successful materialization and do not inherit through descriptors.
Structured entries can override `cwd`; relative overrides resolve against the
repository root. Both entry forms support command templates. See
[Lifecycle hooks](root.md#lifecycle-hooks) for ordering, environment, and retries.

## Consumer metadata (`data`)

`data` is a mapping for templates and external consumers, not Over settings:

```yaml
# ~/.dotfiles/hosts/laptop/over.yaml
data:
  hostname: laptop
  hardware:
    gpu: integrated
  packages: [one, two]
  optional: null
```

Top-level `data: null` is invalid. Nested mappings, lists, scalars, and nulls are
allowed. Over validates the top-level mapping; consumers validate their own
namespaces. `target`, `uses`, and other Over settings remain outside `data`.

### Resolution and precedence

Each overlay first inherits data through its descriptor chain. Execution then
composes these effective mappings:

1. Traverse the configured default's dependencies depth-first in `uses` order,
   then the default itself.
2. Traverse the selected overlay's closure in the same order, skipping previously
   visited overlays. New selected dependencies can override default contributions.
3. Merge the selected overlay last, even when the default depends on it. Each
   overlay contributes once.

Later contributions win. Mappings merge recursively; lists replace whole lists,
scalars replace prior values, and explicit nested nulls replace while retaining
the key. Absent keys leave earlier values intact. Even an inherited selected
value can override a host-specific value. For example, merging
`hardware: {gpu: nvidia}` into `{gpu: integrated, arch: x86_64}` changes only
`gpu`; merging `packages: [three]` replaces `[one, two]` entirely.

Without a default, only the selected closure contributes. All-overlay commands
resolve separate contexts per target, not one pooled mapping. `--no-uses` skips
operations, never metadata dependencies; cycles and missing dependencies still
fail resolution.

### Templates and external tools

Supported destination templates can read `{{ data.hostname }}` or nested values
such as `{{ data.hardware.gpu }}`. Metadata is read-only and strings inside it
are not recursively templated. File bodies and partial content remain literal.
`default_overlay` is resolved before this composition and cannot depend on it.

[`over data`](../usage/data.md) exports compact JSON, preserving types and nulls,
with diagnostics on stderr:

```sh
over data                # Configured default context
over data apps/editor    # Default context composed with apps/editor
over data | jq -r '.hostname'
```

External Rust consumers can use the same resolver:

```rust
use over::exec::Context;
use over::overlays::Repository;

fn example() -> anyhow::Result<()> {
    let repository = Repository::new("/path/to/overlays".into());
    let context = Context::builder().repository(repository.clone()).build();
    let selected = repository.get("base")?;
    let data = repository.resolve_data(&context, Some(&selected))?;
    // None resolves only the configured default/host context.
    let prepared = context.with_overlay(selected).resolve_data()?;
    assert_eq!(prepared.data, data);
    Ok(())
}
```

`DataMap` and `DataValue` are exported from `over::overlays`. Resolve context data
before rendering targets or performing library operations; dependency context
copies retain the selected-root snapshot.
