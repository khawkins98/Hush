#!/usr/bin/env bash
# Test whether AudioHardwareCreateProcessTap triggers Screen Recording TCC
# or the NSAudioCaptureUsageDescription audio-capture permission dialog.
#
# See issue #585 and resources/macos-audio-tap-probe.swift.
#
# What to look for when the dialog appears:
#   Screen Recording (BAD) — lock icon, "record this computer's screen and audio",
#                            "Open System Settings" / "Deny" buttons.
#   Audio Capture  (GOOD) — mic icon, custom text from NSAudioCaptureUsageDescription,
#                            "Allow" / "Don't Allow" buttons.
#
# Usage:
#   ./scripts/test-audio-tap-permission.sh
#
# The script compiles the Swift probe, ad-hoc signs it, runs it, and prints
# the result. Watch the screen for which dialog macOS shows.

set -euo pipefail

PROBE_SRC="resources/macos-audio-tap-probe.swift"
PROBE_BIN="/tmp/hush-audio-tap-probe"

cd "$(git rev-parse --show-toplevel)"

echo "==> Compiling probe binary..."
swiftc "$PROBE_SRC" \
    -framework CoreAudio \
    -framework Foundation \
    -framework AudioToolbox \
    -o "$PROBE_BIN"

echo "==> Ad-hoc signing (minimum required for TCC prompts)..."
codesign -s - "$PROBE_BIN"

echo ""
echo "==> Running probe. Watch your screen for a permission dialog."
echo "    Note whether it's:"
echo "    GOOD: mic icon, custom text, Allow/Don't Allow buttons"
echo "    BAD:  lock icon, 'screen and audio', Open System Settings/Deny"
echo ""

set +e
"$PROBE_BIN"
EXIT_CODE=$?
set -e

echo ""
case $EXIT_CODE in
    0) echo "Result: tap_created — permission granted. Note which dialog appeared!" ;;
    1) echo "Result: permission_denied — user clicked Don't Allow (or previously denied)." ;;
    2) echo "Result: unsupported — macOS 14.2+ required." ;;
    3) echo "Result: error — unexpected HAL error; check stderr above." ;;
    *) echo "Result: unknown exit code $EXIT_CODE" ;;
esac

echo ""
echo "NOTE: If no dialog appeared and exit=1, TCC may have cached a prior deny."
echo "Run: tccutil reset AudioCapture   (if that TCC category exists)"
echo "  or: tccutil reset All             (nuclear option)"
