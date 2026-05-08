#!/usr/bin/env bash
#
# generate-dmg-background.sh
#
# Rasterises src-tauri/assets/dmg-background.svg to a 1320×800 PNG
# (2× Retina for a 660×400 DMG window).
#
# Requires: rsvg-convert  (brew install librsvg)
# Usage: bash scripts/generate-dmg-background.sh

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SVG="$REPO_ROOT/src-tauri/assets/dmg-background.svg"
PNG="$REPO_ROOT/src-tauri/assets/dmg-background.png"

if ! command -v rsvg-convert &>/dev/null; then
    echo "rsvg-convert not found — install with: brew install librsvg" >&2
    exit 1
fi

rsvg-convert -w 1320 -h 800 "$SVG" -o "$PNG"
echo "Generated: $PNG  ($(du -sh "$PNG" | cut -f1))"
