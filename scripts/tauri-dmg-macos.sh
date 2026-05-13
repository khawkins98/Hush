#!/usr/bin/env bash
#
# Build a release macOS DMG for local distribution and smoke-testing.
#
# Usage: npm run tauri:dmg
#
# The .dmg lands at:
#   src-tauri/target/release/bundle/dmg/Hush_<version>_aarch64.dmg
#
# Root-cause note — stale mounts from failed builds and repeated runs:
# Tauri's bundler (bundle_dmg.sh) creates a read-write intermediary DMG
# (rw.XXXXXX.Hush_*.dmg), runs Finder AppleScript to lay out the window,
# then converts it to the final read-only .dmg. If the build fails mid-way
# (e.g. the Finder script times out), the rw DMG is left mounted.
# On the next run hdiutil refuses to create a new volume named "Hush"
# because the name is already taken, and the build fails with:
#   "failed to run bundle_dmg.sh"
#
# Additionally, each successful run leaves a read-only /Volumes/Hush mount.
# On the next run the new DMG mounts as "Hush 1", then "Hush 2", etc.
# Running from any of these stale mounts means running a different binary
# with a different ad-hoc cdhash — TCC grants don't transfer between them.
# The fix is to detach ALL Hush volumes (both kinds) before starting.

set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "tauri:dmg is macOS-only" >&2
    exit 1
fi

# --- detach ALL stale Hush DMG mounts (build-time and user-facing) ----------
#
# Two kinds of stale Hush volumes accumulate across repeated tauri:dmg runs:
#
# 1. Build-time rw mounts (rw.XXXXXX.Hush_*.dmg) — left when the Tauri
#    bundler fails mid-flight.  hdiutil refuses to create a new "Hush"
#    volume while one is already mounted → "failed to run bundle_dmg.sh".
#
# 2. User-facing read-only mounts (/Volumes/Hush, /Volumes/Hush 1, …) —
#    left from previous `open "$DMG_PATH"` calls at the end of this script.
#    Each successive build mounts the new DMG with a deduplicated name
#    ("Hush 1", "Hush 2", …).  When the user tests by double-clicking
#    inside one of these stale volumes, they run a binary from a DIFFERENT
#    build with a DIFFERENT ad-hoc cdhash.  TCC grants are keyed to the
#    cdhash, so the grant from build N is invisible to build N+1 → this was
#    the root cause of "permissions don't stick after restart" during dev
#    iteration even after the quarantine and CGEventTap fixes were in place.

# 1. Eject build-time rw mounts.
stale=$(hdiutil info 2>/dev/null \
    | grep "image-path" \
    | grep -E "rw\.[0-9]+\.Hush" \
    | awk -F': ' '{print $2}' \
    | tr -d ' ' || true)

if [[ -n "$stale" ]]; then
    echo "[hush tauri:dmg] detaching stale Hush DMG mounts from previous failed builds…"
    while IFS= read -r img; do
        echo "  ejecting: $img"
        hdiutil detach "$img" -force 2>/dev/null || true
    done <<< "$stale"
fi

# 2. Eject user-facing read-only mounts (/Volumes/Hush, /Volumes/Hush 1, …).
# hdiutil detach accepts mount-point paths directly.
while IFS= read -r mountpoint; do
    echo "[hush tauri:dmg] ejecting old Hush DMG mount: $mountpoint"
    hdiutil detach "$mountpoint" -force 2>/dev/null || true
done < <(find /Volumes -maxdepth 1 -name 'Hush' -o -name 'Hush *' 2>/dev/null | sort)
# ---------------------------------------------------------------------------

echo "[hush tauri:dmg] building release DMG — this is slow (full release compile + frontend build)…"

# See tauri-bundle-macos.sh for why both deployment target vars are required.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
export CMAKE_OSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET}"

npx tauri build --bundles dmg,app

DMG_PATH=$(find src-tauri/target/release/bundle/dmg -name "*.dmg" -not -name "rw.*" | head -1)
if [[ -z "$DMG_PATH" ]]; then
    echo "[hush tauri:dmg] build succeeded but no .dmg found — check output above" >&2
    exit 1
fi

# Re-sign the loose .app on the regular filesystem first.
# Signing on a real filesystem (APFS) is more reliable than signing
# inside a mounted DMG. The signed .app is then swapped into the DMG
# by inject-dmg-readme.sh, replacing the unsigned original.
RELEASE_APP="src-tauri/target/release/bundle/macos/Hush.app"
if [[ -d "$RELEASE_APP" ]]; then
    echo "[hush tauri:dmg] re-signing release bundle (fixes TCC identifier)…"
    codesign --force --deep --sign - \
        --identifier "io.github.khawkins98.hush" \
        "$RELEASE_APP"
    echo "[hush tauri:dmg] signing identity: $(codesign -dv "$RELEASE_APP" 2>&1 | grep '^Identifier' || echo '(check above)')"
else
    echo "[hush tauri:dmg] WARNING: $RELEASE_APP not found — skipping re-sign" >&2
fi

echo "[hush tauri:dmg] injecting 'Read Me First.txt' into DMG and replacing .app with signed copy…"
bash "$(dirname "$0")/inject-dmg-readme.sh" "$DMG_PATH" "$RELEASE_APP"

echo "[hush tauri:dmg] DMG ready: $DMG_PATH"
# Open the DMG itself so Finder shows the drag-to-Applications installer window,
# NOT the staging directory.  The .app inside the staging directory is a build
# artifact and running it from there bypasses quarantine — TCC is unreliable for
# apps outside /Applications or ~/Applications.  Always test by dragging from the
# mounted DMG to ~/Applications.
open "$DMG_PATH"

echo ""
echo "[hush tauri:dmg] ✓ The DMG is now open in Finder — drag Hush.app to ~/Applications to install."
echo ""
echo "  To test as a first-time user:"
echo "    1. npm run dev-reset          (wipes TCC grants + app DB)"
echo "    2. Drag Hush.app → ~/Applications   (Finder sets quarantine)"
echo "    3. Open ~/Applications/Hush.app      (app strips quarantine, exec-restarts)"
echo "    4. Grant Microphone, then Input Monitoring in the wizard"
echo "    5. Quit and relaunch → both permissions should show ✓ (no re-grant needed)"
