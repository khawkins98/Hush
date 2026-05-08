#!/usr/bin/env bash
#
# Build a release macOS DMG for local distribution and smoke-testing.
#
# Usage: npm run tauri:dmg
#
# The .dmg lands at:
#   src-tauri/target/release/bundle/dmg/Hush_<version>_aarch64.dmg
#
# Root-cause note — stale mounts from failed builds:
# Tauri's bundler (bundle_dmg.sh) creates a read-write intermediary DMG
# (rw.XXXXXX.Hush_*.dmg), runs Finder AppleScript to lay out the window,
# then converts it to the final read-only .dmg. If the build fails mid-way
# (e.g. the Finder script times out), the rw DMG is left mounted.
# On the next run hdiutil refuses to create a new volume named "Hush"
# because the name is already taken, and the build fails with:
#   "failed to run bundle_dmg.sh"
# The fix is to detach any stale Hush volumes before starting.

set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "tauri:dmg is macOS-only" >&2
    exit 1
fi

# --- detach stale Hush DMG mounts left by previous failed builds -----------
# hdiutil info lists all mounted images; grep for rw.*.Hush* paths and eject.
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

# Re-sign the release .app bundle for the same reason as tauri-bundle-macos.sh:
# Tauri's unsigned build leaves a linker-signed binary with a hash-based
# identifier; re-signing binds Info.plist so TCC uses io.github.khawkins98.hush.
RELEASE_APP="src-tauri/target/release/bundle/macos/Hush.app"
if [[ -d "$RELEASE_APP" ]]; then
    echo "[hush tauri:dmg] re-signing release bundle to bind Info.plist…"
    codesign --force --deep --sign - "$RELEASE_APP"
fi

echo "[hush tauri:dmg] injecting 'Read Me First.txt' into DMG…"
bash "$(dirname "$0")/inject-dmg-readme.sh" "$DMG_PATH"

echo "[hush tauri:dmg] DMG ready: $DMG_PATH"
open "$(dirname "$DMG_PATH")"

echo ""
echo "[hush tauri:dmg] tip: to test the installer as a first-time user, run"
echo "  'npm run dev-reset' before mounting the DMG to start from a vanilla state."
