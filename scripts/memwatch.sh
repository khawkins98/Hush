#!/usr/bin/env bash
#
# memwatch — sample the running Hush process's memory decomposition over time.
#
# Usage:
#   npm run memwatch                  # 30 s interval (default)
#   bash scripts/memwatch.sh 10       # custom interval in seconds
#
# Why this exists (learnings.md 2026-06-01): Activity Monitor's "Memory"
# column is *physical footprint* — committed dirty pages including
# compressed/swapped ones — not RSS. Hush's meeting-time growth lives almost
# entirely in compressed dirty pages owned by mimalloc (untagged VM_ALLOCATE
# regions), so RSS-based monitoring (`ps`, the #629 in-app recreation log)
# misses it completely. This script samples both numbers plus full vmmap
# snapshots so growth can be attributed to the right owner (mimalloc arenas
# vs IOAccelerator vs WebKit vs malloc zones).
#
# Output: /tmp/hush-memwatch-<start-time>/
#   memwatch.csv       one row per sample — graph this for the trend
#   vmmap-NNNN.txt     full `vmmap -summary` snapshot per sample — read these
#                      to attribute growth to a region type
#
# Start it before the meeting, leave it running, Ctrl-C when done. The CSV is
# flushed per-sample so it's valid at any moment.

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "memwatch is macOS-only (uses vmmap)" >&2
    exit 1
fi

INTERVAL="${1:-30}"
OUTDIR="/tmp/hush-memwatch-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUTDIR"
CSV="$OUTDIR/memwatch.csv"

echo "time,pid,rss_mb,footprint,footprint_peak,writable_swapped_out,vmmap_snapshot" > "$CSV"

echo "memwatch: sampling every ${INTERVAL}s → $OUTDIR"
echo "memwatch: press Ctrl-C to stop (output stays valid at any point)"
echo ""
printf "%-9s %-7s %10s %12s %12s %14s\n" "TIME" "PID" "RSS_MB" "FOOTPRINT" "PEAK" "SWAPPED_OUT"

find_pid() {
    # The executable inside the bundle (and the dev binary) is named `hush`.
    pgrep -x hush 2>/dev/null | head -1 || true
}

n=0
while true; do
    pid="$(find_pid)"
    if [[ -z "$pid" ]]; then
        printf "%-9s %s\n" "$(date +%H:%M:%S)" "(no hush process — waiting)"
        sleep "$INTERVAL"
        continue
    fi

    n=$((n + 1))
    snap_name="vmmap-$(printf '%04d' "$n").txt"
    snap="$OUTDIR/$snap_name"

    # vmmap can transiently fail if the process is mid-exit; tolerate it.
    if ! vmmap -summary "$pid" > "$snap" 2>/dev/null; then
        printf "%-9s %s\n" "$(date +%H:%M:%S)" "(vmmap failed for pid $pid — retrying next interval)"
        rm -f "$snap"
        sleep "$INTERVAL"
        continue
    fi

    ts="$(date +%H:%M:%S)"
    rss_mb="$(ps -o rss= -p "$pid" | awk '{printf "%.0f", $1 / 1024}')"
    # "Physical footprint:         1.2G" / "Physical footprint (peak):  1.7G"
    footprint="$(awk '/^Physical footprint:/ {print $3; exit}' "$snap")"
    footprint_peak="$(awk '/^Physical footprint \(peak\):/ {print $4; exit}' "$snap")"
    # "Writable regions: Total=7.4G written=1.2G(16%) resident=18.0M(0%) swapped_out=1.2G(16%) ..."
    swapped_out="$(grep -o 'swapped_out=[^( ]*' "$snap" | head -1 | cut -d= -f2)"

    echo "${ts},${pid},${rss_mb},${footprint},${footprint_peak},${swapped_out},${snap_name}" >> "$CSV"
    printf "%-9s %-7s %10s %12s %12s %14s\n" \
        "$ts" "$pid" "$rss_mb" "$footprint" "$footprint_peak" "$swapped_out"

    sleep "$INTERVAL"
done
