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

`format`, `repository`, `default_overlay`, and `overlays` are root-only settings.
Root hooks are also repository-scoped. Ordinary [overlay fields](overlay.md),
such as `target`, `data`, `map`, and materialization rules, may appear here as
inherited policy; they are not additional repository preferences.

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
| `apply`, `unapply`, `commit` | Use the default, otherwise prompt |
| `status`, `diff`, `sync` | Use the default, otherwise all overlays; `--all` overrides selection |
| `data` | Use the default, otherwise fail without prompting |
| `lint` | Check the repository; the default does not narrow it |
| `doctor` | Check all overlay configuration and state; a name only narrows Git-exclude diagnostics |

The default also contributes the host [data context](overlay.md#consumer-metadata-data).
An explicit name or `--all` changes command selection, but retains that context.
The default's files and packages are not processed just because its metadata is
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
  post-apply:
    - ./scripts/render-secrets
```

The TOML equivalent uses a `[hooks]` table with event names mapped to string
arrays. Missing hooks and empty lists do nothing. Unknown events, non-lists,
non-string or empty commands, and NUL bytes are errors.

### Events and order

1. **`post-clone`**: root only, after a fresh clone, root validation, and
   canonical-origin setup, before child discovery. Declared `git` clones do not
   trigger it.
2. **`post-init`**: root only, on every successful init, including existing
   checkouts, after any post-clone hooks. Neither init event applies overlays.
3. **`post-apply`**: after all materialization and Git-exclude reconciliation
   succeed. Root hooks run once, then participating overlays' own hooks run
   dependency-first in depth-first `uses` list order. Repeated dependencies run
   once. A successful no-change apply still executes hooks.

Commands within an event run in declaration order, each in a separate `sh -c`
shell. A POSIX-compatible `sh` must be on `PATH`, including on Windows. Commands
are literal, not templates; quote environment variables used as paths.

Hooks do not inherit. An intermediate descriptor's hooks run only when that
overlay itself participates. Overlay-local post-clone/post-init declarations
are accepted but ignored. `--no-uses` runs only root and selected-overlay hooks;
metadata-only overlays do not participate. Dry runs and read-only commands never
execute hooks. Nested Over commands inherit `OVER_HOOK_ACTIVE` and skip further
hook dispatch.

### Working directory and environment

Root hooks run in the source repository root; overlay hooks run in their own
source directory. They inherit the caller's environment and standard streams.

| Variable | Availability and meaning |
| --- | --- |
| `OVER_HOOK_EVENT` | All hooks: event name |
| `OVER_REPOSITORY_ROOT` | All hooks: absolute repository source root |
| `OVER_HOOK_ACTIVE` | All hooks: `1`, suppressing nested dispatch |
| `OVER_TARGET_ROOT` | Post-apply: absolute execution target root (`--root`) |
| `OVER_OVERLAY_NAME` | Post-apply: repository-relative overlay name |
| `OVER_OVERLAY_ROOT` | Post-apply: absolute overlay source directory |
| `OVER_OVERLAY_TARGET` | Post-apply: absolute resolved base target |

For root post-apply hooks, overlay variables identify the selected overlay; for
overlay hooks, they identify that hook's overlay. Init events remove the four
apply-only variables even if inherited from the caller.

### Failures and retries

The first failure stops subsequent hooks and fails the operation. Diagnostics
identify event, scope, and one-based command index without logging command text
or environment values, even in debug/verbose mode. Commands control their own
output. Numeric exit codes are preserved when representable as an 8-bit code;
spawn errors, signal termination, and unrepresentable codes return `1`.

Failed materialization runs no post-apply hooks. A hook failure leaves completed
filesystem changes and hook effects in place. Apply retries run hooks from the
beginning, so write retry-safe commands.

Init preserves a checkout after failure. Retrying an existing checkout runs only
post-init: recover a failed post-clone manually, or put repeatable setup in
post-init so `over init` can retry it.
