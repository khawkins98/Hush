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
# `tauri build` honours `bundle.active = true` in tauri.conf.json by
# default, so we don't pass --no-bundle / --bundles. --debug skips
# release-profile optimisations that would push the build past the
# 2 min mark.
npx tauri build --debug

APP_PATH="src-tauri/target/debug/bundle/macos/Hush.app"
if [[ ! -d "$APP_PATH" ]]; then
    echo "[hush tauri:bundle] expected bundle at $APP_PATH but it doesn't exist; check the build output above" >&2
    exit 1
fi

echo "[hush tauri:bundle] opening $APP_PATH"
open "$APP_PATH"
