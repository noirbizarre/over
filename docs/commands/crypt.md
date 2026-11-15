# `over crypt`

Manage versioned encryption policy and inspect protected Git storage. For setup,
manual attributes, and day-to-day workflows, see [Repository encryption](../usage/crypt.md).

```text
over crypt add <PATHS>...
over crypt remove <PATHS>... [--yes]
over crypt list
over crypt recover
```

## Repository selection and options

`-H`/`--home` or `OVER_HOME` selects a Git repository explicitly. Otherwise these
commands discover the current Git repository, falling back to the usual Over home
(`~/.dotfiles`). Policy add/remove can resolve owned virtual destinations to the
canonical source; mixed or ambiguous ownership is rejected.

All subcommands accept global `-d`/`--debug`, `-v`/`--verbose`, and `--help`.
There are no overlay/target options, `--dry-run`, or JSON output options.

## Add protection

`add` requires one or more source paths or Git-attributes patterns and an initialized,
unlocked repository key. Existing literal files are anchored and escaped;
directories select descendants with `/**`. Quote patterns so the shell does not
expand them first. Paths inside the source are relative to the invocation directory.

```sh
over crypt add .env 'secrets/**'
over crypt list
git diff --cached
git commit -m 'Protect repository files'
```

The command installs local filters and the diff driver, updates root
`.gitattributes`, cleans/restages selected existing content, verifies ciphertext,
and publishes policy/index changes together. Generated rules use
`filter=over diff=over -text`. It stages changes but does not commit or rewrite
history. Ignored untracked files are not implicitly selected. Bootstrap metadata
and incompatible attribute overrides are rejected.

For ingestion of destination files, use [`over add --crypt`](add.md) instead.

## Remove protection

`remove` accepts the same path/pattern model and requires unlock. It preserves
working content and **stages plaintext** with the removal policy. Interactive
confirmation defaults to no and requires terminal stdin and stderr; `--yes`
authorizes this deliberately in scripts.

```sh
over crypt remove public/example.conf --yes
git diff --cached
```

Generated removal rules clear filtering and return the diff attribute to its
unspecified state. Historical ciphertext and plaintext remain in history.

## List policy and storage diagnostics

**`over crypt list` is the encryption listing/status inspection command.** It is
separate from [`over git status`](over-git-status.md), which inspects overlay
association. There is no `over crypt status` or `over git crypt` command.

It prints human-readable findings to stdout, including policy lines, effective
attribute overrides, required filter and diff-driver configuration, textconv-cache
settings, index/HEAD ciphertext authentication, and known plaintext exposure.
For example:

```text
policy .gitattributes: "/.env" filter=over diff=over -text
.env index: ciphertext (authenticated)
.env HEAD: ciphertext (authentication unverified while locked)
```

These are illustrative findings from different unlock states. Working-tree
representation findings describe expectations rather than proving that every
working file was inspected. History checks are bounded to at most 16 commits and
256 path/blob checks; they are not a complete exposure audit.

The command does not stage or change policy. Reported error findings alone do
**not** produce a nonzero exit status; operational inspection failures do. Read
the findings rather than treating it as a strict pass/fail automation gate.

## Recover

`recover` rolls back an interrupted recorded encryption transaction subject to
current-state checks. It refuses to overwrite incompatible intervening edits;
it is not a generic decrypt or discard command.

```sh
over crypt recover
over crypt list
git diff --cached
```

See [recovery](../usage/crypt.md#recovery) for the complete procedure.
