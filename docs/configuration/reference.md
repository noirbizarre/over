# Configuration reference

This page lists every supported attribute in `over.toml`, `over.yaml`, and
`over.yml`, including nested objects. Keep one descriptor per directory.
The repository defaults to `~/.dotfiles`; select another with `--home` or
`OVER_HOME`.

| Scope | Available in |
| --- | --- |
| Root | The repository-root descriptor only |
| Overlay | Intermediate and overlay-local descriptors only |
| Both | Root, intermediate, and overlay-local descriptors |

There are no overlay-only top-level attributes. Root declarations of shared
attributes supply inherited policy, except for document-local `$schema` and
non-inherited `hooks`. Root-only preferences cannot be overridden by children.

Examples below use YAML unless labeled TOML; the same attributes are available
in both formats. See [Editor support](../configuration.md#editor-support) for
schema associations and completion.

## Top-level attributes

Defaults below apply when no inherited value or matching rule supplies a value.

| Attribute | Scope | Type | Default | Purpose |
| --- | --- | --- | --- | --- |
| `$schema` | Both | String | None | Document-local editor schema URL; ignored by Over and never inherited. Use a schema comment directive for TOML. |
| `format` | Root | `toml` or `yaml` | `toml` | Format for newly written descriptors; does not restrict reading. `--format` takes precedence. |
| `repository` | Root | String | Preserve cloned origin | Canonical literal Git remote used by `over init`. |
| `sync` | Root | [Sync object](#source-synchronization-sync) | Both operations disabled | Remote synchronization policy for the Over source repository. |
| `default_overlay` | Root | String | None | Repository-relative overlay identifier; bootstrap templates are supported. |
| `overlays` | Root | List of strings | No additional declarations | Repository-relative directories or globs added to descriptor-based discovery. |
| `description` | Both | String | None | Human-readable overlay description. |
| `target` | Both | String | `~` | Base destination path; supports destination templates. |
| `uses` | Both | List of strings | No dependencies | Literal repository-relative overlay identifiers, composed in order. |
| `exclude` | Both | String or list of strings | No exclusions | Source exclusion globs relative to the declaring descriptor. |
| `mode` | Both | [Materialization mode](#materialization-mode) | `symlink` | Materialization baseline for this scope. |
| `permissions` | Both | [Octal string](#permissions) | Unmanaged | Permission baseline for directly written partial/decrypted content. |
| `rules` | Both | List of [rule objects](#rules) | No local rules | Ordered source-path assignments of target, mode, and permissions. |
| `data` | Both | Mapping | Empty mapping | Consumer-owned metadata for templates and external tools. |
| `git` | Both | URL string, repository object, or placement mapping | No repositories | [Git repositories and worktrees](#git-repositories-git) to provision. |
| `hooks` | Both | [Hooks object](#lifecycle-hooks-hooks) | No commands | Local lifecycle commands; never inherited. |

### Root preferences

- **`repository`** accepts SSH URLs, SCP-style SSH, HTTPS, `file://`, and local
  paths. Relative local paths are repository-root-relative. It must be a
  nonempty literal string without surrounding whitespace, control characters,
  or template expressions. See [Canonical Git repository](root.md#canonical-git-repository).
- **`default_overlay`** is resolved before metadata composition and cannot
  depend on the `data` it selects or on `paths.target`. An invalid template or
  missing default overlay is an error even with an explicit selection. Command
  fallback behavior is described in [Default overlay selection](root.md#default-overlay-selection).
- **`overlays`** adds existing directories to discovery; it does not replace
  descriptor discovery. Missing literal paths and malformed globs are lint
  errors; unmatched globs are warnings. `.over` administration paths are not
  overlays. See [Overlay discovery](root.md#overlay-discovery).

```yaml
format: yaml
repository: git@github.com:your-user/dotfiles.git
default_overlay: hosts/laptop
overlays:
  - 'apps/*'
  - hosts/laptop
```

### Source synchronization (`sync`)

| Attribute | Scope | Type | Default | Purpose |
| --- | --- | --- | --- | --- |
| `sync.pull` | Root | Boolean | `false` | Fetch and rebase the source branch onto its upstream before fresh selection and local reconciliation. |
| `sync.push` | Root | Boolean | `false` | Push existing source commits after successful reconciliation. |

```toml
[sync]
pull = true
push = false
```

These settings affect [`over sync`](../usage/sync.md) and the repository
containing `OVER_HOME`, not repositories declared under `git`. Neither stages
nor commits changes. CLI pull/push flags override each setting independently;
dry runs perform no remote operations. See [Source synchronization](root.md#source-synchronization).

## Shared policy

### Inheritance and composition

Descriptors cascade from repository root through intermediate directories to the
selected overlay:

| Attributes | Resolution |
| --- | --- |
| `description`, `target` | Nearest declaration wins. |
| `uses`, `exclude` | Nearest declaration replaces the inherited value; `[]` clears it. |
| `mode`, `permissions`, `rules` | At each scope, explicit baselines apply before that scope's rules. Rules accumulate; `rules: []` does not clear inherited rules. |
| `data` | Mappings merge recursively; lists, scalars, and nested nulls replace previous values. Top-level `data` must be a mapping. |
| `git` | Tables merge by key; nearer conflicting values replace earlier values. |
| `hooks` | Never inherited. Root hooks remain repository-scoped; child hooks belong to their own overlay. |
| `$schema` | Document-local; never inherited. |

`uses` composes independent overlays, each with its own target and policy. It
does not extend descriptor inheritance. Missing dependencies, cycles, and
conflicting file ownership are errors. `--no-uses` skips dependency operations,
but retains dependency metadata. See [Composition](overlay.md#composition) and
[Consumer metadata](overlay.md#consumer-metadata-data) for metadata merge order,
including the default overlay's context.

### Paths and templates

Configured paths retain their declaring descriptor's directory as their
relative-path base, even when inherited. Root declarations are repository-root-relative;
local declarations are overlay-root-relative. Absolute paths remain absolute.
Destination templates render before path resolution. `~` means `--target`, then
`OVER_TARGET`, otherwise the user's home directory.

Explicit hook `cwd` is an exception: relative values always use the repository
root. Sidecar paths use their owning overlay root, even in nested directories.

Templates apply to supported destination paths, hook execution fields, and files
explicitly selected by `rules[].template`. Other file bodies, partial content,
and metadata strings remain literal. Overlay
identifiers, source selectors, Git refs, and transport URLs have their own
literal semantics. See [Path resolution](../configuration.md#path-resolution)
and [Targets and templates](overlay.md#targets-and-templates) for template context
and the availability of `paths.home`, `paths.over`, and `paths.target`.

### Materialization mode

These values are accepted by both `mode` and `rules[].mode`:

| Value | Scope | Meaning |
| --- | --- | --- |
| `symlink` | Both | Recurse into directories and link individual files; the implicit default. |
| `symlink-directory` | Both | Link child directories as whole units; the overlay root remains scaffolding. |
| `checkout` | Both | Expose tracked source content in editable virtual checkouts, initially from committed source `HEAD`. |

Whole-directory links and checkouts are atomic boundaries; descendant mode
assignments cannot split them. See [Materialization](materialization.md) for
ownership, target-mapping constraints, and lifecycle guidance.

### Permissions

`permissions` and `rules[].permissions` require quoted octal strings, such as
`"600"`, `"0644"`, or `"0o644"`, in both root and overlay descriptors.
They manage partial-file, decrypted-file, and generated-template outputs, without
chmod of tracked symlink sources or independent chmod of ordinary virtual
checkout content. Without an assignment, partial permissions are unmanaged;
decrypted files default to `0600`, and rendered files use private access with the
source executable bit retained.
See [File permissions](overlay.md#file-permissions).

## Rules

Each `rules` entry requires `path` and at least one non-null assignment among
`target`, `mode`, `permissions`, `template`, and `strip_extension`.

| Attribute | Scope | Type | Default / requirement | Purpose |
| --- | --- | --- | --- | --- |
| `rules[].path` | Both | String | Required | Literal source prefix or glob, relative to the declaring descriptor. |
| `rules[].template` | Both | Boolean | `false` | Render selected file paths and contents using MiniJinja. |
| `rules[].strip_extension` | Both | Boolean | `false` | Remove the final source filename extension independently of rendering. |
| `rules[].target` | Both | String | No assignment | Templated destination prefix replacing the matched literal source prefix. |
| `rules[].mode` | Both | Materialization mode | No assignment | Assign `symlink`, `symlink-directory`, or `checkout`. |
| `rules[].permissions` | Both | Octal string | No assignment | Assign permissions independently of target and mode. |

Selectors and targets must not be empty or whitespace-only. `.` selectors are
forbidden; set overlay-wide baselines directly. Globs may assign mode and
permissions, but **target assignments require literal selectors**. `*` does not
cross path separators. Literal selectors match whole path components: `config`
matches `config/editor`, not `config-old`.

```yaml
target: "~"
mode: symlink
rules:
  - path: config
    target: ~/.config/editor
  - path: config/private
    permissions: "0600"
  - path: 'scripts/*.sh'
    mode: checkout
```

All matching rules apply in declaration order against the original source path.
The last assignment wins independently for each attribute; a later broad rule
can override an earlier specific one. Redirected destinations do not feed into
later selectors. Top-level `target` supplies fallback routing without resetting
inherited target rules. See [Overlay Rules](overlay.md#overlay-rules).

## Git repositories (`git`)

`git` accepts three forms in **Both** descriptor scopes:

```yaml
# One repository at placement ".":
git: https://github.com/tmux-plugins/tpm.git
```

```yaml
# A detailed repository at placement ".":
git:
  url: https://github.com/tmux-plugins/tpm.git
  branch: master
```

```yaml
# Placement paths mapped to URL strings or detailed objects:
git:
  .tmux/plugins/tpm: https://github.com/tmux-plugins/tpm.git
  projects/example:
    url: https://github.com/your-org/project.git
    branch: main
```

Placement paths and relative local clone sources retain their declaring scope.
Placement paths participate in target routing. Inherited root declarations are
not replicated relative to every overlay; declarations outside the selected
overlay's source subtree may be filtered out. A top-level `url` key selects the
detailed-repository form, so do not use `url` as a placement-map key.

### Repository attributes

These fields belong to a detailed repository object, either directly under
`git` or under a placement key such as `git.projects/example`.

| Attribute | Scope | Type | Default / requirement | Purpose |
| --- | --- | --- | --- | --- |
| `url` | Both | String | Required | Repository clone source. |
| `branch` | Both | String | None | Branch selected at clone time. |
| `tag` | Both | String | None | Tag checked out for a non-bare repository. |
| `rev` | Both | String | None | Revision checked out detached for a non-bare repository. |
| `recurse_submodules` | Both | Boolean | `false` | Clone submodules. |
| `worktree` | Both | Boolean | `true` if `worktrees` is present; otherwise `false` | Enable bare/worktree provisioning and automatic default-branch worktree creation. |
| `per_worktree_config` | Both | Boolean | Effective `worktree` value | Enable Git worktree configuration. |
| `worktrees` | Both | Mapping | None | Worktree names mapped to branch strings or [worktree objects](#named-worktrees). |
| `remotes` | Both | Mapping | No extra remotes | Remote names mapped to [remote objects](#remotes). |
| `config` | Both | Git configuration mapping | No entries | Shared repository configuration. |
| `worktree_config` | Both | Git configuration mapping | No entries | Repository's own worktree-level configuration when `per_worktree_config` is enabled. |

Choose only one of `branch`, `tag`, and `rev`; lint rejects combinations. Bare/
worktree provisioning ignores `tag` and `rev`, with lint warnings. A present
`worktrees` map, even an empty one, enables bare/worktree provisioning:
`worktree: false` then suppresses the automatic default worktree rather than
disabling provisioning. It also changes the default of `per_worktree_config`.

Apply reapplies remotes, configuration, and worktree provisioning to existing
repositories. `branch` is a clone-time choice, not a branch-switching policy for
existing clones. Manage these repositories with ordinary Git;
[`over sync`](../usage/sync.md) serves a different purpose.
See [Git declarations](overlay.md#git-declarations) for worktree layout.

### Named worktrees

Each repository's `worktrees` maps a worktree name to a branch string or an
object with these attributes:

| Attribute | Scope | Type | Default / requirement | Purpose |
| --- | --- | --- | --- | --- |
| `worktrees.<name>.branch` | Both | String | Required | Branch for the named worktree. |
| `worktrees.<name>.config` | Both | Git configuration mapping | No entries | Configuration written to that worktree's `config.worktree`. |

```yaml
git:
  url: https://github.com/your-org/project.git
  worktrees:
    review: main
    development:
      branch: develop
      config:
        user.email: dev@example.com
```

### Remotes

Each repository's `remotes` maps a remote name to an object:

| Attribute | Scope | Type | Default / requirement | Purpose |
| --- | --- | --- | --- | --- |
| `remotes.<name>.url` | Both | String | Required | Remote URL. |
| `remotes.<name>.fetch` | Both | String | No custom value | Fetch refspec. |
| `remotes.<name>.push` | Both | String | No custom value | Push refspec. |
| `remotes.<name>.tagopt` | Both | String | No custom value | Git tag option, for example `--tags` or `--no-tags`. |
| `remotes.<name>.<option>` | Both | String | No custom value | Additional remote options; this namespace is open. |

Additional options are written directly in the remote object; there is no
`extras` wrapper.

### Git configuration mappings

Repository `config`, repository `worktree_config`, and named-worktree `config`
all accept arbitrary Git option names in **Both** descriptor scopes. Mix dotted
keys with nested sections, up to three key levels deep. Leaf values must be
strings; quote booleans and numbers. There is no `entries` wrapper.

```yaml
git:
  url: https://github.com/your-org/project.git
  config:
    fetch.prune: "true"
    user:
      email: dev@example.com
    remote:
      origin:
        prune: "true"
```

## Lifecycle hooks (`hooks`)

Every event contains a **list** of command strings or hook objects. Omitted
events and empty lists do nothing. Availability below describes execution:
child `post-clone` and `post-init` declarations are accepted but ignored.

| Attribute | Scope | Type | Default | Execution |
| --- | --- | --- | --- | --- |
| `hooks.post-clone` | Root | List of hooks | `[]` | After a fresh init clone, before child discovery. |
| `hooks.post-init` | Root | List of hooks | `[]` | After every successful init, following any post-clone hooks. |
| `hooks.install` | Both | List of hooks | `[]` | After successful reconciliation, only with `over apply --install` or `-i`. |
| `hooks.post-apply` | Both | List of hooks | `[]` | After successful apply and any opted-in install hooks. |

### Hook attributes

String entries are shorthand for an object with only `command`. Object fields
are available in both descriptor scopes, subject to their event's scope.

| Attribute | Scope | Type | Default / requirement | Purpose |
| --- | --- | --- | --- | --- |
| `command` | Both | String | Required | Nonempty shell command; templated before execution through `sh -c`. |
| `env` | Both | Mapping of names to strings | No overrides | Additional process environment; values are templated, names are literal. |
| `cwd` | Both | String | Repository source root for root hooks; owning overlay source root for child hooks | Templated working directory; explicit relative paths are repository-root-relative. |
| `when` | Both | String | Always run | Bare MiniJinja condition expression, without `{{ }}`. |

```yaml
hooks:
  install:
    - command: ./scripts/install-tools
      when: machine.os == "linux"
  post-apply:
    - command: ./scripts/report
      cwd: "{{ paths.over }}"
      env:
        DEPLOYMENT_HOST: "{{ machine.hostname }}"
```

Commands must not be whitespace-only. Commands, environment values, `cwd`, and
`when` must contain no NUL bytes. Optional `cwd` and `when` must be strings when
present, not nulls. Rendered `cwd` must name an enterable directory; no shell or
tilde expansion occurs. Environment names must be nonempty and contain neither
`=` nor NUL. The following names are reserved, case-insensitively:

- `OVER_HOOK_ACTIVE`
- `OVER_HOOK_EVENT`
- `OVER_REPOSITORY_ROOT`
- `OVER_TARGET_ROOT`
- `OVER_OVERLAY_NAME`
- `OVER_OVERLAY_ROOT`
- `OVER_OVERLAY_TARGET`

Conditions run first. Boolean true or a nonempty string runs the hook; false,
none, or an empty string skips it. The string `"false"` runs it. Numbers,
collections, undefined results, and expression errors fail. Skipped hooks do
not render execution fields.

Hooks never inherit. Root apply hooks run first, then participating overlays
dependency-first in `uses` order; shared dependencies run once. Metadata-only
participation, dry runs, and read-only commands do not execute hooks. Each
command runs in a separate shell; template substitution is not shell escaping.
Failures stop later hooks without undoing completed effects.
See [Lifecycle hooks](root.md#lifecycle-hooks) for context, secrets, environment,
ordering, and retry behavior, and [Install hooks](../install-config.md) for examples.

## Sidecar configuration

Sidecars are separate files, **not attributes in root or overlay descriptors**.
Their fields do not inherit through descriptor tables, and sidecars themselves
are not deployed.

### Link sidecars

`<name>.link.toml`, `<name>.link.yaml`, or `<name>.link.yml`:

| Attribute | Scope | Type | Default / requirement | Purpose |
| --- | --- | --- | --- | --- |
| `target` | Link sidecar only | String | Required | Templated link referent, not link placement. Relative paths use the owning overlay root. |
| `type` | Link sidecar only | `soft` or `hard` | `soft` | Link type; directory hard links and failed file hard links fall back to soft links. |

The sidecar stem determines placement, subject to target rules.
See [Link sidecars](overlay.md#link-sidecars).

### Partial-file sidecars

`<name>.partial.toml`, `<name>.partial.yaml`, or `<name>.partial.yml`:

| Attribute | Scope | Type | Default / requirement | Purpose |
| --- | --- | --- | --- | --- |
| `target` | Partial sidecar only | String | Required | Templated destination path; relative paths use the owning overlay root. |
| `content` | Partial sidecar only | String | Required | Literal managed block content; never templated. |
| `marker` | Partial sidecar only | String | Sidecar stem | Identifier distinguishing managed blocks in one destination file. |

Empty targets or content are skipped with warnings. Markers use fixed `#`
comments, so destinations must support that syntax. See
[Partial files](overlay.md#partial-files) for newline and conflict handling.

For Git-ignore-style exclusions, use a separate
[`.overignore` file](overlay.md#ignoring-files-with-overignore); there is no
`ignore` descriptor attribute.

## Validation and unsupported attributes

Editor schemas reject unknown attributes in closed configuration objects.
At runtime, Over warns and ignores unknown attributes; invalid recognized
values and missing required fields remain errors. `data`, Git option names,
additional remote options, and hook environment names are intentionally open
namespaces with the value types described above. Schemas do not replace
[`over lint`](../usage/lint.md) or runtime validation.

Removed attributes `install`, `default`, `defaults`, `materialization`, and
`link_dirs` are unsupported. Use `hooks.install`, `default_overlay`, scalar
`mode`/`permissions`, and `rules` as appropriate. Old permission-rule arrays
are also unsupported; use octal strings and `rules[].permissions`.
Runtime fields such as overlay name, source root, scopes, and assignment origins
are not descriptor attributes.
