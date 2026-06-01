# Memory debugging

How to diagnose memory growth in Hush. This project has had three significant
memory hunts (#612 whisper state, #641 ORT/Metal, the 2026-06-01 mimalloc
footprint investigation) and each one initially hid in a metric nobody was
watching. This doc exists so the fourth hunt starts from tooling, not from
re-deriving methodology.

## The iron rule: measure BOTH numbers

macOS reports process memory two very different ways:

| Metric | Where you see it | What it counts |
|---|---|---|
| **RSS** (resident set size) | `ps -o rss`, `top`'s MEM, the #629 in-app recreation log | Pages physically in RAM right now |
| **Physical footprint** | Activity Monitor's **"Memory" column**, `vmmap -summary`, `footprint <pid>` | Dirty pages **including compressed/swapped ones** — what the user actually experiences |

A leak can grow footprint by gigabytes while RSS stays flat (macOS compresses
the never-touched-again dirty pages). The 2026-06-01 investigation found a
~40 GB footprint while RSS sawtoothed at 1–3.5 GB. Conversely, the #612-era
fixes were validated against RSS only — which is exactly why the footprint
leak survived them.

**Every memory claim ("fixed", "stable", "leaking") must cite both numbers.**

## Tooling

### `npm run memwatch` — the first thing to run

Samples the running Hush process every 30 s (or `npm run memwatch -- 15` for
15 s) and writes to `/tmp/hush-memwatch-<timestamp>/`:

- **`memwatch.csv`** — one row per sample: RSS, physical footprint, footprint
  peak, total swapped. Graph or eyeball this for the trend.
- **`vmmap-NNNN.txt`** — a full `vmmap -summary` snapshot per sample. Read
  these to attribute growth to a specific owner (see table below).

Start it before a meeting, leave it running, Ctrl-C when done. Output stays
valid at any point. Works on any build — no special flags needed.

### Attributing growth: which vmmap region = which owner

Compare an early snapshot against a late one and look at the **DIRTY** +
**SWAPPED** columns per region type:

| vmmap region type | Owner | Notes |
|---|---|---|
| `VM_ALLOCATE` (untagged) | **mimalloc** — the global allocator, i.e. all Rust + whisper.cpp + tract heap | Should stay near-zero dirty with `alloc_tuning` purge enabled. Growth here = allocator retention (see `alloc_tuning.rs`) |
| `WebKit Malloc` | **The webview / frontend** (main window, HUD, menu-bar web content) | Growth here = frontend state or event-payload accumulation — look at Svelte stores, Tauri event frequency/payload size |
| `IOAccelerator` | **GPU / Metal** | Compositing, animations. Historically the #641 ORT-MPS leak; whisper builds with `GGML_METAL=OFF` so whisper never appears here |
| `MALLOC_LARGE` / `MALLOC_SMALL` / `MALLOC_TINY` | **System malloc zones** | Should be near-empty — mimalloc's `override` claims everything. Growth here means the override isn't active |
| `IOSurface` / `CoreAnimation` | Window compositing buffers | Bounded; scales with window count, not time |

A useful one-liner for comparing two snapshots:

```bash
# dirty + swapped for the key regions, fields counted from the row's end
for f in vmmap-0001.txt vmmap-0040.txt; do
  echo "== $f =="
  grep -E '^(VM_ALLOCATE|WebKit Malloc|IOAccelerator) ' "$f" | grep -v reserved \
    | awk '{print $1, "dirty="$(NF-5), "swapped="$(NF-4)}'
done
```

### In-app instrumentation

- **WhisperState recreation log (#629)** — every 30 streaming inferences, an
  INFO line `recreating WhisperState (#612 periodic recreation)` reports
  `rss_before_mb` / `rss_after_mb` / `delta_mb`. Healthy: delta reliably
  ≈ −200 MB or larger. Delta ≈ 0 means the accumulation is owned by
  something other than the state.

  ```bash
  grep "recreating WhisperState" ~/Library/Logs/io.github.khawkins98.hush/hush.log.*
  ```

- **Allocator purge tuning log** — at startup, `alloc_tuning` logs whether
  mimalloc purge tuning is active:
  `allocator purge tuning enabled: mimalloc purge_delay=0 + post-inference force-collect`

### A/B knobs

| Env var | Effect |
|---|---|
| `HUSH_ALLOC_PURGE=0` | Disable allocator purge tuning (baseline mode). Use to demonstrate the mimalloc retention leak on a fixed build, or to isolate whether new growth is allocator-related |
| `HUSH_WHISPER_STATE_RECREATE_INTERVAL=0` | Disable #612 periodic state recreation (re-creates the original whisper-state leak — only for demonstrations) |
| `MIMALLOC_SHOW_STATS=1` | mimalloc prints its own allocation statistics at process exit |

To run a GUI launch with env vars (so TCC attribution stays correct), set them
through launchd rather than a shell prefix:

```bash
launchctl setenv HUSH_ALLOC_PURGE 0
open ~/Applications/Hush.app
# ... after the test:
launchctl unsetenv HUSH_ALLOC_PURGE
```

## Standard A/B procedure for memory fixes

1. **Baseline**: run `npm run memwatch -- 15`, hold a meeting (or play a long
   video with system-audio capture) for 10–20 min on the unfixed build /
   with the fix disabled via env knob. Keep the output dir.
2. **Fix**: same procedure on the fixed build.
3. **Compare**: footprint trend in both CSVs + per-region attribution from
   the first/last vmmap snapshots of each run.
4. A fix claim needs: footprint growth rate before vs after, AND the region
   that stopped growing.

10 minutes of meeting is enough to distinguish ~1 GB/min (broken) from flat
(fixed); 15–20 min also covers several WhisperState recreation cycles.

## Leak history (what's already been found and fixed)

| When | What | Mechanism | Fix | learnings.md entry |
|---|---|---|---|---|
| 2026-05-07 | #612 whisper state | `create_state()` per inference, ~76 MB never returned | Lazy-init / drop-on-Err / periodic recreate triplet in `whisper.rs` | "#612 not actually closed" |
| 2026-05-08 | #641 ORT / Metal | ORT dispatches through MPS even with CPU EP → unbounded `IOAccelerator` | Replace ORT with pure-Rust `tract-onnx` | "#641 root cause fix" |
| 2026-05-07 | #639 system-malloc hoarding | macOS libmalloc never returns whisper's freed pages (23.5 GB `MALLOC_LARGE`) | mimalloc as `#[global_allocator]` with `override` | "2026-05-07" |
| 2026-06-01 | mimalloc footprint retention | mimalloc keeps freed pages committed-dirty; macOS compresses them → footprint grows ~1 GB/min while RSS stays flat | `alloc_tuning`: `purge_delay=0` + post-inference `mi_collect(force)` | "2026-06-01" |
| open | Webview growth during meetings | `WebKit Malloc` grows ~160 MB/min while a meeting session is active (134 MB → 3.2 GB over a 20-min meeting; ~82% of total growth) and keeps growing post-meeting. NOT proportional to transcript volume — observed with only 15 utterances. Suspects: meeting-panel poll loop, HUD high-frequency events, WebKit retention of IPC payloads | TBD | "2026-06-01" |
