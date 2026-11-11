# Root configuration

Use the repository-root descriptor for repository preferences and policy shared
by overlays. The default location is `~/.dotfiles/over.toml`; `over.yaml` and
`over.yml` are also supported. `--home` or `OVER_HOME` selects another repository.
Keep one descriptor per directory.

```yaml
# ~/.dotfiles/over.yaml
format: yaml
default_overlay: hosts/laptop
target: "~"
overlays:
  - path: apps/*
```

`format`, `repository`, `sync`, `default_overlay`, and `overlays` are root-only settings.
Root hooks are also repository-scoped. Ordinary [overlay fields](overlay.md),
such as `target`, `data`, `map`, and materialization rules, may appear here as
inherited policy; they are not additional repository preferences.

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

`format` controls what [`over new`](../usage/new.md) **writes**: `toml` (the
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

[`over init`](../usage/init.md) sets a fresh clone's `origin` without connecting
to the canonical remote. With no field, it preserves the cloned origin. Existing
checkouts require confirmation or `--set-origin` before changing origin.
Explicit origin push URLs are preserved. Init validates or clones a repository;
it does not create an empty project or apply overlays.

## Source synchronization

Opt into remote operations for [`over sync`](../usage/sync.md) in the root descriptor:

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

## Default overlay selection

`default_overlay` is a repository-relative overlay name. Use a fixed name first:

```toml
default_overlay = "hosts/laptop"
```

For a multi-host repository, `"hosts/{{ machine.hostname }}"` selects the current
host, provided that overlay actually exists. This template uses bootstrap machine
facts, before metadata composition; it cannot depend on the `data` it selects.

| Commands | Without an explicit name |
| --- | --- |
| `apply`, `unapply`, `commit`, `restore`, `data` | Infer context, then use the default, otherwise require an explicit overlay |
| `status`, `diff`, `sync` | Infer context, then use the default, otherwise all overlays; `--all` overrides selection |
| `add`, `git add`, `git mount` | Infer context, otherwise require an explicit overlay; no default fallback |
| `log` | Infer context, otherwise source-repository history; no default selection |
| `lint` | Check the repository; the default does not narrow it |
| `doctor` | Check all overlay configuration and state; a name only narrows Git-exclude diagnostics |

See [contextual overlay selection](../usage/selection.md) for ranked evidence,
Git associations, sibling files, and ambiguity errors. Ambiguity does not fall
through to the default.

The default also contributes the host [data context](overlay.md#consumer-metadata-data).
An explicit name or `--all` changes command selection, but retains that context.
The default's files and hooks are not processed just because its metadata is
included. An invalid or missing default is an error, including with an explicit
name or `--all`; Over never silently selects something else.

## Overlay discovery

Descriptors normally discover overlays. Use root `overlays` to add directories
without giving each one its own descriptor:

```yaml
# ~/.dotfiles/over.yaml
target: "~"
overlays:
  - path: apps/*
  - path: hosts/laptop
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
`env` mapping, `cwd` string, and `when` expression:

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
read the selected overlay's resolved `data`, including inherited and host data.
Each participant sees its own `overlay`, retaining the selected data snapshot;
root install and post-apply hooks see the selected overlay.

Init hooks have machine/environment/repository context but no selected overlay
or composed data: bootstrap never discovers children before unlock hooks run.
Their `paths.target` is undefined; use `paths.target is defined` to guard access.
Default-overlay selection also uses shared context with target unavailable.
See [Targets and templates](overlay.md#targets-and-templates) for path availability
and serialization semantics.
Missing template variables and template/function failures fail the operation.
Hooks can resolve repository-local native secrets with `secret("github-token")`.
An ordered fallback expression such as
`secret("op://dev/github/token | github-token")` tries each candidate until one
succeeds. Provider references use `scheme://payload`; external providers are not
implemented yet, so those candidates are unavailable and allow fallback. Empty
or malformed candidates reject the entire expression before any lookup. Missing
secrets and unavailable providers allow fallback; native unlock, decryption,
corruption and store errors stop it. The pipe is inside the argument string,
not a MiniJinja filter.

Native resolution uses the active source repository and shares the invocation's
lazy Age unlock. Set `OVER_SECRET` or allow interactive prompting; apply's
`--no-prompt` disables prompts. Hook secrets must be valid UTF-8. Secret access
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
2. **`post-init`**: root only, on every successful init, including existing
   checkouts, after any post-clone hooks. Neither init event applies overlays.
3. **`install`**: only with `over apply --install` (or `-i`), after all
   materialization, Git provisioning, and Git-exclude reconciliation succeed.
   Root hooks run once, then participating overlays' own hooks run dependency-first
   in depth-first `uses` list order. Repeated dependencies run once.
4. **`post-apply`**: after reconciliation and any opted-in install hooks complete,
   with the same root-first, dependency-first ordering. Ordinary apply skips
   install hooks entirely. A successful no-change apply still executes applicable
   hooks; repeated install runs have no package deduplication or persistent state.

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
overlay itself participates. Overlay-local post-clone/post-init declarations
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

Hooks inherit the caller's environment and standard streams. Hook-specific
`env` values override inherited values, are passed directly to the process,
and do not affect later hooks. Templates see the inherited environment, not
other entries in the hook's `env` mapping. The lifecycle variables below are
reserved: attempting to override one is a configuration error.

| Variable | Availability and meaning |
| --- | --- |
| `OVER_HOOK_EVENT` | All hooks: event name |
| `OVER_REPOSITORY_ROOT` | All hooks: absolute repository source root |
| `OVER_HOOK_ACTIVE` | All hooks: `1`, suppressing nested dispatch |
| `OVER_TARGET_ROOT` | Install and post-apply: resolved absolute execution target root (`--target` or `OVER_TARGET`, otherwise the home directory) |
| `OVER_OVERLAY_NAME` | Install and post-apply: repository-relative overlay name |
| `OVER_OVERLAY_ROOT` | Install and post-apply: absolute overlay source directory |
| `OVER_OVERLAY_TARGET` | Install and post-apply: absolute resolved base target |

For root install and post-apply hooks, overlay variables identify the selected overlay; for
overlay hooks, they identify that hook's overlay. Init events remove the four
apply-only variables even if inherited from the caller.

### Failures and retries

The first failure stops subsequent hooks and fails the operation. Diagnostics
identify event, scope, and one-based command index without logging command text
or rendered cwd/environment values, even in debug/verbose mode. Preparation
errors identify the affected field without reproducing template source or
provider error details. Commands control their own output, which Over does not
redact. Numeric exit codes are preserved when representable as an 8-bit code;
spawn errors, signal termination, and unrepresentable codes return `1`.

Failed reconciliation runs no install or post-apply hooks. A failed install hook
also suppresses post-apply. A hook failure leaves completed
filesystem changes and hook effects in place. Apply retries run hooks from the
beginning, so write retry-safe commands.

Hook commands are outside Over's managed-filesystem recovery guarantees.
Force/no-prompt flags do not suppress hooks or make child processes
non-interactive. Install scripts and package managers have their own effects
and prompt policies too; see [Safety and recovery](../usage/safety.md).

Init preserves a checkout after failure. Retrying an existing checkout runs only
post-init: recover a failed post-clone manually, or put repeatable setup in
post-init so `over init` can retry it.
