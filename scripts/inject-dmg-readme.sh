#!/usr/bin/env bash
#
# inject-dmg-readme.sh — add "Read Me First.txt" to a Tauri-built DMG
#                         and set explicit Finder icon positions for all files.
#
# Usage: bash scripts/inject-dmg-readme.sh <path/to/Hush.dmg>
#
# The DMG must already exist (produced by `npx tauri build --bundles dmg`).
# This script:
#   1. Converts the read-only UDZO image to a writable UDRW copy.
#   2. Mounts the writable image at a standard /Volumes/<name> path.
#   3. Copies docs/dmg-readme.txt → "Read Me First.txt" inside the volume.
#   4. Writes the README icon position directly into .DS_Store using Python
#      (ds_store library), bypassing Finder entirely. Deliberately leaves all
#      other window settings (background, bounds, icon size, Hush.app /
#      Applications positions) intact — Tauri's bundler set those and we only
#      add the one new Iloc entry we need.
#   5. Detaches the volume via its device node (more reliable than detaching
#      by mount path, which Finder can briefly hold).
#   6. Converts back to a compressed UDZO image and replaces the original.
#   7. Cleans up all temp files on exit via trap.

set -euo pipefail

# ── argument validation ────────────────────────────────────────────────────
if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <path/to/Hush.dmg>" >&2
    exit 1
fi

DMG_INPUT="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"

if [[ ! -f "$DMG_INPUT" ]]; then
    echo "[inject-dmg-readme] ERROR: DMG not found: $DMG_INPUT" >&2
    exit 1
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
README_SRC="$REPO_ROOT/docs/dmg-readme.txt"

if [[ ! -f "$README_SRC" ]]; then
    echo "[inject-dmg-readme] ERROR: README source not found: $README_SRC" >&2
    exit 1
fi

# ── Python dependency check ───────────────────────────────────────────────
# `ds_store` (pip install ds_store) is required for the DS_Store Iloc write.
if ! python3 -c "import ds_store" 2>/dev/null; then
    echo "[inject-dmg-readme] installing ds_store Python package…"
    pip3 install --quiet ds_store --break-system-packages 2>/dev/null \
        || pip3 install --quiet ds_store 2>/dev/null \
        || { echo "[inject-dmg-readme] ERROR: could not install ds_store" >&2; exit 1; }
fi

# ── temp workspace + cleanup trap ─────────────────────────────────────────
TMPDIR_WORK="$(mktemp -d)"
RW_DMG="$TMPDIR_WORK/rw.dmg"
MOUNT_POINT=""   # populated after hdiutil attach (reads from plist)
DEVICE_NODE=""

cleanup() {
    if [[ -n "${DEVICE_NODE:-}" ]]; then
        hdiutil detach "$DEVICE_NODE" 2>/dev/null \
            || hdiutil detach "$DEVICE_NODE" -force 2>/dev/null \
            || true
    fi
    rm -rf "$TMPDIR_WORK"
}
trap cleanup EXIT

mkdir -p "$TMPDIR_WORK"

# ── convert to writable ────────────────────────────────────────────────────
# Convert UDZO → UDRW. The resulting UDRW has enough slack for the small
# README file; the old `hdiutil resize +16m` step was removed because
# hdiutil's relative-resize fails on converted images (exit 22) on
# current macOS, and the text file (~3 KB) fits without expansion.
echo "[inject-dmg-readme] converting to writable image…"
hdiutil convert "$DMG_INPUT" -format UDRW -o "$RW_DMG" -quiet

# ── mount ─────────────────────────────────────────────────────────────────
echo "[inject-dmg-readme] mounting…"
# Use -plist to get device/mount info reliably; avoid -mountpoint so the
# volume lands in the standard /Volumes hierarchy (Finder may briefly access
# it after mount — a non-/Volumes path can confuse disk arbitration).
ATTACH_OUT=$(hdiutil attach "$RW_DMG" \
    -readwrite -noverify -noautoopen \
    -plist 2>/dev/null)

# Extract device node AND the actual mount point from the plist.
PARSED=$(echo "$ATTACH_OUT" \
    | python3 -c "
import sys, plistlib
pl = plistlib.loads(sys.stdin.buffer.read())
for entry in pl.get('system-entities', []):
    if entry.get('mount-point'):
        print(entry['dev-entry'])
        print(entry['mount-point'])
        break
")
DEVICE_NODE=$(echo "$PARSED" | sed -n '1p')
MOUNT_POINT=$(echo "$PARSED" | sed -n '2p')

# ── copy README ────────────────────────────────────────────────────────────
echo "[inject-dmg-readme] adding 'Read Me First.txt'…"
cp "$README_SRC" "$MOUNT_POINT/Read Me First.txt"

# ── set Finder icon position via direct DS_Store write ────────────────────
# We write the README icon position directly into .DS_Store using Python, which
# is completely reliable (no Finder timing/flush issues). osascript was used
# previously but Finder's DS_Store writes are async and the position silently
# failed to persist before detach.
#
# The ds_store library's BookmarkCodec fails on Apple's newer bookmark format
# stored in pBBk entries; we patch decode() to return raw bytes so traversal
# can proceed to insert the Iloc record.
echo "[inject-dmg-readme] writing icon position to .DS_Store…"
python3 - "$MOUNT_POINT/.DS_Store" << 'PYEOF'
import sys
import os
import ds_store.store as _store

# Apple's newer bookmark format (pBBk entries) is not supported by mac_alias;
# patch the codec to pass through raw bytes instead of failing.
_store.BookmarkCodec.decode = staticmethod(lambda b: b)

import ds_store
path = sys.argv[1]
with ds_store.DSStore.open(path, 'r+') as d:
    # Move any hidden dot-files (e.g. .background) far off-screen so they
    # don't appear in the Finder window even when "Show Hidden Files" is on.
    vol_dir = os.path.dirname(path)
    for name in os.listdir(vol_dir):
        if name.startswith('.') and name not in ('.DS_Store',):
            try:
                d[name]['Iloc'] = (3000, 100)
            except Exception:
                pass

    # Position: centred inside the amber attention zone in dmg-background.svg (330, 390).
    # Coordinate system matches the DS_Store values Tauri wrote for app icons
    # (appPosition 165,185 / applicationFolderPosition 495,185) — same 1:1 mapping.
    d['Read Me First.txt']['Iloc'] = (330, 390)
PYEOF

# ── detach ────────────────────────────────────────────────────────────────
echo "[inject-dmg-readme] detaching…"
hdiutil detach "$DEVICE_NODE" -quiet
DEVICE_NODE=""  # prevent double-detach in cleanup

# ── convert back to compressed read-only ──────────────────────────────────
echo "[inject-dmg-readme] converting back to compressed read-only…"
NEW_DMG="${DMG_INPUT%.dmg}.tmp.dmg"
rm -f "$NEW_DMG"
hdiutil convert "$RW_DMG" -format UDZO -imagekey zlib-level=9 -o "$NEW_DMG" -quiet
mv "$NEW_DMG" "$DMG_INPUT"

echo "[inject-dmg-readme] done — 'Read Me First.txt' injected into $DMG_INPUT"
