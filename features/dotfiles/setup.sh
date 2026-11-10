#!/usr/bin/env bash
set -euo pipefail

# Resolve beside the installed script rather than the lifecycle workspace cwd.
# shellcheck source=/dev/null
source "$(dirname "${BASH_SOURCE[0]}")/options.sh"
: "${OVERHOME:?Missing installed overHome option}"

case "${OVERHOME}" in
    \~) OVER_HOME="${HOME}" ;;
    \~/*) OVER_HOME="${HOME}/${OVERHOME#\~/}" ;;
    *) OVER_HOME="${OVERHOME}" ;;
esac
export OVER_HOME

if [ -n "${REPOSITORY}" ]; then
    echo "Initializing Over repository at ${OVER_HOME} from ${REPOSITORY}..."
    # Init owns clone/reuse policy and preserves existing checkouts without pulling.
    if ! over --home "${OVER_HOME}" init -- "${REPOSITORY}"; then
        echo "ERROR: Could not initialize ${OVER_HOME}; check the repository source and checkout, then rerun $0." >&2
        exit 1
    fi
fi

if [ -n "${OVERLAY}" ]; then
    if [ ! -d "${OVER_HOME}" ]; then
        echo "ERROR: OVER_HOME (${OVER_HOME}) does not exist; supply 'repository' or mount your dotfiles at 'overHome'." >&2
        exit 1
    fi
    echo "Applying overlay '${OVERLAY}' from ${OVER_HOME}..."
    # Lifecycle commands cannot ask for consent; no-prompt refuses unresolved conflicts.
    if ! over --home "${OVER_HOME}" apply --no-prompt -- "${OVERLAY}"; then
        echo "ERROR: Could not apply '${OVERLAY}'. Check the overlay name and resolve any conflicts with the Over CLI, then rerun $0. Existing conflicting files are not forcibly replaced." >&2
        exit 1
    fi
fi
