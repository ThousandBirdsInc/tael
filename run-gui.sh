#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/tael-gui"

# Install frontend deps on first run (or after they're cleared).
if [ ! -d node_modules ]; then
  npm install
fi

exec npm run tauri dev -- "$@"
