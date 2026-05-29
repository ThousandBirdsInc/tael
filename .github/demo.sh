#!/usr/bin/env bash
# Regenerate and replay the asciinema demo.
#
# For deterministic timing and a usable live-mode segment, the maintained demo
# is rendered by gen_demo.sh instead of being recorded directly from a TUI.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAST="$SCRIPT_DIR/demo.cast"

"$SCRIPT_DIR/gen_demo.sh"

if command -v asciinema >/dev/null 2>&1; then
    asciinema play "$CAST"
else
    echo "Wrote $CAST"
    echo "Install asciinema to play it locally: asciinema play $CAST"
fi
