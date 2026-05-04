#!/usr/bin/env bash
#
# Build a macOS `.app` bundle and open it. Smoke-testing tool for
# anything that depends on macOS treating Hush as a "real" app —
# specifically the Screen Recording / Microphone TCC prompts that
# only fire reliably on a code-signed `.app` bundle, not on the
# bare `target/debug/hush` binary `npm run tauri dev` produces.
#
# This is **not** a hot-iteration workflow. The bundle build is
# 30 s – 2 min depending on cache state, and the resulting `.app`
# is a snapshot — Rust changes need a fresh `npm run tauri:bundle`
# to take effect, and the frontend is loaded from the static
# `frontendDist` (not the dev server), so a Svelte change also
# needs a re-bundle. Use `npm run tauri dev` for the iteration
# loop; reach for this only when verifying TCC behaviour, code-
# signing, dock-icon UX, or anything else that the dev binary
# can't represent faithfully. Background on the dev-binary TCC
# limitation lives in `learnings.md`.

set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "tauri:bundle is macOS-only — Hush's TCC quirk doesn't apply on other platforms" >&2
    exit 1
fi

echo "[hush tauri:bundle] building debug .app — this is slow (full link + frontend build)…"

# whisper-rs-sys (and ggml inside it) use std::filesystem APIs that require
# macOS 10.15+. Without a deployment target set, cmake defaults to an older
# baseline and the build fails with "'exists' is unavailable: introduced in
# macOS 10.15" errors. Both vars are required: MACOSX_DEPLOYMENT_TARGET is
# read by cargo/rustc; CMAKE_OSX_DEPLOYMENT_TARGET is read by cmake-rs.
# 14.0 matches the release workflow (release.yml "Set macOS clang flags" step).
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
export CMAKE_OSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET}"

# --bundles app: produce only the .app bundle, not the .dmg.
# For TCC smoke-testing we only need the .app; DMG is a release artifact
# produced by `npm run tauri:dmg`. --debug skips release-profile optimisations.
npx tauri build --debug --bundles app

APP_PATH="src-tauri/target/debug/bundle/macos/Hush.app"
if [[ ! -d "$APP_PATH" ]]; then
    echo "[hush tauri:bundle] expected bundle at $APP_PATH but it doesn't exist; check the build output above" >&2
    exit 1
fi

echo "[hush tauri:bundle] opening $APP_PATH"
open "$APP_PATH"

echo ""
echo "[hush tauri:bundle] tip: to test fresh onboarding or first-run permission prompts,"
echo "  run 'npm run dev-reset' before launching the bundle to start from a vanilla state."
