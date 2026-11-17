#!/usr/bin/env bash
# Build Pitwall.app for this Mac and install it, so pitwall opens from the Dock
# and Spotlight like any app. Same as `make install`.
# Usage: scripts/macos-app.sh [install-dir]   (default: /Applications)
set -euo pipefail
cd "$(dirname "$0")/.."
exec make install DEST="${1:-/Applications}"
