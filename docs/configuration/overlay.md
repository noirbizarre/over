# Overlay configuration

An overlay is a directory of source files plus configuration inherited from
repository-root, intermediate, and local descriptors. It need not be a separate
Git repository. Use a local `over.toml`, `over.yaml`, or `over.yml` to override
inherited policy. Root-declared overlays may omit a local descriptor entirely.

See the unified [Configuration reference](reference.md) for all attributes,
their types and defaults, and their availability in root and overlay descriptors.

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

## Editor support

Use the [overlay schema](https://noirbizarre.github.io/over/schemas/overlay.json) for intermediate and local
descriptors. For YAML (`over.yaml` or `over.yml`), set `$schema` at the top:

```yaml
$schema: https://noirbizarre.github.io/over/schemas/overlay.json
description: Laptop configuration
```

For TOML, use a schema directive at the top of `over.toml`:

```toml
#:schema https://noirbizarre.github.io/over/schemas/overlay.json
description = "Laptop configuration"
```

Fields may be omitted to inherit their values. Use the
[root schema](root.md#editor-support) for repository-only preferences instead.
See [Editor support](../configuration.md#editor-support) for validation scope.

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
| `mode` | Directory materialization baseline | Ordered policy cascade |
| `permissions` | Octal baseline for directly written content | Ordered policy cascade, independent of mode |
| `rules` | Ordered path-specific target/mode/permission assignments | All scopes contribute; `[]` does not clear ancestor rules |
| `data` | Consumer-owned metadata mapping | Recursive mapping merge; other values replace |
| `git` | Repositories/worktrees to provision | Tables merge by key; nearer conflicting values replace |
| `hooks` | Overlay-local install and post-apply commands | Never inherited |

Root-only preferences are documented in [Root configuration](root.md). Runtime
fields such as the overlay's name and source root are not descriptor settings.

Unknown keys are ignored with a warning identifying the declaring descriptor.
This includes extra fields in rules, Git repository/worktree objects,
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
| `paths.target` | The current overlay's resolved base destination, independent of rule targets |

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

Templates apply to supported path fields, lifecycle hook execution fields, and
explicitly selected [file templates](#file-templates). Other file bodies,
partial content, and metadata strings remain literal. See
[Path resolution](../configuration.md#path-resolution) for relative paths and
[Default overlay selection](root.md#default-overlay-selection) for bootstrap
template limitations.

### File templates

Enable MiniJinja rendering through the existing ordered rules:

```yaml
rules:
  - path: "**/*.jinja"
    template: true
    strip_extension: true
  - path: "**/*.dotfile"
    template: true
    strip_extension: true
  - path: "config/extensionless"
    template: true
```

`template` renders both the selected source's destination path and UTF-8 file
content. For example, `config.toml.jinja` containing
`hostname = "{{ data.hostname }}"` produces `config.toml` with the resolved
hostname. `machine.os`, `machine.arch`, and `machine.hostname` provide detected
machine facts; `data` includes inherited and dependency-provided values.

`strip_extension` independently removes the final filename extension. It does
not enable rendering. Extensionless names and dotfiles without another suffix
are unchanged. Rules match original source paths, and later matching assignments
win independently for each property; explicit `false` disables an inherited
assignment. Neither a conventional suffix nor Jinja-looking text enables
rendering automatically. Rendered destinations must remain safe relative paths
and cannot collide with another managed entry.

Data and dependencies are resolved before rendering. Every file consumes that
immutable snapshot, with its owning overlay's path bindings. Templates cannot
produce data for another template. Rendering never modifies the source template.

In symlink mode, destinations link to atomic, reconstructible artifacts under
`$XDG_STATE_HOME/over/rendered` (normally `~/.local/state/over/rendered`). A
whole-directory symlink containing templates is expanded into leaf links.
In checkout mode, generated output is an ordinary physical file, rendered from
committed source `HEAD`; commit template-file edits with Git in the source
repository before syncing them, or use
[`over commit`](../usage/commit.md#rendered-file-edits) to propagate compatible
literal edits from the rendered file. Context
changes can rerender without a source commit. See
[generated checkout synchronization](../usage/sync.md#generated-checkouts).

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

## Overlay Rules

Use one ordered `rules` list for source-path configuration. Each rule has a
`path` selector and at least one non-null assignment among `target`, `mode`,
`permissions`, `template`, and `strip_extension`. Explicit boolean `false`
counts as an assignment, so rendering-only rules need no extra mode or target:

```yaml
# ~/.dotfiles/workspace/client/over.yaml
target: ~/Workspace/Client
rules:
  - path: config
    target: ~/.config/client
  - path: state
    target: ~/.local/share/client
  - path: config/private
    permissions: "0600"
```

`config/settings.json` lands at `~/.config/client/settings.json`, not below
`~/Workspace/Client/config`. Unmapped `main/app` stays at
`~/Workspace/Client/main/app`. [`over add`](../usage/add.md) routes the other way:
`~/.config/client/other.yaml` belongs to `workspace/client/config/other.yaml`.

### Matching and cascade

Literal `path` selectors match source prefixes by component: `config` matches
`config/editor/file`, never `config-old`. A literal `.` selector is forbidden;
set overlay-wide defaults directly with `target`, `mode`, or `permissions`.
Paths retain their [declaring scope](../configuration.md#path-resolution)
when inherited. Globs such as `dotfiles/ssh/*.pub` match files or directories;
`*` does not cross a path separator. **Rules defining `target` require literal
selectors**, even when they also set other attributes.

Every matching rule participates in declaration order against the **original
source path**. A redirected destination never becomes another rule's input.
The last matching assignment wins **independently for each attribute**:

- An absent attribute leaves the earlier value intact.
- A later broad rule can override an earlier specific rule.
- A permissions-only rule preserves target and mode, and a target-only rule
  preserves permissions and mode.

Descriptors cascade from the repository root through intermediate directories
to the selected overlay. At each scope, explicit mode/permission baselines apply
before its rules; omitted baselines preserve earlier decisions. Rule lists append
to this cascade; `rules: []` does not clear ancestor rules. Implicit mode is
`symlink`, with permissions unmanaged. `uses` composes independent overlays;
it does not extend another overlay's inheritance chain.

### Target redirection and defaults

A matching target rule replaces its source prefix with `target`, preserving
the remaining relative suffix. A later matching target rule replaces that
mapping using its own source prefix:

```yaml
rules:
  - path: dotfiles/ssh
    target: ~/.ssh
  - path: dotfiles/ssh/*.pub
    permissions: "0666"
  - path: dotfiles/ssh/id_rsa
    permissions: "0600"
  - path: dotfiles/ssh/hosts
    target: ~/.config/ssh-hosts
```

`dotfiles/ssh/id_rsa` resolves to `~/.ssh/id_rsa` with permission policy `0600`.
`dotfiles/ssh/hosts/work` resolves to `~/.config/ssh-hosts/work`. Public keys
retain the SSH destination and receive permission policy `0666`. Permission
enforcement depends on the materialization mechanism described below.

Without a matching target assignment, the overlay-level target applies, then
the inherited root configuration target, then home (`~`). Top-level target
settings supply this fallback; they do not reset inherited target rules.
Targets use existing templates, home expansion, and lexical normalization.
Relative destinations are **descriptor-relative**; use `~/.config/env` for a
home-relative destination. `paths.target` in templates remains the resolved
base target, independent of rule redirection.

Directory targets preserve descendant layout; file targets can rename individual
files. Literal directory sources may be absent until reverse add populates them.
Link placement and declared Git placement follow target rules. Link referents
and partial-file explicit destinations keep their own semantics.

Redirection is resolved before desired-state construction. Effective directory
redirects partition checkout ownership; shadowed or layout-equivalent rules do
not create redundant units. A redirect cannot split a whole-directory symlink
or declared Git checkout. Independently redirected files inheriting
`mode: checkout` are rejected: redirect their containing directory or select
`mode: symlink` for the file. Source overlap and conflicting destination ownership
are rejected before changes. Before changing a live checkout's layout, resolve
its changes and unapply with the old configuration, then edit and apply again.
See [Materialization](materialization.md#atomic-boundaries-and-mappings).

### Target-based inference

When no overlay is specified, a uniquely identifying rule target also identifies
its overlay and corresponding source directory. Inside `~/.ssh/subdir`, the SSH
rule above resolves the source directory `dotfiles/ssh/subdir`. Relative suffixes
are preserved, including for missing source paths. Shadowed destinations cannot
identify their former source paths.

Multiple overlays or multiple valid rule/source explanations are ambiguous;
Over does not choose by specificity or use a default to break a tie. Naming an
overlay does not resolve a source-path ambiguity inside it. See
[contextual selection](../usage/selection.md) for evidence priorities.

### Materialization mode

`mode` accepts `symlink`, `symlink-directory`, or `checkout`. See
[Materialization](materialization.md) for directory boundaries and lifecycle
guidance. Mode and permissions use the same rule cascade as target routing.

### File permissions

Use an octal **string** for a baseline or path-specific assignment:

```toml
# ~/.dotfiles/apps/shell/over.toml
permissions = "644"

[[rules]]
path = "ssh/agent"
permissions = "600"
```

Permissions manage **partial-file and decrypted-file targets**. Symlinks share the source
inode, so applying permissions there would mutate source files. This setting
does not independently chmod virtual checkout content.

For partials, the selector matches the sidecar's own stem (`ssh/agent` for
`ssh/agent.partial.toml`), not its rendered destination. Encrypted files use the
decrypted logical name, without `.age`. Without an assignment, partial permissions
remain unmanaged and decrypted files default to `0600`. Permission drift is a
pending change fixed by apply, not a blocking content conflict.

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
Fresh virtual checkouts project individual owned files, so applications can share
home and fragment directories without claiming every descendant. See
[flat application composition](materialization.md#compose-flat-application-overlays).

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
are not recursively templated. Explicitly selected file templates can consume
the same data; partial content remains literal.
`default_overlay` is resolved before this composition and cannot depend on it.

[`over data`](../usage/data.md) exports JSON, preserving types and nulls, with
diagnostics on stderr. Terminal output is pretty-printed; piped or redirected
output is compact and uncolored:

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
