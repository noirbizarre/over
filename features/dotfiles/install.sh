#!/usr/bin/env bash
set -euo pipefail

# Feature options (injected as env vars by devcontainer CLI)
OVERLAY="${OVERLAY:-}"
REPOSITORY="${REPOSITORY:-}"
OVERHOME="${OVERHOME:-"~/.over"}"
VERSION="${VERSION:-latest}"

OVER_IMAGE="ghcr.io/noirbizarre/over"
INSTALL_DIR="/usr/local/bin"
SETUP_DIR="/usr/local/share/over-dotfiles"

# ── Helpers ──────────────────────────────────────────────────────────────────

detect_arch() {
    local arch
    arch="$(uname -m)"
    case "${arch}" in
        x86_64 | amd64)  echo "amd64" ;;
        aarch64 | arm64) echo "arm64" ;;
        *)
            echo "Unsupported architecture: ${arch}" >&2
            exit 1
            ;;
    esac
}

install_crane() {
    local arch="$1"
    local crane_version="0.20.3"
    local crane_url="https://github.com/google/go-containerregistry/releases/download/v${crane_version}/go-containerregistry_Linux_${arch}.tar.gz"

    echo "Installing crane v${crane_version} (${arch})..."
    local tmp
    tmp="$(mktemp -d)"
    curl -fsSL "${crane_url}" | tar -xz -C "${tmp}" crane
    mv "${tmp}/crane" /usr/local/bin/crane
    chmod +x /usr/local/bin/crane
    rm -rf "${tmp}"
}

# ── Install over binary ─────────────────────────────────────────────────────

install_from_image() {
    local arch="$1"
    local tag="${VERSION}"
    local image="${OVER_IMAGE}:${tag}"

    echo "Extracting over binaries from ${image} (${arch})..."

    local tmp
    tmp="$(mktemp -d)"

    # Export the image filesystem and extract the binaries
    crane export --platform "linux/${arch}" "${image}" - \
        | tar -xf - -C "${tmp}" \
            usr/local/bin/over \
            usr/local/bin/git-over \
        2>/dev/null || true

    if [ -f "${tmp}/usr/local/bin/over" ]; then
        mv "${tmp}/usr/local/bin/over" "${INSTALL_DIR}/over"
        chmod +x "${INSTALL_DIR}/over"
        echo "Installed over to ${INSTALL_DIR}/over"
    else
        echo "ERROR: Failed to extract over binary from image" >&2
        rm -rf "${tmp}"
        return 1
    fi

    if [ -f "${tmp}/usr/local/bin/git-over" ]; then
        mv "${tmp}/usr/local/bin/git-over" "${INSTALL_DIR}/git-over"
        chmod +x "${INSTALL_DIR}/git-over"
        echo "Installed git-over to ${INSTALL_DIR}/git-over"
    fi

    rm -rf "${tmp}"
}

# ── Write the setup script (runs at postCreateCommand) ───────────────────────

write_setup_script() {
    mkdir -p "${SETUP_DIR}"

    cat > "${SETUP_DIR}/setup.sh" << 'SETUP_EOF'
#!/usr/bin/env bash
set -euo pipefail

# ── Configuration (baked in by install.sh) ───────────────────────────────────
SETUP_EOF

    # Append the baked-in configuration values
    cat >> "${SETUP_DIR}/setup.sh" << SETUP_EOF
OVERLAY="${OVERLAY}"
REPOSITORY="${REPOSITORY}"
OVERHOME="${OVERHOME}"
SETUP_EOF

    cat >> "${SETUP_DIR}/setup.sh" << 'SETUP_EOF'

# ── Resolve paths ───────────────────────────────────────────────────────────

resolve_home() {
    local path="$1"
    case "${path}" in
        "~/"*)  echo "${HOME}/${path#"~/"}" ;;
        "~")    echo "${HOME}" ;;
        *)      echo "${path}" ;;
    esac
}

OVER_HOME="$(resolve_home "${OVERHOME}")"
export OVER_HOME

# ── Clone dotfiles repository if needed ──────────────────────────────────────

if [ -n "${REPOSITORY}" ] && [ ! -d "${OVER_HOME}/.git" ]; then
    echo "Cloning dotfiles repository ${REPOSITORY} into ${OVER_HOME}..."
    git clone "${REPOSITORY}" "${OVER_HOME}"
elif [ -n "${REPOSITORY}" ] && [ -d "${OVER_HOME}/.git" ]; then
    echo "Dotfiles repository already exists at ${OVER_HOME}, pulling latest..."
    git -C "${OVER_HOME}" pull --ff-only 2>/dev/null || true
fi

# ── Apply overlay ────────────────────────────────────────────────────────────

if [ -n "${OVERLAY}" ]; then
    if [ ! -d "${OVER_HOME}" ]; then
        echo "WARNING: OVER_HOME (${OVER_HOME}) does not exist."
        echo "Provide a 'repository' option or mount your dotfiles to ${OVER_HOME}."
        exit 0
    fi

    echo "Applying overlay '${OVERLAY}' from ${OVER_HOME}..."
    over --home "${OVER_HOME}" apply "${OVERLAY}" --force
fi
SETUP_EOF

    chmod +x "${SETUP_DIR}/setup.sh"
    echo "Setup script written to ${SETUP_DIR}/setup.sh"
}

# ── Main ─────────────────────────────────────────────────────────────────────

main() {
    local arch
    arch="$(detect_arch)"

    # Install crane to extract binaries from the OCI image
    if ! command -v crane > /dev/null 2>&1; then
        install_crane "${arch}"
        local cleanup_crane=true
    fi

    # Install over from the published Docker image
    install_from_image "${arch}"

    # Clean up crane if we installed it (not needed at runtime)
    if [ "${cleanup_crane:-}" = "true" ]; then
        rm -f /usr/local/bin/crane
    fi

    # Write the postCreateCommand setup script
    write_setup_script

    echo "Over dotfiles feature installed successfully."
}

main "$@"
