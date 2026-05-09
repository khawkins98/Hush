#!/usr/bin/env bash
#
# generate-dmg-background.sh
#
# Rasterises src-tauri/assets/dmg-background.svg to a 580×340 PNG
# (1:1 with the DMG window — Finder renders backgrounds at 1 px = 1 logical pt).
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

rsvg-convert -w 660 -h 500 "$SVG" -o "$PNG"
echo "Generated: $PNG  ($(du -sh "$PNG" | cut -f1))"
