# Over Dev Container Feature

Install Over and optionally initialize a repository and apply an overlay as the
container's lifecycle user. See the [Dev Containers guide](https://noirbizarre.github.io/over/devcontainer/)
for clone/mount examples, options, supported platforms and retry behavior.

The Feature is published at `ghcr.io/noirbizarre/features/over`. Its version and
default binary version match the Over release. Use `:0` for the current major,
or an exact published release for reproducibility; `:1` applies once Over reaches
1.x. The `version` option can explicitly select another binary image tag.

## Test locally

From the repository root, with Docker, Git, Python 3.11+, Node.js and network
access available:

```sh
mise install devcontainer-cli
mise run test:features
devcontainer features package ./devcontainer --output-folder /tmp/over-feature
```

Mise locks the CLI version in `mise.lock`. Set `DEVCONTAINER` to an absolute CLI
path if necessary. If the checked-in release image predates the Feature's CLI
behavior, or release preparation names an unpublished image, use
`OVER_FEATURE_TEST_VERSION=latest mise run test:features`; explicit scenario
options still take precedence. Without this override the suite checks that the
default binary matches the Feature version.

The suite stages this directory as a local `over` Feature and runs real
`devcontainer up`/`exec` commands against minimal Debian images, using a non-root
lifecycle user and deterministic Git fixtures. It exercises binary downloads,
clone/mount behavior, retries, literal options, conflicts and expected failures.
Containers, anonymous volumes and temporary workspaces are removed; Docker's
image/build caches remain.

## Release publication

Release preparation updates Cargo, Feature metadata and the default binary
version together. After the release's amd64/arm64 binary image manifest is
published, CI tests the matching default on both architectures, then uses the
same mise-locked CLI to publish this single Feature directory. Exact Feature
versions are immutable; changes ship with the next Over release through gh-ship.

Collection metadata lives at `ghcr.io/noirbizarre/features:latest`, separate from
the binary image's `ghcr.io/noirbizarre/over:latest`. The new GHCR Feature and
collection packages must be public, and the `noirbizarre/over` repository must
have Actions write access to them. New GHCR packages may need owner configuration
after their first publication.

## Migration

Replace `ghcr.io/noirbizarre/over/features/dotfiles:1` or
`ghcr.io/noirbizarre/over/dotfiles:1` with `ghcr.io/noirbizarre/features/over:0` and
rebuild. The old `:1` was independent of Over's release version; old references
are not redirected. Lifecycle helpers now live in `/usr/local/share/over/`.
