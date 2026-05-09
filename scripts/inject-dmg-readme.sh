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
#   2. Expands the writable image by 16 MB (Tauri sizes DMGs tightly).
#   3. Mounts the writable image (visible to Finder so AppleScript can reach it).
#   4. Copies docs/dmg-readme.txt → "Read Me First.txt" inside the volume.
#   5. Runs AppleScript via osascript to position "Read Me First.txt" inside
#      the already-configured Finder window. Deliberately leaves all other
#      window settings (background, bounds, icon size, Hush.app / Applications
#      positions) intact — Tauri's bundler set those in the original .DS_Store
#      and re-running "set current view" would reset the icon view options,
#      erasing the background image reference.
#   6. Detaches the volume via its device node (more reliable than detaching
#      by mount path, which Finder can briefly hold).
#   7. Converts back to a compressed UDZO image and replaces the original.
#   8. Cleans up all temp files on exit via trap.

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
# Do NOT use -mountpoint: mounting at a non-/Volumes path prevents Finder
# from recognising the disk by name, so osascript `tell disk "…"` silently
# fails and icon positions (including "Read Me First.txt") are never written.
# Let hdiutil choose the standard /Volumes/<name> mount point instead.
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

# Get the volume name Finder knows this disk by.
VOLUME_NAME=$(diskutil info "$DEVICE_NODE" \
    | awk -F: '/Volume Name/{gsub(/^ +| +$/,"",$2); print $2}')

# ── copy README ────────────────────────────────────────────────────────────
echo "[inject-dmg-readme] adding 'Read Me First.txt'…"
cp "$README_SRC" "$MOUNT_POINT/Read Me First.txt"

# ── set Finder icon positions via osascript ────────────────────────────────
# This positions the three user-visible files and pushes hidden macOS bookkeeping
# files (e.g. .VolumeIcon.icns, .background folder) far off-screen so developers
# who have "Show Hidden Files" enabled don't see them in awkward spots.
echo "[inject-dmg-readme] setting icon positions…"
osascript << APPLESCRIPT || echo "[inject-dmg-readme] osascript warning (non-fatal): $?"
tell application "Finder"
  tell disk "$VOLUME_NAME"
    open
    -- Do NOT touch view/background/bounds/icon-size here.
    -- Tauri's bundler already wrote those to .DS_Store and calling
    -- "set current view" would reset the icon view options, erasing
    -- the background image reference. Only add what we need.
    set position of item "Read Me First.txt" to {330, 305}
    update without registering applications
    delay 2
    close
  end tell
end tell
APPLESCRIPT

# Give the Finder / disk-arbitration layer a moment to flush .DS_Store.
sleep 2

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
