<p align="center">
  <img src="docs/images/logo-text.svg" alt="over" width="640">
</p>

<p align="center"><strong>A git-based file overlay manager (dotfiles as overlays)</strong></p>

<p align="center">
  <a href="https://github.com/noirbizarre/over/actions/workflows/ci.yaml">
    <img src="https://github.com/noirbizarre/over/actions/workflows/ci.yaml/badge.svg" alt="CI">
  </a>
  <a href="https://codecov.io/gh/noirbizarre/over">
    <img src="https://codecov.io/gh/noirbizarre/over/graph/badge.svg" alt="Codecov">
  </a>
  <a href="https://crates.io/crates/dot-over">
    <img src="https://img.shields.io/crates/v/dot-over" alt="crates.io">
  </a>
  <img src="https://img.shields.io/github/v/release/noirbizarre/over" alt="Release">
  <a href="https://noirbizarre.github.io/over/">
    <img src="https://img.shields.io/badge/docs-noirbizarre.github.io-blue" alt="Documentation">
  </a>
  <img src="https://img.shields.io/github/license/noirbizarre/over" alt="License">
</p>

---

Over is a git-based file overlay manager that lets you define file overlays in Git repositories, with support for nested references and installation requirements. It is particularly well-suited for managing dotfiles.
It is inspired by tools like GNU Stow and Chezmoi but focuses on a Git-centric workflow with flexible configuration and installation capabilities.

## Installation

```bash
cargo install dot-over
```

Or download a binary for your platform from the
[latest release](https://github.com/noirbizarre/over/releases/latest).

## Usage

```bash
over --help
```

### Commands

| Command | Description |
|---------|-------------|
| `over add` | Add files or directories to an overlay |
| `over new` | Create a new overlay |
| `over list` (`ls`) | List known overlays |
| `over show` | Display details about an overlay |
| `over apply` | Apply a given overlay |
| `over lint` | Check overlays for configuration issues |
| `over completion` | Generate shell completion scripts |
| `over status` | Get the current repository/directory overlays status |
| `over diff` | Show differences between desired and actual overlay state |
| `over sync` | Reconcile a checkout-materialized overlay with its git source |
| `over unapply` | Remove a given overlay's own entries from the target |

A companion binary `git-over` integrates with git workflows:

| Command | Description |
|---------|-------------|
| `git over mount` | Mount the current git repository to an overlay |
| `git over add` | Add files from the current git repository to an overlay |
| `git over status` | Show overlay status for the current git repository |

See the [documentation](https://noirbizarre.github.io/over/) for the full
command reference, configuration format, and install-manager reference
(package managers, precedence rules, platform sections).

## Documentation

<https://noirbizarre.github.io/over/>

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT — see [LICENSE](LICENSE).
