#!/usr/bin/env bash
#
# inject-dmg-readme.sh — add "Read Me First.txt" to a Tauri-built DMG.
#
# Usage: bash scripts/inject-dmg-readme.sh <path/to/Hush.dmg>
#
# The DMG must already exist (produced by `npx tauri build --bundles dmg`).
# This script:
#   1. Converts the read-only UDZO image to a writable UDRW copy.
#   2. Expands the writable image by 16 MB so the copy doesn't run out
#      of space (Tauri sizes DMGs tightly around the app bundle).
#   3. Mounts the writable image at a temp mount point.
#   4. Copies docs/dmg-readme.txt → "Read Me First.txt" inside the volume.
#   5. Detaches the volume via its device node (more reliable than
#      detaching by mount path, which Finder can briefly hold).
#   6. Converts back to a compressed UDZO image and replaces the original.
#   7. Cleans up all temp files on exit — even if a step fails — via trap.

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
MOUNT_POINT="$TMPDIR_WORK/mnt"
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

mkdir -p "$MOUNT_POINT"

# ── convert to writable ────────────────────────────────────────────────────
echo "[inject-dmg-readme] converting to writable image…"
hdiutil convert "$DMG_INPUT" -format UDRW -o "$RW_DMG" -quiet

# Expand by 16 MB so the copy has room (Tauri sizes DMGs tightly).
hdiutil resize -size +16m "$RW_DMG" -quiet

# ── mount ─────────────────────────────────────────────────────────────────
echo "[inject-dmg-readme] mounting…"
# -nobrowse: keeps the volume out of Finder's sidebar / Spotlight indexer
#            so macOS doesn't briefly hold the volume open on us.
ATTACH_OUT=$(hdiutil attach "$RW_DMG" \
    -readwrite -noverify -noautoopen -nobrowse \
    -mountpoint "$MOUNT_POINT" \
    -plist 2>/dev/null)

# Extract the device node (e.g. /dev/disk3) from the plist so we can
# detach by node rather than by mount path — this is more reliable when
# Finder/Spotlight has a brief hold on the volume.
DEVICE_NODE=$(echo "$ATTACH_OUT" \
    | python3 -c "
import sys, plistlib
pl = plistlib.loads(sys.stdin.buffer.read())
for entry in pl.get('system-entities', []):
    if entry.get('mount-point'):
        print(entry['dev-entry'])
        break
")

# ── copy README ────────────────────────────────────────────────────────────
echo "[inject-dmg-readme] adding 'Read Me First.txt'…"
cp "$README_SRC" "$MOUNT_POINT/Read Me First.txt"

# ── detach ────────────────────────────────────────────────────────────────
echo "[inject-dmg-readme] detaching…"
hdiutil detach "$DEVICE_NODE" -quiet
DEVICE_NODE=""  # prevent double-detach in cleanup

# ── convert back to compressed read-only ──────────────────────────────────
echo "[inject-dmg-readme] converting back to compressed read-only…"
NEW_DMG="${DMG_INPUT%.dmg}.tmp.dmg"
rm -f "$NEW_DMG"  # remove any stale temp from a previous interrupted run
hdiutil convert "$RW_DMG" -format UDZO -imagekey zlib-level=9 -o "$NEW_DMG" -quiet
mv "$NEW_DMG" "$DMG_INPUT"

echo "[inject-dmg-readme] done — 'Read Me First.txt' injected into $DMG_INPUT"
