# `over crypt`

Manage versioned encryption policy and inspect protected Git storage. For setup,
manual attributes, and day-to-day workflows, see [Repository encryption](../usage/crypt.md).
Git-invoked `over crypt filter`, `filter-process`, and `textconv` commands are
documented in [Encryption filters and textconv](crypt-filters.md).

```text
over crypt add <PATHS>...
over crypt remove <PATHS>... [--yes]
over crypt list
over crypt recover
over crypt migrate
```

## Repository selection and options

`-H`/`--home` or `OVER_HOME` selects a Git repository explicitly. Otherwise these
commands discover the current Git repository, falling back to the usual Over home
(`~/.dotfiles`). Policy add/remove can resolve owned virtual destinations to the
canonical source; mixed or ambiguous ownership is rejected.

All subcommands accept global `-d`/`--debug`, `-v`/`--verbose`, and `--help`.
There are no overlay/target options, `--dry-run`, or JSON output options.

The Git-invoked helpers use Git's repository environment and current directory
instead of this policy-command selection; `--home` does not redirect them.

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

## Migrate legacy encryption

`migrate` converts legacy Age files and native secrets to repository-key encryption.
It takes no additional arguments and requires an initialized, unlocked repository
key, plus access to decrypt the legacy Age inputs. Run it from the source repository
or select that repository with `--home`.

```sh
over crypt migrate
over crypt list
git diff --cached --stat
git diff --cached
git commit -m 'Migrate legacy encryption'
```

The command processes tracked `.age` files and entries in the legacy
`.over/secrets/` directory. Ordinary files migrate to the same path with the `.age`
extension removed; native secrets migrate to `.over/secrets/<name>`, preserving
binary values. All inputs must be real regular files. Tracked legacy files must
match their index snapshots, and incompatible destination content or unrecognized
legacy-store entries must be resolved before migration.

Legacy `.over/crypt/access.json` or nested split `.over/crypt/repository.json`
metadata migrate first into `.over/crypt.json` and individual
`.over/users/<user>/<key>` records, with an isolated configurable `chore(crypt)`
commit. The user-existence record is named `user.json`, replacing `.user` in the
nested layout. Split-record bytes and encryption generations remain unchanged;
bootstrap attributes are upgraded in the same commit.
Native secret migration then creates an isolated `chore(secrets): migrated store`
commit, honoring Git hooks and signing. It also accepts an existing
`.over/secrets` or `.over/secrets.yaml` aggregate. Ordinary `.age` files are processed afterward;
their ciphertext, policy, and legacy removals are staged for explicit review and
commit. A later ordinary-file migration failure does not undo a successful native
secret migration commit. Old history is unchanged.

Original ordinary-file Age ciphertext is preserved under
`over-crypt/legacy-backup/` in Git's administrative directory, retaining its
repository-relative paths. Native directory migration instead retains the
original file/directory under `over-crypt/legacy-state-backup-*`. These local backups
are not committed. Native secret mutations also migrate historical stores
automatically; read-only secret commands do not migrate.
