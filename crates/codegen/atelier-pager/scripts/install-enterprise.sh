#!/usr/bin/env bash

set -euo pipefail

: "${ATELIER_RELEASE_BASE_URL:?ATELIER_RELEASE_BASE_URL must point to the Atelier release directory}"
export ATELIER_CHANNEL="${ATELIER_CHANNEL:-enterprise}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
INSTALLER="$SCRIPT_DIR/install.sh"
if [ -f "$INSTALLER" ]; then
    exec bash "$INSTALLER" "$@"
fi

INSTALLER_URL="${ATELIER_INSTALLER_URL:-${ATELIER_RELEASE_BASE_URL%/}/install.sh}"
TEMP_INSTALLER="$(mktemp "${TMPDIR:-/tmp}/atelier-install.XXXXXX.sh")"
trap 'rm -f "$TEMP_INSTALLER"' EXIT

if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$INSTALLER_URL" -o "$TEMP_INSTALLER"
elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$TEMP_INSTALLER" "$INSTALLER_URL"
else
    echo "Error: curl or wget is required to fetch $INSTALLER_URL" >&2
    exit 1
fi

bash "$TEMP_INSTALLER" "$@"
