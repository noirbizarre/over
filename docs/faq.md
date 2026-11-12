# FAQ

New to Over? Start with what it manages, why it exists, and how it fits your workflow.

??? question "What is Over?"

    Over is a Git-backed manager for **composable working environments**. Its
    “dotfiles” can include configuration files, directories, and the Git
    repositories that make a workspace ready to use—not just files in your home
    directory. An **overlay** groups one concern; a complete workstation can
    combine several overlays or simply use one.

    Composition has been central to the design from the beginning. The aim is to
    bring files, repository setup, development-environment configuration, and
    protection into one reusable workflow, reducing the separate scripts and
    manual steps needed to prepare a machine. This is a focused approach to
    environment provisioning, rather than a complete operating-system manager.

    Over **materializes** source content at configured targets, using links or
    ordinary files from virtual checkouts. See the [examples](examples.md) for
    setups ranging from one overlay to a multi-destination workspace.

??? question "How do I get started?"

    Follow [Getting started](index.md#getting-started): install Over, create a
    small Git-backed overlay in temporary directories, preview with `--dry-run`,
    apply, and inspect the result. It demonstrates the complete first workflow
    without changing your existing dotfiles.

    Already have an Over repository? [`over init`](usage/init.md) clones or
    validates it; then apply the overlay you want. Initialization and application
    are separate steps. You can start with [one flat overlay](examples.md#one-flat-overlay)
    and introduce composition when you need shared building blocks.

??? question "Why was Over created?"

    I started Over to manage my own dotfiles, and used it as a personal tool for
    years. I originally used GNU Stow, but reliability and packaging problems made
    that setup difficult to maintain. In particular, it depended on a Perl version
    that was no longer available in my packaging environment. Chezmoi did not
    exist yet, so I started writing my own dotfiles manager in Python.

    The project evolved through several substantial iterations: Python, then Go,
    and eventually Rust. These were evolutions of the approach, not just language
    ports or a predetermined roadmap. Portability was one reason I chose Rust: I
    wanted to carry an executable on a USB drive and prepare a working environment
    without first setting up a language environment. Later, colleagues noticed
    the tool, liked the approach, and encouraged me to share it.

    For me, dotfiles have always included the files and repositories that make a
    working environment usable. I wanted reusable compositions instead of a
    growing collection of unrelated tools, scripts, and manual setup steps. I
    have needed the same workflow on Linux and macOS, including Macs at work;
    Windows also belongs in that cross-platform goal. I prefer a focused,
    incremental tool over committing my setup to an ecosystem such as Nix. That
    is a personal design trade-off, not a judgement that Nix is technically worse.

    Over predates the current wave of AI-assisted development. AI did not create
    the original need or the project, but it has significantly accelerated recent
    development and made capabilities practical that would otherwise have taken
    me much longer to implement.

## Alternatives { #alternatives }

??? question "How is Over different from other tools?"

    The main distinction is the combination: **composable overlays, workspace
    repositories as first-class declarations, Git-backed materialization, and
    integrated protection**. Other tools overlap with individual capabilities;
    they do not all address the same scope. In particular, Home Manager is an
    adjacent user-environment manager, not just another dotfiles installer.

    **Checked 12 November 2026.** Over's column describes the implementation in
    this documentation's source revision. Repository encryption/native secrets
    and the revised Dev Container Feature are implemented **after release 0.1.0**,
    the latest published release at this check. They are not promises about what
    installing 0.1.0 provides. Alternative columns describe their current upstream
    implementations; the sources below identify the relevant documentation.

    **Status:** 🟢 supported and available in the current implementation/version;
    ❌ explicitly unsupported in the stated scope; 🚧 planned but not available.
    Markers describe support, not quality. A cell naming an **external** script,
    plugin, or module means that integration is available, not built into the
    core. Descriptive rows identify each tool's supported model and requirements.

    | Dimension | Over | [GNU Stow](https://www.gnu.org/software/stow/) | [chezmoi](https://www.chezmoi.io/) | [yadm](https://yadm.io/) | [Nix Home Manager](https://nix-community.github.io/home-manager/) | [Dotbot](https://github.com/anishathalye/dotbot) | [dotdrop](https://dotdrop.readthedocs.io/en/latest/) |
    | --- | --- | --- | --- | --- | --- | --- | --- |
    | Scope and source model | 🟢 Git-backed workspace overlays | 🟢 Package trees, including dotfiles | 🟢 Dotfile source/target state | 🟢 One Git repository over live dotfiles | 🟢 Declarative user files, programs, services | 🟢 VCS-agnostic bootstrap tasks | 🟢 Dotfiles and host profiles |
    | Reuse across machines/roles | 🟢 `uses` graph and descriptor inheritance | 🟢 Select multiple packages | 🟢 Templates, data, external sources | 🟢 Host/class alternatives | 🟢 Nix module imports and options | 🟢 Task files and conditional links | 🟢 Profiles and shared dotfiles |
    | Workspace repositories | 🟢 Declared clones/worktrees | ❌ No repository operations | 🟢 `git-repo` externals | 🟢 Git submodules; bootstrap scripts | 🟢 External Git commands in activation scripts | 🟢 External Git commands in shell tasks | 🟢 External Git commands in actions |
    | Dev Container configuration/setup | 🟢 Config files plus install/apply Feature | 🟢 Config-file links | 🟢 Config files and documented container bootstrap | 🟢 Tracked config files; bootstrap scripts | 🟢 Generated config files; external container tooling | 🟢 Config links; shell setup | 🟢 Config files; actions |
    | File deployment | 🟢 File/directory links, virtual checkouts, managed blocks | 🟢 Symlink farm | 🟢 Ordinary/generated files and symlinks | 🟢 Git working-tree files; alternative links | 🟢 Links to Nix-store files | 🟢 Symbolic/hard links | 🟢 Copies/generated files and links |
    | Permission handling | 🟢 Scoped modes and metadata preservation; see below | 🟢 Source permissions through links | 🟢 Private/executable file attributes | 🟢 Restrict private/decrypted files | 🟢 Executability; external secret modules for private files | 🟢 Directory creation modes | 🟢 `chmod`/preserve policy |
    | Encryption/secret handling | 🟢 Repository encryption, access keys, native secrets (post-0.1.0) | ❌ No encryption facility | 🟢 Age/GPG and password-manager integrations | 🟢 Encrypted archive via external GPG/OpenSSL | 🟢 External module, e.g. sops-nix | 🟢 External plugins, e.g. dotbot-age | 🟢 External GPG via transformations |
    | File-content generation | 🚧 File templates ([#61](https://github.com/noirbizarre/over/issues/61)); path/hook templates available | ❌ No rendering | 🟢 Go templates | 🟢 Built-in templates; optional external processors | 🟢 Nix expressions and format generators | ❌ Core directives do not render files | 🟢 Jinja2 templates |
    | Platforms/runtime model | 🟢 Linux/macOS/Windows binary; Git and hook tools as needed | 🟢 Perl and filesystem symlinks | 🟢 Linux/macOS/Windows binary; integrations may need tools | 🟢 Shell/Git; Linux/macOS, Windows via compatible environment | 🟢 Nix ecosystem on Linux/macOS; WSL for Windows | 🟢 Python; Unix and Windows with symlink privileges | 🟢 Python/dependencies; Linux/macOS/WSL |

    **What those differences mean:**

    - **Composition and Git:** an Over workstation is a composition of overlays,
      each with its own targets. The source repository versions the setup; an
      overlay need not be a separate repository. Declared workspace repositories
      are independent Git repositories, not virtual checkouts of the overlay.
      Chezmoi also provisions external repositories; scripts/submodules provide
      other approaches. Over brings these declarations into its overlay model.
    - **Materialization and system files:** links expose source content; virtual
      checkouts put ordinary files at the destination, without a target `.git`.
      That matters for locations rejecting symlinks and files needed before home
      is mounted. See the system-target answer below for the permission boundary.
    - **Containers:** Over can deploy `devcontainer.json` and related files as
      ordinary content. Its [Dev Container Feature](devcontainer.md) installs Over
      and optionally initializes/applies an overlay at container post-create.
      This Feature supports specified Debian/Ubuntu Linux images on amd64/arm64.
      It does not semantically merge container JSON or build/start/attach
      containers; external Dev Container tooling handles that lifecycle. The
      table's configuration-file support for other tools does not imply native
      container orchestration either.
    - **Protection:** Over's [cryptography](usage/crypt.md) and
      [native secrets](usage/secrets.md) share repository access management.
      They protect selected content, not filenames or all Git metadata. Locking
      does not erase deployed plaintext or copies. External secret-provider
      execution and [hardware recipients](https://github.com/noirbizarre/over/issues/246)
      are not available yet. Integrations in other tools can be a better fit for
      an existing password-manager workflow.
    - **Portability and commitment:** Over aims to reuse a workspace workflow on
      existing operating systems. Home Manager brings Nix's package and module
      ecosystem, reproducible generations, and deeper user-environment management.
      It can run standalone on an existing Linux/macOS installation; adopting it
      does **not** require replacing that OS with NixOS. Choosing that ecosystem
      is a different commitment from adopting Over's focused overlay workflow.

    Choose Stow for a simple symlink farm; chezmoi for mature file rendering and
    secret integrations; yadm for a direct Git-over-home workflow; Dotbot for a
    small bootstrap task list; dotdrop for profiles and templated variants; or
    Home Manager for declarative programs and user configuration in Nix. Over is
    useful when reusable overlays should assemble the files **and repositories**
    of a working environment with a choice of materialization strategies.

    **Sources and maintenance:**

    - [Stow's manual](https://www.gnu.org/software/stow/manual/stow.html) defines
      its symlink-only operations and Perl implementation. It remains here as
      the historical starting point for Over, independent of maintenance status.
    - Chezmoi documents [externals](https://www.chezmoi.io/user-guide/include-files-from-elsewhere/),
      [file types](https://www.chezmoi.io/user-guide/manage-different-types-of-file/),
      [encryption](https://www.chezmoi.io/user-guide/encryption/), and
      [container setup](https://www.chezmoi.io/user-guide/machines/containers-and-vms/).
      [v2.74.1](https://github.com/twpayne/chezmoi/releases/tag/v2.74.1) was released
      on 10 November 2026.
    - Yadm's [overview](https://yadm.io/docs/overview),
      [templates](https://yadm.io/docs/templates),
      [encryption](https://yadm.io/docs/encryption),
      [installation](https://yadm.io/docs/install), and
      [manual](https://github.com/yadm-dev/yadm/blob/master/yadm.md) explain its Git-first model.
      Its development branch last changed in March 2025, but
      [maintainer participation on 26 October 2026](https://github.com/yadm-dev/yadm/issues/548)
      provides current maintenance evidence beyond repository push timestamps.
    - Home Manager's [introduction](https://nix-community.github.io/home-manager/introduction.html),
      [standalone installation](https://nix-community.github.io/home-manager/installation/standalone.html),
      and [file/activation options](https://nix-community.github.io/home-manager/options/home-manager/home.html)
      describe its scope; [Nix's supported platforms](https://nix.dev/manual/nix/stable/installation/supported-platforms.html)
      define the runtime boundary. [sops-nix](https://github.com/Mic92/sops-nix#use-with-home-manager)
      documents its external secrets module. Home Manager had
      [upstream commits](https://github.com/nix-community/home-manager/commits/master/)
      on 10 November 2026.
    - Dotbot's [upstream guide](https://github.com/anishathalye/dotbot#readme)
      documents its complete core directive set, Windows setup, and external
      plugins. Its [upstream history](https://github.com/anishathalye/dotbot/commits/master/)
      includes changes on 31 October 2026.
    - Dotdrop documents [file modes and deployment](https://dotdrop.readthedocs.io/en/latest/config/config-dotfiles/),
      [secret transformations](https://dotdrop.readthedocs.io/en/latest/howto/sensitive-dotfiles/),
      and [runtime dependencies](https://dotdrop.readthedocs.io/en/latest/installation/).
      [v1.18.1](https://github.com/deadc0de6/dotdrop/releases/tag/v1.18.1) was
      released on 6 November 2026.

## Concepts and workflow

??? question "What are the core concepts?"

    - **Source repository:** the Git-backed home of your setup, normally
      `~/.dotfiles`. Its [root descriptor](configuration/root.md) supplies defaults.
    - **Overlay:** a source directory containing files, discovered through a local
      [overlay descriptor](configuration/overlay.md) or a root `overlays` declaration.
      Its local descriptor is optional; inherited configuration controls where and
      how its content appears. It groups a reusable concern.
    - **Composition:** `uses` includes other overlays transitively, dependency-first,
      once per shared dependency. Each keeps its own configuration and target.
      [Inheritance](configuration/overlay.md#fields-and-inheritance) supplies
      descriptor defaults; [composition](configuration/overlay.md#composition)
      includes content. Composition is not a last-wins override stack: conflicting
      ownership of a target file must be resolved.
    - **Workspace/workstation/profile:** useful names for your working directories
      or complete machine/role setup, not extra object types. For example, a host
      overlay can compose common, development, and machine-specific overlays.
    - **Target and materialization:** a target is a destination directory;
      [materialization](configuration/materialization.md) determines whether
      content appears through links, a virtual checkout, or a complementary
      mechanism such as managed blocks or declared repositories.

    Conceptually, after creating overlays named `common`, `development`, and
    `machines/laptop`, a `hosts/laptop/over.toml` could select them:

    ```toml
    uses = ["common", "development", "machines/laptop"]
    ```

    These are example names, not reserved categories. You would apply
    `hosts/laptop`; each dependency would contribute its own files and repository
    declarations. A second workstation could reuse `common` and `development`.
    See [Compose a host](examples.md#compose-a-host) for a complete working example
    and [several destinations](examples.md#one-workspace-several-destinations)
    for workspace routing. A single overlay remains enough for a simpler setup.

??? question "Can Over manage system files or files needed before home is mounted?"

    Yes, targets can be outside your home directory. A **virtual checkout** writes
    ordinary files, so a file physically deployed to the root filesystem can be
    read before home is mounted without following a symlink into `~/.dotfiles`.
    This also helps with software that rejects symlinks. Choose an appropriate
    target and checkout directory; Git records content and executability, not
    arbitrary ownership or full permission modes.

    There are two distinct capabilities here: checkout removes dependence on a
    source symlink, while [permission policy](configuration/overlay.md#file-permissions)
    controls **partial-file and legacy decrypted-file targets**. That policy does
    not independently chmod virtual-checkout files, and Over has no general
    declarative owner/group assignment. Existing metadata is preserved within
    the [documented platform limits](usage/safety.md#recovery-limitations).
    Arrange required ownership/restrictive checkout permissions separately and
    run with appropriate privileges; Over does not elevate itself.

    The target filesystem must actually be available when the file is needed.
    Over needs its executable, source, and state when provisioning/updating;
    reading an already deployed ordinary file does not. This is file deployment,
    not boot-service or initramfs orchestration.

??? question "Does Over replace Git or other tools in my workflow?"

    Git remains the version-control system. Over uses Git repositories as
    first-class inputs and can [provision workspace repositories/worktrees](configuration/overlay.md#git-declarations),
    but their ongoing development remains ordinary Git. Apply is not a bulk pull
    of those repositories. The [Git command group](usage/git.md) integrates overlay
    files with host repositories, while [checkout recording and sync](configuration/materialization.md#editing-and-lifecycle)
    reconcile edits in Over's virtual checkouts.

    Over is distributed as a native Rust executable: you do not need an installed
    Python, Go, or Rust runtime to run it. The removable-media goal does not make
    every workflow dependency-free. It embeds libgit2 **and** invokes the Git
    executable for operations including diff, checkout filtering, encryption
    policy, and optional source pull/push. Hooks require a POSIX-compatible `sh`
    and whatever tools they invoke; container tools and package managers keep
    their own responsibilities.

    [Release binaries](https://github.com/noirbizarre/over/releases/latest) exist
    for Linux, macOS, and Windows (amd64 MSVC for Windows). Windows symlinks still
    require native permissions/support, Unix permission policy is not Windows
    ACL policy, and hooks still require `sh`. Cross-platform reuse means sharing
    the model with platform-appropriate content and commands, not identical OS
    behavior. See [installation options](https://github.com/noirbizarre/over#installation)
    and [hook requirements](configuration/root.md#lifecycle-hooks) for details.
