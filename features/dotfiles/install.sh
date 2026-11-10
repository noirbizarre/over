#!/usr/bin/env bash
set -euo pipefail

OVERLAY="${OVERLAY:-}"
REPOSITORY="${REPOSITORY:-}"
OVERHOME="${OVERHOME:-~/.dotfiles}"
VERSION="${VERSION:-latest}"

SETUP_DIR=/usr/local/share/over-dotfiles
TEMP_DIR=""

cleanup() {
    if [ -n "${TEMP_DIR}" ]; then
        rm -rf "${TEMP_DIR}"
    fi
}
trap cleanup EXIT

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

[ "$(id -u)" = 0 ] || fail "Run this Feature installer as root."
[ "$(uname -s)" = Linux ] || fail "Only Linux is supported."

# The image binary requires glibc; installing tools cannot make Alpine compatible.
[ -f /etc/os-release ] || fail "Expected a Debian/Ubuntu base image."
# os-release also defines VERSION, which must not replace the Feature option.
# shellcheck disable=SC1091
distribution="$(source /etc/os-release; printf '%s' "${ID}")"
case "${distribution}" in
    debian | ubuntu) ;;
    *) fail "Unsupported distribution '${distribution}'; use Debian 12+ or Ubuntu 24.04+." ;;
esac

case "$(uname -m)" in
    x86_64 | amd64) arch=amd64; crane_arch=x86_64 ;;
    aarch64 | arm64) arch=arm64; crane_arch=arm64 ;;
    *) fail "Unsupported architecture: $(uname -m)" ;;
esac

# installsAfter only orders already-selected Features; it does not install Git.
if ! command -v curl >/dev/null || ! command -v git >/dev/null \
    || ! command -v tar >/dev/null || [ ! -s /etc/ssl/certs/ca-certificates.crt ]; then
    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl git tar
    rm -rf /var/lib/apt/lists/*
fi

TEMP_DIR="$(mktemp -d)"
if command -v crane >/dev/null; then
    crane_bin="$(command -v crane)"
else
    crane_version=0.20.3
    crane_url="https://github.com/google/go-containerregistry/releases/download/v${crane_version}/go-containerregistry_Linux_${crane_arch}.tar.gz"
    echo "Downloading crane v${crane_version} (${crane_arch})..."
    if ! curl -fsSL "${crane_url}" | tar -xz -C "${TEMP_DIR}" crane; then
        fail "Failed to download/extract crane from ${crane_url}."
    fi
    # Keep our crane private so failures cannot remove a pre-existing installation.
    crane_bin="${TEMP_DIR}/crane"
fi

image="ghcr.io/noirbizarre/over:${VERSION}"
echo "Installing Over from ${image} (linux/${arch})..."
if ! "${crane_bin}" export --platform "linux/${arch}" "${image}" "${TEMP_DIR}/image.tar"; then
    fail "Failed to extract Over from ${image} (linux/${arch}); check the version and registry access."
fi
# Finish the export before extraction so registry and archive failures stay distinct.
if ! tar -xf "${TEMP_DIR}/image.tar" -C "${TEMP_DIR}" usr/local/bin/over; then
    fail "Failed to extract Over from ${image} (linux/${arch}); check the version and registry access."
fi
install -m 0755 "${TEMP_DIR}/usr/local/bin/over" /usr/local/bin/over
/usr/local/bin/over --version || fail "Over from ${image} cannot run; use Debian 12+ or Ubuntu 24.04+."

mkdir -p "${SETUP_DIR}"
{
    printf '#!/usr/bin/env bash\nset -euo pipefail\n'
    # Options are data, never shell expressions evaluated by the lifecycle user.
    printf 'OVERLAY=%q\nREPOSITORY=%q\nOVERHOME=%q\n' "${OVERLAY}" "${REPOSITORY}" "${OVERHOME}"
} > "${SETUP_DIR}/options.sh"
chmod 0644 "${SETUP_DIR}/options.sh"
install -m 0755 "$(dirname "${BASH_SOURCE[0]}")/setup.sh" "${SETUP_DIR}/setup.sh"
echo "Over dotfiles Feature installed successfully."
