#!/usr/bin/env bash
# Kill stale Hush dev processes left over from a hung `npm run tauri dev`.
#
# Usage:
#   npm run dev-cleanup
#
# What it kills:
#   - The dev binary itself                       (target/debug/hush)
#   - Tauri's dev runner                          (cargo run / tauri dev)
#   - Vite's dev server                           (vite dev, port 1420)
#
# Why this exists: the dev cycle leaves processes around when the parent
# terminal dies, when `cargo tauri dev` hits a build error mid-rebuild, or
# when macOS denies a permission and the app exits without cleaning up its
# Vite child. Symptoms: "port 1420 already in use", or two windows on next
# launch. Running this clears them all.
#
# To also wipe app state and TCC permissions (full clean slate), use:
#   npm run dev-reset
#
# Each `pkill` returns 1 if no processes matched — that's the *common* case
# (nothing stuck), so we ignore non-zero exits.

set +e
set -u

for arg in "$@"; do
  case "$arg" in
    --help|-h)
      awk 'NR>1 && /^[^#]/{exit} NR>1{sub(/^# ?/,""); print}' "$0"
      exit 0
      ;;
    *)
      echo "[hush dev-cleanup] unknown argument: $arg (try --help)" >&2
      ;;
  esac
done

echo "[hush dev-cleanup] looking for stale processes..."

# Patterns are anchored loosely on purpose — pkill -f matches the full
# command line, and these substrings are specific enough not to hit
# unrelated processes.
declare -a patterns=(
  "target/debug/hush"
  "Hush.app/Contents/MacOS/hush"
  "tauri dev"
  "vite dev"
)

killed_any=0
for pattern in "${patterns[@]}"; do
  if pkill -f "$pattern" 2>/dev/null; then
    echo "  killed processes matching: $pattern"
    killed_any=1
  fi
done

# Free port 1420 (Vite dev server) by killing whatever holds it. Vite is
# already covered by the pattern above, but a stuck process owned by a
# different parent (e.g. a previous shell) might not match the command
# pattern.
if command -v lsof >/dev/null 2>&1; then
  pids="$(lsof -ti :1420 2>/dev/null || true)"
  if [ -n "$pids" ]; then
    echo "  freeing port 1420 (PIDs: $pids)"
    # shellcheck disable=SC2086
    kill $pids 2>/dev/null || true
    killed_any=1
  fi
fi

if [ "$killed_any" -eq 0 ]; then
  echo "  no stale processes found."
fi

echo "[hush dev-cleanup] done."
