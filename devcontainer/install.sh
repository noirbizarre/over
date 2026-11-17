#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
# Bundled GNU binaries require a compatible base, not another download method.
# shellcheck disable=SC1091
source /etc/os-release
case "$ID" in
    debian | ubuntu) ;;
    *) echo "Use a Debian 12+ or Ubuntu 24.04+ base." >&2; exit 1 ;;
esac

# Soft ordering does not install prerequisites, but an already prepared base needs no apt run.
if ! command -v git >/dev/null || [ ! -s /etc/ssl/certs/ca-certificates.crt ]; then
    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends git ca-certificates
    rm -rf /var/lib/apt/lists/*
fi
install -m 0755 "bin/$(dpkg --print-architecture)/over" /usr/local/bin/over
over --version

mkdir -p /usr/local/share/over
# Build-time options must remain literal data when read by the lifecycle user.
printf 'OVERLAY=%q\nREPOSITORY=%q\nOVERHOME=%q\n' \
    "${OVERLAY:-}" "${REPOSITORY:-}" "${OVERHOME:-}" > /usr/local/share/over/options.sh
chmod 0644 /usr/local/share/over/options.sh
install -m 0755 setup.sh /usr/local/share/over/setup.sh
