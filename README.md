# over-rs

Git-based file overlays.

## Root Configuration

`over` supports a root configuration file at `~/.over/over.toml` (or `over.yaml`/`over.yml`). This file controls global preferences that apply across all overlays.

### Format Preference

The `format` field sets the default descriptor format used by `over new` when creating new overlays. Accepted values are `toml` (default) and `yaml`.

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

## Install Configuration

Define installation requirements per overlay under an `install` key in the overlay config (TOML/YAML). Supports system package managers and language-specific installers with optional pre/post script hooks.

Supported managers:
- System: `archlinux`, `apt`, `brew`
- Language: `cargo`, `python` (uv, pipx, pip), `node` (npm)

### Forms
Each manager accepts either a flat list (shorthand) or a full object with `packages`, `pre`, `post` (and manager-specific fields):

Flat (YAML):
```yaml
install:
  archlinux:
    - pkg1
    - pkg2
  apt:
    - curl
  brew:
    - jq
  cargo:
    - ripgrep
  python:
    - requests
  node:
    - typescript
```

Flat (TOML):
```toml
[install]
archlinux = ["pkg1", "pkg2"]
apt = ["curl"]
brew = ["jq"]
cargo = ["ripgrep"]
python = ["requests"]
node = ["typescript"]
```

Full (YAML):
```yaml
install:
  pre:
    - echo "setup"
  archlinux:
    packages: [pkg1, pkg2]
  apt:
    packages: [curl]
  brew:
    taps: [my/tap]
    packages:
      - name: jq
      - name: firefox
        cask: true
  cargo:
    packages:
      - name: ripgrep
        locked: true
      - git: https://github.com/sharkdp/fd
        tag: v9.0.0
  python:
    packages:
      - name: requests
        tool: uv
        extras: [security]
      - name: black
        tool: pipx
  node:
    packages:
      - name: typescript
        options: "--force"
  post:
    - echo "done"
```

Full (TOML):
```toml
[install]
pre = ['echo "setup"']
archlinux.packages = ["pkg1", "pkg2"]
apt.packages = ["curl"]
brew.taps = ["my/tap"]
brew.packages = [
  { name = "jq" },
  { name = "firefox", cask = true }
]
cargo.packages = [
  { name = "ripgrep", locked = true },
  { git = "https://github.com/sharkdp/fd", tag = "v9.0.0" }
]
python.packages = [
  { name = "requests", tool = "uv", extras = ["security"] },
  { name = "black", tool = "pipx" }
]
node.packages = [
  { name = "typescript", options = "--force" }
]
post = ['echo "done"']
```

### Brew Package Options
Brew packages can specify `options` (string split by whitespace) and `cask: true` to install via the cask tap. The `--cask` flag is automatically added when `cask: true` and de-duplicated if already present in `options`.

### Cargo Packages
Fields: `name`, `version`, `git`, `tag`, `branch`, `rev`, `path`, `features` (array), `locked` (bool), `options` (extra flags). Provide one of: name only, git + optional tag/branch/rev + optional name, or path.

### Python Packages
Fields: `name`, `tool` (one of `uv`, `pipx`, `pip` or omit for auto), `extras` (array), `options` (additional flags). Auto selection prefers `uv`, then `pipx`, then `pip` based on availability.

### Node Packages
Installed globally via `npm install -g`. Field: `name`, optional `options` appended to the command before the package name.

### Precedence & Execution Order
Order:
1. Global/Platform `pre` scripts
2. System managers (Linux precedence determined by distro; macOS: brew)
3. Language managers (`cargo`, `python`, `node`)
4. Platform `post` scripts
5. Global `post` scripts

Linux system manager precedence:
- Arch: archlinux, brew
- Debian/Ubuntu: apt, brew
- Other: archlinux, apt, brew (attempt those present)
If a platform section matching the distro exists, all listed managers in precedence order run; otherwise the first available top-level manager only.

### Platform Sections
Add distro/OS-specific overrides using keys inside `install` (e.g. `ubuntu`, `archlinux`, `macos`). These mirror top-level structure but apply only on that platform.

### Scripts
Each manager can define its own `pre` / `post` arrays executed immediately before/after that manager's install step.

### Windows
Windows installation logic is currently a placeholder and will be implemented later.

### Composition via Uses
Packages from overlays referenced in `uses` are merged (set union) to avoid duplicates across overlays.

---
This file documents installation configuration. Other functionality TBD.
