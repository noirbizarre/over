# Root configuration

Use the repository-root descriptor for repository preferences and policy shared
by overlays. The default location is `~/.dotfiles/over.toml`; `over.yaml` and
`over.yml` are also supported. `--home` or `OVER_HOME` selects another repository.
Keep one descriptor per directory.

See the unified [Configuration reference](reference.md) for all attributes,
their types and defaults, and their availability in root and overlay descriptors.

```yaml
# ~/.dotfiles/over.yaml
format: yaml
host_overlay: hosts/laptop
apply:
  default_to_host: false
target: "~"
overlays:
  - 'apps/*'
```

`format`, `repository`, `sync`, `host_overlay`, `apply`, `crypt`, `secrets`, and `overlays` are root-only settings.
Root hooks are also repository-scoped. Ordinary [overlay fields](overlay.md),
such as `target` and overlay rules, may appear here as inherited policy.
Root `data` and `host` belong only to the repository-root overlay and do not
inherit into children; they are not additional repository preferences.

## Secret commit messages

Native secret mutations use operation-specific commit messages. Customize them
in the repository-root descriptor (the following shows the defaults):

```toml
[secrets.commit_messages]
add = "chore(secrets): added {{ name }}"
replace = "chore(secrets): replaced {{ name }}"
remove = "chore(secrets): removed {{ name }}"
```

Only `name` is available to these strict templates; secret values and secret
lookup functions are unavailable. Missing settings retain their defaults.
Empty messages, NULs, and undefined template variables are errors.

The defaults expose **secret names in Git history**, even though the values
remain encrypted. Use a name-free template such as `chore(secrets): added a secret`
to omit names from commit messages. Names remain visible in Git filenames.
YAML supports the same nested settings.

## Access commit messages

User/key mutations, including first enrollment and rotation, create isolated
commits. These root-only templates show their defaults:

```toml
[crypt.commit_messages]
user_add = "chore(crypt): added user {{ user }}"
user_delete = "chore(crypt): deleted user {{ user }}"
user_revoke = "chore(crypt): revoked user {{ user }}"
key_add = "chore(crypt): added keys for {{ user }}"
key_detach = "chore(crypt): detached {{ user }}/{{ key }}"
key_revoke = "chore(crypt): revoked {{ user }}/{{ key }}"
key_rotate = "chore(crypt): rotated repository key"
migrate = "chore(crypt): migrated repository metadata"
```

User operations expose `user`; enrollment also exposes `keys` and `count`, plus
`key` for a single-key enrollment. Detach/key revocation expose `user` and `key`.
Rotation and standalone access migration have no named inputs. Templates are
strict, and secret values, credentials, environment data, and secret lookup
functions are unavailable. Empty/NUL messages and non-string settings fail before
tracked publication. Defaults apply to omitted settings. YAML uses the same keys.

## Default materialization

Set `mode` in the root descriptor to persist the repository's inherited
[materialization policy](materialization.md):

```toml
mode = "checkout"
```

[`over init`](../commands/init.md#choose-a-repository-default) offers to create this
minimal configuration when no root descriptor exists, recommending virtual
checkout and offering symlink as an alternative. Scripts can use
`over init --mode checkout`, `--mode symlink`, or `--no-config` to skip creation.
These setup-only options preserve existing configuration. Nearer descriptors
and rules still take precedence; choosing a default does not migrate deployed
files. Descriptors without a mode retain the implicit symlink fallback.

## Editor support

Use the [repository-root schema](https://noirbizarre.github.io/over/schemas/config.json) for this descriptor.
For YAML (`over.yaml` or `over.yml`), set `$schema` at the top:

```yaml
$schema: https://noirbizarre.github.io/over/schemas/config.json
format: yaml
```

For TOML, use a schema directive at the top of `over.toml`:

```toml
#:schema https://noirbizarre.github.io/over/schemas/config.json
format = "toml"
```

The YAML `$schema` key is editor metadata, ignored by Over and never inherited.
The TOML directive is a comment.
See [Editor support](../configuration.md#editor-support) for validation scope.

## Format preference

`format` controls what [`over new`](../commands/new.md) **writes**: `toml` (the
default) or `yaml`. Resolution is `--format`, then root `format`, then `toml`.
It does not restrict reading: all three descriptor extensions remain supported.

## Canonical Git repository

Use `repository` when the initial clone source differs from the remote you want
to keep, such as bootstrapping over HTTPS before using SSH:

```toml
# ~/.dotfiles/over.toml
repository = "git@github.com:user/dotfiles.git"
```

This is a literal Git remote, not a homepage or template. SSH, SCP-style SSH,
HTTPS, `file://`, and local paths are accepted. Relative local paths are
repository-root-relative. Empty values, non-strings, control characters, and
template expressions are errors. Child descriptors cannot override it.

[`over init`](../commands/init.md) sets a fresh clone's `origin` without connecting
to the canonical remote. With no field, it preserves the cloned origin. Existing
checkouts require confirmation or `--set-origin` before changing origin.
Explicit origin push URLs are preserved. Init validates or clones a repository;
it does not create an empty project or apply overlays.

## Source synchronization

Opt into remote operations for [`over sync`](../commands/sync.md) in the root descriptor:

```toml
[sync]
pull = false
push = false
```

The equivalent YAML is:

```yaml
sync:
  pull: false
  push: false
```

Both keys default to `false`, including when the table or either key is omitted.
`pull = true` fetches and rebases the current source branch onto its configured
upstream before fresh overlay selection and local reconciliation. `push = true`
publishes existing source commits to that upstream after successful reconciliation.
Neither remote operation stages or commits changes. These settings apply only to
the Git repository containing `OVER_HOME`, never overlay-declared repositories.

`--pull` / `--no-pull` and `--push` / `--no-push` independently override their
corresponding settings. Policy is resolved once before pull; updated remote
defaults take effect on the next invocation. Dry-run performs no remote operation.

## Host overlay context

`host_overlay` is an optional repository-relative overlay name identifying machine
configuration. It supplies host values and an inspection fallback. It is distinct
from the **active overlay** inferred from the invocation context and the overlays
whose files have actually been **applied**. Configuring a host does not deploy it.

```toml
host_overlay = "hosts/laptop"

[apply]
default_to_host = false
```

For a multi-host repository, `"hosts/{{ machine.hostname }}"` selects the current
host, provided that overlay actually exists. This template uses bootstrap machine
facts, before metadata composition; it cannot depend on the `data` it selects.

| Commands | Without an explicit name |
| --- | --- |
| `apply` | Infer active context, then use the host only if `apply.default_to_host` is true; otherwise require an explicit overlay |
| `sync` | Infer active context, then use the host only if `sync.default_to_host` is true; otherwise require an explicit overlay or `--all` |
| `data` | Infer active context, then use the host, otherwise require an explicit overlay |
| `status`, `diff` | Infer active context, then use the host, otherwise all overlays; `--all` overrides selection |
| `add`, `git add`, `git mount`, `unapply`, `commit`, `restore` | Infer active context, otherwise require an explicit overlay |
| `log` | Infer context, otherwise source-repository history; no default selection |
| `lint` | Check the repository; the host does not narrow it |
| `doctor` | Check all runtime overlays unless named; static lint and global checks remain repository-wide |

See [contextual overlay selection](../usage/selection.md) for ranked evidence,
Git associations, sibling files, and ambiguity errors. Ambiguity does not fall
through to the host.

The host overlay's own `host` mapping supplies machine-specific overrides to each
overlay's local `host` defaults. Its `data`, ancestors, and dependency graph do not
contribute to this merge. The host is optional; without one, local defaults apply.
See [data scopes](overlay.md#consumer-metadata-data). The host's files and hooks
are not processed just because its host overrides are included.
An invalid or missing configured host is an error during data resolution, including with an explicit
name or `--all`; Over never silently selects something else.

For single-host deployments, explicitly opt into bare `over apply`:

```yaml
host_overlay: hosts/laptop
apply:
  default_to_host: true
```

This setting defaults to false. Explicit names always take precedence, followed
by active context. `over apply hosts/laptop` works with either setting. The option
does not change any other command's targeting or host-data resolution.

Sync has an independent root `sync.default_to_host` option, also false by default.
Enable it to use the host for bare `over sync` when no active context exists.
Neither option affects unapply, commit, restore, or add. Both options require
literal booleans; strings and numbers are rejected.

### Migrate from `default_overlay`

The removed `default_overlay` key is ignored with a migration warning. Rename it
to `host_overlay`; enable `apply.default_to_host` separately only if you want
the host deployed by bare apply. Enable `sync.default_to_host` separately for
bare sync. Other mutations require active or explicit selection; use
`over sync --all` to select every overlay.

## Overlay discovery

Descriptors normally discover overlays. Use root `overlays` to add directories
without giving each one its own descriptor:

```yaml
# ~/.dotfiles/over.yaml
target: "~"
overlays:
  - 'apps/*'
  - hosts/laptop
```

`overlays` is a list of strings, each a literal path or glob pattern. In TOML:

```toml
overlays = ['apps/*', 'hosts/laptop']
```

Literal paths and globs are repository-root-relative. Only existing directories
count. Declared directories and descriptor discovery form a union. Discovery
prefers more-specific overlays over their ancestors, so an intermediate
descriptor can supply policy without appearing as a separate listed overlay.
An ancestor can still be selected explicitly. Descriptor-less overlays inherit
ancestor policy and built-in defaults just like other overlays.

`over lint` validates these root declarations, including when checking a named
overlay. Malformed globs and missing literal paths are errors; globs matching no
usable overlay directories are warnings. Files and `.over` administration paths
do not count as overlay matches. An optional unmatched glob can be retained
without failing lint. Diagnostics identify the descriptor and declaration entry
and suggest a fix. Discovery itself continues to skip invalid or unmatched
patterns.

## Lifecycle hooks

Hooks run repository-provided shell commands. Only initialize or apply
repositories you trust. Hook-enabled operations require exactly one valid,
plaintext root descriptor, even when overlay content is encrypted.

```yaml
# ~/.dotfiles/over.yaml
hooks:
  post-clone:
    - git-crypt unlock
  post-init:
    - ./scripts/ensure-local-setup
  install:
    - command: ./scripts/install-tools
      when: machine.os == "linux"
  post-apply:
    - ./scripts/render-secrets
```

Each entry can also be an object with a required `command` string and optional
`env` mapping, `cwd` string, `when` expression, and `sudo` boolean:

```yaml
hooks:
  post-apply:
    - ./scripts/setup --host {{ machine.hostname }}
    - command: ./deploy
      cwd: "{{ paths.over }}/scripts"
      when: machine.os == "linux"
      env:
        DEPLOYMENT_HOST: "{{ machine.hostname }}"
        DEPLOYMENT_LABEL: "{{ data.deployment.label }}"
```

The TOML equivalent uses a `[hooks]` table with event names mapped to arrays
of strings or inline tables. Missing hooks and empty lists do nothing. Unknown
events and object fields warn and are ignored; unknown events never execute.
Warnings omit unknown hook names and values to avoid exposing secrets. Ignoring
a misspelled optional field leaves the default behavior in effect: for example,
a misspelling of `when` does not set a condition, so the hook runs unconditionally.
Invalid recognized field types, missing commands, empty commands, and NUL bytes
remain errors. Environment names must be nonempty and contain neither `=` nor NUL.

### Templates and conditions

Commands (including shorthand), every `env` value, and `cwd` use the shared
MiniJinja environment immediately before execution. They expose machine facts,
the inherited process `env`, resolved `overlays`, and the shared `paths` object
(`home`, `over`, and `target`), alongside the execution context. The paths mean
the actual user's home, the source repository root, and the current overlay's
effective base destination, respectively. Install and post-apply hooks can
read their own local `data`, resolved `host`, and explicit dependency `overlays`.
Each participant sees its own `overlay` and scope, using the same loaded snapshot;
root install, post-install, and post-apply hooks see the selected overlay only when one distinct
root is selected. With multiple distinct roots they have empty `data`, the configured
host's overrides, and a namespace union of roots and dependencies, but no `overlay`
or `paths.target`; guard optional
target access with `paths.target is defined`.

During apply, `allowOverlays` is the ordered sequence of actual participant
objects, including selected roots and their complete transitive dependency closure.
Each object exposes its `name`, source `root`, local `data`, `host`, and configured
`target`, alongside the existing overlay fields. This shared snapshot is ready
before root install hooks run and remains available through root post-install.
Order is dependency-first, following selected-root and authored `uses` order;
overlapping dependencies appear once. With `--no-uses`, the sequence contains only
selected overlays, even though `overlays` retains metadata-only dependencies.

Init hooks have machine/environment/repository context but no selected overlay
and empty `data`, `host`, and `overlays`: bootstrap never discovers children before unlock hooks run.
Their `paths.target` is undefined; use `paths.target is defined` to guard access.
Host-overlay selection also uses shared context with target unavailable.
See [Targets and templates](overlay.md#targets-and-templates) for path availability
and serialization semantics.
Missing template variables and template/function failures fail the operation.
Hooks can resolve repository-local native secrets with `secret("github-token")`.
An ordered fallback expression such as
`secret("op://dev/github/token | github-token")` tries each candidate until one
succeeds. Provider references use `scheme://payload`; external providers are not
implemented yet, so those candidates are unavailable and allow fallback. Empty
or malformed candidates reject the entire expression before any lookup. Missing
secrets, unavailable providers, and a missing native unlock session allow
fallback. Decryption, corruption, and store errors stop it. The pipe is inside
the argument string, not a MiniJinja filter.

Native resolution uses the active source repository. Native stores require
the local repository key established by `over unlock`; hook lookup does not prompt
to unlock them. Hook secrets must be valid UTF-8. Secret access
is not available in overlay destination or sidecar path templates. Prefer
injecting secrets through `env`, for example:

```yaml
hooks:
  post-apply:
    - command: ./deploy --token "$TOKEN"
      env:
        TOKEN: "{{ secret('github-token') }}"
```

Over does not include resolved values in its own diagnostics or persist them in
caches. Hook programs inherit stdout/stderr and are responsible for their output.

`when` is a bare MiniJinja expression, without `{{ }}` delimiters. Boolean true
or a nonempty string runs the hook; false, none, and an empty string skip it.
The string `'false'` is nonempty and therefore runs. Numbers, collections,
undefined results, and expression errors fail with a diagnostic. Conditions
are evaluated first: skipped hooks do not render commands, environment values,
or cwd, resolve execution-only secrets, or spawn a process.

### Events and order

1. **`post-clone`**: root only, after a fresh clone, root validation, and
   canonical-origin setup, before child discovery. Declared `git` clones do not
   trigger it.
2. **`post-init`**: root only, on successful unlocked init, including existing
   checkouts, after any post-clone hooks. Neither init event applies overlays.
3. **Root `install`**: once per `over apply --install` (or `-i`), after graph,
   data, and target resolution, before any overlay deployment.
4. **Overlay `install`**: only with `--install`, after all materialization, Git
   provisioning, and Git-exclude reconciliation succeed. Overlay-local hooks run
   dependency-first in depth-first `uses` list order. Repeated dependencies run once.
5. **`post-apply`**: after reconciliation and any opted-in overlay install hooks
   complete, root once then overlays dependency-first.
6. **`post-install`**: root only, once with `--install`, after all overlays and
   post-apply hooks succeed.

Ordinary apply skips both root install phases and overlay install hooks entirely.
A successful no-change apply still executes applicable hooks; repeated install
runs have no package deduplication or persistent state.

Initialization can succeed while an encrypted checkout remains locked; both
bootstrap events are skipped in that case. Unlock and rerun init for post-init
hooks. Skipped post-clone hooks are not replayed on an existing checkout; recover
those steps explicitly. See [locked initialization](../commands/init.md).

Install hooks are arbitrary executable commands: applying an untrusted repository
with `--install` executes repository-provided code. Commands must provide their
own idempotency. See [Install hooks](../install-config.md) for examples and migration.

Commands within an event run in declaration order, each in a separate `sh -c`
shell. A POSIX-compatible `sh` must be on `PATH`, including on Windows. Commands
are rendered before shell parsing; template substitution is not shell escaping.
Quote environment variables used as paths. Prefer passing sensitive values via
`env` and referencing quoted shell variables rather than substituting them into
command text.

Hooks do not inherit. An intermediate descriptor's hooks run only when that
overlay itself participates. Overlay-local post-clone/post-init/post-install declarations
are accepted but ignored. `--no-uses` runs only root and selected-overlay hooks;
metadata-only overlays do not participate. Dry runs and read-only commands never
execute hooks. Nested Over commands inherit `OVER_HOOK_ACTIVE` and skip further
hook dispatch.

### Working directory and environment

By default, root hooks run in the source repository root; overlay hooks run in
their own source directory. Explicit `cwd` accepts an absolute path or a path
relative to the source repository root, even for overlay-local hooks. It must
resolve to an existing directory that can be entered. No shell or tilde
expansion is performed on cwd.

Ordinary hooks inherit the caller's environment and standard streams. Hook-specific
`env` values override inherited values, are passed directly to the process,
and do not affect later hooks. Templates see the inherited environment, not
other entries in the hook's `env` mapping. The lifecycle variables below are
reserved: attempting to override one is a configuration error.

An explicit `sudo: true` on a command hook uses the same sudo session as protected
target operations. It starts with sudo's environment, HOME, and PATH, plus the
hook's explicit `env` overrides and lifecycle variables. Conditions and templates
are still evaluated as the invoking user. Working directories, redirected input
and output, and interactive standard streams are preserved. Overlay/rule sudo
does not elevate an unmarked hook. `--no-prompt` controls sudo authentication;
the command may still request its own input.

| Variable | Availability and meaning |
| --- | --- |
| `OVER_HOOK_EVENT` | All hooks: event name |
| `OVER_REPOSITORY_ROOT` | All hooks: absolute repository source root |
| `OVER_HOOK_ACTIVE` | All hooks: `1`, suppressing nested dispatch |
| `OVER_TARGET_ROOT` | Apply hooks (including post-install): resolved absolute execution target root (`--target` or `OVER_TARGET`, otherwise the home directory) |
| `OVER_OVERLAY_NAME` | Apply hooks: repository-relative overlay name |
| `OVER_OVERLAY_ROOT` | Apply hooks: absolute overlay source directory |
| `OVER_OVERLAY_TARGET` | Apply hooks: absolute resolved base target |

For root apply hooks with one distinct selected root, overlay
variables identify that selected overlay. With multiple distinct roots, root
hooks remove all three `OVER_OVERLAY_*` variables, including inherited values;
`OVER_TARGET_ROOT` remains available. Overlay hooks identify that hook's own
overlay regardless of root selection. Init events remove the four apply-only
variables even if inherited from the caller.

### Failures and retries

The first failure stops subsequent hooks and fails the operation. Diagnostics
identify event, scope, and one-based command index without logging command text
or rendered cwd/environment values, even in debug/verbose mode. Preparation
errors identify the affected field without reproducing template source or
provider error details. Commands control their own output, which Over does not
redact. Numeric exit codes are preserved when representable as an 8-bit code;
spawn errors, signal termination, and unrepresentable codes return `1`.

Root install failure stops before overlay deployment. Failed reconciliation runs
no overlay install, post-apply, or root post-install hooks. Overlay install or
post-apply failure also suppresses root post-install, and a root post-install
failure fails the command. A hook failure leaves completed
filesystem changes and hook effects in place. Apply retries run hooks from the
beginning, so write retry-safe commands.

Hook commands are outside Over's managed-filesystem recovery guarantees.
Force/no-prompt flags do not suppress hooks or make child processes
non-interactive. Install scripts and package managers have their own effects
and prompt policies too; see [Safety and recovery](../usage/safety.md).

Init preserves a checkout after failure. Retrying an existing checkout runs only
post-init: recover a failed post-clone manually, or put repeatable setup in
post-init so `over init` can retry it.
