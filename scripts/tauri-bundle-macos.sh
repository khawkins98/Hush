#!/usr/bin/env bash
#
# THE canonical way to build Hush for TCC / permission testing.
#
# Builds a debug .app, re-signs it so TCC uses the stable bundle ID
# (io.github.khawkins98.hush), and installs it to ~/Applications/Hush.app —
# a standard macOS app location that macOS TCC treats identically to
# /Applications. This gives the same permission reliability as a DMG
# install without requiring a full release compile.
#
# Workflow:
#   npm run dev-reset    ← wipe all Hush state (optional, for clean-slate testing)
#   npm run tauri:bundle ← build, sign, install to ~/Applications, and launch
#
# For hot UI/Rust iteration use `npm run tauri dev` instead — it's much faster
# but cannot test TCC permissions reliably (see learnings.md for why).
# For a distributable release DMG use `npm run tauri:dmg`.

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

# Re-sign with ad-hoc codesign to bind Info.plist and fix the identifier.
#
# Tauri's debug build leaves a linker-signed binary whose code-signing
# identifier is a binary-hash like `hush-44ac88ddc8db2594`, NOT the bundle ID
# `io.github.khawkins98.hush`. TCC keys permission entries to this identifier,
# so `tccutil reset io.github.khawkins98.hush` becomes a no-op and grants
# appear to vanish on the next rebuild. Forcing a fresh ad-hoc signature after
# the bundle is assembled corrects the identifier and binds Info.plist.
echo "[hush tauri:bundle] re-signing bundle to bind Info.plist (fixes TCC identifier)…"
codesign --force --deep --sign - "$APP_PATH"

# Install to ~/Applications/ so TCC grants are reliable.
#
# macOS TCC is significantly more cooperative with apps in standard locations
# (/Applications, ~/Applications) than with paths inside a dev build tree.
# Running directly from target/debug/bundle/macos/Hush.app — a non-standard
# path — causes TCC to sometimes accept the grant dialog but then silently
# reject the permission check at runtime. ~/Applications is a standard per-user
# app directory that TCC treats identically to /Applications, with no sudo
# required to write there. This mirrors the reliability of the DMG workflow
# (which installs to /Applications) without requiring a full release build.
DEV_INSTALL="$HOME/Applications/Hush.app"
echo "[hush tauri:bundle] installing to ~/Applications/Hush.app (reliable TCC path)…"
pkill -f "Hush.app/Contents/MacOS/hush" 2>/dev/null || true
sleep 1
mkdir -p "$HOME/Applications"
rm -rf "$DEV_INSTALL"
cp -R "$APP_PATH" "$DEV_INSTALL"

echo "[hush tauri:bundle] opening $DEV_INSTALL"
open "$DEV_INSTALL"

echo ""
echo "[hush tauri:bundle] tip: to test fresh onboarding or first-run permission prompts,"
echo "  run 'npm run dev-reset' before launching the bundle to start from a vanilla state."
