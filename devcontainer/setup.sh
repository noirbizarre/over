#!/usr/bin/env bash
set -euo pipefail

# Lifecycle commands run from the workspace, not beside the installed options.
# shellcheck source=/dev/null
source "$(dirname "${BASH_SOURCE[0]}")/options.sh"
if [ "${READONLY:-false}" = true ] && [ -n "$REPOSITORY" ]; then
    echo "The 'readOnly' option requires an existing checkout: omit 'repository' and mount the complete checkout at 'overHome'." >&2
    exit 1
fi
if [ -n "$OVERHOME" ]; then
    # Only explicit overrides need expansion; otherwise the CLI owns home selection.
    case "$OVERHOME" in
        \~) OVER_HOME="$HOME" ;;
        \~/*) OVER_HOME="$HOME/${OVERHOME#\~/}" ;;
        *) OVER_HOME="$OVERHOME" ;;
    esac
    export OVER_HOME
fi

overlays=()
if [ -n "$OVERLAY" ]; then
    remaining="$OVERLAY"
    while true; do
        name="${remaining%%,*}"
        # Feature CSV is an input adapter; the CLI takes literal separate arguments.
        name="${name#"${name%%[![:space:]]*}"}"
        name="${name%"${name##*[![:space:]]}"}"
        if [ -z "$name" ]; then
            echo "Invalid 'overlay' option: empty name at position $((${#overlays[@]} + 1))" >&2
            exit 1
        fi
        overlays+=("$name")
        [[ "$remaining" == *,* ]] || break
        # Keep a trailing empty element so it fails before init can run hooks.
        remaining="${remaining#*,}"
    done
fi

if [ -n "$REPOSITORY" ]; then
    over init -- "$REPOSITORY"
fi
if [ "${#overlays[@]}" -gt 0 ]; then
    apply_options=()
    if [ "${READONLY:-false}" = true ]; then
        apply_options+=(--read-only)
    fi
    # An unattended lifecycle must refuse conflicts instead of prompting.
    over apply "${apply_options[@]}" --no-prompt -- "${overlays[@]}"
fi
