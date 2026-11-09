# Configuration

## Root Configuration

`over` supports a root configuration file at `~/.dotfiles/over.toml` (or
`over.yaml`/`over.yml`). This file controls global preferences that apply
across all overlays.

### Format Preference

The `format` field sets the default descriptor format used by `over new`
when creating new overlays. Accepted values are `toml` (default) and `yaml`.

TOML (`~/.dotfiles/over.toml`):

```toml
format = "yaml"
```

YAML (`~/.dotfiles/over.yaml`):

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

### Canonical Git Repository

The optional root-only `repository` field declares the canonical Git remote,
independently of the source used for the initial clone:

```toml
repository = "git@github.com:user/dotfiles.git"
```

```yaml
repository: git@github.com:user/dotfiles.git
```

This is a literal Git remote, not a project homepage or a template. SSH URLs,
SCP-style SSH, HTTPS, `file://` URLs and local repository paths are accepted.
Relative local paths follow Git's normal resolution rules. Empty values,
non-string values, control characters and template expressions are errors.
Nested descriptors cannot override the canonical remote.

[`over init`](usage/init.md) sets a fresh clone's `origin` to this value without
connecting to it. With no `repository` field, the clone's origin is preserved.
Existing checkouts require confirmation or `--set-origin` before origin changes.
Explicit origin push URLs are preserved and continue to govern pushes.

### Default Overlay Selection

The `default_overlay` field names the overlay commands should act on when
no `NAME` argument is given. It's rendered as a template against the same
context used for an overlay's own `target` field (`{{ machine.* }}`, etc.),
so it can resolve to a different overlay per machine:

```toml
default_overlay = "hosts/{{ machine.hostname }}"
```

Selection priority (highest to lowest) for `over apply`/`over unapply`/
`over commit`:

1. An explicit `NAME` CLI argument.
2. The resolved `default_overlay`.
3. The existing interactive fuzzy-select prompt.

For `over status`/`over diff`/`over sync`, omitting `NAME` normally reports
on every overlay; once `default_overlay` is configured, it narrows an
omitted `NAME` to just that overlay instead. Pass `-a`/`--all` to force
"every overlay" regardless of a configured default.

A `default_overlay` naming an overlay that doesn't exist, or that fails to
render, is a hard error — never a silent fallback to another selection
method. See
[ADR-019](adr/019-default-overlay-selection-extends-templating-to-overlay-choice.md).

The default also selects the execution's host **data context**. An explicit
`NAME` overrides the command target, but retains the default overlay's metadata
and its transitive dependencies. A broken default therefore fails data resolution
even with an explicit `NAME` or `--all`. The default's files and packages are not
processed merely because its data is included.

[`over doctor`](usage/doctor.md) deliberately does not participate in this:
its Overlay configuration and XDG state sections always cover every overlay
in the repository regardless of `NAME`, so narrowing the whole report to a
single default overlay would leave those sections inconsistently scoped
with the Git-exclude section. `doctor`'s `NAME` argument only narrows which
overlay's `.git/info/exclude` drift gets checked; there is no `--all` flag.
Its destination rendering still uses the default data context.

## Lifecycle Hooks

Hooks execute repository-provided commands. **Only initialize or apply repositories
you trust:** their hooks can execute arbitrary code with your user permissions.
Hook-enabled operations require exactly one valid root `over.toml`, `over.yaml`,
or `over.yml`. Keep this root descriptor plaintext when encrypting overlay content.

Declare root hooks as ordered lists of shell-command strings:

```yaml
hooks:
  post-clone:
    - git-crypt unlock
  post-init:
    - ./scripts/ensure-local-setup
  post-apply:
    - ./scripts/render-secrets
```

The equivalent TOML is:

```toml
[hooks]
post-clone = ["git-crypt unlock"]
post-init = ["./scripts/ensure-local-setup"]
post-apply = ["./scripts/render-secrets"]
```

Each overlay may declare its own `hooks.post-apply`, for example:

```yaml
hooks:
  post-apply:
    - 'op inject -i "$OVER_OVERLAY_ROOT/app.tpl" -o "$OVER_OVERLAY_TARGET/app.conf"'
```

Commands use `sh -c`, like install scripts, so a POSIX-compatible `sh` must be on
`PATH`, including on Windows. Commands are not rendered as templates; pass paths
through quoted environment variables. Each command runs in a separate shell.
Unknown event names, non-list values, non-string commands, empty commands, and NUL
bytes are errors. Missing hooks or empty command lists do nothing.

### Events and Order

1. **`post-clone`**: root hooks only, after a successful fresh clone, root
   validation, and canonical-origin setup. Runs before child overlay discovery
   or reading encrypted overlay descriptors. Clones declared in an overlay's
   `git` section do not trigger this event.
2. **`post-init`**: root hooks only, after initialization and any `post-clone`
   hooks succeed. Runs on every successful `over init`, including reused
   checkouts. Neither init event discovers or applies child overlays.
3. **`post-apply`**: after the entire apply succeeds, including dependency
   materialization and Git-exclude reconciliation. Root hooks run once first,
   then participating overlay hooks run in depth-first, dependency-first `uses`
   list order. Shared or repeated dependencies run once. Commands within each
   event run in declaration order. A successful no-change apply still runs hooks.

Hooks do not inherit through ancestor descriptors. Root hooks are separate from
overlay hooks, even when the repository root is selected as the overlay.
Intermediate ancestor hooks run only when that ancestor itself participates.
Overlay-local `post-clone` and `post-init` declarations are accepted but ignored.
`--no-uses` runs root hooks and the selected overlay's hooks only. Overlays included
solely for metadata do not participate.

Dry runs and read-only commands such as `status`, `diff`, and `lint` never execute
hooks. Hook subprocesses receive an `OVER_HOOK_ACTIVE` marker; Over commands
launched from them perform their operations without triggering further hooks.

### Working Directory and Environment

Root hooks run in the repository root. Overlay hooks run in their own source
directory. Commands inherit the caller's environment and standard input/output.
Over supplies these additional environment variables, with absolute paths:

| Variable | Availability and meaning |
| --- | --- |
| `OVER_HOOK_EVENT` | All hooks: `post-clone`, `post-init`, or `post-apply` |
| `OVER_REPOSITORY_ROOT` | All hooks: repository source root |
| `OVER_HOOK_ACTIVE` | All hooks: `1`, suppressing nested lifecycle dispatch |
| `OVER_TARGET_ROOT` | Post-apply: target home/root (`--root`) |
| `OVER_OVERLAY_NAME` | Post-apply: relevant overlay's repository-relative name |
| `OVER_OVERLAY_ROOT` | Post-apply: relevant overlay's source directory |
| `OVER_OVERLAY_TARGET` | Post-apply: relevant overlay's resolved base target |

For root post-apply hooks, the relevant overlay is the selected overlay; for
overlay hooks, it is that hook's overlay. Post-clone and post-init remove the
four apply-only variables from the child environment, even if inherited.

### Failures and Retries

The first failing hook stops later commands and hooks, and fails the operation.
Diagnostics identify the event, scope, and one-based command index. Over preserves
the hook's numeric exit code when representable as an 8-bit exit code; spawn errors,
signal termination, and unrepresentable codes return `1`. Over does not log command
text or environment values, including in debug/verbose mode, or copy command output
into errors. Commands control their own output: do not print secrets or enable shell
tracing for secret-bearing commands.

No post-apply hooks run after failed or partially failed materialization. If a hook
itself fails, already-applied files and completed hook side effects remain. Retrying
apply reruns hooks from the beginning, so write retry-safe commands.

Init preserves the checkout after failure. Retrying with that existing checkout
runs **only post-init**, not post-clone. Recover a failed post-clone command manually
in the preserved repository; use post-init for unlock/setup commands that should
automatically retry on every init. Post-init failures can be retried with `over init`.

## Consumer Metadata (`data`)

Descriptors may carry arbitrary consumer-owned metadata under `data`:

```yaml
data:
  hostname: workstation
  hardware:
    gpu: nvidia
  provisioning:
    archlinux:
      kernel: linux
      filesystem: btrfs
```

`data` must be a mapping when present; omitting it is equivalent to an empty
mapping. Nested values may be mappings, lists, scalars, or null. Over validates
the top-level mapping, while consumers validate their own namespaces, such as
`data.provisioning.archlinux`. Over's configuration (`target`, `uses`, `exclude`,
etc.) stays outside `data`; metadata does not configure Over itself.

### Resolution and Precedence

Each overlay first inherits `data` through the normal ancestor descriptor cascade.
The execution context then composes those effective mappings:

1. Traverse the configured default overlay's transitive `uses` dependencies,
   depth-first in list order, before the default overlay itself.
2. Traverse the selected command overlay's closure in the same way, skipping
   overlays already visited. Newly encountered selected dependencies can override
   earlier default contributions; shared dependencies are not replayed.
3. Merge the selected overlay itself last, even if the default depends on it.
   Each overlay contributes exactly once.

Later contributions have higher priority. Later siblings in `uses` override
earlier siblings. The selected overlay's effective mapping includes inherited
values, so even an inherited selected value can override a host-specific value.

Mappings merge recursively. All other conflicts use the higher-priority value:
lists replace whole lists, scalars replace scalars or mappings, and an explicit
null replaces a previous value while retaining the key. An absent key leaves
the previous value intact. For example:

```yaml
# Lower priority
data:
  hardware: {gpu: integrated, architecture: x86_64}
  packages: [one, two]
  optional: enabled
```

```yaml
# Higher priority
data:
  hardware: {gpu: nvidia}
  packages: [three]
  optional: null
```

The result retains `hardware.architecture`, changes `hardware.gpu` to `nvidia`,
replaces `packages` with `[three]`, and keeps `optional` with a null value.
Top-level `data: null` is invalid; null values inside the mapping are allowed.

Without a configured default, explicit selection includes only the selected
overlay and its dependencies. Without either, operational commands retain their
existing interactive/all-overlay selection behavior; `over data` instead fails
with a missing-selection error. Commands processing all overlays resolve a separate
context per target; unrelated overlays' data is not pooled together.
`--no-uses` skips dependency **operations**, but metadata always includes complete
dependency closures. Invalid defaults, missing dependencies, and dependency cycles
are resolution errors, including when dependency operations are skipped.

### Templates and External Tools

Existing path templates can read nested values:

```jinja2
{{ data.hostname }}
{{ data.hardware.gpu }}
{{ data.provisioning.archlinux.kernel }}
```

This works in overlay targets, directory mapping targets, `.link.*` targets,
and `.partial.*` targets. Data is read-only during rendering; metadata strings
are literal values rather than additional templates. File bodies and partial
content remain literal. `default_overlay` is resolved using the bootstrap
context (for example, `machine.hostname`) before metadata is composed; it cannot
depend on the data it selects.

External tools can consume the resolved mapping as compact JSON through
[`over data`](usage/data.md), the supported CLI interface for custom data:

```sh
over data                  # Configured default overlay context
over data base             # Default context composed with base
over data | jq -r '.hostname'
```

The command uses the canonical resolver from
[#179](https://github.com/noirbizarre/over/issues/179). It preserves JSON types and
explicit nulls, writes diagnostics to stderr, and fails without prompting when
neither an explicit name nor a default is available.

External Rust consumers can use the same public resolver without parsing files
or invoking CLI selection:

```rust
use over::exec::Context;
use over::overlays::Repository;

fn example() -> anyhow::Result<()> {
    let repository = Repository::new("/path/to/overlays".into());
    let context = Context::builder().repository(repository.clone()).build();
    let selected = repository.get("base")?;
    let data = repository.resolve_data(&context, Some(&selected))?;
    // Pass None instead to resolve only the configured default/host context.
    let prepared = context.with_overlay(selected).resolve_data()?;
    assert_eq!(prepared.data, data);
    Ok(())
}
```

`DataMap` and `DataValue` are exported from `over::overlays`. Call
`Context::resolve_data` before rendering targets or executing library operations;
context copies for dependency overlays preserve that selected-root snapshot.

## Ignoring Files with `.overignore`

Place `.overignore` beside the root `over.toml` (or `over.yaml`/`over.yml`)
to exclude source files throughout the project. Add another `.overignore`
inside an overlay or a nested directory for rules local to that subtree:

```text
~/.dotfiles/
├── over.toml
├── .overignore
└── overlays/
    ├── base/
    │   ├── over.toml
    │   └── .overignore
    └── development/
        └── over.toml
```

The root rules apply to both overlays. Rules in `overlays/base/.overignore`
apply only to `base`, and never affect `development`. Descriptor-less
overlays follow the same rules. Ignore patterns live exclusively in these
files; there is no `ignore` configuration field.

The syntax follows Git's `.gitignore` format:

```gitignore
# Build artifacts and local secrets
target/
dist/
.cache/
.env
.env.*
*.pem

# Keep the example file
!.env.example
```

- Patterns are relative to the directory containing their `.overignore`.
  A leading `/` anchors a pattern there: root `/secrets` excludes only the
  root's `secrets`, whereas an overlay's `/secrets` excludes that overlay's
  `secrets`. A basename such as `*.pem` matches at any depth below its file.
- `*` and `?` match within a path component; `**` matches across directories.
  A trailing `/` matches directories, not regular files or symlinks.
- Blank lines and lines beginning with `#` are ignored. Escape an initial
  `#` or `!` with `\` to match it literally. Unescaped trailing spaces are
  ignored; escape a trailing space to include it in the pattern.
- The last matching rule wins within a file. Deeper rule files take
  precedence over parent files. Prefix a pattern with `!` to re-include a
  path, provided its parent directories are not excluded. For example,
  `cache/` followed by `!cache/keep` still excludes `cache/keep`; reopen
  `cache/` itself before selecting files inside it.

Selection uses **source paths**, including for mapped destinations and
virtual checkouts. It runs before sidecar parsing: ignoring an
`app.link.toml` or `app.partial.yaml` suppresses that declaration, regardless
of its rendered target. Descriptors, sidecars, and `.overignore` remain
control metadata and are never deployed as ordinary files, even through
negation. Ignoring content does not unregister overlays or disable their
configuration loading.

The existing `exclude` configuration retains its glob syntax and applies
independently; `.overignore` negation cannot undo it. Over does not consult
`.gitignore`, `.ignore`, global Git excludes, or `.overignore` files above
the Over repository root. Symlinked `.overignore` files are not followed.

Current source working-tree rules govern apply, status, diff, add, commit,
sync, and unapply, including already tracked checkout content. When a new
rule ignores an already deployed file, it becomes unmanaged: existing
target content (including symlinks) stays in place. Over stops reporting,
staging, committing, syncing, or removing it. Already staged ignored source
changes remain in the source index. Removing a rule brings the path back
under normal reconciliation and conflict checks.

A whole-directory symlink (`symlink-directory` or `link_dirs`) cannot hide
individual children. Over rejects it before making changes if it would
expose ignored descendants or control metadata. Use individual file
symlinks or `checkout` materialization for such directories. Reverse add
performs the same check; recursive file-level add skips ignored children,
and explicitly adding an ignored file reports an error.

## Directory Target Mappings

An overlay can redirect selected directories while keeping its other files
under the usual `target`:

```yaml
target: ~/Workspace/Client
map:
  etc:
    target: /etc/client
  config:
    target: ~/.config/client
```

For an overlay named `workspace/client`:

| Overlay path | Applied path |
| --- | --- |
| `etc/config.toml` | `/etc/client/config.toml` |
| `config/settings.json` | `~/.config/client/settings.json` |
| `main/app` | `~/Workspace/Client/main/app` |

The mapped source prefixes are replaced, so `~/Workspace/Client/etc` and
`~/Workspace/Client/config` are not created. [`over add`](usage/add.md) uses
the inverse routing: `/etc/client/other.yaml` belongs to
`workspace/client/etc/other.yaml`.

- Keys name literal relative directories, without `.` or `..` components.
  Nested keys such as `config` and `config/editor` are rejected. A source
  directory may be absent until files are added.
- Absolute destinations remain absolute. `~` means the execution-context
  root (`--root`, otherwise the home directory). Relative destinations are
  resolved under the overlay's resolved `target`. Mapping targets support
  the same templates as the base target.
- Mapping entries cascade by source key; a nearer descriptor overrides the
  same key and preserves other inherited mappings.
- Mapped destinations cannot overlap one another, contain the base target,
  overlap the overlay source, or collide with residual managed content.
  A destination below the base target is supported.
- Rules, excludes, and permissions keep their original overlay-relative
  names. Each mapped root is a new materialization boundary, resolved using
  its own matching rule or the default strategy.
- A mapped root can be a whole-directory symlink. A mapping inside another
  whole-directory symlink is rejected because that symlink owns its entire
  subtree.
- `.link.*` placement and declared `git` paths use the same routing.
  `.partial.*` files retain their explicitly declared destinations.

With `defaults.materialization: checkout`, the residual overlay and each
mapped directory become separate virtual checkout units. Mapped directories
are excluded from the residual checkout; status, sync, commit, and unapply
respect these boundaries even when a mapped destination sits below the
base target. See [ADR-025](adr/025-directory-mappings-and-checkout-staging.md).

Before adding or changing mappings on an existing virtual checkout, commit
or resolve its changes and unapply it with the **old configuration**. Then
edit the mapping and apply again. An existing checkout association is not
silently reused with different ownership boundaries.

## Materialization Rules

By default, every file in an overlay is symlinked individually and every
directory is recursed into. Two fields, available both in the repository
root config and in any overlay's own descriptor (they inherit through the
same cascading chain as every other field — see
[ADR-002](adr/002-overlay-is-a-directory-not-a-repository.md)), let you
override this per overlay or per subtree:

```toml
[defaults]
materialization = "symlink"          # symlink | symlink-directory | checkout

[[rules]]
path = "some/directory"               # glob or literal, relative to the overlay root
materialization = "symlink-directory"
```

- `defaults.materialization` sets the materialization applied to every
  directory with no more specific `rules` match. `symlink` (the default)
  recurses into the directory and symlinks each file individually.
  `symlink-directory` symlinks the directory as a single unit — its
  contents are not separately enumerated or tracked. `checkout` makes the
  subtree a **virtual checkout** — ordinary, directly editable files with
  no `.git` at the target — see [Virtual checkouts](usage/status.md#virtual-checkouts-materialization--checkout)
  and [ADR-022](adr/022-virtual-checkout-materialization.md).
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

## File Permissions

`defaults.mode` and `permissions` mirror `defaults.materialization`/`rules`
exactly (same cascading inheritance, same specificity precedence), but
declare a desired permission mode instead of a materialization strategy:

```toml
[defaults]
mode = "644"                          # octal string; applied when no more
                                       # specific `permissions` entry matches

[[permissions]]
path = "ssh/agent"                     # matches a partial sidecar's own stem
mode = "600"
```

- Only meaningful for [Partial Files](#partial-files) — the one place
  `over` writes real, target-owned content directly today. Every symlinked
  file/directory shares its overlay source's inode: there's no independent
  target permission to set without `over` mutating that tracked source
  file, which stays out of scope (see
  [ADR-020](adr/020-file-permissions-are-hierarchical-rules-scoped-to-partial-files.md)).
- `path` matches a `<name>.partial.{toml,yaml,yml}` sidecar's own stem
  (e.g. `ssh/agent` for `ssh/agent.partial.toml`), not its rendered
  `target`.
- No `permissions`/`defaults.mode` at all (the default) leaves permissions
  entirely unmanaged — identical to pre-#65 behavior.
- A drift (the block's content already matches, but the target's actual
  mode doesn't) is auto-fixed on the next `over apply`, reported by `over
  status`/`over diff` like any other pending change — never a blocking
  conflict.

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

- `target`: the file to modify. Templated the same way a `.link.*` sidecar's
  `target` is (`{{ env.* }}`, `{{ machine.* }}`, `{{ overlays[...] }}`) — not
  the same context as an overlay's own `target` field, which currently only
  exposes `{{ machine.* }}` and has no `env`/`overlays` maps. See
  [ADR-010](adr/010-templating-is-scoped-to-path-strings.md) for the
  templating design these two paths share.
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
