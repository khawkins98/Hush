#!/usr/bin/env bash
# Full Hush dev reset — restores the machine to vanilla "first-ever-install" state.
#
# By default, transcription and meeting history is preserved so you keep your
# recordings between dev cycles.  Use --nuke-db to also wipe the database (full
# wipe, equivalent to pre-2026-06 behaviour).
#
# Usage:
#   npm run dev-reset                        # reset for the current (or sudo-originating) user
#   npm run dev-reset -- --user alice        # reset for a specific macOS user account
#   npm run dev-reset -- --nuke-models       # also delete downloaded models (~GB)
#   npm run dev-reset -- --nuke-db           # also wipe transcription + meeting history
#   npm run dev-reset -- --user alice --nuke-models
#
# What gets removed (default):
#   macOS TCC permissions (ScreenCapture, Microphone, ListenEvent, Accessibility)
#   settings / dictionary terms / text replacements rows (inside hush.db)
#   <home>/Library/Preferences/io.github.khawkins98.hush.plist             (NSUserDefaults)
#   <home>/Library/Caches/io.github.khawkins98.hush/                       (WebKit etc.)
#   <home>/Library/Caches/hush/
#   autostart LaunchAgent (if enabled via Settings → Launch at Login)
#   Legacy com.khawkins.hush data/TCC/prefs (from before PR #526 bundle rename)
#
# What is PRESERVED by default (to keep your dev recordings):
#   transcription history
#   meeting sessions + utterances
#
# With --nuke-db the entire database file is deleted (no history preserved).
#
# Models are kept by default because they are large and slow to re-download.
# Pass --nuke-models to wipe them too.
#
# Target user resolution (highest priority first):
#   1. --user <name>   explicit override
#   2. $SUDO_USER      when this script is run via `sudo npm run dev-reset`
#   3. current user    default (id -un)
#
# This is macOS-only (the app itself is macOS-only).

set -euo pipefail

BUNDLE_ID="io.github.khawkins98.hush"
# Legacy bundle ID from before the rename (PR #526). TCC entries, prefs, and
# app-support data keyed to this ID linger after an upgrade and must be purged.
LEGACY_BUNDLE_ID="com.khawkins.hush"
nuke_models=0
nuke_db=0
explicit_user=""

# ── Argument parsing ──────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --nuke-models)
      nuke_models=1
      shift
      ;;
    --nuke-db)
      nuke_db=1
      shift
      ;;
    --user)
      if [[ -z "${2:-}" ]]; then
        echo "[dev-reset] --user requires a username argument" >&2
        exit 1
      fi
      explicit_user="$2"
      shift 2
      ;;
    --help|-h)
      awk 'NR>1 && /^[^#]/{exit} NR>1{sub(/^# ?/,""); print}' "$0"
      exit 0
      ;;
    *)
      echo "[dev-reset] unknown argument: $1 (try --help)" >&2
      exit 1
      ;;
  esac
done

if [ "$(uname -s)" != "Darwin" ]; then
  echo "[dev-reset] This script is macOS-only. Exiting."
  exit 1
fi

# ── Resolve target user and home directory ────────────────────────────────────
# Priority: --user flag > $SUDO_USER (running via sudo) > current user.
if [[ -n "$explicit_user" ]]; then
  TARGET_USER="$explicit_user"
elif [[ -n "${SUDO_USER:-}" ]]; then
  TARGET_USER="$SUDO_USER"
else
  TARGET_USER="$(id -un)"
fi

# Derive home directory via dscl (reliable even for accounts with non-standard
# home paths) with a /Users/<name> fallback.
TARGET_HOME="$(dscl . -read "/Users/$TARGET_USER" NFSHomeDirectory 2>/dev/null \
  | awk '{print $2}')" \
  || TARGET_HOME="/Users/$TARGET_USER"

if [[ -z "$TARGET_HOME" || "$TARGET_HOME" == "/" ]]; then
  echo "[dev-reset] could not determine home directory for user '$TARGET_USER'" >&2
  exit 1
fi

TARGET_UID="$(id -u "$TARGET_USER" 2>/dev/null)" || {
  echo "[dev-reset] user '$TARGET_USER' not found on this system" >&2
  exit 1
}

echo "[dev-reset] targeting user: $TARGET_USER (uid=$TARGET_UID, home=$TARGET_HOME)"

APP_SUPPORT="$TARGET_HOME/Library/Application Support/$BUNDLE_ID"

# ── 1. Kill any running Hush process ─────────────────────────────────────────
echo "[dev-reset] killing any running Hush processes..."
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
bash "$SCRIPT_DIR/dev-cleanup.sh" 2>/dev/null || true

# ── 2. TCC permissions ────────────────────────────────────────────────────────
echo "[dev-reset] resetting TCC permissions for $BUNDLE_ID..."
# When running as root targeting a different user, route tccutil through
# `launchctl asuser <uid>` so it modifies that user's TCC database.
# Otherwise call tccutil directly.
_tccutil() {
  if [[ "$(id -u)" -eq 0 && "$TARGET_USER" != "$(id -un 2>/dev/null || true)" ]]; then
    launchctl asuser "$TARGET_UID" tccutil "$@" 2>/dev/null
  else
    tccutil "$@" 2>/dev/null
  fi
}

_tccutil reset ScreenCapture "$BUNDLE_ID" && echo "  ScreenCapture cleared" || true
_tccutil reset Microphone    "$BUNDLE_ID" && echo "  Microphone cleared"    || true
# ListenEvent: a bundle-ID-scoped reset misses TCC entries that macOS 26
# records under a quarantine-context identity for ad-hoc signed apps
# copied from a DMG (see learnings.md 2026-05-13). A nuclear reset (no
# bundle ID) clears all Input Monitoring grants for the user, ensuring a
# clean slate regardless of which identity was recorded. Developers will
# need to re-grant IM for other apps (e.g. iTerm) after dev-reset.
if _tccutil reset ListenEvent; then
    echo "  ListenEvent cleared (all apps)"
elif _tccutil reset ListenEvent "$BUNDLE_ID"; then
    echo "  ListenEvent cleared (bundle only)"
else
    echo "  ListenEvent clear skipped (tccutil unavailable)"
fi
_tccutil reset Accessibility "$BUNDLE_ID" && echo "  Accessibility cleared" || true

# Also purge any lingering TCC entries from the legacy bundle ID.
echo "[dev-reset] purging legacy TCC permissions for $LEGACY_BUNDLE_ID..."
_tccutil reset ScreenCapture "$LEGACY_BUNDLE_ID" 2>/dev/null && echo "  legacy ScreenCapture cleared" || true
_tccutil reset Microphone    "$LEGACY_BUNDLE_ID" 2>/dev/null && echo "  legacy Microphone cleared"    || true
_tccutil reset ListenEvent   "$LEGACY_BUNDLE_ID" 2>/dev/null && echo "  legacy ListenEvent cleared"   || true
_tccutil reset Accessibility "$LEGACY_BUNDLE_ID" 2>/dev/null && echo "  legacy Accessibility cleared" || true

# ── 3. App data ───────────────────────────────────────────────────────────────
echo "[dev-reset] clearing app data..."

DB_FILE="$APP_SUPPORT/hush.db"

if [ "$nuke_db" -eq 1 ]; then
  # Hard wipe: remove the entire database file (and WAL/SHM).
  for f in hush.db hush.db-shm hush.db-wal; do
    target="$APP_SUPPORT/$f"
    if [ -f "$target" ]; then
      rm "$target"
      echo "  removed $target"
    fi
  done
elif [ -f "$DB_FILE" ]; then
  # Soft wipe: preserve transcription and meeting history; clear settings,
  # dictionary, and replacements only so the next launch feels like a
  # first-run for settings/permissions without losing recordings.
  _sqlite3() {
    if [[ "$(id -u)" -eq 0 && "$TARGET_USER" != "$(id -un 2>/dev/null || true)" ]]; then
      sudo -u "$TARGET_USER" sqlite3 "$@"
    else
      sqlite3 "$@"
    fi
  }
  if command -v sqlite3 >/dev/null 2>&1; then
    _sqlite3 "$DB_FILE" \
      "DELETE FROM settings; DELETE FROM dictionary_terms; DELETE FROM replacements;" \
      && echo "  cleared settings, dictionary_terms, replacements (history preserved)"
  else
    echo "  sqlite3 not found — skipping selective DB clear (install sqlite3 or use --nuke-db)" >&2
  fi
else
  echo "  no database found, nothing to clear"
fi

# Models: kept by default; wiped with --nuke-models
if [ "$nuke_models" -eq 1 ]; then
  if [ -d "$APP_SUPPORT/models" ]; then
    rm -rf "$APP_SUPPORT/models"
    echo "  removed $APP_SUPPORT/models"
  fi
else
  echo "  keeping downloaded models (pass --nuke-models to remove them too)"
fi

# Legacy app support directory from before the bundle ID rename.
legacy_app_support="$TARGET_HOME/Library/Application Support/$LEGACY_BUNDLE_ID"
if [ -d "$legacy_app_support" ]; then
  rm -rf "$legacy_app_support"
  echo "  removed legacy $legacy_app_support"
fi

# ── 4. Preferences (NSUserDefaults / window geometry / recent dirs) ───────────
pref="$TARGET_HOME/Library/Preferences/$BUNDLE_ID.plist"
if [ -f "$pref" ]; then
  rm "$pref"
  echo "  removed $pref"
fi
# Legacy pref file from before the bundle ID rename.
legacy_pref="$TARGET_HOME/Library/Preferences/$LEGACY_BUNDLE_ID.plist"
if [ -f "$legacy_pref" ]; then
  rm "$legacy_pref"
  echo "  removed legacy $legacy_pref"
fi
# Flush cfprefsd cache so the deleted plist takes effect immediately.
# Run as the target user to avoid flushing a different user's daemon.
if [[ "$(id -u)" -eq 0 && "$TARGET_USER" != "$(id -un 2>/dev/null || true)" ]]; then
  launchctl asuser "$TARGET_UID" killall cfprefsd 2>/dev/null || true
else
  killall cfprefsd 2>/dev/null || true
fi

# ── 5. Caches ─────────────────────────────────────────────────────────────────
for cache_dir in \
  "$TARGET_HOME/Library/Caches/$BUNDLE_ID" \
  "$TARGET_HOME/Library/Caches/$LEGACY_BUNDLE_ID" \
  "$TARGET_HOME/Library/Caches/hush"; do
  if [ -d "$cache_dir" ]; then
    rm -rf "$cache_dir"
    echo "  removed $cache_dir"
  fi
done

# ── 6. Autostart LaunchAgent ──────────────────────────────────────────────────
for launch_agent in \
  "$TARGET_HOME/Library/LaunchAgents/$BUNDLE_ID.plist" \
  "$TARGET_HOME/Library/LaunchAgents/$LEGACY_BUNDLE_ID.plist"; do
  if [ -f "$launch_agent" ]; then
    if [[ "$(id -u)" -eq 0 && "$TARGET_USER" != "$(id -un 2>/dev/null || true)" ]]; then
      launchctl asuser "$TARGET_UID" launchctl unload "$launch_agent" 2>/dev/null || true
    else
      launchctl unload "$launch_agent" 2>/dev/null || true
    fi
    rm "$launch_agent"
    echo "  removed autostart LaunchAgent: $launch_agent"
  fi
done

# ── 7. App installs ───────────────────────────────────────────────────────────
# Remove ALL known Hush.app locations so stale binaries with different
# codesign identities can't pollute TCC during permission testing.
#
# Background: every Tauri build without --sign leaves a linker-signed binary
# whose codesign identifier is `hush-<hash>` (not `io.github.khawkins98.hush`).
# TCC keys ListenEvent/Microphone/etc rows to the codesign identifier, so a
# linker-signed /Applications/Hush.app carries separate TCC rows from the
# re-signed binaries produced by tauri:bundle / tauri:dmg. If the old install
# is still present it creates a confusing "two binaries with different permission
# states" situation — one app sees Denied, the other sees NotDetermined — and
# macOS may auto-focus or single-instance-redirect to the wrong one.
# (see learnings.md 2026-05-13 "linker-signed vs re-signed TCC identity")
for app_loc in \
  "$TARGET_HOME/Applications/Hush.app" \
  "/Applications/Hush.app"; do
  if [[ -d "$app_loc" ]]; then
    rm -rf "$app_loc"
    echo "  removed app install: $app_loc"
  fi
done

echo ""
echo "[dev-reset] done. Next launch of Hush will behave as a first-ever install."
echo "            Note: Screen Recording rows from previous builds may still appear"
echo "            in System Settings → Privacy → Screen & System Audio Recording."
echo "            Remove any stale 'Hush' rows there manually before testing onboarding."
