# Over Dev Container Feature

Install Over into the user's selected development image and optionally initialize
a repository and apply overlays as the lifecycle user. See the
[user guide](https://noirbizarre.github.io/over/devcontainer/) for configuration.

The Feature contains `bin/amd64/over` and `bin/arm64/over`, built by the existing
binary jobs. The installer selects and copies one; it does not download binaries
or extract a Docker image. The Feature reference selects the binary release.

`installsAfter` orders Over after common-utils or Git if they were selected. The
installer checks for the Git command and CA bundle, and installs prerequisites
only when missing. Prepared base images skip package management entirely.

`setup.sh` only adapts the repository, CSV overlay selection and explicit home
options to CLI calls. Defaults, initialization policy, errors and conflicts stay
with Over. Empty options install only the binary.

## Stage and test locally

With Docker, Git, Python 3.11+, Node.js and the mise-locked Dev Container CLI
available, stage both binaries for the checked-out version. For a published
release, for example:

```sh
tag=0.4.0
for arch in amd64 arm64; do
    mkdir -p "devcontainer/bin/$arch"
    gh release download "$tag" --repo noirbizarre/over \
        --pattern "over_${tag}_linux-${arch}.tar.gz" --dir /tmp/over-feature-assets --clobber
    tar -xzf "/tmp/over-feature-assets/over_${tag}_linux-${arch}.tar.gz" -C "devcontainer/bin/$arch"
done
chmod +x devcontainer/install.sh devcontainer/setup.sh
devcontainer features package ./devcontainer --output-folder /tmp/over-feature-package
mise run test:features
```

Unreleased testing uses the matching Linux build outputs at the same paths.
Staged binaries are ignored by Git. Missing inputs fail installation rather than
falling back to a download. The tests verify native installed-binary identity,
option boundaries, non-root lifecycle behavior, clone/retry, mounted sources and
conflict refusal. Containers and anonymous volumes are removed; Docker caches
remain. Set `DEVCONTAINER` to an absolute CLI path if needed.

## CI and publication

CI packages both same-run Linux artifacts once, then tests that payload on native
amd64 and arm64 runners. No candidate binary is mounted over another installation.
Public versioned releases instead stage the exact published release archives and
verify that the tag, metadata and installed binary versions match. Both tests must
succeed before the restored payload is published to `ghcr.io/noirbizarre/features/over`.

Docker image packaging is independent and consumes the same canonical builds.
Rolling development releases do not publish immutable Feature versions. Release
preparation updates the Feature metadata version with Cargo; there is no separate
binary-version default to synchronize.

Collection metadata remains at `ghcr.io/noirbizarre/features:latest`, separate from
the Over runtime image. These GHCR packages must be public and allow this
repository's Actions token to write them.
