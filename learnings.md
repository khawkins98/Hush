# Learnings Log

Engineering decision log for Hush. Append-only, dated entries. Captures dependency choices, platform quirks, false starts, and anything future contributors would benefit from knowing.

---

## 2026-05-06 — Meeting mode silent-audio root cause: SCK codec artefacts defeated Whisper's noise gate (#533)

### Root cause (confirmed by community reviewer)

Meeting mode sources that used ScreenCaptureKit for system audio appeared to transcribe silence even when audio was clearly playing. The root cause: ScreenCaptureKit's audio pipeline applies **Opus/AAC codec processing** internally before delivering PCM to the app. The codec artefacts (pre-echo, spectral smearing) inflated Whisper's `no_speech_thold` comparison, pushing nearly every audio segment into the "no speech" bucket. Effective threshold: segments that would pass at the raw-PCM level were silently discarded.

This is **not a Whisper bug** — `no_speech_thold = 0.6` is the correct default for clean PCM. The issue is that SCK does not deliver clean PCM for system audio: it delivers post-codec samples that look like noise to a model calibrated for raw capture.

**Fix (shipped in #588):** Switch system-audio sources to `CoreAudioTapSession` (the `AudioHardwareCreateProcessTap` backend). CoreAudio tap delivers raw f32 PCM with zero codec round-tripping. No `no_speech_thold` workaround needed.

> **Note:** The codec-artefact cause is inferred from the absence of transcription on SCK + its presence on the CoreAudio tap path post-#588. We never directly measured Whisper's `no_speech_prob` values on the corrupted samples. If the silence returns on the tap path, instrument `no_speech_prob` per-segment in `WhisperContext::transcribe` before chasing the codec theory further.

### Diagnostic additions (#533 follow-up)

To make this class of bug reproducible in the future, three diagnostics were added in #533:

1. **Per-source final utterance counter** (`pump.rs`): at pump shutdown, logs `source_kind=<kind> finals=<N>` for each source. `finals=0` from a system-audio source that was visibly playing audio is the unambiguous reproduction signal.

2. **First-drain RMS log** (`pump.rs`): on the first drain tick for each source (empty or non-empty), logs `rms=<f>` of the drained samples. Near-zero RMS from a system-audio source confirms no PCM is reaching the pump — distinguishes a capture failure from a transcription failure.

3. **`MEETING_SOURCE_FAILED_EVENT` on streaming-session init failure** (`lifecycle.rs`): if `drain_into` (pre-warm) or `start_stream` fails, the frontend now receives an event and displays an amber banner — the failure was previously silent.

### Reproduction protocol

```
RUST_LOG=hush::meeting=debug npm run tauri dev
```
1. Open a YouTube video (audio visible in system volume indicator).
2. Start a meeting in Hush with system audio enabled.
3. At meeting stop, grep log for `source_kind=system finals=0` — if present, confirms the bug.
4. Also check `first_drain rms=0.0` lines — confirms no samples reached the pump.

### Lesson

Codec pipelines (SCK, any OS media stack) can produce PCM that passes amplitude checks but fails deep spectral analysis. For transcription, **raw PCM = correctness guarantee**. Any audio API that recompresses before delivery is untrustworthy for speech-to-text. The per-source final counter is the right end-to-end sanity metric; add it to any new audio source type.

---

## 2026-05-06 — `isExclusive = true` is required for `CATapDescription` to capture any audio (#593, #594)

**The bug:** `CATapDescription.processes = []` with `isExclusive = false` delivers silence — it means "tap no processes". With `isExclusive = true`, the empty list means "exclude no one from the tap", i.e. capture the entire system mix. This is documented in Korus's source code but not in Apple's headers or developer documentation.

Every working open-source implementation confirmed in research (OpenWhispr, Korus, Atoll, yogurt) uses `isExclusive = true`. Hush originally copied the wrong default.

**Second bug:** Using AVAudioEngine pointed at an aggregate device where the main sub-device is output-only (no input channels) returns silence from the AUHAL unit's non-existent input bus. The fix is `AudioDeviceCreateIOProcIDWithBlock` directly on the aggregate device — the IOProc's `inInputData` receives the tap's PCM from the aggregate input bus regardless of sub-device channel configuration.

**Lesson:** When a CoreAudio tap delivers samples (non-zero byte counts in the log) but Whisper sees only silence, suspect the tap configuration before the transcription stack. The `isExclusive` flag is the first thing to check. The `real_finals` / `blank_finals` counter added in #591 is the right end-to-end signal: `real_finals=0 blank_finals=N` from a system-audio source confirms audio is flowing but content is silence.

**Acoustic echo risk (follow-up):** When the user has speakers (not headphones), the microphone will acoustically pick up whatever the tap captures digitally, producing a duplicate stream attributed to a separate diarizer speaker. The standard fix is Apple's Voice Processing I/O unit (built-in AEC) for the microphone capture path. Tracked as a comment on #594; scoped to a future issue.

---

## 2026-05-06 — System audio on macOS: `AudioHardwareCreateProcessTap` is the right approach on macOS 14.2+ (#585)

This entry is the authoritative summary. Several earlier entries explored ScreenCaptureKit (SCK) and the tap API separately; those entries are marked **[SUPERSEDED]** below and preserved for historical context.

### Definitive answer

On macOS 14.2+, **`AudioHardwareCreateProcessTap` / `CATapDescription` captures all system audio with no TCC permission prompt of any kind.** No "Screen Recording" dialog, no microphone dialog. The user sees nothing.

For any macOS app that:
- targets macOS 14.2+ (Hush targets 26+)
- is distributed outside the MAS sandbox (sideloaded, notarised, Homebrew cask)

…this API is strictly superior to ScreenCaptureKit for system-audio capture.

### Architecture (what Hush ships)

A Swift helper binary (`resources/macos-audio-tap.swift`) compiled by `build.rs` and bundled into `Contents/Resources/resources/`:

1. `CATapDescription(processes:[])` — captures all system audio; `isExclusive: false` (don't mute tapped apps), `isMixdown: true` (mix all to one stream)
2. Aggregate device backed by the tap, with the default output device as the main sub-device (ties clock to system output)
3. `AVAudioEngine` → `installTap` on the output node → f32 PCM chunks to stdout

**Wire protocol:** 12-byte header — `HUSH` magic (4) + u32le sample\_rate (4) + u32le channels (4) — followed by continuous f32 LE interleaved PCM. Header is written before `engine.start()` so it always precedes PCM in the pipe.

**Rust side:** `CoreAudioTapSession` (implementing `AudioSession`) spawns the binary, reads the header, pumps samples into an `rtrb` ring via a reader thread. Stop: SIGTERM → 1 s poll (50 ms intervals) → SIGKILL fallback → join reader thread.

### Why the 2026-04-26 entry was wrong

The 2026-04-26 entry concluded: *"prefer TCC path (ScreenCaptureKit); the tap API is entitlement-required and only pays back in MAS."* That was based on developer forum posts describing macOS 14.x sandboxed-MAS behaviour. **For unsandboxed/sideloaded apps on macOS 14.2+ (and confirmed macOS 26), no entitlement is required and no TCC prompt fires.** The forum posts described the OS-level audio-recording entitlement for sandboxed MAS apps — a different code path.

### Problems with SCK that this eliminates

- "Screen & System Audio Recording" label alarmed users (they thought Hush recorded their screen)
- `mediaserverd`/`coreaudiod` cached TCC deny for the current process → required full process relaunch after every grant
- Ad-hoc rebuild signature changes produced stale TCC rows in System Settings
- App had to call `SCShareableContent::get()` to get enrolled in the TCC pane before the user could toggle the row on — confusing dead end if they navigated there first

### Implementation gotchas

- Call `engine.prepare()` **before** `installTap` so the output node format is resolved before writing the header to stdout
- Use `DispatchSemaphore(value:32)` + `tryWait` in the audio callback — drop chunks when stdout backpressures rather than blocking the CoreAudio thread
- `SIGTERM` handler: use `signal(SIGTERM, SIG_IGN)` + `DispatchSource` handler; a raw C signal handler cannot safely run Cocoa/AV cleanup
- Link `AVFAudio` (not `AVFoundation`) — `AVAudioEngine` lives in the `AVFAudio` sub-framework
- Resource path: `tauri.conf.json` entry `"resources/hush-audio-tap-capture"` lands at `Contents/Resources/resources/hush-audio-tap-capture`; Rust lookup requires `resource_dir.join("resources").join("hush-audio-tap-capture")` (not just `resource_dir.join("hush-audio-tap-capture")`)
- Binary is macOS-only; `build.rs` writes an empty placeholder on Linux/Windows. All Rust code is `#[cfg(target_os = "macos")]`-gated — cross-platform CI is unaffected.

---

## 2026-05-06 — Community input and open-source references that shaped the system-audio architecture

Two external inputs proved decisive on the system-audio journey. Both are worth crediting explicitly because they establish precedents for how Hush engages with the community and with other MIT-licensed projects.

### m13v — recurring contributor, consistently right about system audio

GitHub user [@m13v](https://github.com/m13v) commented on three issues across the system-audio arc and was correct at each juncture:

- **#33** (2026-04-26, the original system audio issue): *"macOS 14.4+ added Core Audio process taps which avoid the screen recording prompt entirely for system audio — that's the path I'd build toward over SCK long-term."* This was filed the same day Hush chose SCK, before the SCK work was complete. The advice was accurate; we didn't act on it at the time.
- **#579** (2026-05-05): Explained the real root cause of the relaunch requirement (`mediaserverd`/`coreaudiod` caches the deny for the current process before the grant lands — every SCK app faces this). Proposed the architecturally correct fix (helper-process pattern). Agreed the "Screen Recording" label is "genuinely alarming for a dictation tool."
- **#585** (2026-05-06): Confirmed `helper-binary-via-stdout` is the cleanest integration path; noted that direct Rust FFI to `AudioHardwareCreateProcessTap` requires Obj-C toll-free bridging for `CATapDescription`'s `NSArray` of pids (i.e., the Swift helper binary is the right call, not just a convenience). Also flagged the DRM caveat: both SCK and CAT are silenced by macOS protected-output flags — don't try to use either as a DRM workaround.

**Precedent:** m13v's comments are uncompensated, technically accurate, and saved significant refactor time. This kind of community guidance from issue comments is legitimate input — read it seriously even when it contradicts an already-shipped implementation.

### OpenWhispr — MIT reference implementation

[OpenWhispr](https://github.com/OpenWhispr/openwhispr) (MIT licence, Electron/React app) independently arrived at the same `AudioHardwareCreateProcessTap` + Swift helper binary + stdout streaming architecture we shipped. Studying their MIT source code was legal and productive: it confirmed the helper-binary approach, the `CATapDescription` parameters, and the lack of a TCC entitlement requirement before we committed engineering time to the port.

**This is distinct from the VoiceInk discipline** (CLAUDE.md §"Black-box reimplementation discipline"): VoiceInk is off-limits because Hush is a black-box reimplementation of it and reading its source would taint the independence claim. OpenWhispr is an unrelated MIT project in a different tech stack solving a shared sub-problem. Studying MIT code for a standalone, technically fungible sub-system (system audio capture) is normal open-source engineering. Hush's CoreAudio tap is an independent implementation of the same API, not a port of OpenWhispr's Swift file.

**DRM caveat to retain (from m13v + confirmed independently):** Both `AudioHardwareCreateProcessTap` and ScreenCaptureKit are silenced by macOS protected-output flags when DRM content is playing. Meeting Mode cannot capture audio from DRM-protected streams. This is by design in macOS and should be documented in the UI when we add a "not all audio sources are capturable" explanation.

---

## 2026-05-06 — Parallel Whisper model loads at startup (#561)

**Problem:** Startup took 2–4 s on typical hardware because `build_default` loaded two `WhisperTranscription` contexts sequentially. Each load mmaps the GGUF file and initialises a `whisper.cpp` context — ~1–2 s each for large models.

**Why two contexts?** Dictation and meeting pump each own a private `WhisperContext` to avoid mutex contention on the single-threaded `whisper.cpp` inference path (#248). The RAM cost is minimal — `whisper-rs` mmaps the GGUF so the OS shares physical pages between the two contexts.

**Fix:** Wrap `WhisperTranscription::new` in a `load_whisper_model` helper that calls `tokio::task::spawn_blocking`. Replace the two sequential `build_transcriber` calls with `tokio::join!`. Both blocking loads run on separate tokio blocking-pool threads, so the total cost is roughly max(load1, load2) rather than load1 + load2.

**Why splashscreen is deferred:** The setup hook uses `tauri::async_runtime::block_on`, which blocks the macOS main thread. During that block the OS won't update any window — including a splashscreen. Showing a splash that updates while models load requires moving `build_default` off the blocking setup hook (deferred `app.manage()` after an async spawn + a frontend readiness signal). This is a larger refactor tracked separately.

**Profiling:** `RUST_LOG=info npm run tauri dev` now emits timestamped trace events at each phase of `build_default`: `database and repositories ready`, `whisper contexts loaded`, `diarizer ready`, and `build_default complete`.

---

## 2026-05-05 — CI rustfmt version differs from local toolchain

**Problem:** CI uses `dtolnay/rust-toolchain` pinned to a January 2026 stable SHA (rustfmt 1.7.x). Local development typically runs a newer stable (e.g. 1.8.0). The two versions produce different output for borderline-length macro invocations (`anyhow!(...)`, `tracing::info!(...)`, function calls with 3+ args near the 100-char `max_width`). The result: `cargo fmt --all -- --check` passes locally but fails on CI after a push.

**Symptoms:** CI `rustfmt check` job fails with a diff showing the exact wrapping it expects. The diff is reliable — applying it manually always produces a passing run.

**Workaround:** When the CI fmt check fails, read the exact diff from the CI job log (`gh run list --branch <branch>`, then `gh run view <id> --log`) and apply those changes directly to the file. Do not rely on local `cargo fmt --all` to find these differences — it won't because the version differs.

**Long-term fix:** Pin the local toolchain to match CI (add a `rust-toolchain.toml` at the repo root), or keep the CI pin moving in sync with local. Deferred because it would require all contributors to re-install the pinned toolchain.

---

## 2026-05-07 — CI clippy version differs from local toolchain (parallel to rustfmt gap)

**Problem:** Same root cause as the rustfmt entry above: CI's pinned January-2026 stable runs Rust 1.95+ while local stable can be older (e.g. 1.94.0 as of this entry). New clippy lints land between point releases — `clippy::collapsible_match` triggering on a nested-`if`-inside-`match-arm` pattern landed in 1.95 but not 1.94. The result: `cargo clippy --all-targets -- -D warnings` passes locally but fails on CI after a push.

**Symptoms:** CI's `clippy (default features)` step fails with a clear diagnostic naming the lint and a suggested fix. The lint isn't visible locally because the older toolchain doesn't include it.

**Workaround:** Same shape as the rustfmt one — read the diagnostic from the CI log, apply the suggested rewrite (clippy diagnostics include the exact code edit), push again. The pre-push hook (`.githooks/pre-push`) runs clippy `--lib --no-default-features` which is the right cross-platform shape but uses local clippy's lint set. There is no local check that catches lints CI's stricter version will flag.

**Concrete example caught in #604:** a `match RunEvent::ExitRequested { ref api, .. } => { if !flag { api.prevent_exit(); } }` pattern. Rust 1.94's clippy was silent; Rust 1.95's `collapsible_match` flagged it and suggested collapsing the inner `if` into a match guard. Behaviour identical either way.

**Long-term fix:** Same as the rustfmt entry: pin local toolchain via `rust-toolchain.toml`. Deferred for the same coordination reason. When enough version-mismatch incidents accumulate to justify the friction, both gaps close together with one `rust-toolchain.toml` commit.

---

## 2026-05-05 — Meeting pump diarizer buffer drift on drain failure (#553)

**Problem:** In `meeting/pump.rs`, when `drain_into` fails for a tick (e.g. transient SCK interruption), `tick_formats[i]` stays `None` and the diarizer's `AudioRollingBuffer` receives no samples for that tick. The streaming transcription session continues advancing its internal timeline, so the diarizer buffer falls behind. When utterance finals arrive with `[started_at_ms, ended_at_ms)` timestamps, `audio_buffer.slice_ms()` returns stale or misaligned audio, degrading speaker-labelling quality for the rest of the session.

**Fix:** Cache the last successful `CaptureFormat` per source in `last_known_formats`. On drain failure, if a format is known, compute `(sample_rate * PUMP_TICK_secs * channels) as usize` zero samples and append them to `audio_buffers[i]`. This keeps the diarizer timeline aligned with the tick cadence without introducing artificial speech content (zeros are silence and don't trigger embedding drift).

**Lesson:** Any rolling audio buffer that must stay wall-clock aligned needs compensation for missed ticks. The rule: *if a consumer has a time-indexed view of the audio stream, every tick must advance that index even if no real samples arrived.*

---

## 2026-05-05 — PTT trailing-silence buffer and minimum-hold guard (#548)

**Problem:** Two related PTT/recording UX bugs:

1. **Last word clipped.** Whisper processes audio in chunks. When `stop_dictation` / `meeting_stop_manual` is called immediately on PTT key-up or record-button click, audio buffered in the last ~500 ms hasn't flushed into the current chunk yet and is silently discarded. The final word gets dropped.

2. **Stuck-recording race.** A PTT key tap shorter than the time it takes for the start IPC to complete (`start_dictation` ≈ 5–50 ms) means: press fires `dictation.start()` (sets `busy=true`); release arrives while IPC is in-flight; release handler sees `busy=true, recording=false` and returns without stopping; start resolves to `recording=true`; user is stuck recording.

**Fixes implemented (frontend-only):**

1. `dictation.stop()` now accepts a `trailingMs` parameter (default `0`). PTT release and the record-button `onStop` callback pass `TRAILING_SILENCE_MS = 500`. The toggle hotkey and command palette pass `0` (explicit stop-now semantics). During the trailing window the state machine holds `busy=true, recording=true`, blocking re-entry from any other stop path.

2. PTT press/release handlers now run a **PTT state machine** with two additions:
   - `PTT_MIN_HOLD_MS = 100` guard: a timer arms on press; if key-up arrives before the timer fires, the tap is discarded. This eliminates accidental taps and OS key-bounce.
   - `pttIsDown` flag: set on every press, cleared on every release. The timer callback checks it before calling `start()`, preventing starts for keys already released. After `start()` resolves, the callback checks `!pttIsDown` again and calls `stop()` if needed — this closes the stuck-recording race.

**Thresholds:** 500 ms trailing silence and 100 ms minimum hold are empirically chosen starting points, matching PTT conventions in Discord / Mumble. Tuning may be needed based on real-world chunk sizes once the whisper streaming path matures.

---

## 2026-05-05 — Apple Developer ID signing deferred; ad-hoc stale-row UX mitigated instead (#520)

**Decision:** Signing Hush with an Apple Developer ID (which would stabilise the `csreq` hash across builds and eliminate stale TCC rows permanently) requires an Apple Developer Program membership at $99/year. As a solo hobby project, this was deemed not worth the cost at this time.

**Root cause of stale permissions:** TCC keyed grants on `(service, client_bundle_id, csreq)`. With ad-hoc signing, `csreq` is derived from the binary content hash — every rebuild gets a new hash, orphaning the previous grant row. `tccutil reset` only removes rows matching the *current* build's `csreq`; older rows survive and accumulate.

**Mitigation implemented:**
1. **Stale banner** — `+page.svelte` derives `anyPermsStale` from `get_permission_health`; an amber banner appears at the top of the main content area when any permission is stale, linking directly to Settings → Permissions. Hidden when the user is already on the Permissions tab.
2. **Guided recovery flow** — After "Reset permissions" succeeds, `PermissionsTab.svelte` automatically opens System Settings → Screen Recording (deep-link only, *no* SCK priming) and `MacosDiagnosticPanel.svelte` shows a step-by-step walkthrough to remove stale rows and quit/reopen Hush.

**Why deep-link-only after reset (no SCK priming):** `openPrivacyPane("screen-recording")` normally calls `prime_screen_recording_permission()` first (to ensure Hush appears in the list before the user arrives). After a `tccutil reset`, the user is removing rows, not granting fresh — priming there would fire an unwanted TCC prompt. The fix is to call `invoke("open_macos_privacy_pane", { target: "screen-recording" })` directly after reset, bypassing the priming step.

**Why "Quit and reopen" not "click Refresh":** `tccutil reset` only takes effect on next launch. A Refresh button in the same session would show the same stale state, confusing the user into thinking the reset failed.

**If Developer ID signing ever happens:** Track on #10. It would eliminate this entire class of UX friction — stale rows would never accumulate, `tccutil reset` would clean all grants in one shot, and the Gatekeeper bypass step on first launch would disappear too.

## 2026-05-03 — Drag on macOS borderless windows needs explicit `setMovable: YES` via objc2 (#427 Item 1)

**TL;DR:** Tauri 2's `data-tauri-drag-region` + `startDragging()` don't work on a `decorations: false` + `transparent: true` + `alwaysOnTop: true` window on macOS. `resizable: true` doesn't fix drag — it just makes the window user-resizable, which is a separate (unwanted) regression. The actually-working path is to call `[NSWindow setMovable: YES]` + `[NSWindow setMovableByWindowBackground: YES]` via objc2 from the `setup` hook. After that, `data-tauri-drag-region` starts working as documented.

### The chase

Three approaches to make the menu-bar popover (and the HUD pill, which has the same window flags) draggable all silently failed in practice:

1. `data-tauri-drag-region` on the root (the pattern the HUD's docstring claims works).
2. `getCurrentWebviewWindow().startDragging()` from a JS `mousedown` handler.
3. Hand-rolled drag tracking cursor delta + calling `setPosition` from window-level `mousemove`.

In every case cursor-grab CSS fired, clicks registered, but the window stayed put. Even direct programmatic `setPosition` from JS during drag did nothing — though `setPosition` *from Rust* (e.g. for tray-anchored positioning of the popover) did work, suggesting the AppKit-level drag handling, not a Tauri IPC issue, was the blocker.

### Cause

`decorations: false` strips the NSWindow's movable styleMask bits. AppKit then ignores Tauri's drag-region (which calls `[NSWindow performWindowDragWithEvent:]` under the hood, requiring movable bits) and any JS `mousemove` events likely never propagate cleanly because AppKit doesn't recognise the click as a drag candidate.

`transparent: true` and `alwaysOnTop: true` aren't the trigger on their own. The discussion at [tauri-apps/tauri#4362](https://github.com/tauri-apps/tauri/discussions/4362) and the macOS-specific drag-region issues [#11605](https://github.com/tauri-apps/tauri/issues/11605) / [#9503](https://github.com/tauri-apps/tauri/issues/9503) / [#12042](https://github.com/tauri-apps/tauri/issues/12042) cover variations on the same theme.

### What didn't fix it

- **`resizable: true` (no min/max clamps)** — this is the most-cited workaround in Tauri discussions. Empirically it makes the window user-resizable but doesn't unlock drag. Worse: with `decorations: false` the user can resize via mouse-drag at the edges (no visible handles, but the OS hit-tests drag zones), introducing a UX regression.
- **`resizable: true` with matched `minWidth`/`maxWidth`/`minHeight`/`maxHeight` clamps** — supposed to lock the size while keeping drag, but on this transparent always-on-top window it produced a *sizing* regression where the window rendered at a fraction of its declared height. Reverted.
- **JS `mousedown` → `startDragging()`** — silently fails. Tauri's API call returns Ok but no drag begins.
- **JS-rolled mousedown/mousemove/`setPosition` chain** — `setPosition` calls during drag don't move the window (though they work fine outside the drag scenario).

### What worked: explicit objc2 setMovable

A small Rust helper called from the `setup` hook for both the popover and the HUD:

```rust
#[cfg(target_os = "macos")]
fn unlock_macos_window_drag<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    let Ok(ptr) = window.ns_window() else { return; };
    let ns_window = ptr as *mut AnyObject;
    if ns_window.is_null() { return; }
    unsafe {
        let _: () = msg_send![ns_window, setMovable: true];
        let _: () = msg_send![ns_window, setMovableByWindowBackground: true];
    }
}
```

Plus `data-tauri-drag-region` on the popover root + `data-tauri-drag-region="false"` opt-outs on the buttons. After `setMovable: true` is called, AppKit accepts the drag-region's `performWindowDragWithEvent:` calls and the window moves smoothly.

`setMovableByWindowBackground: true` makes the entire window background act as a drag handle. This is what enables the "drag from anywhere non-interactive" UX without forcing the user to find a specific drag region. Buttons still get their click events because NSView hit-testing places interactive controls above the background drag.

### Window-config recommendations

- Keep `resizable: false` for popover/HUD-style windows — users shouldn't be able to resize them, and `setMovable: YES` doesn't depend on it.
- `decorations: false` + `transparent: true` + `alwaysOnTop: true` are all fine alongside `setMovable: YES`.
- Call the objc2 helper once per window in `setup`. Tauri doesn't expose a JSON config flag for `setMovable` and adding one upstream is the correct long-term fix; tracking that as a future contribution.

### `objc2` already in tree

Hush already pulls `objc2 = "0.6"` and `objc2-foundation = "0.3"` for the audio-cues + macos-perms paths. No new dependency for this fix.

### What does NOT help (verified during the chase)

- More `data-tauri-drag-region` placement gymnastics — root + header + footer attributes don't change anything.
- `webkit-app-region: drag` CSS — Electron-only Chromium fork attribute; wry/WebKit ignores it.
- `tauri-plugin-positioner` — purely positioning, doesn't unlock drag.
- `tauri-plugin-decorum` — Windows/Linux decorations polish; doesn't touch macOS movable bits.

The HUD pill (`focus: false` + `acceptFirstMouse: true` + `decorations: false` + `resizable: false`) has the same theoretical limitation but its size + auto-hide lifecycle mean nobody actually drags it. The popover is the first window where drag matters for UX.

---

## 2026-05-02 — Sync-primitive conventions in `AppState` and `SessionManager` (#431)

`AppState` (`src-tauri/src/ipc/mod.rs`) and `SessionManager` (`src-tauri/src/meeting/manager.rs`) collectively reach for four kinds of synchronisation primitive — `std::sync::Mutex`, `std::sync::RwLock`, `tokio::sync::Mutex`, and the `Atomic*` family. Each call site is individually defensible, but the rules for which one to pick weren't written down. Audit follow-up (#431) flagged this as a "next contributor will re-derive it" smell. Recording the convention here so they don't.

### Rule of thumb

1. **Reach for an `Atomic*` first.** A primitive `bool` / `i32` / `u8` that's set on one path and read on many is the right shape for an atomic. Examples: `hud_enabled`, `inference_threads`, `meeting_autostart_mode`, `ptt_active`, `diarization_enabled`. No locking, no contention, no risk of starving a reader.
2. **Use `std::sync::Mutex` for short critical sections in sync code.** Synchronous IPC handlers, `setup` hooks, and most `AppState` field initialisation run synchronously; a non-async mutex is the cheapest fit and avoids dragging tokio into call sites that don't need it. Examples: `pending_foreground`, `last_update_check`, the inner `Option<Arc<dyn Trait>>` slot for `TranscribeSlot` / `DiarizeSlot`.
3. **Use `tokio::sync::Mutex` only when the critical section needs to `.await`.** The async lock holds across awaits; the std lock would deadlock if the runtime parked the task while the lock was held. Example: `sck_probe_lock` in #422, which serialises a `spawn_blocking` Cocoa probe across concurrent IPC calls.
4. **Use `std::sync::RwLock` only when reads dominate and the writer doesn't need to `.await`.** Examples: `ptt_combo` (read by every event-loop iteration; written rarely from Settings).
5. **Wrap collections in `Arc<Mutex<…>>` rather than `Mutex<Arc<…>>`.** The Arc gives the wrapped state shared ownership across spawned tasks; the Mutex provides exclusion. Inverting the order means clones produce independent locks. Example: `downloads: Arc<Mutex<HashMap<String, CancelHandle>>>`.

### Anti-patterns to avoid

- **Don't reach for `tokio::sync::Mutex` "just in case" we go async later.** It's slower than `std::sync::Mutex` because every lock acquisition allocates a future, and the `Send` requirement infects every consumer of the guard. Pick the sync one and migrate later if the call site genuinely needs to await — the upgrade is mechanical.
- **Don't reach for `RwLock` over `Mutex` without proof of read contention.** Reader/writer locks have measurable per-acquisition overhead; for typical IPC handlers a `Mutex` is faster on the contended path and only marginally slower on the uncontended path.
- **Don't mix `std::sync` and `tokio::sync` on the same field.** A single field has a single lock type; pick by call-site needs and stick with it. The mix-and-match in `AppState` happened organically and that's why this entry exists.

### Audit context

The 22-field `AppState` currently uses 9 atomics, 5 std `Mutex`es, 1 std `RwLock`, and 1 tokio `Mutex` — each defensible individually. The audit's other open recommendation (regrouping fields into a `RuntimeFlags` substruct) is independent of this convention; this entry just documents which lock type to pick when adding new fields, regardless of where they live structurally.

## 2026-05-02 — Traffic-light permission health: two-signal model + implementation decisions (#378)

The `macos_perms::PermissionHealth` classifier (landed in the unnamed colleague's PR, post-#378) surfaces three states — Confirmed / Stale / NotGranted — by combining two independent signals:

```
match (os_preflight_result, last_confirmed_timestamp.is_some()) {
    (Granted, _)      → Confirmed
    (false,   Some)   → Stale          // was granted, now revoked
    (false,   None)   → NotGranted     // never granted
    (NotApplicable, _) → NotApplicable
}
```

Four non-obvious decisions baked into the implementation:

**1. `CGPreflightScreenCaptureAccess()` maps false → `NotDetermined`, not `Denied`.** The OS API can't distinguish "never asked" from "explicitly denied" — both return `false`. We call both cases `NotDetermined` rather than `Denied` so the frontend hint copy stays neutral ("not yet granted") rather than accusatory. Both map to the same `PermissionHealth` outcome, so the naming doesn't affect logic — it only affects any future `PermissionStatus`-level display surface that tries to differentiate them.

**2. `CGPreflightScreenCaptureAccess()` returns `u8`, not `bool`.** Apple's `Boolean` typedef is `unsigned char`; declaring the return as Rust `bool` is technically UB if the OS ever returns a value outside {0, 1}. Use `u8` with `!= 0`. In practice macOS always returns 0 or 1, but the type-correct form is the `u8` path.

**3. Auto-confirm on first-seen-Granted (inside `get_permission_health`).** Rather than requiring the UI to call `confirm_permission` explicitly on first successful use, `get_permission_health` seeds the `last_confirmed` row the first time it sees `Granted` for a permission that has no row yet. This is what makes the Stale verdict possible later: future probes that flip to false against an existing row read as "was granted, now revoked". Restricting the write to the first-seen-Granted case keeps the timestamp stable instead of re-stamping on every read.

**4. Wake-grace suppression is not needed — yet.** `CGPreflightScreenCaptureAccess` transiently returns false for ~10 s after sleep/wake (undocumented by Apple, observed by ScreenPipe in production). The current implementation probes only on Permissions tab open and on Refresh click — there is no auto-probe-on-window-focus. Because there's no background probing, the transient post-wake false can't produce a spurious Stale verdict. If window-focus auto-probing is ever added, the 10 s wake grace (suppress results within 10 s of `NSWorkspace.didWakeNotification`) **must** land in the same PR. Don't add auto-probing without the grace window.

The primary staleness scenario is a notarisation rebuild rotating the ad-hoc signing identity — TCC silently invalidates the entry because the bundle-ID + signature fingerprint no longer matches. The user sees yellow "Was granted — now revoked" in Settings → Permissions and can use the per-row deep-link to get back into System Settings and re-grant.

---

## 2026-05-02 — Lifecycle: prevent_exit + custom Quit menu items (#328)

Tauri 2's runtime auto-exits when the last webview window goes away. Hush's close-hide pattern (#263) hides every window on red-✕, which on Linux/Windows means the runtime hits zero visible webviews after a normal close and quits the whole app — tray icon and all. macOS dodges it via `set_activation_policy(Accessory)` on the background-launch path, but only there.

**Fix.** Intercept `RunEvent::ExitRequested` in the `app.run` callback and `api.prevent_exit()` unless a `USER_QUIT_REQUESTED` static `AtomicBool` is set. The flag is set synchronously by the tray's "Quit Hush" menu item and the macOS app-menu's Quit item via a shared `request_user_quit(app)` helper that calls `app.exit(0)` after the store. Both menu items were converted from `PredefinedMenuItem::quit` / `SubmenuBuilder::quit()` to custom `MenuItem::with_id` items wired to the helper — Tauri's predefined Quit goes through the platform-native terminate path that fires `ExitRequested` with no way for us to know it was user-initiated.

**Why a static, not AppState.** The menu / tray builders run in `setup` closures that capture `&AppHandle`, not `tauri::State`. Threading a state cell through every closure for one bool isn't worth it. A `static AtomicBool` has no coordination cost and the memory model is deterministic.

**Why the flag never resets.** Once set, the process is on its way out. There's no "consumer" pattern that needs the flag to flip back, and an explicit reset would add a window where a runtime-driven exit could sneak in between the user clicking Quit and the actual exit.

**Out of scope.** Hands-on smoke testing on Linux + Windows release artifacts to confirm the behaviour holds end-to-end. Code-side this is correct per Tauri 2's documented `RunEvent::ExitRequested` semantics, but the issue's hands-on acceptance criteria (close → tray stays + autostart-survives-relogin) needs an actual Linux / Windows desktop.

---

## Supply-chain pins (policy, last reviewed 2026-05-01)

Two production deps live outside the "stable crates.io release" baseline. Both are deliberate; both have a documented exit condition. Don't bump either without re-reading this section.

### `ort = "=2.0.0-rc.12"` (exact pin, RC)

**Why we're on an RC.** ort 2.0 stable hasn't shipped. We need 2.0 for the macOS CoreML acceleration path the wespeaker diarizer (#111) takes; 1.x is missing several execution-provider features. The `download-binaries` feature pulls vendored ORT runtime libs from `pyke.io` at build time, so we don't depend on a system-installed onnxruntime — important for the Linux distro packagers and our Windows release artifact.

**Why exact-pinned (`=`, not `^`).** rc.10 → rc.12 ate one breakage already (an upstream `ureq::tls` path change in ort-sys's build script). RCs ship API changes between versions; an unpinned `cargo update` could re-break the diarizer between PRs. The exact pin makes any version movement an explicit, reviewed change.

**Bump-when policy.** Switch to a caret pin or stable version *only when* either:
1. ort 2.0 stable ships and our compile is clean against it, **or**
2. a future rc fixes a security advisory we care about (then bump to the next exact rc, run the diarizer integration smoke, document the bump in this section).

`ndarray = "=0.17"` is exact-pinned for the same reason — ort's exposed types are generic over `ndarray::ArrayBase`, so bumping either crate in isolation breaks the build.

### `rdev` git fork pin

**Why a fork.** Narsil/rdev's upstream is incomplete on macOS 26+ for the `listen` path — the `CGEventTap` needs to be attached to `CFRunLoopGetMain()`, and Narsil's PR #147 only fixed the `send` path. fufesou's fork (the one RustDesk ships) has the listen-path fix.

**Bump-when policy.** Switch to a published crate version *only when* either:
1. Narsil ships an upstream release that completes the listen-path fix, **or**
2. fufesou publishes their fork to crates.io.

If you're considering bumping the rev (`rev = "..."`) to track newer fufesou commits, read the fork's CHANGELOG / open issues first — the rev is currently load-bearing because it predates a refactor we haven't validated. The 2026-04-30 entry on rdev::listen has the architectural reasoning.

---

## 2026-04-30 — Whisper context split for dictation vs meeting (#248)

`AppState` previously held a single `TranscribeSlot` shared between the dictation one-shot path (`stop_dictation`) and the meeting pump (`WhisperStreamingSession::drain`). Both dispatched inference via `tokio::task::spawn_blocking`, so two blocking-pool threads could land on the same `Mutex<WhisperContext>` simultaneously. Pressing the dictation hotkey during a meeting pump tick made one thread wait the full inference duration (200 ms – 2 s on Tiny / Small models) for the lock — and because the pump runs on a fixed drain interval, repeated contention pushed pump ticks past their window, accumulating audio, lengthening the next inference, and compounding latency over long meetings.

**Fix.** Two slots: `transcribe` (dictation) and `transcribe_meeting`. `model_select` loads two `WhisperTranscription` instances from the same GGUF path and writes both via `swap_transcriber(new_dictation, new_meeting)`. `SessionManager` is constructed with the meeting slot only; `stop_dictation` reads the dictation slot only. The two paths now have independent `Mutex<WhisperContext>`s.

**Why the marginal cost is small.** `whisper-rs` mmap's the GGUF file. Two `WhisperContext`s constructed from the same path share the underlying weight pages on disk; the only incremental RAM is the per-context working state (KV cache, decoder buffers — order of MB on small models, not tens of MB).

**Why not split inference parameters per path.** The split deliberately keeps the same model in both slots — diverging parameters (e.g. beam-search for dictation vs greedy for meetings) is a possible future refinement, but introducing it now would conflate "fix the contention bug" with "tune for accuracy vs latency tradeoffs", which want separate decisions and separate tests.

---

## 2026-04-30 — rdev::listen has no clean stop API; deliberate decision to leave it (#257)

The PTT listener spawns a thread that calls `rdev::listen` (fufesou fork, rev `a90dbe1172f8832f54c97c62e823c5a34af5fdfe`). The thread blocks on `CFRunLoopRun()` for the life of the process and we abandon it on quit. Issue #257 asked us to investigate clean shutdown.

**What the fork exposes.** Nothing for the `listen` path. fufesou's `src/macos/listen.rs` is ~30 lines and ends with a bare `CFRunLoopRun()`; the run-loop ref is a local that's never stored or returned. There is a private `static mut CUR_LOOP` for the *grab* path's `exit_grab()`, but it's `pub(self)` and only visible inside `grab.rs`. Upstream Narsil has the same shape — neither fork has a stop API for `listen`.

**CFRunLoopStop *is* thread-safe.** Apple documents `CFRunLoopStop`, `CFRunLoopWakeUp`, and `CFRunLoopAddSource/RemoveSource` as the thread-safe members of the API; calling `CFRunLoopStop(loop_ref)` from any thread causes the target loop's current `CFRunLoopRun()` invocation to return on the next iteration. The blocker is that we don't have the loop ref — `listen()` calls `CFRunLoopGetMain()` and discards it.

**Dedicated-CFRunLoop alternative is feasible but costly.** We'd have to inline ~40 lines of fufesou's `listen.rs` ourselves, swapping `CFRunLoopGetMain` → `CFRunLoopGetCurrent` and storing the ref in an `AtomicPtr` we own. Doing so means re-deriving the macOS 26 `CGEventTap` fix that was the reason we're on the fork — net negative until we actually need teardown.

**Process-exit behaviour is fine.** A `CGEventTap` is owned by the process; on exit the kernel reaps the Mach port and `WindowServer` removes the tap. No hung shutdown, no kernel leak, no zombie tap. The leaked thread (blocked on `CFRunLoopRun`) just goes away when the process does. Espanso, RustDesk, and every other rdev consumer ships the same "spawn-and-forget" pattern.

**Decision.** Leave `register_ptt_listener` as-is. The spawn-and-forget pattern is correct for "listener lives for the life of the app" — which is exactly what we want. If a future feature needs teardown without quit (e.g. a "disable global hotkeys" toggle that must release Input Monitoring at runtime), the cheapest path is option (b): a ~40-line internal `listen_with_handle()` that mirrors fufesou's listen.rs but captures `CFRunLoopGetCurrent()` into an `AtomicPtr<__CFRunLoop>` and calls `CFRunLoopStop` from `Drop`. Avoid switching crates — `device_query`, `global-hotkey`, and `livesplit-hotkey` all have worse macOS 26 stories than fufesou/rdev does.

Comment in `src-tauri/src/hotkey/ptt.rs::register_ptt_listener` cites this entry so a future contributor sizing up the same problem doesn't re-derive it.

References: fufesou [listen.rs](https://github.com/fufesou/rdev/blob/a90dbe1172f8832f54c97c62e823c5a34af5fdfe/src/macos/listen.rs), fufesou [grab.rs `exit_grab`](https://github.com/fufesou/rdev/blob/a90dbe1172f8832f54c97c62e823c5a34af5fdfe/src/macos/grab.rs#L78), Apple [CFRunLoop reference](https://developer.apple.com/documentation/corefoundation/cfrunloop).

---

## 2026-04-30 — SCK audio buffer migrated to lock-free `rtrb` ring (#251)

Pre-#251 the SCK system-audio path wrote into an `Arc<Mutex<Vec<f32>>>` from inside `did_output_sample_buffer`. The cpal mic path had been on an `rtrb` SPSC ring since #55 — asymmetric. If the consumer (meeting pump) wedged on a SQLite write or a long Whisper inference, the framework's libdispatch callback thread would block waiting on the mutex, putting the OS audio scheduler at risk of degrading the capture session.

**Why `rtrb::Producer` needs an `UnsafeCell`-and-`unsafe impl Sync` wrapper here.** `Producer` is `Send + !Sync` — the correct shape for an SPSC ring (two threads concurrently calling `Producer::push` would race on the head pointer). cpal's input-stream callback is `FnMut` (so it can capture the producer by `move` and call `push` directly). SCK's `SCStreamOutputTrait::did_output_sample_buffer` takes `&self`, so we need interior mutability. Wrapping in `Mutex<Producer>` would defeat the lock-free goal — the whole point of the migration. So we wrap `Producer` in `UnsafeCell` and `unsafe impl Sync` on the wrapper, with a SAFETY comment grounded in the fact that **ScreenCaptureKit dispatches callbacks serially per output handler** (libdispatch serial queue). Concrete-the-invariant tests live in `audio::screencapturekit::tests` (Send/Sync compile-check + push/drain round-trip + full-ring overflow surfacing).

**Consumer side stays `Mutex<Consumer>`.** `Consumer::read_chunk` is itself wait-free; the `Mutex` is just providing interior mutability so `drain_buffer(&self)` and `stop(self)` can both touch the consumer end. The lock is never contended in practice — the consumer side is single-threaded (the meeting pump's drain tick or the stop path, not both at once). Using `Mutex` for "give me `&mut` from `&self`" is fine when the realtime thread is on the producer side, which is where the discipline matters.

**Drain helpers shared via `pub(super)`.** `drain_consumer` and `log_overflow_if_set` lived in `audio::mod.rs` for the cpal path. Marked `pub(super)` rather than copy-pasted into the SCK submodule — same overflow-rate-limiting policy across both sources, one source of truth. The cpal mic path's existing tests (rtrb shape, drain-after-overflow logging) cover the helpers; the SCK module adds wrapper-specific tests on top.

---

## 2026-04-30 — D2 diarization decisions (#111 chain)

Six PRs (#295–#300) shipped the initial chain and three follow-ups (#303–#305) closed audit findings. Capturing the non-obvious calls so future-Claude doesn't re-derive them from the diff.

**ort over candle for the ONNX runtime.** `candle-onnx` (HF's pure-Rust path) was tempting for binary size (~5 MB vs ~50 MB) and dep transparency, but it has incomplete operator coverage and is 3–5× slower on CPU than ort. CoreML acceleration on Apple Silicon — the project's design target — is the load-bearing reason to take ort: it lets us hand inference to the Neural Engine on supported Macs. Hush already ships whisper.cpp at ~50 MB, so the incremental ORT cost is real but not prohibitive. Trade-off accepted.

**Why pump-side rolling audio buffer, not a `StreamingTranscribeSession` API extension.** D2 needs each utterance's audio to embed. The streaming session owns its sliding window internally; surfacing per-utterance audio at finals time would have meant adding a method to `StreamingTranscribeSession` and forcing every backend + test mock to grow it. We kept an independent `meeting::audio_buffer::AudioRollingBuffer` per source instead — bounded at 30 s (matches the streaming window), zeroized on drop, slices by absolute-session-time `[started_at_ms, ended_at_ms)`. Smaller diff, cleaner trait surface, mirrors the pattern `transcription::streaming::SlidingWindowState` already established for the same kind of data.

**Online 1-NN with threshold, not per-tick agglomerative.** Initial PR-D wired `cluster::cluster_with_threshold` (offline complete-link agglomerative) on each pump tick. Audit caught that this resets cluster IDs every tick — "Speaker 1" in tick N could be a different person from "Speaker 1" in tick N+1. Fixed in #303 by replacing per-tick clustering with `OnnxDiarizer::SessionClusterState`: keeps every embedding + label seen in the session, assigns each new embedding to the closest existing one within threshold (else allocates a new ID). Cluster IDs are stable for the diarizer's lifetime. Memory: ~100 KB at typical 100-utterance meetings — negligible. The offline `cluster_with_threshold` stays for one-shot use cases; the streaming matcher is what production uses.

**Mel-FB matches `torchaudio.compliance.kaldi.fbank` defaults but not bit-exact.** `diarization::features` mirrors the kaldi config wespeaker was trained on (Povey window, 25 ms / 10 ms, HTK mel scale, 80 bins, no dither for determinism). Module docstring is explicit about the gap — we trade exact reference fidelity for fewer deps and a simpler test story. End-to-end correctness (does the model emit sane embeddings?) is verified hands-on against real meetings, not against a numpy reference vector.

**SHA-256 verification both at download and at load.** Catalog has a one-line entry; download path SHA-verifies the bytes that land on disk; `OnnxDiarizer::new` re-hashes on load. Defends against a sibling app sharing the macOS account substituting the model file. ~80 ms per app boot — cheap.

**Hot-swap via `DiarizeSlot = Arc<RwLock<Arc<dyn Diarize>>>`.** AppState owns the slot; `FlagGatedDiarizer` reads from a clone every pump tick; the IPC `download_diarizer_model` writes a fresh `OnnxDiarizer` after a successful download. RwLock (not Mutex) because reads happen on every meeting tick and writes are rare. Recovery via `unwrap_or_else(into_inner)` mirrors the pattern `OnnxDiarizer::Mutex<Session>` uses — a transient panic shouldn't kill diarization for the rest of the session.

---

## 2026-04-29 — D1 EnergyDiarizer reverted to NoopDiarizer (cross-source heuristic collapses to "Speaker A")

**Supersedes the 2026-04-28 (#206) "EnergyDiarizer wired in production" entry below.**

Hands-on testing of a Meeting Mode session capturing mic + YouTube system audio showed every utterance rendering as "Speaker A", regardless of which source produced it. Investigation: `EnergyDiarizer` operates on a chronologically-merged stream of utterances with a silence-gap heuristic. With concurrent mic + system finals interleaving and no reliable inter-source gap, the heuristic collapses everything into a single label — a regression vs the source-only "You" / "Remote" labels it was supposed to refine.

Within a single source (multiple speakers sharing the user's mic), D1 was useful. Across sources it's wrong, and cross-source is Meeting Mode's whole point — Hush ships system-audio capture so the user can transcribe both sides of a Zoom / Meet / Teams call.

Shipped #243: swap production wiring to `NoopDiarizer`. The dispatch fallback in `dispatch_utterances` writes the source-derived `"mic"` / `"system"` tag, which the frontend already maps to "You" / "Remote". Source-only labels are honest: we tell the user which side of the call produced each utterance without inventing speaker IDs we can't verify.

`EnergyDiarizer` impl + tests stay on disk (still useful as a mic-only path or D1-level reference). D2 (model-based ONNX speaker embeddings, #111) is the upgrade path that can actually distinguish voices across sources.

**Lesson for future diarization work.** Single-stream diarization heuristics don't generalise to multi-stream input. The merge-then-label approach assumed the silence pattern in the merged stream would mirror what a single-microphone recording would produce; in practice the second stream fills the "silence" between the first stream's utterances, and the heuristic loses its signal. D2 needs to be source-aware (per-source embedding extraction, then matching across the union) — or run independently per source and accept that "Speaker A on mic" and "Speaker A on system" are different speakers.

---

## 2026-04-29 — macOS adds an app to the Screen Recording list only after the app actively requests SCK

> **[SUPERSEDED]** Hush no longer uses ScreenCaptureKit for system audio (replaced by `AudioHardwareCreateProcessTap` in #585). The SCK enrollment quirk described below is moot. The general TCC lesson (apps only appear in a pane after first requesting it) remains true for Microphone and Input Monitoring.

User caught this hands-on after the first end-to-end smoke of the post-#234 build: clicking **Permissions → Screen Recording → Grant in Settings…** deep-linked into System Settings → Privacy & Security → Screen & System Audio Recording, and Hush wasn't in the list. Microphone and Input Monitoring rows were both `GRANTED`, so the app was registered with TCC — just not under Screen Recording.

The cause is a documented macOS behaviour: an app only gets enrolled in a permission's pane the first time it actively requests that permission. Hush requests Microphone on first dictation Start (via cpal's input stream open) and Input Monitoring on first launch (via rdev's listener spawn, default-on since #194). It only requests Screen Recording when starting a Meeting Mode session **with system audio enabled** — and a brand-new install hasn't done that yet. Deep-linking to a list the app isn't in produces a dead end: there's no row to toggle on, and no obvious next action.

**Fix shipped:** the per-row Grant button on the Screen Recording row now calls a new IPC `prime_screen_recording_permission` *before* the deep-link. The backend helper (`audio::prime_screen_recording_permission`) calls `screencapturekit::SCShareableContent::get()` and discards the result. `SCShareableContent::get()` is the lightweight enumeration call SCK uses for "what displays/windows are shareable?" — it has the same TCC check as a full capture stream, but completes in milliseconds and doesn't allocate a stream handle. The side effect is that macOS notices the request and adds Hush to the pane (and fires the standard prompt for not-determined state). The user lands in Settings with the row visible.

**Why not start a synthetic Meeting Mode session.** That would open the DB, spawn the audio pipeline, run diarization briefly, and write a session row — heavy and visible. The shareable-content enumerate is the canonical "warm SCK" call and is what `audio::screencapturekit::ScreenCaptureKitSession::start` already does as its first line.

**Why not auto-prime on app launch.** That would prompt every fresh install with a "Hush wants Screen Recording" dialog even when the user has no intention of touching Meeting Mode. The button click is the explicit consent surface; honouring it lazily keeps the prompt deliberate.

The fix is symmetric with how Microphone "just works" today: clicking Start dictation triggers the prompt at exactly the moment the user has signalled they want the feature. Until the per-row Grant button shipped (#231), we'd been relying on the same lazy-prompt flow for SCK, but the button changed the contract — a user can now ask for the permission *without* starting Meeting Mode, and the priming call closes that gap.

Backend impl is ~5 LOC; the new IPC command is registered alongside `open_macos_privacy_pane` / `reset_macos_permissions` in `commands/macos.rs`. Frontend wires it in front of the deep-link inside `openPrivacyPane("screen-recording")` only — Microphone and Input Monitoring don't need it because their underlying request paths already fire as soon as the user uses Hush at all.

---

## 2026-04-25 — Project scaffold and stack decisions

**Tauri 2 + Svelte + TypeScript** chosen as the app framework.
- Svelte was preferred over React for a smaller JS bundle and cleaner reactivity model for the HUD overlay.
- Tauri 2 provides good access to platform APIs (global shortcuts, clipboard, notifications, autostart, updater) as first-party plugins.
- TypeScript over plain JavaScript: catches type errors at compile time, better IDE support.

**whisper-rs** chosen as the transcription backend.
- Direct Rust bindings to whisper.cpp; no FFI shim needed.
- Parakeet/FluidAudio/CoreML explicitly out of scope — see §5 of the PRD.

**cpal** chosen for audio capture.
- Cross-platform (macOS CoreAudio, Windows WASAPI, Linux ALSA/PulseAudio/JACK).
- Alternative considered: `cubeb`. Decision deferred to implementation — see TODO(#1).

**sqlx** chosen for SQLite persistence.
- Compile-time query verification.
- Async-native (tokio runtime).
- Embedded migrations via `sqlx::migrate!()`.

**rdev** chosen for push-to-talk key-down/key-up events.
- `tauri-plugin-global-shortcut` registers shortcuts but does not cleanly expose key-down vs key-up. rdev fills this gap.
- Known limitation: rdev may require Input Monitoring permission on macOS and has reduced reliability under Wayland. Documented in §10 of the PRD.

**active-win-pos-rs** chosen for foreground app detection.
- Provides app name and window title on macOS, Windows, and Linux via a single API.
- URL detection is not available and is deferred to a future release.

---

## 2026-04-25 — Black-box reimplementation discipline recorded

No Hush contributor reads VoiceInk's Swift source. Design is taken from VoiceInk's public README and observable runtime behaviour only. See §13.8 of `hush-prd.md` and `CONTRIBUTING.md`.

---

## 2026-04-25 — IPC: model loaded from env var (`HUSH_MODEL_PATH`), not settings

The whisper transcriber needs a path to a GGUF model. The proper home for that is a settings file in the platform app-data directory, written by the in-app model picker (PRD M3). For M1/M2 we don't have a picker yet, and committing to a settings schema now means migrating it later when the picker lands and exposes `quality` / `download URL` / `sha256` fields.

Decision: read `HUSH_MODEL_PATH` from the environment at app startup. If unset or the file fails to load, the app still boots — device enumeration works, `stop_dictation` returns `IpcError::TranscriptionUnavailable` with a recovery hint pointing at the env var. The eventual replacement will read from `settings.json` and the env var becomes a development override (or goes away entirely).

This keeps the M1 spike unblocked without locking us into a settings format we'd just have to redesign in M3.

---

## 2026-04-25 — Tauri `generate_handler!` does not see commands through re-exports

Hit this while wiring the IPC commands: re-exporting `pub use commands::{list_input_devices, ...}` from `ipc/mod.rs` and then writing `tauri::generate_handler![ipc::list_input_devices, ...]` produced `could not find __cmd__list_input_devices in ipc`.

The macro generates a hidden `__cmd__<name>` symbol as a sibling of each `#[tauri::command]` function in the module where the function is defined; a `pub use` re-export brings the function into scope but not the sibling symbol. Fix: refer to commands by their original module path inside `generate_handler!`. We use `ipc::commands::list_input_devices` etc. Re-exports were dropped — they were misleading because they suggested the command could be addressed from the parent module by Tauri, which is not true.

Worth knowing if anyone later splits commands across files: the macro is path-sensitive in a way that ordinary `pub use` doesn't paper over.

---

## 2026-04-25 — Recording HUD: secondary Tauri window, show/hide tracks the audio stream

PRD §9's "transparent floating HUD with level meter" is a second window — borderless, transparent, always-on-top — rather than reusing the main window in a "compact mode". The user dictates *into* another app, so Hush's main window is in the background; the HUD has to be visible while that other app is focused. Tauri's `windows[]` array in `tauri.conf.json` accepts the relevant flags (`decorations: false`, `transparent: true`, `alwaysOnTop: true`, `skipTaskbar: true`, `visible: false` to start hidden). The HUD loads `/hud` — a separate Svelte route that renders only the indicator, no dictation UI.

**Show/hide cadence:** the HUD lifecycle tracks the *audio stream*, not the transcription. So:
- `show()` runs as the **last** step of `start_dictation` (after `audio.start` succeeds) — a failed start never flashes the HUD on/off.
- `hide()` runs **immediately** after `audio.stop` returns. The transcription that follows can take seconds; by then the user has stopped speaking and is waiting on the result, not on "is Hush still listening". The HUD answer to "is the mic hot?" should track the mic, not the model.
- On error paths in `stop_dictation`, the HUD also hides — the user pressed Stop, they shouldn't see the HUD persist.

**Why no level meter in this PR:** streaming an audio level from the cpal callback (which is on the realtime audio thread, can't directly emit Tauri events) requires either a `std::sync::mpsc` channel + a Tauri-aware dispatcher task, or a shared atomic the frontend polls. Both are non-trivial refactors of `audio::CpalAudioCapture`'s worker loop and worth their own scoped change — see refactor #38 (`stop_dictation` decomposition) which lands in the same neighbourhood.

**HUD-as-second-window vs. HUD-as-mode-of-main:** folding the HUD into the main window would mean making it borderless / always-on-top during recording and restoring afterwards — twice the OS window state to juggle, and the settings panes (replacements, vocabulary, model picker) disappear during recording. Keeping a dedicated minimal `/hud` route means both surfaces are independent and stable.

The `acceptFirstMouse: false` and `focus: false` config minimises the HUD's interaction-claim — it appears, the user keeps typing in their target app, the HUD doesn't steal keyboard focus. macOS `set_focus(false)` to keep the previous app focused is platform-quirky; Tauri 2 doesn't expose a clean "show without focus" call. The current behaviour is "shows briefly, target app reclaims focus on next input" — acceptable; bake-in time before deciding if a `set_focus(false)` shim is needed.

---

## 2026-04-25 — macOS first-run: explain, don't probe; you can't read grant state

The original instinct on #22 was to add "Test microphone" / "Test Input Monitoring" buttons that programmatically trigger the OS prompts. That's how iOS / Android apps usually do permission onboarding. macOS desktop doesn't work the same way:

- **The OS prompts already fire at app startup.** rdev's `listen()` triggers Input Monitoring the first time it runs (which is on every app start, on the PTT thread). cpal triggers Microphone the first time `build_input_stream(...).play()` runs — which happens the first time the user clicks Start Recording, not at startup. By the time the welcome modal renders, at least the Input Monitoring prompt has already fired.
- **There's no API to read whether a permission was granted.** macOS deliberately doesn't expose this — it's a privacy stance. Apps can either try and observe failure, or rely on the user to grant.

So the welcome's job becomes "explain what already happened (or is about to happen) and tell the user how to recover if they declined" — not "trigger prompts in a curated order". The deep-link buttons (`x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone`, `Privacy_ListenEvent`) open System Settings on the right pane; the user grants or denies there.

**Implementation notes:**

- `open_macos_privacy_pane(target)` is a Tauri command rather than the frontend invoking the URL via `tauri-plugin-shell` because the shell plugin's capability config doesn't whitelist `x-apple.systempreferences:` schemes by default and adding it would broaden permissions further than needed. The command takes an enum-shaped string (`microphone` / `input-monitoring` / `screen-recording`) and rejects anything else, so a frontend bug can't pivot it into an arbitrary `open` launcher. (`accessibility` was previously on the whitelist but Hush legitimately doesn't request that permission; removed in #273.)
- The flag is just a settings-table row (`first_run_completed=true`), not a typed wrapper. Reuses the K/V infra; one new command per get/set.
- The welcome renders on **all** platforms, not just macOS. Linux / Windows users see the explanation copy and click "Got it"; the deep-link buttons no-op via the cfg-gated backend command. The cost-of-platform-gating tradeoff in this entry's original argument has since been resolved differently — `@tauri-apps/plugin-os` was added in #286 for the deprecated-`navigator.platform` swap, so a future "hide the welcome on non-macOS" gating would be cheap to add via `await platform()` if that ever becomes desired.

If we ever want to *avoid* triggering Input Monitoring at startup until the user has dismissed the welcome — e.g. so the prompt fires *after* the welcome is visible to provide context — that requires gating `hotkey::register_ptt_listener` on the first-run flag. Possible follow-up if the prompt-fires-with-no-context UX turns out to be a real problem in user testing.

---

## 2026-04-25 — Audio test fixture: env-var path + `hound` for WAV parsing, no committed bytes

Two design choices on the file-based integration test in `tests/audio_fixture.rs`:

1. **Fixture is contributor-supplied via env var, not committed.** A recognisable-transcript WAV is a few hundred KB to a few MB. Committing one bloats clone size for a test that's `#[ignore]`d and that most contributors will never run; LFS adds quota / setup friction. The test reads `HUSH_TEST_AUDIO` and skips with a clear message if the file doesn't exist. The fixtures directory ships only a README documenting recommended sources (JFK speech excerpt, LibriVox, Common Voice). Trade-off: non-trivial first-run setup, accepted because the test is dev-loop tooling rather than a CI gate.

2. **`hound` over a hand-rolled WAV parser.** A minimal PCM-only parser is ~30 lines. `hound` is ~5 KB of crate source, dev-dep only, and handles every sample-format whisper-rs's contributors might throw at us (16-bit / 24-bit int, IEEE float). Test stability is more valuable than the dep saving here. `hound` is also stable; it hasn't shipped a breaking change in years.

The test is structured so it's easy to extend with a `(b)` loopback-capture variant when system-audio capture (#33) lands. The same fixture file goes through the speakers, gets captured via the loopback source, and runs through the whole pipeline rather than just the inference half.

---

## 2026-04-25 — Frontend per-card download state: two `Map<id, …>`s, swap-don't-mutate

The auto-download UI has four states per card — idle, downloading-with-progress, failed (with retry), downloaded — and several events fire concurrently (multiple cards can be downloading at once if the user clicks Download on Tiny then Base). Two design choices worth pinning:

1. **Two parallel `Map<id, …>`s** rather than embedding the per-card status into the catalog array. `downloading: Map<id, {received, total}>` and `downloadFailed: Map<id, message>`. Lookup is O(1) per event; the catalog stays the source of truth for the static metadata; the catalog's order doesn't matter for routing an event to the right card. The alternative — folding `downloadStatus` into each `ModelCard` — would couple the static catalog to transient download state and force a `models = models.map(...)` allocation on every progress chunk (Svelte's reactivity doesn't notice mutations on individual array elements without that).

2. **Swap, don't mutate.** Svelte 5 runes don't observe internal mutations on built-in `Map`s — `downloading.set(...)` doesn't trigger reactivity. Every update wraps in `new Map(prev)` and reassigns. Slightly wasteful at the per-chunk progress firehose (we do a full Map clone per progress event), but the Map only has one entry per concurrent download (rare to be > 2) and the chunks come at ~tens of times per second, so the realistic cost is negligible. The alternative would be a `$state.raw`-flavoured opt-out, but the explicit-swap pattern is more obvious to a future contributor reading the file.

Cancel-flow goes through the backend rather than touching the frontend state directly: `cancelDownload` calls `model_cancel_download`; the backend fires a `model:download-failed` with a "cancelled" message; the existing failed-event handler updates the Maps. That keeps a single state machine driving the UI.

---

## 2026-04-25 — Model auto-download: SHA-required, .part + atomic rename, reqwest+rustls

Three decisions worth pinning while the auto-download is the freshest network surface in the codebase:

1. **No trust-on-first-use.** The download orchestrator refuses to start when the catalog's `sha256` is empty, surfacing a clear "auto-download is not yet enabled — download manually for now" error. The temptation was to compute-and-store the hash on first download, but that defeats the purpose of SHA verification (we'd be trusting the same response we want to verify). Hashes get filled in by contributors who verify against the upstream MANIFEST out-of-band; #41 tracks the verification work.

2. **`.part` file + atomic rename.** Bytes stream into `<filename>.part`; a successful complete-and-verify flow renames it to `<filename>`. Failure / cancel deletes the `.part`. Crash-safety: a half-finished download never looks like a complete file to the picker. Drop the file handle before unlinking — Windows blocks unlink on an open handle.

3. **`reqwest` + `rustls-tls`, not `ureq` and not OpenSSL.** Smaller-binary alternatives existed:
   - `ureq` is sync; the streaming-progress flow needs an async story to share the tokio runtime with sqlx and tauri.
   - reqwest with `default-features = false` + `rustls-tls` + `stream` skips the OpenSSL/native-tls baggage. Cross-platform binary, one set of TLS roots (`webpki-roots`).
   - The transitive dep cost is real (~10 crates beyond what we already had) but the alternatives all involved per-platform build complexity we haven't paid yet.

`wiremock` (dev-dep only) drives the test suite end-to-end against a local mock server — happy path, SHA mismatch, cancel, empty-SHA gating, progress callback monotonicity. No real Hugging Face round-trips in CI.

---

## 2026-04-25 — CSP disabled for the dev minimum, document the trade

> **Update (2026-04-30, #282 / #267):** the trade flipped. `csp:` is now set to a strict policy: `default-src 'self' tauri'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' asset: data:; font-src 'self' data:; connect-src ipc: http://ipc.localhost tauri: https://api.github.com`. `'unsafe-inline'` on `style-src` is required by SvelteKit's scoped-CSS injection (verified — every other source restricts to `'self'`). `connect-src https://api.github.com` is the only outbound network host whitelisted (the updater probe in `updater::check_for_updates`); model downloads go through Rust's reqwest, not the webview, so they don't need a CSP allowance. Any new outbound host the webview talks to needs a `connect-src` edit. The original argument below stays as historical context for what the trade-off looked like before the policy was filled in.

Tauri's `csp: null` (in `src-tauri/tauri.conf.json`) opts the webview out of Content-Security-Policy enforcement. The round-1 security review flagged it as `[MED]` for an eventual public release — without CSP, an XSS via user-supplied content in the webview would have less defence. For where Hush is right now this is acceptable:

- The frontend never injects user-supplied HTML (`innerHTML` is unused; everything binds via Svelte's escape-by-default pipeline).
- All content rendered comes from local IPC, not the network.
- The minimal frontend is ~700 lines of straightforward Svelte; the audit surface is small.

The trade-off becomes meaningful when the frontend grows or when we ship to non-technical users. At that point we should:

1. Define a strict default CSP (`default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'` — Svelte's hashed inline styles need the latter without nonces).
2. Test against any new IPC outputs that contain user-controlled text.
3. Update `tauri.conf.json` to set the CSP string.

Tracked separately in #23 alongside the `tauri-plugin-opener` removal that landed at the same time.

---

## 2026-04-25 — Model picker: static catalog + settings-backed selection, no hot-swap in v1

The picker is the M3 settings surface for choosing among Whisper sizes (tiny → large-v3 per PRD §6 / §9). Three decisions worth recording:

1. **Static catalog, not a discovered list.** Whisper's model line-up is fixed by upstream; there are exactly five variants and that's all the picker needs to know about. Hardcoding metadata (size, speed/accuracy ratings, description, expected filename) lets the picker render every card up front, including greyed-out ones for models that aren't downloaded yet — no network round-trip, in line with the "no cloud" privacy claim. The `default_model() == whisper-base` choice mirrors PRD §6 explicitly; a test pins this so renaming the default forces an update to the PRD.

2. **`Arc<dyn Transcribe>` is *not* swapped at runtime.** Selecting a new model writes `selected_model_id` to settings; the transcriber for the running process stays whatever it was at startup. The frontend surfaces a "restart Hush to use the new model" hint after a successful selection. Hot-swap (taking the existing `state.transcribe: Option<Arc<dyn Transcribe>>` behind a `Mutex` and constructing a new `WhisperTranscription` on the fly) is doable but expanded the PR's blast radius significantly — the M3 picker ships shippable today and hot-swap is its own follow-up. The trade-off: a slightly worse UX on first model change in exchange for keeping the type system honest about transcriber identity within a process.

3. **Auto-download is deferred.** The PRD §9 lists "download progress, SHA verification, disk-usage display" as in-scope for v1, but the bulk of that work is HTTP infrastructure (reqwest, progress events, cancel/retry, integrity checks) that's worth its own PR rather than tacked onto the picker UI. The picker ships as "select among models you've placed in `<app-data>/models/` yourself" — greyed-out cards include the expected filename so the user knows what to download. The next picker PR can add auto-download without changing the public surface.

Resolution path at startup is a layered fallback (settings → legacy `HUSH_MODEL_PATH` env var → none). Step 2 keeps the M1/M2 dev workflow working until a contributor explicitly opens the picker. Once the picker is the primary path, the env var becomes a development override and eventually goes away.

---

## 2026-04-25 — Vocabulary biasing: comma-list prompt, not free-form prose

`format_vocabulary_prompt` builds a comma-separated list (e.g. `"Hush, Tauri, whisper.cpp"`) and hands it to whisper.cpp's `set_initial_prompt`. The alternative — letting users write prose like *"The user is talking about Hush, a dictation app built with Tauri…"* — is tempting because it mirrors how OpenAI-style "system prompts" feel familiar, but it's the wrong tool here:

- Prose biases the *content* of the transcription, not just the vocabulary. Whisper interprets the prompt as "this is what came before" and may insert recovered topic words. A bare list reads to the LM as "these tokens are likely tokens to expect", which is exactly the bias we want.
- A list is composable from individual rows the user manages in the UI. Prose is one big text blob with all the editing-friction problems that implies.

Other notable decisions in the formatter:

- **Case-insensitive deduplication** keeping the first spelling. The user's first entry is the canonical one (proper-noun typing usually nails the right capitalisation on the first try); subsequent variants are silently dropped.
- **Character cap at `MAX_PROMPT_CHARS = 1024`** rather than token cap. Whisper.cpp tokenises and truncates at ~224 tokens internally; 1024 chars stays comfortably under that without us having to ship a tokenizer dep just for the cap. Truncation happens at term boundaries, never mid-word.
- **`Transcribe::transcribe_with_prompt` has a default impl** that delegates to `transcribe(audio)`, so the IPC layer can call the prompt-biased path unconditionally — backends that don't support biasing (none today, but the trait is forward-looking) just ignore the prompt without forcing every call site to branch.

Vocabulary load failure is non-fatal in the same way replacement load failure is: the dictation pipeline keeps running with an empty prompt and logs at `error` level. Better than a hard error blocking the user's clipboard.

---

## 2026-04-25 — Replacement rules: literal substrings, not regex; failure is non-fatal

`apply_replacements` runs literal `str::replace` calls in `(sort_order, id)` order. Two decisions worth recording:

1. **Literal, not regex.** A regex engine would let users do word-boundary matches, anchors, capture groups — power-user features that nobody is asking for yet. The cost of pulling in `regex` (a non-trivial dep) for a list that realistically has 5–20 entries isn't justified. If users start asking, the upgrade path is an enum on the rule (`Mode::Literal | Mode::Regex`) rather than swapping wholesale; backwards-compatible. Documenting the literal default in the module header so users don't get tripped up by metacharacters in their rules.

2. **Replacement-load failure demotes to "no rules applied", not a hard error.** `stop_dictation` already gives the user the transcribed text on the clipboard; a failed `SELECT * FROM replacements` shouldn't block that. We log at `error` level and apply the empty-rules identity. If this turns out to matter in practice (rules silently not applying for hours) we add a "rules failed to load" banner driven off settings (M3) — but for the first cut, "the user's text is the deliverable" trumps "the user's preferences are the deliverable".

Empty `find_text` is silently skipped (a `str::replace("hello", "", ...)` would wedge the replacement between every byte boundary — never the user's intent). Empty `replace_text` is the explicit delete path. Both are tested.

---

## 2026-04-25 — History repository: trait-at-the-boundary, fire-and-forget insert from `stop_dictation`

The `HistoryRepository` trait sits at the storage boundary so the IPC layer holds an `Arc<dyn HistoryRepository>` and tests can mock at that seam without spinning up SQLite. The concrete `SqliteHistoryRepository` is one borrow on top of the pool from `SqliteDatabase` (#18) — every method is a single round-trip query, no caching, no domain logic. Future per-domain repos (dictionary, settings) will follow the same shape.

The auto-insert from `stop_dictation` is fire-and-forget via `tauri::async_runtime::spawn`. Two reasons:

1. **The user already has the text on the clipboard**, which is the actual deliverable. If the history insert fails — disk full, db corrupt, anything — surfacing that as a hard error from `stop_dictation` would block the user from getting on with their work for a strictly secondary feature. Logged at `error` level so failures are still observable.
2. **`stop_dictation` is the latency-sensitive command in the app.** The Whisper inference call dominates, but tacking on an awaited insert pushes "ready to paste" out by another DB round-trip. Spawning keeps the user-perceived latency unchanged.

Trade-off: the row may not be visible the instant `stop_dictation` returns, so the frontend's history refresh fires after a 150ms delay (slow disk could miss the new row otherwise). On a real machine this is invisible. If history ever becomes load-bearing for downstream features (e.g. a "rerun last transcription" command), this should be reconsidered.

---

## 2026-04-25 — `AppState::build_default` moved into `setup` so it can resolve the platform app-data dir

Originally `AppState::build_default()` was sync and called at the top of `run()`, before the Tauri builder. That worked when state didn't need a filesystem path, but the SQLite-backed history needs the platform app-data dir, which is only available via `tauri::App::path().app_data_dir()` — i.e. inside the `setup` hook.

Refactor: `build_default` is now async and takes `&Path`. `lib.rs::run`'s `setup` hook resolves the path, calls `tauri::async_runtime::block_on(AppState::build_default(&db_path))`, then `app.manage(state)`. Hotkey registration moves with it.

Side effect: error handling at startup is now strictly fail-stop — if the database can't open (perms, disk full, corruption) the app exits cleanly with the error in the dev console rather than starting in a half-working state. Acceptable trade for M3; if we ever want graceful degradation here we'd need to either move history behind an `Option` like the transcriber, or surface a "history unavailable" mode in the UI.

---

## 2026-04-25 — FTS5 search: wrap user input in quotes, escape any embedded quotes

SQLite FTS5's `MATCH` syntax interprets the query as an expression, not a phrase. A user typing `foo OR bar` would get FTS5's logical-OR rather than a search for the literal string. Worse, an unbalanced double quote (`said "hi`) returns a confusing parser error rather than zero rows.

Fix is small: wrap the user's input in double quotes (treats it as a phrase), and double any embedded quotes (FTS5's escape). Result is a literal-substring "find this" feel, which matches the UI's "type to filter" pattern. If we ever want operator support we'd add a separate "advanced query" mode rather than letting FTS5 syntax leak into the basic search box.

---

## 2026-04-25 — Error classification: structural at the call site, not heuristic on merged strings

First cut of `stop_dictation` collapsed the audio-stop and Whisper-transcribe calls into a single helper that returned `anyhow::Result<String>`, then ran a `classify_pipeline_error` over the resulting message to pick between `IpcError::Audio` and `IpcError::Transcription`. The classification was substring matching on words like "device", "recording", "model", "buffer". It worked for the cases I had in mind and was obviously fragile for the ones I hadn't yet seen — a code review caught a real misroute (a Whisper error mentioning "device" being labelled an audio failure).

The fix turned out to be a deletion, not an upgrade: split the two calls back out in the Tauri command body, `map_err` each one to its proper variant at the source. The pipeline helper still exists as a test-only convenience for unit tests that want a one-shot `audio → transcribe` against mocks; it just isn't on the production path. Removing the heuristic also let the per-variant Display strings stay accurate: `audio: ...` actually corresponds to the audio layer.

Lesson is generic but worth re-stating: when you find yourself classifying an error after the fact, it's usually a sign the merge happened too early. Keep the boundary explicit and let the type system carry the layer information.

---

## 2026-04-25 — Frontend dispatches recovery-shaped copy from `kind`, backend stays terse

The Rust `IpcError` carries a short `Display` string per variant — engineering-shaped, not user-shaped (e.g. "transcription not available — set HUSH_MODEL_PATH and build with --features whisper"). Earlier the frontend just rendered `${kind}: ${message}`, which dumped that string verbatim into the UI. Code review (rightly) called this out: a non-developer user has no idea what `HUSH_MODEL_PATH` is.

Decision: keep the backend `Display` strings as developer-shaped diagnostics — they're what shows up in `tracing::error!` and the dev console — and have the frontend's `formatError` function map `kind` to user-shaped recovery copy. The frontend is where product voice lives; the backend is where engineering precision lives.

This means localisation, when we get to it, lives in the frontend (i18n on the `kind` switch) rather than in the Rust `thiserror` derives. Cheaper and more consistent.

---

## 2026-04-25 — DB: WAL + Normal sync, foreign keys forced ON, migrations run on construction

Three SQLite knobs that are easy to skip and expensive to revisit:

1. **Journal mode = WAL.** Default `DELETE` mode serialises readers behind a writer. Hush concurrently reads (history view, settings hot-reload) while a transcription is being inserted; `WAL` lets readers proceed against the previous snapshot. Cost is two sidecar files (`-wal`, `-shm`) next to the db, irrelevant for a desktop app.

2. **Synchronous = Normal.** Default `Full` does an extra fsync per commit, which is overkill for a dictation history that the user re-derives if it's lost. `Normal` is durable across app crashes (the case we care about), only at risk on power loss between commit and the next checkpoint.

3. **`PRAGMA foreign_keys = ON` per connection.** SQLite's foreign-key enforcement is opt-in per *connection*, not per database file (long-standing default-off footgun). We set it via `SqliteConnectOptions::foreign_keys(true)` so every pool connection enforces referential integrity, rather than relying on each call site to remember.

Migrations are run inside `SqliteDatabase::open` and `open_in_memory`, so callers cannot accidentally use an unmigrated pool. Embedded via `sqlx::migrate!("./migrations")` so the binary carries the schema and we don't have to ship the migration files alongside the bundle.

In-memory pool pinned to `max_connections=1` because SQLite's `:memory:` is per-connection: with the default sizing each pool connection would get its own empty database and the migration would only land in one of them. Took me a moment to figure out the first time I tried it.

---

## 2026-04-25 — Hotkey emits an event, frontend toggles state

Two ways to wire a global hotkey to the dictation pipeline:

1. **Backend-driven**: hotkey handler runs the audio + transcription pipeline directly, then emits the result to the UI as an event.
2. **Frontend-driven**: hotkey handler emits a "you pressed it" event; the frontend's existing recording-state machine decides whether this press starts or stops, and invokes the existing IPC commands.

We picked (2). The frontend already owns `recording`, `busy`, and `selected device` state; route #1 would have meant duplicating that bookkeeping in the backend (and re-emitting "started"/"stopped" events to keep the UI in sync), or accepting drift between two sources of truth. Route #2 keeps a single state machine per concern: the backend owns the audio session and the model handle; the frontend owns the UI's view of "are we recording?". The hotkey is an accelerator, not a parallel pipeline.

The cost of (2) is that hotkey-driven dictation only works when the frontend window/process is alive. For M2 that's always — Tauri keeps the webview alive even when minimised — so the constraint is invisible. If we ever want headless / tray-only dictation, the standalone helpers in `ipc::*` are still available and we can lift the orchestration into the backend at that point.

---

## 2026-04-25 — IPC error shape: tagged-content enum, not free-form strings

The frontend needs to react differently to `audio: device gone` (let user pick a different device) vs. `transcription not available` (point at `HUSH_MODEL_PATH`). Returning `Result<T, String>` from a Tauri command works but forces the frontend to substring-match — fragile and hostile to localisation.

We chose `#[serde(tag = "kind", content = "message", rename_all = "kebab-case")]` on a `thiserror`-derived enum. The frontend gets `{ kind: "transcription-unavailable" }` (no `message` field for unit variants) or `{ kind: "audio", message: "..." }`. Switch on `kind`; render `message` as the technical detail. Same shape will scale to history / dictionary / settings commands as #6, #7, and the others land.

---

## 2026-04-25 — Audio capture: capture at native format, defer downmix and resample

The original module sketch said "open the selected device at 16 kHz mono PCM (whisper.cpp's required format)." That is aspirational. Almost no consumer microphone exposes 16 kHz mono natively; CoreAudio, WASAPI, and ALSA all routinely refuse to honour an arbitrary sample-rate request and return `StreamConfigNotSupported`. Negotiating a format at capture time means we either fail to open the stream on common hardware, or we silently fall back to a different format the caller does not know about.

Decision: capture at the device's `default_input_config()` and surface both the f32 PCM samples and the `CaptureFormat` they were captured in. Channel downmix lives in `audio::format` (pure-logic, unit-tested). Sample-rate conversion to 16 kHz will land alongside the transcription work (TODO(#2)) — `whisper-rs` can be evaluated for whether it accepts a native-rate buffer or whether we need `rubato`/equivalent in front of it. Either way, the audio capture layer does not need to know.

This keeps the audio module's contract narrow ("hand back what the device gave us, in a uniform sample type") and pushes format negotiation to the layer that can recover from it without losing the buffer.

---

## 2026-04-25 — Whisper transcription: linear resampler over `rubato`

Whisper.cpp expects 16 kHz mono f32 PCM but consumer microphones almost
universally capture at 44.1 or 48 kHz. The transcription pipeline must
resample. Two viable options for M1:

- `rubato`: production-quality crate offering windowed-sinc, FFT-based, and
  polyphase resamplers. Higher fidelity, but pulls in `realfft`/`rustfft`
  and adds a few hundred KB of compiled code.
- A handwritten linear-interpolation resampler in `transcription::resample`.

Picked the linear resampler. Reasons in priority order:
1. Whisper's first stage is a mel spectrogram with 25 ms windows and 10 ms
   hops; aliasing artifacts above ~4 kHz are smoothed away by the mel
   filterbank long before they reach the encoder. Linear-vs-sinc accuracy
   delta on dictation audio is within measurement noise.
2. Zero additional dependencies on the default-feature build. Contributors
   without cmake can still run the resampler tests; CI without the
   `whisper` feature stays cheap.
3. The public surface is `resample_to_mono(samples, in_rate, out_rate) ->
   Vec<f32>`. If a future quality regression test shows linear is the
   bottleneck, swap the body for `rubato::FftFixedIn` without touching any
   caller.

Not addressed: pre-filter for downsampling. With 48 → 16 kHz, energy in the
8–24 kHz band aliases. For human speech (essentially no useful information
above 8 kHz) this is benign. If we ever target non-speech audio, this
assumption breaks and it is reason enough to swap in `rubato` regardless.

## 2026-04-25 — Whisper model path: caller-provided in M1, auto-download in M3

`WhisperTranscription::new` takes a `PathBuf` rather than auto-downloading
a model. Two reasons:

1. M1 is a transcription spike — we want to confirm the Rust path works
   end-to-end before building model-management infrastructure. Mixing
   "does whisper-rs work?" and "does our download/SHA-verify/caching pipe
   work?" into one milestone hides which side fails when something breaks.
2. The auto-download flow needs UX decisions (default model? download
   progress? failure recovery? disk-quota messaging?) that belong with the
   model picker UI, which lands in M3.

`new` does pre-check `Path::exists()` so the user gets a clean error rather
than whatever whisper.cpp surfaces from its file open path.

## 2026-04-25 — Whisper inference: `Mutex<WhisperContext>`, fresh state per call

`whisper.cpp` is not thread-safe across `whisper_full` calls on the same
context, so we hold a single `Mutex<WhisperContext>`. Dictation is
fundamentally serial (one mic, one user) so the lock is never contended in
practice; the mutex exists to keep the type `Sync` for IPC use, not for
real concurrency.

`whisper-rs` 0.14 separates context from state: the context holds the
model weights, the state holds the decoder KV cache. We create a fresh
state per `transcribe()` call rather than reusing one — this both avoids
cross-utterance attention-state leakage and keeps the per-call code path
simple. Cost is small (state allocation is microseconds against a
multi-second inference).

Thread count is fixed at 4 rather than `num_cpus`-based. Whisper.cpp
scales sub-linearly past ~4 threads on Apple Silicon and modern x86, and
we'd rather not fight the UI thread on small machines. The model picker
(M3) will expose this as a setting.

## 2026-04-25 — `cpal::Stream` is `!Send`: dedicated audio worker thread

`cpal::Stream` is `!Send` on most backends — its backing audio thread keeps thread-locals pointing at the host that constructed it, and moving the stream across threads is undefined behaviour on at least the macOS and Windows backends. That rules out the obvious `Mutex<Option<Stream>>`-on-the-public-struct pattern, because the stream cannot be sent across an `&self` boundary that is itself `Send + Sync`.

Pattern adopted: `CpalAudioCapture` spawns a long-lived worker thread (named `hush-audio`) that owns the stream. Public methods send `Cmd::{Start, Stop, ListDevices, Shutdown}` over an `mpsc` channel and block on a one-shot reply channel. The host is also constructed on the worker thread for the same thread-local-state reason.

The `mpsc::Sender` is wrapped in a `Mutex` because it is `Send` but `!Sync`, and the trait API is `&self`. Lock contention is irrelevant on the control plane (start/stop is human-paced) and the audio callback never touches it. If the control plane ever becomes hot we can move to `crossbeam-channel` (Sync sender) without a public-API change.

A lock-free `is_recording: AtomicBool` lives outside the channel so callers can poll without a round-trip; `Acquire`/`Release` ordering pairs the flag with the worker's session state.

## 2026-04-25 — PTT via `rdev`: dedicated thread, frontend dispatch, X11-only on Linux

Implementing push-to-talk surfaced three platform realities worth recording before the next person reaches for `rdev`.

**`rdev::listen` is blocking, by design.** The 0.5 API is `pub fn listen<T>(callback: T) -> Result<(), ListenError> where T: FnMut(Event) + 'static`. It installs a low-level OS hook (CGEventTap on macOS, an X11 grab on Linux, a Windows hook on Windows) and pumps events from the calling thread for the rest of the process. There is no `stop()`; the only exit is process termination or a hook error. Implication: PTT must run on a dedicated `std::thread` whose only job is forwarding events. We give the thread a name (`hush-ptt`) and detach it; reaping is handled by process exit. We capture the `AppHandle` by clone-and-move into the listener closure (`AppHandle` is `Clone + Send`, internally an `Arc`), which is the supported way to bridge into rdev's `'static` callback bound.

**macOS Input Monitoring permission is a silent failure mode.** On first call to `listen`, macOS prompts the user to grant the binary Input Monitoring (and in some configurations Accessibility) access. Until granted, the OS silently drops events: `listen` returns `Ok(())` and the callback is simply never invoked. There is no programmatic way to detect denial — no API to query the permission state from a sandboxed Tauri build, no error from `rdev`. Documented in the module header and the README so contributors running locally know to look at System Settings → Privacy & Security if PTT seems dead. The toggle hotkey going through `tauri-plugin-global-shortcut` does *not* require Input Monitoring (it uses `RegisterEventHotKey`, a higher-level Carbon API), which is the main practical reason the toggle ships first.

**Wayland is not supported by `rdev` 0.5.** The Linux back-end is X11-only; under most Wayland compositors `listen` exits immediately with `ListenError::EventTapError` (or similar). Per PRD §10 we document GNOME-on-X11 as the supported initial Linux target. Failure mode: we log at `error` level from the listener thread and continue. The toggle hotkey (which goes through the compositor's portal) and button-driven dictation both keep working — losing PTT is degraded service, not an outage. Long-term we will need a separate Wayland implementation (likely the `XdgGlobalShortcuts` portal extended to expose key-up, or a compositor-specific binding); that lives behind a future issue.

**Surprising: rdev does not expose left vs right modifier keys uniformly with the global-shortcut crate.** rdev distinguishes `ControlLeft` from `ControlRight`, `AltGr` from `Alt`, `MetaLeft` from `MetaRight` etc. — at the OS hook layer, those are physically different keys. We deliberately exposed both halves in our `PttKey` enum so a user binding "RightControl" gets only the right-control key and not the left, which matches what hold-to-talk users expect from Discord/OBS. The parse layer accepts common aliases (`Cmd`, `Win`, `Super` for `LeftMeta`; `RCtrl` for `RightControl`; `Option` for `LeftAlt`) so users typing the names they reach for first don't get a parse error.

**Frontend dispatch mirrors the toggle hotkey.** Same rationale as 2026-04-25 — Hotkey emits an event, frontend toggles state: a single source of truth for `recording`/`busy` lives in the UI. PTT just emits two events instead of one (`hotkey:ptt-press` and `hotkey:ptt-release`); the frontend gates each on the existing flags. A spurious release event (e.g. user released a key after the press was ignored because the UI was busy) is a no-op, not an error. Auto-repeat on X11 sends repeated KeyPress events but no spurious KeyRelease, so the `if (recording) return` guard on press handles it without extra dedupe logic.

## 2026-04-25 — `stop_dictation` decomposition: keep the orchestration shape, extract named helpers

Round-3 architecture review flagged `stop_dictation` (~95 lines, eight inline steps) as the longest IPC command and the obvious next refactor candidate. The decomposition (#38) is intentionally conservative: it does not introduce a builder, an executor, or a state-machine type. It splits the linear sequence into named functions whose names *are* the documentation.

**What stayed inline.** Calls that need an `AppHandle` — `crate::hud::hide`, `app.clipboard().write_text`, `app.notification().builder()…show()` — stay close to the orchestration. Wrapping them in helpers adds a parameter without removing complexity, and it makes the test seam worse (every helper would need a mock `AppHandle`, which Tauri does not really expose). The two side-effect wrappers we *did* extract (`write_to_clipboard`, `fire_ready_notification`) take `&AppHandle` because the helper boundary captures the success/failure policy, not because it improved testability.

**What moved into helpers.** Everything that operates on `&AppState` alone: `stop_audio_capture` (audio.stop + error mapping), `load_vocabulary_prompt` and `load_replacement_rules` (best-effort repository reads), `take_foreground_snapshot` (mutex pop), `spawn_history_insert` (fire-and-forget). These are now testable without spinning up Tauri — the existing `Noop*` mocks in `ipc::tests` cover the trait bounds, and the new helper tests pin the structural error mapping that `stop_dictation` previously got "for free" from being one big function.

**Why the helper boundaries matter for the bug we already fixed.** The original `stop_dictation` had a fragile substring-classifier that routed "device" errors to the audio variant regardless of which layer produced them; the structural fix mapped each backend's error to its own `IpcError::*` variant at the call site. The decomposed version preserves that — `stop_audio_capture` returns `IpcResult<CapturedAudio>` with `IpcError::Audio` on failure, so the audio classification is *still* at the boundary, just expressed once instead of inline. A unit test (`stop_audio_capture_maps_backend_error_to_ipc_error_audio`) now pins that mapping so a future refactor can't accidentally collapse it.

**Best-effort vs fatal stays explicit.** Each helper's doc comment names which it is. `load_vocabulary_prompt` and `load_replacement_rules` swallow errors with `tracing::error!` and demote to the no-prompt / no-rules path. `write_to_clipboard` is fatal because the clipboard is the user's actual artefact; `fire_ready_notification` is best-effort because notifications fail on Linux without a daemon and the user is still in good shape without the toast.

**Function size as a goal vs. function size as a symptom.** Round-3 flagged the line count, but the actual problem `stop_dictation` had was that its eight steps were not labelled. After decomposition the body is a flat sequence where each line *says* what the step does (`load_vocabulary_prompt(&state).await`). The reader can drill into the helper if the why matters; otherwise they read the names and move on. That's the win — not "fewer lines", but "fewer lines of mental load to follow the happy path". Future architecture refactors (#37 AppStateBuilder, #36 repository abstraction, #39 dictionary split) should follow the same pattern: extract the boundary, name it, test it independently — don't introduce abstractions that aren't earning their keep.

## 2026-04-25 — Reqwest redirect policy: host allowlist beats hop limit

Round-4 security review (#49) flagged that the AppState's reqwest client used reqwest's default redirect policy — up to 10 hops, any host, no allowlist. SHA-256 verification still catches a swapped file, but only *after* the bytes have been transferred to the wrong server. A BGP hijack of `huggingface.co` could redirect bytes through an attacker host before we noticed.

**Why a host allowlist, not just a smaller hop count.** Capping at 1 or 0 hops would break the legitimate Hugging Face flow: `/resolve/main/<file>` redirects to `cdn-lfs.huggingface.co`, which sometimes redirects again to a signed S3-style URL still on the CDN. Two hops is normal; four leaves headroom for re-architecture. The actual attack we're defending against isn't hop count, it's *cross-origin redirect* — so the policy enforces both: ≤ 4 hops AND every hop's host must be `huggingface.co` or a subdomain.

**Suffix-match trap.** First draft used `host.ends_with("huggingface.co")` — accepts `evilhuggingface.co` because it literally ends with that string. Fixed with `host == "huggingface.co" || host.ends_with(".huggingface.co")` (note the leading dot). Pinned by a unit test (`huggingface_host_predicate_rejects_typosquats_and_lookalikes`) that exercises both the exact-match and suffix-with-dot paths plus the obvious lookalike `huggingface.co.attacker.com`. The dot-prefix matters; future contributors changing the predicate will hit the test before they hit a CVE.

**Why the predicate is its own function.** `reqwest::redirect::Attempt` has no public constructor, so the closure passed to `Policy::custom` is not directly testable. Extracting just the host-decision logic into `is_huggingface_host(host: Option<&str>) -> bool` keeps the security check unit-testable while leaving the closure as thin glue. Same pattern as the test seam everywhere else: the load-bearing pure logic is a free function; the framework boundary is a one-liner.

**SHA verification and redirect policy are layered, not redundant.** SHA catches *what* arrived (was the model swapped?). The redirect policy controls *where the bytes went* (did we accidentally upload our request headers + IP + user-agent to an attacker?). Both matter; neither replaces the other. Documented in the inline comment so a future "we have SHA, do we still need this?" review has its answer in front of it.

## 2026-04-25 — Removing the unused `tauri-plugin-shell`

The shell plugin was registered in `lib.rs` and present as `@tauri-apps/plugin-shell` in `package.json` but had zero call sites in either Rust or TS code. `open_macos_privacy_pane` — the obvious candidate to use it — uses `std::process::Command::new("open")` directly with hard-coded whitelisted URLs. Removing the plugin entirely tightens the capabilities surface (no `shell:allow-execute` exposure), shrinks the dep tree, and removes a future-PR footgun where a contributor reaches for the plugin and accidentally widens the privilege envelope.

Lesson: when a security review's recommendation is "scope the capability tighter for X plugin", first check whether X plugin is actually used. If it isn't, removing it is strictly better than scoping it — fewer moving parts, fewer dep updates to track, fewer surfaces for a future PR to abuse. We followed the same pattern in PR #31 with `tauri-plugin-opener`; cleaning up the npm-side leftover (`@tauri-apps/plugin-opener` in `package.json`, never imported) belonged in this PR rather than its own.

## 2026-04-25 — HUD level meter: AtomicU32(f32::to_bits) handoff, no channel needed

Closing the level-meter half of #21 forced a small audio-pipeline architecture decision. The cpal callback runs on a real-time-ish thread and must not block; the HUD pump runs as a tokio task at ~30 Hz and reads from somewhere; in between, we need a non-blocking writer-many-readers handoff for a single f32 RMS value.

**Three options considered:**

1. `mpsc::Sender<f32>` from the callback into a consumer task. Unbounded send is non-blocking but allocates per message and the consumer has to drain a possibly-large queue every tick. Wrong shape — we don't care about the history of levels, only the latest.
2. `crossbeam_channel::bounded(1)` + try_send. Drops on full, fine for "latest only", but adds a dep for a one-value queue we can replace with two atomic ops.
3. `Arc<AtomicU32>` storing `f32::to_bits()`. Single store, single load, no allocation, no locks, no extra deps. The reader sees a stale value at most for one `Relaxed` window — irrelevant for a 30 Hz visualisation.

**Picked #3.** The pattern is well-known in audio engineering (Paul Adenot's wait-free SPSC ring buffer post documents it); web-search guidance during this PR specifically endorsed `AtomicU32` storing `f32::to_bits()` for level meters over a channel. `Relaxed` ordering is correct: the level field is independent of every other shared state, the audio callback writes it once per buffer, and the HUD pump's read does not need to synchronise with anything. A momentarily-stale read costs one frame of meter lag — well within human perception.

**Why a default trait method instead of a level-stream type.** `AudioCapture` already had four methods that every implementation has to think about (list, start, stop, is_recording). Adding `current_level()` as a *default-impl* method that returns `0.0` means existing test mocks (NoopAudio, MockAudio) keep compiling and the HUD's meter just idles for them — exactly the right behaviour. The cpal backend overrides; everything else inherits the no-op. If we ever want a streaming-events level (push instead of pull), that's a separate trait, not a refactor of this one.

**Why a 30 Hz polling pump rather than push-on-callback.** The cpal callback already touches a `Mutex<Vec<f32>>` to append samples (a flagged real-time concern, see #21 follow-up TBD); adding a Tauri-emit on the same thread would compound the realtime risk and emit at 100 Hz when the HUD only needs 30 Hz. Decoupling via the AtomicU32 + tokio interval keeps the audio thread doing strictly audio work and lets the UI layer set its own cadence. Throttling on the producer side would be a constant `if elapsed > 33ms` branch; throttling on the consumer side is just a `tokio::time::interval`.

**Frontend smoothing belongs in the renderer.** The Tauri event carries the raw RMS; the Svelte page applies an attack/release envelope on `requestAnimationFrame`. Doing the smoothing in JS rather than Rust means the renderer can adjust feel (faster attack, slower release, motion-reduced bypass) without going through an IPC boundary or a backend rebuild. PRD §13.7 framing: the smoothing is presentation, not signal — it lives in the layer that owns presentation.

**Realtime-safety follow-up.** The web-research pass during this PR flagged `Mutex<Vec<f32>>` in the cpal callback as priority-inversion-prone; an SPSC ring (`rtrb`) is the standard fix. That refactor is intentionally **not** in this PR — the level meter is presentation work and can ship under the existing locking discipline (the lock is uncontended on the hot path; only `stop_session` contends, and only after the stream has been paused). Filed as a TODO for the next audio-internal sweep.

## 2026-04-25 — Playwright with mocked Tauri IPC: Vite alias, not deep stub

Added a frontend e2e suite (Path A in the testing decision; #57 tracks the future tauri-driver path). The interesting design call: Tauri 2's `@tauri-apps/api/core` and `@tauri-apps/api/event` packages route through internal protocols that don't exist outside a real Tauri runtime, and they expect specific globals (`window.__TAURI_INTERNALS__`) to be present. Three options for mocking:

1. **Spoof `__TAURI_INTERNALS__`** — fragile; the internal shape is undocumented and changes between Tauri minor versions. Tests would silently break on every upgrade.
2. **Refactor the app behind a `src/lib/ipc.ts` indirection** — pure-frontend wrapper, swap the wrapper in tests. Adds a layer for the sake of testing; we'd be paying complexity in production code for a test-only seam.
3. **Vite resolve.alias swap** — replace the `@tauri-apps/api/*` modules at *build time* in e2e mode. Production code imports unchanged; tests get pure-JS stubs that never touched the real package.

Picked #3. Triggered by `HUSH_E2E=1`; vite resolves `@tauri-apps/api/core` to `tests/e2e/setup/core-stub.ts`, `@tauri-apps/api/event` to `event-stub.ts`. The stubs read mock state from `window.__hush_e2e` (set via `page.addInitScript` before navigation), so each test configures its handlers without touching anything global to the suite.

**Why unmocked invokes throw.** `core-stub.ts` errors instead of returning `undefined` for any unknown command. If a future PR adds a new `invoke('foo')` call site without a corresponding default in `_mock.ts`, the failing test names exactly which command is missing instead of passing with `undefined` and surfacing as a UI render bug.

**Default mocks vs. per-test overrides.** The dictation page calls ~half a dozen invokes on mount (history list, replacements list, vocabulary list, model list, settings get, list_input_devices, get_first_run_completed). If every test redeclared all of them the fixtures would drift. `_mock.ts` ships shared defaults — every spec gets a working app baseline — and overrides win on top. The override transport is stringified-and-rebuilt because Playwright's `addInitScript` cannot cross functions over its serialization boundary; rebuilding via `new Function` on the page side is ugly but tightly scoped.

**`test.fixme` as a regression marker.** The Escape-key dismissal test for the welcome modal is `fixme`'d (skipped) today because the underlying a11y bug (#48) hasn't been fixed. It will flip green automatically when the fix lands; until then it documents the gap inline next to the other modal tests, where future contributors will see it. Cheaper than a separate tracking spreadsheet of "tests I want to write someday".

**What this suite cannot catch.** Real IPC errors (serialisation mismatches, unregistered commands like the bug surfaced in PR #46), HUD lifecycle, hotkey registration, real audio, real model download. Those need the platform webview, which Playwright doesn't drive — that's the tauri-driver path tracked in #57. The trade-off: Path A is half a day of investment, runs on every PR, catches the round-4 reviewer's modal a11y / aria-attribute / error-copy class of finding. Path B is a multi-day setup with macOS rough edges; we'll add it when the value of full-stack coverage exceeds that complexity cost.

## 2026-04-26 — CI's blind spot: startup-time panics, and the `npm run tauri dev` smoke

Hit a regression today that none of our automated suites caught: the `tauri-plugin-updater` plugin was registered in `lib.rs` without a corresponding `plugins.updater` block in `tauri.conf.json`, and the plugin's deserialiser panics on null at app startup. Pre-fix:

```
Running `target/debug/hush`
thread 'main' panicked at src/lib.rs:167:10:
error while running Hush: PluginInitialization("updater",
  "Error deserializing 'plugins.updater' within your Tauri
   configuration: invalid type: null, expected struct Config")
```

The user hit it on their first `npm run tauri dev` after a stretch of CI-green PRs. PR #61 deferred the registration until #10.

**Why CI couldn't catch this.** Hush's CI runs `cargo test --lib`, `cargo clippy --all-targets`, `cargo fmt --check`, `npm run check`, and `npm run test:e2e` (Playwright + mocked Tauri IPC). None of those instantiate a real `tauri::Builder`. The unit tests construct `AppState` directly via `mock_state()` — they never call `tauri::Builder::default().setup(...).run(...)`. Clippy doesn't execute code. Playwright runs in plain Chromium with `@tauri-apps/api/{core,event}` aliased to in-tree stubs, so the Rust runtime never starts. Even `cargo test` against a real binary wouldn't help — Tauri's plugin init only fires under `Builder::run`.

That means the entire class of "app fails to start" bugs is invisible to automation:

- Plugin-config deserialisation failures (this case).
- `tauri::generate_handler!` referencing a command symbol that's been removed but not deregistered.
- `app.manage(state)` panicking because two managed states have the same type.
- A capability file referencing a window label that no longer exists.
- A Cargo dep update that breaks `Builder::default()` at link time but compiles individual lib targets fine.

**The smoke fix.** A single `npm run tauri dev` run before opening a PR is the cheapest possible coverage: Tauri compiles, runs `setup`, registers plugins, and waits for the event loop. If any of the above fails, the panic appears within ~5 seconds of "Running `target/debug/hush`". Killing the process after that confirms the boot path; the contributor doesn't need to interact with the window.

**Why this isn't in CI.** A real Tauri runtime needs a display server, microphone permissions on macOS, and roughly two minutes of Cargo compile time even with caching. Adding a "boot the app, look for a panic, kill it" CI job would double per-PR runtime and add platform-specific permission flake. The cost-benefit doesn't work — the same coverage costs ~30 seconds locally.

**Where this lives now.** Required smoke step for any PR that touches `lib.rs`, `tauri.conf.json`, `Cargo.toml` plugin deps, capability files, or `.plugin(...)` registrations. Documented in `CONTRIBUTING.md` (Testing → Dev-launch smoke), the PR template, and the PR checklist. The smoke is a checklist item, not a CI gate, because requiring it gates the workflow on the contributor's own honesty — but the alternative (a CI job that costs minutes per PR for a check the contributor can do in 30 seconds) is worse.

**Concrete heuristic for this repo.** When an edit touches any of those files, run `npm run tauri dev` before opening the PR. Same shape as running `cargo test` after a Rust change or `npm run check` after a Svelte change — it's a fixed habit, not a judgement call.

## 2026-04-26 — macOS window transparency needs both Tauri Cargo feature and config flag

The HUD window has `"transparent": true` in `tauri.conf.json`. The design depends on it — the dark translucent pill the HUD CSS draws is meant to sit on top of whatever's behind it, not inside a solid window. On macOS this only works if Tauri uses Apple's private window-shape APIs, and Tauri gates that behind two switches that **both** have to be flipped:

1. **Cargo feature** — `tauri = { version = "2", features = ["macos-private-api"] }`. Compiles the implementation in.
2. **App config** — `app.macOSPrivateApi: true` in `tauri.conf.json`. Activates it at runtime.

If only the config flag is set without the Cargo feature, Tauri's build script fails with "The `tauri` dependency features on the `Cargo.toml` file does not match the allowlist defined under `tauri.conf.json`". If neither is set but the window is configured `transparent: true`, the dev startup logs a warning and the window renders with a solid background — silent product breakage, since the HUD looks "fine" but the design intent (translucent pill, see-through to the desktop) is lost.

**App Store implication. Resolved (2026-05-03, #114).** `macOSPrivateApi: true` permanently disqualifies Hush from MAS distribution. This is accepted: Hush is a side project distributing via GitHub image downloads. MAS is off the roadmap. Do not attempt MAS without redesigning the HUD to avoid Apple private APIs.

**Smoke confirms the fix.** Pre-fix: dev log emits `The window is set to be transparent but the macos-private-api is not enabled`. Post-fix: warning is absent. The dev-launch smoke (which just landed in #61) is exactly the workflow that caught this — a contributor running `npm run tauri dev` sees the warning and the visibly-solid HUD background, and knows to fix it. Without the smoke, the warning would have only been noticed by a user complaining the HUD looks wrong.

## 2026-04-26 — rdev 0.5 crashes on macOS 26+ via TSM dispatch-queue assertion

The user's first hands-on round on macOS 26.4.1 surfaced an immediate hard crash: the app started, registered the toggle hotkey + PTT listener, and then aborted with `EXC_BREAKPOINT (SIGTRAP)` on the first modifier-key press. The crashing thread was `hush-ptt`, and the stack walked from rdev's CGEventTap callback into HIToolbox's TSM:

```
rdev::macos::listen::raw_callback
  → rdev::macos::common::convert
  → rdev::macos::keyboard::Keyboard::create_string_for_key
  → rdev::macos::keyboard::Keyboard::string_from_code
  → TSMGetInputSourceProperty
  → islGetInputSourceListWithAdditions
  → dispatch_assert_queue_fail
  → __builtin_trap
```

**The mechanism.** rdev unconditionally computes a Unicode "name string" for every key event via `TSMGetInputSourceProperty`. On macOS 26 Apple tightened the dispatch-queue assertions on the TSM functions: they now `dispatch_assert_queue_fail` if called from any thread other than the main dispatch queue. rdev calls them from its own listener thread (which runs the CGEventTap callback). Crash.

**What makes this nasty.**

- It's not a Rust panic. `dispatch_assert_queue_fail` is a hard `__builtin_trap`, so `std::panic::catch_unwind` doesn't catch it. The whole process aborts.
- It only fires on certain key codes (the ones rdev hasn't cached a string for yet), so the app appears to start cleanly and crashes only on the first uncached modifier press. Looks intermittent unless you trace the actual cause.
- It's not specific to our code. *Any* app using rdev 0.5 on macOS 26+ will hit this on the first modifier press.
- The string rdev computes is data we never read. Hush only matches on `Key` (the keycode enum); the `Event::name` field could be `None` for our purposes and PTT would still work.

**The defence we shipped (#69 PR).** PTT listener is skipped by default on macOS. Two env vars: `HUSH_PTT_ENABLE=1` to opt in (for users on macOS 13/14/15 where rdev still works) and `HUSH_PTT_DISABLE=1` as the cross-platform kill switch. The toggle hotkey (`tauri-plugin-global-shortcut`, doesn't go through rdev) and button-driven dictation are unaffected. The enablement decision is unit-tested in `hotkey::ptt::tests` so a future regression won't accidentally re-enable PTT on macOS without the user's opt-in.

**The proper fix is a native CGEventTap.** Replace rdev on macOS with a thin `core-graphics`/`objc2` event-tap wrapper that registers for `kCGEventKeyDown` + `kCGEventKeyUp` + `kCGEventFlagsChanged` and reads keycodes directly without going through TSM. ~half-day of work — tracked as a follow-up issue. Linux + Windows continue to use rdev (those don't have this issue).

**Lesson worth keeping.** When a third-party crate calls into platform UI APIs from non-main threads, look for `dispatch_assert_queue` in the stack on the next macOS major. Apple's been progressively tightening these checks for a decade, and the result is always "code that worked on N now hard-aborts on N+1". The defence is either: (a) a thin platform-specific wrapper you control, or (b) the affordance to disable the third-party code that broke. Hush has both available now — env-var disable today, native wrapper later.

**Why I didn't notice this in CI.** CI doesn't run a real Tauri runtime — same blind spot called out in the dev-launch-smoke entry above. Even the dev-launch smoke I run as part of the new convention only boots the app and waits ~20s; it doesn't simulate key events, so it would miss this. The user hit it in actual hands-on testing, which is exactly what hands-on testing is *for*. Worth remembering: there are bug classes that no automation reaches; for those, the human at the keyboard is the test.


## 2026-04-26 — `AudioSource` enum vs overloading `device_id`

When Phase A1 of the meeting-mode pivot needed system-audio capture alongside the existing mic path, the trait method `AudioCapture::start(device_id: Option<&str>)` had two obvious extensions:

1. **Overload the string** — pick a sentinel like `"system"` (or `"system-audio"`) that the cpal backend recognises and dispatches to a different platform primitive (ScreenCaptureKit / WASAPI loopback / PulseAudio monitor).
2. **Discriminated union** — replace `device_id: Option<&str>` with `source: AudioSource` where `AudioSource` is `Microphone(Option<String>) | SystemAudio`.

Picked (2). The string-sentinel approach was tempting because it kept the trait surface unchanged and would have shipped in one PR rather than two, but it has a real cost: it pushes the dispatch into prose ("`'system'` is the magic value") rather than the type system. A frontend caller, a future test mock, or a contributor adding a third source kind would have to remember the sentinel. Worse, a real device named `"system"` (vanishingly unlikely but possible on Linux) would silently collide with the system-audio path with no compiler help.

**The discriminated-union approach makes each dispatch arm visible in the type.** `start_with_source(AudioSource::SystemAudio)` is unambiguous; the frontend's serde wire shape becomes `{ kind: "system-audio" }` instead of an opaque string; future variants (`AppAudio(BundleId)` for per-app capture) extend the enum and get an exhaustive-match prompt at every call site.

The trade-off: trait surface grows. We carry `start(device_id)` AND `start_with_source(source)` both for one transitional release, with `start_with_source` defaulting to dispatch on the `Microphone` arm and erroring on `SystemAudio` for backends that haven't shipped support yet. Cost is one extra method on the trait — paid back the moment the second platform's SystemAudio impl lands.

**Lesson worth keeping.** When a method's parameter is "kind plus details", reach for an enum, not a sentinel string. Even if the enum has only two variants today and a string would cover both. The compile-time exhaustiveness is what makes the third variant safe to add later.

## 2026-04-26 — `Transcribe::transcribe_chunks` as default impl, not separate trait

Phase B foundation needed a streaming entry point on the transcription layer — somewhere a future Whisper-sliding-window or Parakeet backend could emit `Vec<Utterance>` instead of a single `String`. Two shapes:

1. **Separate trait** — `pub trait StreamingTranscribe { ... }`, held alongside `dyn Transcribe` in `AppState`.
2. **Add the methods to `Transcribe`** with a default impl that calls the existing one-shot `transcribe_with_prompt`.

Picked (2). The IPC layer already holds `Mutex<Option<Arc<dyn Transcribe>>>`. A separate trait would force a choice between holding two parallel object types (and keeping them in sync at every swap point) or downcasting at every dispatch. Default impl on the existing trait keeps the IPC surface unchanged: every backend, including test mocks and the future "no model loaded" stub, continues to satisfy `Transcribe` with no per-impl boilerplate.

**The default-impl is observably equivalent to the legacy one-shot path.** It concatenates the chunks into a single `CapturedAudio`, calls `transcribe_with_prompt`, and emits exactly one `is_final = true` utterance whose end timestamp is computed from total frames. So the dictation hot path's behaviour is unchanged through the refactor — we verified by leaving the existing tests (135 of them) green through both #103 (foundation) and #104 (call-site refactor).

The cost is that the streaming-aware backends need a capability flag to disambiguate "I support real partials" from "I'm using the fallback." That's `supports_streaming() -> bool`, default `false`. The IPC layer reads this when deciding whether to forward partial-utterance Tauri events to the frontend.

**Lesson worth keeping.** Trait surfaces for "this is how the engine emits results" should accept the most expressive shape (a sequence of utterances with timestamps), with a default impl that degrades gracefully for backends still operating in the simpler one-shot world. The bridge — capability-flag-plus-default — costs less than two parallel traits, both at the type-system level and at the dispatch level. Where the dictionary repos diverge from this pattern (markers + extension trait, see #113 review notes) is a design tension we'll resolve before the streaming pump in #110 starts driving real writes.

## 2026-04-26 — Round-7 reviewer cycle: the "byte-identical" trap

A pattern surfaced in #103 + #104 that's worth pinning. The PR descriptions claimed the refactor was "byte-identical" to the prior behaviour — meaning the default `transcribe_chunks` impl produces the same final transcript text the legacy `transcribe_with_prompt` did. Round-7 technical-writing reviewer correctly flagged that "byte-identical" is precise CPU-cache-line vocabulary, not a description of transcription text equivalence.

**The accurate claim is "observably equivalent" or "semantically unchanged".** Round-7 also caught a real silent-failure-mode that "byte-identical" would have masked: the `is_final` filter at the call site would silently produce empty text if a future streaming backend emitted only partials. That's not byte-identical to anything — it's a new failure mode introduced by the refactor.

**Lesson worth keeping.** Prefer "observably equivalent" or "no behaviour change for users on the default-impl path" when describing refactors that route the same data through a new code path. "Byte-identical" claims more than is actually true and the gap is where the silent-failure modes hide.

## 2026-04-26 — ScreenCaptureKit as the only sanctioned macOS system-audio path

> **[SUPERSEDED]** The conclusion of this entry ("prefer TCC path / ScreenCaptureKit for unsigned distribution") was wrong. `AudioHardwareCreateProcessTap` requires **no entitlement** for unsandboxed/sideloaded apps on macOS 14.2+ (and macOS 26). Hush replaced SCK with the CoreAudio tap in #585. See the 2026-05-06 entry at the top of this file for the definitive account.
> 
> The body below is preserved as historical context.

Phase A2 of meeting-mode delivery needed actual system-audio capture on macOS. Three plausible routes were on the table:

1. **CoreAudio HAL plug-in / Aggregate Device** — wire BlackHole-style virtual loopback into a multi-output device. Requires user installation of a third-party driver, and Apple has been deprecating HAL plug-ins since macOS 14.
2. **`AudioHardwareCreateProcessTap` / `AudioHardwareCreateAggregateDevice`** with the `kAudioHardwareTapType` API new in macOS 14.4. These work *but* require entitlements that Apple only grants to MAS-distributed apps, putting them on the wrong side of the #114 (MAS-vs-`macOSPrivateApi`) decision the user explicitly deferred.
3. **ScreenCaptureKit with `captures_audio = true`** — Apple's sanctioned, non-entitled path. Gated behind the Screen Recording TCC bucket but otherwise works for any signed/unsigned developer build.

Picked (3). The `screencapturekit` crate (1.5.4) bridges Swift's `SCStream` through stable FFI, so we stay in pure Rust at the Hush boundary. Trade-offs that matter:

- **The TCC bucket is "Screen Recording" even when you capture only audio.** Confusing for users — we capture zero pixels — but Apple bundles audio-from-display under the same prompt and there is no separate "system audio" TCC category. The first call to `SCShareableContent::get()` triggers the prompt; the existing `MacosDiagnosticPanel` already covers Screen Recording in its TCC sweep.
- **Sample format is fixed-set f32 PCM** at one of `{8000, 16000, 24000, 48000}` Hz × `{mono, stereo}`. We picked 48 kHz / stereo to match what the OS mixer is already running internally — avoids a forced resample at capture time. Downstream `downmix_to_mono` + the resampler ahead of whisper handle the rate/channel reduction the same way they do for any cpal mic input.
- **`AudioBufferList` layout is "1 buffer = interleaved, N buffers = planar".** The crate exposes both shapes; we fold the planar case into interleaved before pushing into the shared `Vec<f32>` so the rest of the pipeline doesn't branch on layout. Discovered by reading `cm/audio.rs` rather than from Apple's docs — the docs only describe the high-level format, not the buffer-count convention.
- **The crate links libSwift_Concurrency at runtime.** On end-user macOS 12+ the Swift runtime ships in `dyld_shared_cache` so `/usr/lib/swift` resolves implicitly, but on the dev machine here the cache resolution doesn't apply to `cargo test` binaries — tests need `DYLD_FALLBACK_LIBRARY_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx` to load. Production app builds (`cargo tauri dev` / bundled `.app`) inherit the cache and don't need this.

**Lesson worth keeping.** When Apple offers two paths to the same capability — one entitled (Tap APIs) and one TCC-prompted (ScreenCaptureKit) — prefer the TCC path for unsigned/sideloaded distribution. The entitlement-required path pays back only inside MAS, and the MAS decision is its own multi-quarter trade-off. ScreenCaptureKit's "Screen Recording prompt for audio-only capture" is awkward UX, but it works on every distribution channel (sideloaded, notarised, Homebrew cask) the entitled path doesn't.

## 2026-04-26 — Streaming whisper (#108): sliding-window policy + in-memory partials

The meeting pump pre-#108 stopped capture every 10 s, drained the buffer, ran one-shot whisper, and restarted. Two costs: ~10 s of latency between user speech and the panel update, and word-clipping at chunk boundaries. PR1 of #108 introduces a `StreamingTranscribeSession` trait + a `SlidingWindowState` policy machine + a whisper-rs impl that runs inference on a rolling 30 s window every ~3 s of new audio, emitting partials for the trailing tail and finals for segments that age past an 8 s commit threshold.

**Two architectural decisions worth recording.**

**1. Time-based commit, not stability-based.** The two reasonable strategies for "when does a partial firm up into a final": (a) any segment ending more than `commit_tail_ms` before the window's leading edge commits; (b) commit only after N consecutive inferences produce identical text for the same segment range. (a) is simpler and shipped first. The smoke test against the bundled JFK clip + the `base` model showed whisper produces stable, consistent text across overlapping windows — the partial revised three times mid-stream and the finish-flushed final exactly matched the canonical "ask not what your country can do for you" transcript. If a real-meeting smoke test surfaces aggressive whisper rewrites of old text, the policy is ready to swap to (b) without changing the trait shape — `SlidingWindowState::tick` is the only call site.

**2. Partials live in memory, not the database.** PR3 surfaces partials by extending the `meeting_session_get` IPC response with a `current_partials: Vec<Utterance>` field — one entry per active source (mic, system) since the pump runs one streaming session per source — instead of writing them as `is_final = false` rows the frontend has to filter. The DB only ever sees finals. Trade-off: ~3 s polling latency floor (vs sub-second for Tauri events), but zero frontend listener wiring, no event-ordering hazards, and partials don't pollute the persisted history. Tauri events stay available as a small follow-up if subjective latency feels off after the smoke test.

**Whisper-rs API specifics learned in passing.**

- `WhisperState::full_get_segment_t0` / `t1` return `i64` in 10 ms units (centiseconds). Multiply by 10 to get ms.
- `FullParams::set_no_context(true)` is the right setting for sliding-window: each inference re-tokenises the window from scratch rather than carrying KV-cache across calls. KV-cache reuse would technically reduce per-call cost but propagates segment-level mistakes from earlier inferences into later ones; no-context lets the policy converge on a stable transcript independently per inference.
- The `WhisperContext` itself is `!Sync` but `Send`; wrapping in `Arc<Mutex<...>>` and serialising inferences behind the mutex works because dictation + meeting are inherently serial (one user, one process). The streaming session holds a clone of the parent transcription's `Arc` so it can run inferences from a different thread (the meeting pump's blocking pool) without coupling to the original `&self` lifetime.

**Smoke-test observation worth noting.** The 11 s JFK clip never exercises the `tick`-emits-final branch — the audio is too short to age anything past the 8 s `commit_tail_ms` during streaming, so all text stays in the partial until `finish` flushes. A long meeting (tens of minutes) would exercise the in-tick commit path naturally. The smoke test is honest about this — it asserts "at least one partial mid-stream" + "finals concatenate to the expected words" + "finish flushes near the end of audio", all of which the JFK clip exercises.

**Lesson worth keeping.** When the policy and the engine can be cleanly separated, do it: `SlidingWindowState` is whisper-agnostic and unit-testable with a scripted `WhisperLikeInferer` mock; the whisper bridge is ~80 lines of FFI translation + a `Vec<StreamSegment>` return. The 15 unit tests pinning the policy ran in 0 ms; the smoke test against real whisper ran in 1.2 s. The split also means a future Parakeet ONNX backend (#32) inherits the same policy state machine — only the inferer adapter changes.


## 2026-04-27 — `cargo:rustc-link-arg` from a transitive dep is a CI/dev split hazard

I shipped #144 in the morning, dropping the `screencapturekit` Cargo feature flag so SCK linked unconditionally on macOS. The same PR added a `DYLD_FALLBACK_LIBRARY_PATH` to the macOS CI workflow because `cargo test` started SIGABRTing on `dyld[…]: Library not loaded: @rpath/libswift_Concurrency.dylib`. CI went green. **The actual app launch did not.** Ken hit `npm run tauri dev` later and got the same dyld error in the running binary — by which point CI had been masking the regression for hours.

**Why CI was green and dev wasn't.** The screencapturekit crate's build script emits its rpaths via:

    cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift
    cargo:rustc-link-arg=-Wl,-rpath,/Library/Developer/CommandLineTools/...

Those directives only propagate to the link line of the **immediate** parent crate. Hush links screencapturekit transitively (it's a dep of our root crate, not a direct compile target). Cargo's transitive-link-arg propagation rules silently drop these flags. Result: `otool -l target/debug/hush | grep LC_RPATH` returns **zero entries**. The `@rpath/libswift_Concurrency.dylib` reference in the binary's link record has nothing to resolve against.

CI test binaries had the same zero-rpath state, but the env-var I'd added (`DYLD_FALLBACK_LIBRARY_PATH=/Applications/Xcode.app/.../swift-5.5/macosx`) gave dyld a fallback search path that resolved the dylib. The actual app launch — `cargo tauri dev` shelling out to `cargo run` — inherited a different env (no env var), so the same binary failed.

**The fix (#147).** A `src-tauri/.cargo/config.toml` adds the rpaths from our root crate (where cargo does honour `link-arg`):

    [target.aarch64-apple-darwin]
    rustflags = [
        "-C", "link-arg=-Wl,-rpath,/usr/lib/swift",
        "-C", "link-arg=-Wl,-rpath,/Applications/Xcode.app/.../swift-5.5/macosx",
    ]

Order matters: `/usr/lib/swift` first means dyld resolves `libswift_Concurrency.dylib` from the system shared cache, the same copy all the indirectly-linked Swift dylibs use. Putting the Xcode path first instead caused four `objc[…]: Class _Tt… is implemented in both` duplicate-class warnings because both copies loaded.

**Lessons worth keeping.**

1. **`cargo:rustc-link-arg` from a transitive dep is a footgun.** If a crate's build script needs to add rpaths to the *binary*, and that crate is a transitive dep, the rpath effectively doesn't exist. Cargo has `cargo:rustc-link-arg-bins=...` to propagate, but it's the dep author's call to use that form — and most don't. Defensive posture: when adding any crate that needs runtime-resolved dylibs, verify the rpath landed via `otool -l`. The `cargo build` succeeding is not the same signal as "the binary will run."

2. **Env-var workarounds in CI mask real regressions.** I added `DYLD_FALLBACK_LIBRARY_PATH` to the CI workflow when CI started failing. That made CI go green, but it papered over a real bug — the binary itself was broken; only my env var made it work in the test harness. The right shape would have been to fix the binary (the rpath) and let CI naturally pass without an env-var crutch. The rpath fix in #147 means the env-var addition in #144 is now redundant; leaving it in as belt-and-suspenders, but the binary doesn't need it.

3. **CLAUDE.md's dev-launch-smoke checklist is load-bearing.** It explicitly calls out "CI does not run a real Tauri runtime — every test target is `cargo test --lib` or `cargo clippy` or Playwright with mocked IPC. A panic at app boot is invisible to CI and only surfaces when a contributor pulls the branch." This is exactly the class of bug it warns about. The required-when list mentions `Cargo.toml` adding/removing a Tauri plugin dep — should be expanded to "any change that affects the binary's link record on macOS, including making a transitive dep unconditional." Adding that to the checklist would have caught this in 30 seconds.

## 2026-04-27 — macOS TCC and the Tauri dev-binary parent-attribution quirk

After dropping the `screencapturekit` Cargo feature flag (#144) and adding `NSScreenCaptureUsageDescription` to the embedded Info.plist (#149), the Screen Recording prompt still didn't fire on `npm run tauri dev`. Hush never appeared in System Settings → Privacy & Security under any category. The user could see iTerm.app in the Microphone list, but no Hush entry anywhere.

**Why.** `cargo tauri dev` produces a bare unsigned executable at `target/debug/hush` — not a `.app` bundle. macOS TCC keys permissions by some combination of bundle ID, code-signing identity, and binary path. For an unsigned binary with no `.app` wrapper, TCC falls back to **attributing the permission request to the parent process** — the terminal that launched `npm run tauri dev`. That's why iTerm.app appeared in the Microphone list and was sufficient for mic access — when Hush asked for mic, macOS saw it as an iTerm child and used iTerm's grant.

**Mic and Input Monitoring** fall through this parent-attribution path cleanly. **Screen Recording does not.** SCK is stricter — the calling binary must be its own TCC entry, not a child of one. Adding `CFBundleIdentifier` / `CFBundlePackageType` / `NSScreenCaptureUsageDescription` to the dev binary's embedded `__info_plist` Mach-O section helps macOS recognise it as an app-shaped thing, but doesn't survive the unsigned-bare-binary distinction. The dev binary can't reliably access SCK no matter how complete its embedded plist.

**The fix.** New `npm run tauri:bundle` script (`scripts/tauri-bundle-macos.sh`) runs `cargo tauri build --debug` to produce a real `.app` at `src-tauri/target/debug/bundle/macos/Hush.app` and opens it. macOS treats the `.app` as a proper app, prompts cleanly with the Info.plist description, and persists the grant across re-bundles of the same path. The bundle build is 30 s – 2 min and not a hot-iteration tool — `cargo tauri dev` remains the inner loop, `tauri:bundle` is reserved for SCK / TCC / code-signing / dock-icon smoke testing.

**Lessons worth keeping.**

1. **Embedded `__info_plist` ≠ proper `.app`.** Tauri's auto-embedded plist (`tauri::embed_plist::embed_info_plist!`) gets all the right keys — but the binary still isn't a code-signed `.app`. macOS treats the two differently for any TCC bucket that requires per-app entries (Screen Recording, Accessibility, sometimes Calendar/Photos/etc.). Adding plist keys is necessary but not sufficient for those gates.

2. **Mic ≠ Screen Recording in dev mode.** A workflow that relies on parent-process attribution (mic capture working because iTerm has the grant) gives a misleading "everything works" signal. Test the strict-attribution paths (SCK) against a real `.app` from day one rather than fighting the dev binary.

3. **The dev-binary path is a separate sandbox identity per `target/`.** macOS keys TCC by absolute binary path for unsigned binaries. A `cargo clean` + rebuild keeps the same path, so grants persist. Moving `target/` does invalidate. Worth documenting alongside the `.cargo/config.toml` rpath fix (also from 2026-04-27, see entry above) — both are "how dev binaries are different from production app bundles" caveats.

---

## 2026-04-27 — macOS TCC status IS readable for the three categories Hush touches

Earlier comments (in `ipc/commands.rs` and elsewhere) claimed macOS doesn't expose programmatic read access to TCC grant state, so `diagnose_macos_permissions` could only emit hint copy. That's true for *some* TCC buckets — Accessibility, Full Disk Access, Calendar, etc. — but **false for the three Hush actually cares about**:

- **Microphone** — `+[AVCaptureDevice authorizationStatusForMediaType:]` returns `AVAuthorizationStatus` (NotDetermined/Restricted/Denied/Authorized) without prompting.
- **Screen Recording** — `CGPreflightScreenCaptureAccess()` (CoreGraphics) returns a Bool without prompting. There's no NotDetermined variant; "false" covers both "never asked" and "explicitly denied", which the UI can normalise.
- **Input Monitoring** — `IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)` (IOKit) returns `IOHIDAccessType` (Granted/Unknown/Denied) without prompting.

All three are passive reads — calling them does NOT trigger the OS dialog. Implemented in `src-tauri/src/macos_perms/mod.rs` (#166). The frontend uses these to render a green "Permissions OK" pill on the Dictation tab when everything is granted, and a per-permission status list in Settings → Permissions.

**Takeaway:** when a TCC category genuinely matters to the app's UX (Hush leans heavily on Microphone + Screen Recording), check Apple's framework headers before assuming "programmatic read isn't possible." The blanket "TCC is opaque" reputation comes from the buckets where it really is opaque, not from the privacy framework as a whole.

---

## 2026-04-27 — rdev macOS-26 abort: Narsil's PR #147 was incomplete; fixed via fufesou's fork

#69 documented `rdev::listen` hard-aborting on macOS 26+ on the first modifier press: the rdev CGEventTap callback called `TISGetInputSourceProperty` from a non-main thread, and macOS 26's stricter dispatch-queue assertions kill the process via `dispatch_assert_queue_fail` (which is `__builtin_trap`, not a Rust panic — `catch_unwind` cannot save us).

**First attempt (didn't work):** pinned to Narsil's upstream `main` past [rdev#147](https://github.com/Narsil/rdev/pull/147) (May 2025, "MacOS: set_is_main_thread"). Hands-on test on macOS 26 with `HUSH_PTT_ENABLE=1`: instant crash on the first modifier press. Reading the patch: PR #147 only adds a `set_is_main_thread` opt-in on the `Keyboard` struct used by the *send* path. The `listen()` path's `raw_callback` calls `convert(...)` which still invokes TSM, and `listen()` itself never calls `set_is_main_thread(false)` — so the fix never runs for our use case.

**Second attempt (works):** pinned to [fufesou/rdev](https://github.com/fufesou/rdev), the fork RustDesk ships in production. Diff against Narsil's `main`: in `listen()`, the tap is attached to `CFRunLoopGetMain()` instead of the calling thread's run loop. The callback runs on main, TSM is happy, no abort.

We pin via git rev (`a90dbe1172f8832f54c97c62e823c5a34af5fdfe` as of this entry). The API surface we use (`listen`, `Event`, `EventType::{KeyPress, KeyRelease}`, `Key`) is identical between forks. Bump-to-published when EITHER Narsil ships a release that completes the listen-path fix, OR fufesou publishes their fork to crates.io.

PTT stays opt-in via `HUSH_PTT_ENABLE=1` even with the abort fixed: enabling triggers the Input Monitoring permission prompt, which is a privacy surprise for users who don't realise a dictation app would be reading every keystroke. The env gate keeps the prompt to power users who deliberately turn PTT on. A future settings-window toggle will replace the env gate.

**Takeaway for future Apple-framework FFI bugs:** "PR merged" ≠ "your bug is fixed." Read the diff. PR #147 was a real fix for *a* TSM call site, but not the one our code path hits. The cheap-path heuristic ("just bump the dep") is right to try first, but verify with the actual error reproduction, not just "did it merge upstream." Production users (RustDesk in this case) often patch around upstream's incompleteness for years before upstream catches up.

---

## 2026-04-28 — D1 EnergyDiarizer wired: multi-source caveat is structural, not tunable

> **Superseded by the 2026-04-29 entry above — `EnergyDiarizer` was reverted to `NoopDiarizer` in #243 after hands-on testing showed it collapsed cross-source utterances to a single "Speaker A". The structural caveat called out in this entry turned out to be the load-bearing problem rather than a refinement to layer on top.**

#191 shipped the `Diarize` trait + an `EnergyDiarizer` impl that alternates Speaker A / Speaker B based on inter-utterance silence gaps. #201 (this entry) flipped the production wiring from `NoopDiarizer` → `EnergyDiarizer::default()`.

**The caveat surfaced wiring it up.** The pump dispatches per-source: each tick drains the mic source's streaming session, dispatches its finals, then drains the system-audio source's, dispatches its finals — independently. Each call to `diarize.label_utterances` sees one source's batch only. That means the EnergyDiarizer's internal "current speaker" letter resets between sources: mic source runs A → B → A; system source runs A → B → A. Same labels, different actual speakers.

For mic + system meetings (the canonical Zoom-style config) the Speaker A label means "you said this on mic" if the utterance came from `mic`, but means "the first remote person to talk in this batch" if it came from `system`. The user can't tell which is which without a per-source visual hint.

**Why we shipped anyway.** The mic-only path (no system audio) doesn't have this problem — every utterance comes from one source, the alternating heuristic is honest. For mixed meetings the source-derived `"mic"` / `"system"` fallback in `dispatch_utterances` only kicks in when the diarizer leaves `speaker_label = None`; EnergyDiarizer always produces a label, so the fallback is bypassed once D1 is on. That's intentional — D1 is the more specific signal — but it does mean the "You" / "Remote" badges stop rendering for mixed meetings unless the user reverts to NoopDiarizer.

**Fixes considered, deferred:**

1. **Pass source context to the diarizer.** Extend `Diarize::label_utterances` to take a source-kind parameter, let `EnergyDiarizer` use the source as the starting letter (mic → A, system → C). Cheapest fix; visually disambiguates the two sides at the cost of a fixed 4-letter cap.
2. **Stateful per-source diarizer.** Track the running "current speaker" per `(session_id, source_kind)` so a session keeps its mic-A and system-C series consistent across pump ticks. Better than (1) for long meetings where the per-tick reset would otherwise cause labels to flip mid-conversation.
3. **D2: model-based diarization.** ONNX speaker-embedding model that genuinely knows who's who. Right answer; the heaviest lift.

(1) and (2) are small follow-ups if user hands-on testing of D1 finds the multi-source labels actively confusing. The trait already takes `audio_chunks` + `format` (D2's needs) so threading source context through the same call doesn't widen the API surface much.

**Takeaway:** when shipping a heuristic that runs on a per-shard pipeline (per-source here), the labels it produces are scoped to its shard. If the user-facing display merges shards (the meeting timeline does), the labels need cross-shard context — either provided to the heuristic or composed at a higher layer. The primitive is fine; the wiring needed the cross-shard awareness.

**Update 2026-04-28 (#206):** fix landed via the third bullet from the maintainability review: the pump now collects per-source utterances into `TickBucket`s for the tick, calls `diarize_and_dispatch_merged` once over the chronologically-merged batch, then splits the labelled result back into per-source slices for the existing `dispatch_utterances` path. The trait surface didn't move; the wiring carries the cross-source coordination. Tail flush uses the same shape so a single-tick edge case can't bypass it. EnergyDiarizer now sees the true mic + system sequence — "Speaker A" means the same person regardless of which side it came from. Tests `diarize_and_dispatch_merged_runs_diarizer_in_chronological_order` + `..._is_a_no_op_for_empty_buckets` pin the new contract.

---

## 2026-04-29 — Release pipeline smoke caught a deployment-target tarpit

The release workflow (`.github/workflows/release.yml`, #226) ships
`tauri-action`-built artefacts on `v*` tag pushes. A
`workflow_dispatch` smoke run was the first time we'd actually
exercised it. Three iterations produced concrete learnings worth
writing down — the macOS leg is the tarpit, the rest worked clean.

### Smoke #1: Intel and Apple Silicon both fail with `<filesystem>`

```
error: '~directory_iterator' is unavailable: introduced in macOS 10.15 unknown
fatal error: too many errors emitted, stopping now
```

whisper.cpp's GGML uses C++17 `<filesystem>` (`directory_iterator`,
`exists`, `path`, etc.), all marked unavailable below macOS 10.15.
Tauri's release-build path defaults the deployment target somewhere
older than that. `ci.yml` doesn't catch this because cargo-test goes
through a different build path that doesn't bake in a deployment
target — `tauri-action`'s bundler does.

### Smoke #2 (#229): drop Intel + bump deployment target via $GITHUB_ENV

Two things at once:
- macOS 26 (Tahoe) is the project's primary target per CLAUDE.md.
  26 is Apple-Silicon-only; an Intel build leg has nothing to run
  on inside the supported window. Dropped from the matrix.
- Set `MACOSX_DEPLOYMENT_TARGET=26.0` in `$GITHUB_ENV` for the
  Apple Silicon leg, expecting the cc crate's deployment-target
  logic (which reads this env via `deployment_from_env`) to
  return 26.0.

The Apple Silicon leg **still failed with the same error**. Logs
showed the env was set:

```
MACOSX_DEPLOYMENT_TARGET: 26.0
```

…but the actual cc command had:

```
cc ... --target=arm64-apple-macosx -mmacosx-version-min=10.13 \
   -w -march=armv8.6-a -mmacosx-version-min=14.0 \   # the $CFLAGS we set
   ... -arch arm64 -mmacosx-version-min=10.13 ...
```

Three `-mmacosx-version-min` flags, last-wins is 10.13.

### Smoke #3 (#230): pass via CFLAGS, hit the same wall

We tried `-mmacosx-version-min=14.0` directly through `CFLAGS` and
`CXXFLAGS`, plus `MACOSX_DEPLOYMENT_TARGET=14.0` (a value the GH
runner's macOS 15 SDK actually accepts — Xcode 16.4 can't deploy-
target above 15). Same triple-flag situation in the cc command,
same 10.13 winning. The cmake configure log showed where the
flags came from:

```
-DCMAKE_C_FLAGS=-ffunction-sections -fdata-sections -fPIC \
                --target=arm64-apple-macosx -mmacosx-version-min=10.13 \
                -w -march=armv8.6-a -mmacosx-version-min=14.0
```

**The 10.13 is being injected by cmake-rs (or the cc crate it asks
for compile flags) before our user CFLAGS get appended.** Then
cmake itself appends another `-mmacosx-version-min=10.13` after
our flags as a `-arch` companion pair. We're sandwiched.

### Where 10.13 actually comes from (best current understanding)

- The `cc` crate at v1.2.61 has logic that reads
  `MACOSX_DEPLOYMENT_TARGET` env, and if absent falls through to
  `default_deployment_from_sdk()` (runs `xcrun --show-sdk-version`)
  and finally a hardcoded 11.0 for `aarch64`. Our env *is* set to
  14.0; cc *should* return 14.0.
- But it doesn't — cmake-rs ends up emitting flags with 10.13.
  Where 10.13 comes from is still unclear from a code-only audit:
  it's not in cmake-rs's source, not in cc's, not in whisper.cpp's
  CMakeLists, not in whisper-rs-sys's build.rs. Likely a deeper
  cmake auto-detection path that fires during the configure step,
  but I burned three smoke runs trying to find it without
  resolution.

### Three things to try next (none of them attempted yet)

1. **Bump whisper-rs.** We're on 0.13.1; a newer whisper.cpp
   pin in a newer whisper-rs may have removed the `<filesystem>`
   call site or fixed the deployment-target plumbing in its
   `cmake::Config` invocation by adding an explicit
   `.define("CMAKE_OSX_DEPLOYMENT_TARGET", "14.0")`.
2. **Vendor / patch.** Add a `[patch.crates-io]` entry that
   points whisper-rs-sys at a fork with the explicit
   `.define()`.
3. **Build macOS locally.** The release pipeline produces clean
   Linux + Windows artefacts; the maintainer attaches the macOS
   `.dmg` produced by `npm run tauri:bundle` by hand.

### Takeaways

- **The smoke caught a real bug.** Three iterations of "fix and
  re-run" were not wasted runner minutes — they progressively
  narrowed down where the deployment-target string was coming from.
  The discipline is: read the actual cc command line in the failing
  log before writing the next fix.
- **Design target ≠ deployment target.** macOS 26 is the *design*
  target (what we hands-on test on) per CLAUDE.md. The deployment
  target is the *technical* lower-bound the binary is compatible
  with — constrained by the runner's SDK version (Xcode 16.4 →
  macOS 15 SDK ceiling). 14.0 is the realistic floor that's
  Apple-Silicon-supported, above whisper.cpp's `<filesystem>` need,
  and below the SDK ceiling. Bumping the deployment target to 26.0
  has to wait for GH runners to ship Xcode 26.x.
- **Linux + Windows worked first try.** The pipeline is real; the
  macOS leg is one targeted upstream fix away from being green
  too. `docs/releases.md` documents the maintainer recipe so the
  release-cutting happy path doesn't need this learnings entry.
- **Tracking issue:** the cmake-rs flag-construction propagation
  would benefit from a focused ticket (try option 1 above first
  since it's free, then option 2 if needed). For now the workflow
  ships in a "Linux + Windows artefacts attach cleanly, macOS leg
  needs an upstream poke" state.

### Update 2026-04-30 — local bundling: `CMAKE_OSX_DEPLOYMENT_TARGET` is the magic env var

After a clean-cache rebuild during local hands-on testing
(`npm run tauri:bundle` after Cargo's whisper-rs-sys cache had
been invalidated by an unrelated dep change), the same
`<filesystem>` failure surfaced on the maintainer's dev box.
`CFLAGS=-mmacosx-version-min=14.0` + `MACOSX_DEPLOYMENT_TARGET=14.0`
weren't enough — the cc-emitted compile line still showed
`-mmacosx-version-min=10.13` *appended after* the user CFLAGS, and
the C++ filesystem header rejected accordingly.

**The fix that worked locally: also set `CMAKE_OSX_DEPLOYMENT_TARGET=14.0`.**
cmake-rs reads this env var directly (separate from
`MACOSX_DEPLOYMENT_TARGET` which the `cc` crate honours) and threads
it into the cmake configure step's compile-flag construction. Once
set, the `-mmacosx-version-min=10.13` injection went away and the
build succeeded.

The full local-bundle invocation that worked:

```bash
CMAKE_OSX_DEPLOYMENT_TARGET=14.0 \
MACOSX_DEPLOYMENT_TARGET=14.0 \
CFLAGS="-march=armv8.6-a" \
CXXFLAGS="-march=armv8.6-a" \
npm run tauri:bundle
```

`-march=armv8.6-a` stays for the i8mm target-feature, same reason
as the GH Actions matrix. The two deployment-target env vars are
both required: `CMAKE_OSX_DEPLOYMENT_TARGET` is what cmake-rs reads,
`MACOSX_DEPLOYMENT_TARGET` is what cc reads, and the two crates
each contribute compile flags, so missing either re-introduces the
mismatch.

This points at option 4 for the upstream-pipeline fix (in addition
to the three listed above): **set both env vars in `release.yml`**.
The release pipeline currently only sets `MACOSX_DEPLOYMENT_TARGET`,
which is why the macOS leg has been stuck. Worth a single-PR smoke
to verify before declaring the full fix.

For maintainer recipe-doc purposes (`docs/releases.md`): include
both env vars in the local-bundle invocation. Without that the
next contributor to do a hands-on bundle on a fresh build cache
will hit this and have to re-derive the workaround.

---

## 2026-04-29 — TCC Reset bug + dev-loop polish (#231)

Two related lessons from the dev iteration after first
`npm run tauri:bundle`:

### The Reset button silently skipped Screen Recording

> **[MOOT for Screen Recording]** Hush no longer uses ScreenCaptureKit. The `reset_macos_permissions` call no longer resets `ScreenCapture` because Hush holds no ScreenCapture TCC grant. The underlying lesson (every service the app touches must be covered by Reset) still applies to **Microphone** and **InputMonitoring/ListenEvent**.

`reset_macos_permissions` ran `tccutil reset` for `Microphone`,
`ListenEvent`, and `Accessibility` — but not `ScreenCapture`. We
caught it hands-on: clicked Reset, saw the Screen Recording entry
still in System Settings under "GRANTED". Trivial bug (one missing
string in an array), worth noting because it sat in production for
weeks: an in-app "Reset all" affordance that visibly looks like it
did all four things but actually did three. Test coverage for IPC
commands would have caught this; we have unit tests for some
commands (HUD toggle gained tests under #220) but not for the
macOS-specific ones because they shell out to `tccutil`. A test
that mocks the command runner would be cheap.

### Stale Hush.app rows survive `tccutil reset`

`npm run tauri:bundle` ad-hoc-signs the `.app`. The signing identity
is derived from binary contents, so it changes every rebuild.
macOS keys TCC entries by signing identity, **not** bundle id, when
the identity differs. Two consequences:

1. Multiple Hush.app rows accumulate in System Settings →
   Privacy & Security under different identities.
2. `tccutil reset ScreenCapture com.khawkins.hush` resets the entry
   that matches the bundle id but the *other* row(s) under different
   identities don't go anywhere. They keep their grants.

The user-visible failure: macOS doesn't prompt on the next
recording attempt because *some* Hush.app row is granted, but the
running build's identity matches none of those rows, so it's
blocked anyway. Silent block, no prompt, no grant.

> **Note (2026-05-06):** The `ScreenCapture` rows are now irrelevant (Hush no longer requests ScreenCapture). This stale-row behaviour still applies to **Microphone** and **Accessibility / ListenEvent** rows.

**Recovery procedure documented in `docs/macos-permissions.md`
"Dev-loop":** reset → click `−` on each Hush.app row in System
Settings → relaunch → re-grant. The Settings → Permissions Reset
button's success copy now spells this out explicitly so the user
doesn't have to grep docs.

### Takeaway

Iteration on macOS apps that fall under TCC has an OS-level state
that doesn't go away when our app does. Any "reset our state"
affordance has to either a) cover every TCC service the app
touches (we now do — fixed the bug), and b) tell the user about the
out-of-band cleanup steps that the OS API can't do for us (the `−`
button case). The post-reset summary is a good place for the
latter; a GUI button can't do it because reaching into System
Settings requires user consent.


---

### 2026-05-04 — Tauri debug bundle linker-signed identifier breaks TCC (and tccutil reset)

**Symptom:** After running `npm run dev-reset` and opening the debug `.app` bundle, `tccutil reset io.github.khawkins98.hush` succeeds but TCC grants immediately vanish on the next rebuild. Input Monitoring and Screen Recording show as "Not yet granted" even after being toggled ON in System Settings.

**Root cause:** Tauri's `cargo tauri build --debug` build on Apple Silicon leaves the binary with a *linker-signed* ad-hoc signature. The code-signing identifier embedded by the linker is a hash of the binary contents (`hush-44ac88ddc8db2594`), **not** the bundle identifier `io.github.khawkins98.hush`. Additionally, `Info.plist=not bound` — the Info.plist is not sealed into the signature.

TCC keys permission entries to the code-signing identifier. So:
- All grants are stored under `hush-<old-hash>`
- `tccutil reset io.github.khawkins98.hush` is a no-op (no entries under that key exist)
- Every rebuild produces a new hash, invalidating all stored grants

**Confirmed by:** `codesign -dv Hush.app` showing `Identifier=hush-44ac88ddc8db2594`, `Info.plist=not bound`

**Fix:** Run `codesign --force --deep --sign - Hush.app` after Tauri builds the bundle. This re-signs with a proper ad-hoc signature that sets `Identifier=io.github.khawkins98.hush` and seals the Info.plist. Now TCC entries are stable across rebuilds (until the binary contents change, which triggers the normal CSReq-mismatch flow documented earlier in this file).

**Automation:** `scripts/tauri-bundle-macos.sh` and `scripts/tauri-dmg-macos.sh` both now run `codesign --force --deep --sign -` after building. `npm run tauri:bundle` is safe to use for TCC smoke-testing after this fix.

**Why this didn't surface before:** The `com.khawkins.hush` era used the same linker-signed approach, so TCC was equally broken — but contributors didn't notice because the permissions screen was less prominently used. The bundle ID rename (#526) forced a full permission reset which exposed the underlying issue.

---

### 2026-05-04 — `data-theme` vs `@media` dark mode gap in Svelte components

**Symptom:** Some UI elements remain light-mode coloured when the user has forced dark mode via the in-app toggle (i.e., OS is in light mode, but `data-theme="dark"` is set on `<html>`).

**Root cause:** Hush's dark mode uses two mechanisms simultaneously:
1. `@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) ... }` — respects the OS preference
2. `:root[data-theme="dark"] ...` — respects the in-app override

Components that only implemented the `@media` block fail when the user forces dark via the toggle (OS light, `data-theme="dark"`). Several components were authored this way (FirstRunModal, MeetingTab, etc.).

**Fix options:**
- **Preferred:** Use CSS custom properties (`var(--bg-surface)`, `var(--text-primary)`, `var(--text-muted)`, `var(--info-text)`, `var(--accent)`) directly in the base rule. `app.css` defines all tokens under both mechanisms already — no explicit dark block needed in the component.
- **Fallback:** Add both a `@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) .selector { ... } }` block AND a matching `:root[data-theme="dark"] .selector { ... }` block.

**Pattern in `app.css`:** `src/app.css` is the authoritative source for all CSS tokens. Inspect it before hardcoding any colour value.

---

### 2026-05-04 — AudioWaveform: log scale + adaptive gain for waveform sensitivity

**Problem:** Linear `level × levelScale` mapping made the waveform nearly flat for quiet-to-normal speech. At −38 dBFS (typical conversational level) the linear amplitude is ~1.3 % of full scale, giving a ~5 % bar height that's visually indistinguishable from silence. Different microphones and system-audio boost levels compounded this — a quiet USB mic with no software gain looked dead while a heavily boosted system capture railed.

**Solution:** dBFS logarithmic mapping with an adaptive ceiling tracker.

*Log scale math:*
```
db  = 20 * log10(level)
norm = (clamp(db, DB_FLOOR) - DB_FLOOR) / (dynamicCeil - DB_FLOOR)
height% = clamp(norm * 100, silenceFloorPct, 100)
```
At −38 dBFS with `DB_FLOOR = −70` and `dynamicCeil = −12` this yields ~43 % height — clearly visible and proportionally accurate.

*Adaptive ceiling:*
- `adaptivePeak` tracks a slow EMA of `displayLevel` with a fast attack (0.15/frame ≈ 60 ms) and very slow release (0.0015/frame ≈ 11 s).
- `dynamicCeil = clamp(adaptivePeakDb + HEADROOM_DB, DB_CEIL_MIN, DB_CEIL_DEFAULT)` so bars spend most of their range on the actual signal rather than headroom the mic never reaches.
- Adaptive tracking only runs during `effectiveMode === "recording"` to prevent ceiling decay eating scale when the user pauses between sessions.
- Initialised at 0.01 (−40 dBFS) so first-frame speech looks proportional immediately.

**Why not a manual gain knob?** Different mics, OS boosts, and recording scenarios have a multi-decade dynamic range. A static knob either clips a hot mic or stays invisible on a quiet one. Adaptive gain handles all cases without user config.

**Constants chosen:**
- `DB_FLOOR = −70`: floor below conversational speech; softer noise stays hidden.
- `DB_CEIL_DEFAULT = −3`: headroom so a very loud mic doesn't permanently rail.
- `DB_CEIL_MIN = −48`: prevents the adaptive ceiling from dropping so low that normal speech takes the whole range.
- `ADAPTIVE_HEADROOM_DB = 6`: 6 dB above tracked peak; bars hit ~85–90 % at typical loudest frames.

**Where the code lives:** `src/lib/AudioWaveform.svelte` — constants block, `adaptivePeak` state, adaptive update inside `tick()`, and IIFE height formula in the `{#each waveform}` block.


---

### 2026-05-04 — Debug console window: light-mode terminal text invisible

**Symptom:** In light mode, the debug console showed invisible text — timestamps, log targets, and the entry count had no contrast against the dark `#141414` terminal background.

**Root cause (original):** `DebugConsole.svelte` used `var(--text-primary)` and `var(--bg-code)`. In light mode `--text-primary` is `#111111`, so dark text on dark background = invisible.

**Root cause (round 2):** The initial fix hardcoded `#141414`/`#e6edf3` for the output area but left `.log-time`, `.log-target`, and `.debug-console-count` reading `var(--text-secondary)`. In light mode `--text-secondary` is `#444444` — still invisible on `#141414`.

**Final fix:** Define a `--debug-*` token set on a `display: contents` wrapper at the root of `DebugConsole.svelte`. All colours inside the component read from these tokens (`var(--debug-text-muted)`, `var(--debug-border)`, `var(--debug-level-*)`, etc.). The `display: contents` wrapper propagates the custom properties to all children without affecting the surrounding flex layout.

**Pattern:** When a surface is always-dark (terminal, code block, diffs):
1. Give it a *dedicated token set* (`--debug-*`) defined on the component's root wrapper, not borrowed theme vars.
2. Use `display: contents` on that wrapper so the token scope is the whole component without disrupting layout.
3. Never use `var(--text-*)` or `var(--border)` inside the surface — those tokens flip in light mode.

---

### 2026-05-04 — About moved to top-level sidebar section (not a Settings tab)

**Decision:** About was previously one of many Settings tabs (`settings-tab-about`). Moved to a fourth top-level sidebar section (`sidebar-nav-about`) so it's reachable in ~one click from anywhere in the app.

**Affected places:**
- `SidebarNav.svelte` — `SidebarSection` type extended, items array, icon branch
- `SettingsPanel.svelte` — "about" removed from `SettingsTab` type, `baseTabs`, template body, and the `SettingsGotoTab` event listener
- `+page.svelte` — `openSettingsTab("about")` intercept, About render block, `.about-panel` CSS
- E2E tests — `settings-tab-about` selectors replaced with `sidebar-nav-about`

**Why not keep it in Settings:** Settings is configuration-space; About is informational / version-space. At one-click distance it's more discoverable; at two-click distance (Settings → tab) it got lost, especially for new users who want "what version is this?" without guessing which tab has it.

---

### 2026-05-04 — alwaysOnTop floating window: hide-on-close to prevent focus stranding

**Symptom:** Closing the debug console palette (red-✕) made the main Hush window appear to also close. The main window was still alive but no longer visible or focused.

**Root cause:** The debug window had no `CloseRequested` handler, so Tauri destroyed it. When an `alwaysOnTop` window is destroyed on macOS the window server returns focus to the desktop (not to the app's other windows) because floating windows live at a different NSWindowLevel and aren't part of the normal focus-restoration chain. The user saw the desktop and concluded the main window had also closed.

**Fix:** Add `"debug"` to the `["main"]` loop in `lib.rs` that intercepts `CloseRequested` and calls `window.hide()` instead of letting Tauri destroy. Same three benefits: (1) focus stays with the app, (2) the log buffer is preserved for the next open, (3) window creation cost is paid once.

**Rule:** Any `alwaysOnTop: true` window should use hide-on-close. When a floating window is destroyed, macOS does not automatically restore focus to the underlying application.

---

### 2026-05-04 — macOS ⌘\` window cycling requires set_as_windows_menu_for_nsapp

**Symptom:** ⌘\` (Cycle Through Windows) did nothing in Hush despite a "Window" submenu being present in the menu bar.

**Root cause (ordering):** `set_as_windows_menu_for_nsapp()` works by calling `[NSApp mainMenu]` and walking the installed menu tree to find the correct NSMenu for the submenu. If called *before* `app.set_menu()`, `mainMenu()` returns whatever was there before, the submenu can't be found, and the call silently does nothing. The fix is to call it *after* `app.set_menu(menu)`.

**Root cause (registration):** Even if called in the right order, `init_app_menu` only auto-registers submenus with ID `WINDOW_SUBMENU_ID` (`"__tauri_window_menu__"`). A custom Window submenu built with `SubmenuBuilder::new(app, "Window")` gets a random ID so the auto-registration never fires. Call `window_submenu.set_as_windows_menu_for_nsapp()?` explicitly in your own code.

**Root cause (window level — second failure):** Even after fixing the ordering, ⌘\` still didn't cycle. `setWindowsMenu:` populates the Window menu and the windows appeared there, but macOS's ⌘\` only cycles windows at the **same NSWindowLevel**. A window with `alwaysOnTop: true` is promoted to `NSWindowLevelFloating`; a normal main window is at `NSWindowLevelNormal`. Windows on different levels are in separate cycle groups and ⌘\` won't bridge them. Fix: remove `alwaysOnTop` from any window you want to participate in the same ⌘\` cycle as the main window.

**Full fix sequence:**
1. Call `set_as_windows_menu_for_nsapp()` *after* `app.set_menu(menu)?`
2. Ensure all windows you want in the ⌘\` cycle are at `NSWindowLevelNormal` (no `alwaysOnTop: true`)

**Alternative for ID:** Use `SubmenuBuilder::with_id(app, WINDOW_SUBMENU_ID, "Window")` — Tauri's automatic path then finds and registers it. Prefer the explicit call because it's self-documenting and doesn't depend on the magic string staying stable across Tauri releases.

---

### 2026-05-05 — Frontend recording state machine: design decisions (#558 / #560)

**What:** Replaced 7 flat interdependent `$state` variables in `dictation.svelte.ts` with a `RecordingPhase` discriminated union (`idle | starting | recording | stopping | transcribing`). Decomposed `stop()` into `_stopDictation()` and `_stopMeeting()`.

**Why a union, not separate booleans:** The flat-var approach made illegal combinations representable (`recording && busy && transcribing` simultaneously). A discriminated union makes illegal states structurally impossible and exhaustive pattern matching catches unhandled phases at compile time.

**Two start paths must be preserved:**  `start_dictation` (hotkey/PTT) applies vocabulary prompt biasing, replacements, and backend clipboard write. `meeting_start_manual` (UI button) adds system-audio capture. Consolidating to one path would silently regress transcription quality and clipboard reliability for unfocused windows. Always thread both paths through any future lifecycle changes.

**`setTimeout` delays removed safely:** `meeting_stop_manual` awaits pump drain before returning (confirmed in `meeting/lifecycle.rs::stop_manual()`). The session is fully finalised in SQLite at return time. Direct `await` is safe; no additional delay is needed.

**`appProfileNoticeTimer` is plain `let`, not `$state`:** Timer handles are implementation details — they're never read reactively in a template. Using `$state` for them fires unnecessary Svelte reactive updates on every set/clear. Keep timer handles and other non-UI implementation state as plain `let`.

**Trailing silence applies to ALL stop paths:** PTT key-up, record button, toggle hotkey, and command palette stop are all "natural end of speech". Only a hypothetical "cancel/abort" action would skip the buffer. Don't add a stop caller that omits `TRAILING_SILENCE_MS` unless it explicitly means "discard this recording".

**Gap not yet addressed:** The state machine has no dedicated unit tests — the Playwright e2e suite validates external behaviour but not the transition graph itself (e.g. failed `start_dictation` → idle, stop during `starting` is ignored). Tracked in #562.

---

### 2026-05-06 — Meeting pump diagnostic logging: distinguishing 0-utterance failure modes (#533)

**Symptom:** Meeting mode reports 0 utterances after 1-2 minutes of real speech; both mic and system-audio sources affected simultaneously.

**Why both sources fail together:** Both `WhisperStreamingSession` instances (one per source) clone the *same* `Arc<Mutex<WhisperContext>>` from the meeting transcriber snapshot taken at session start (`lifecycle.rs::start_manual`). The pump processes sources sequentially (not concurrently) so there is no lock contention, but a performance regression in one inference run delays the other.

**Three ranked failure modes:**
1. **Transcriber slot None at start** — user hasn't loaded a model yet, or model load failed. Already logged as `WARN meeting pump: no streaming transcription session for source`.
2. **Audio not flowing** — SCK not capturing virtual-device call audio (Teams/Zoom route audio through a virtual driver that SCK's display-level capture misses), or mic device error. Shows as `samples = 0` on every "meeting pump: drained" debug line.
3. **Whisper no-speech filtering** — Whisper runs but all segments have empty text because `no_speech_thold` (default 0.6) rejects compressed call audio. Previously invisible; now surfaced by `raw_segments` vs `non_empty_segments` in the "streaming tick: inference ran" debug log.

**Logging gaps filled (commit accompanying this entry):**
- `streaming tick: inference ran` → `raw_segments`, `non_empty_segments`, `window_ms` at DEBUG level. If `raw_segments > 0` but `non_empty_segments = 0` for every inference, no-speech filtering is the culprit.
- `streaming tick: interval gate not open` at TRACE level per tick.
- `streaming tick: waiting for min-first audio threshold` at DEBUG level for first ~3 ticks.
- `streaming finish: tail flush inference ran` → same segment counts for the stop-time flush.
- `whisper: inference complete` → `n_segments`, `window_samples` at DEBUG (whisper-feature only).
- `meeting pump: inference tick` now also logs `elapsed_ms` for the full feed+drain round-trip.

**How to use these logs to diagnose:** Run `RUST_LOG=hush=debug npm run tauri:bundle && open ~/Applications/Hush.app`. Start a meeting recording, speak for 30+ seconds, then check the Tauri dev console or attach `cargo tauri dev` output. Look for: (a) `samples = 0` every tick → audio not flowing; (b) `inference ran` lines appearing every ~3 s → inference is working; (c) `raw_segments > 0, non_empty_segments = 0` → no-speech filtering; (d) no `inference ran` lines at all → something upstream.

**Ring buffer is not a concern:** SCK ring buffer is sized at `48_000 × 2 × 120 = 11.5 M` f32 samples (120 s). Even if inference takes several seconds, audio accumulates without overflow.

---

### 2026-05-06 — e2e mock override closure-capture limitation

**Bug:** Test override functions that reference module-level constants (e.g. `DEFAULT_SESSION_ID`) fail at runtime with `ReferenceError: <name> is not defined`, silently falling through to the catch block instead of setting the expected state.

**Root cause:** `installMocks` serialises per-test overrides via `fn.toString()` and reconstructs them in the page context via `new Function(...)`. The reconstructed function executes in a fresh scope — no access to the originating module's top-level bindings. Any constant defined outside the arrow function is stripped away.

**Fix / rule:** All values inside `installMocks` override functions must be **inline literals**, not references to variables or constants declared in the test file.

```ts
// ✅ OK — literal value inlined
meeting_session_get: () => ({ session: { id: 1, ... } })

// ❌ BAD — closure capture fails silently at test runtime
meeting_session_get: () => ({ session: { id: DEFAULT_SESSION_ID, ... } })
```

If per-test counters or dynamic values are genuinely needed, use `page.exposeFunction` to bridge them across the serialisation boundary. This constraint is also documented with examples in `_mock.ts` alongside the `new Function` call.

---

### 2026-05-06 — System Audio TCC grant requires a process relaunch; "Screen Recording" label is alarming (#579)

> **[SUPERSEDED]** Hush no longer uses ScreenCaptureKit for system audio. The entire class of problem described here — relaunch requirement, alarming TCC label, `mediaserverd` deny cache — is gone. `AudioHardwareCreateProcessTap` fires no TCC prompt at all. See the 2026-05-06 entry at the top of this file.
> 
> Historical notes preserved below.

**Proper fix vs chosen tradeoff:** The architecturally correct fix is a small helper process for SCK: on `TCCDeny`, kill and respawn the helper while the main app stays alive. This avoids any relaunch for the user. However, this is significant complexity for a once-per-install event. The chosen approach — auto-detect grant + prompt-relaunch — is the right cost/benefit tradeoff for production.

**Detection: use real SCK probe, not just `CGPreflightScreenCaptureAccess`:** Preflight can return true on cached TCC state while a real `SCShareableContent::get()` call still fails (already documented in #378). The grant-watcher validates with `validate_screen_recording_capability()` before emitting the event. Don't trust preflight alone for user-facing "it worked" signals.

**Duplicate-watcher guard:** The watcher is spawned by `prime_screen_recording_permission`. If the user clicks "Grant in Settings" multiple times, only one watcher should run. A process-scoped `static AtomicBool` (not an `AppState` field) is sufficient because `tauri-plugin-single-instance` guarantees a single Hush process — no shared state between processes needed.

**"Screen Recording" label is alarming for users:** macOS's TCC category is `ScreenCapture`, but "Screen Recording" makes users think Hush is watching their screen. The same permission is labeled "System Audio (optional)" in OpenWhispr. Hush now uses "System Audio" in all user-visible copy (the internal `screenRecording` key in `PermissionStatuses` and the `ScreenCapture` TCC category remain unchanged). This is a framing change only — the underlying permission and how it's requested is identical.

---

### 2026-05-06 — Compile-time build timestamp via `build.rs` + `cargo:rustc-env` (#583)

**Pattern:** `build.rs` emits `HUSH_BUILD_TIMESTAMP` (Unix seconds) via `println!("cargo:rustc-env=HUSH_BUILD_TIMESTAMP={secs}")`. The IPC command `get_build_info` reads it at runtime with `env!("HUSH_BUILD_TIMESTAMP")`. No external crate (`vergen` or similar) needed — a handful of lines of stdlib code in `build.rs` is sufficient.

**Incremental-build caveat:** Cargo only re-runs `build.rs` when a watched file changes. Here, `cargo:rerun-if-changed=build.rs` means the stamp only refreshes when `build.rs` itself is edited. Incremental dev builds reuse the previous stamp — good enough for a "when was this binary built" display. Release and CI builds always start clean, so the stamp is accurate there.

**Why not `vergen`?** The `vergen` crate provides richer build metadata (git SHA, dirty flag, etc.) but adds a build dependency. The plain `SystemTime` approach is simpler and covers the use case (debug identification). If git SHA is ever needed, `vergen` would be the right reach.

---

### 2026-05-06 — OpenWhispr uses `AudioHardwareCreateProcessTap` (CoreAudio), not ScreenCaptureKit, for system audio

> **[RESOLVED & IMPLEMENTED]** The uncertainty at the bottom of this entry ("not yet verified by hands-on testing on macOS 26 and should be confirmed before investing in a port") is now resolved. Probe confirmed no TCC prompt; implementation shipped in #585. See the 2026-05-06 authoritative entry at the top of this file for the complete picture. The OpenWhispr research and the probe results below remain valid as supporting evidence.

- **`tap_created: status=0 tapID=222` with Screen Recording TCC intentionally NOT granted.** The tap was created successfully — no Screen Recording permission required.
- **No audio-capture dialog appeared.** On macOS 26 the tap runs silently without any TCC prompt (neither the lock-icon Screen Recording dialog nor the mic-icon NSAudioCaptureUsageDescription dialog).
- **A "Files in your Documents folder" dialog appeared** at the same time — this is unrelated to the tap (likely from Hush's own SQLite or model storage path touching `~/Documents/`). Tracked separately.

**Conclusion:** `AudioHardwareCreateProcessTap` on macOS 26 is **fully independent of Screen Recording TCC** and requires no user-facing permission prompt of its own. Switching Hush's meeting audio to this API would eliminate the Screen Recording dialog entirely.

**The uncertainty in the original entry is now resolved:** The conflicting forum accounts were about older macOS versions (14.x/15). On macOS 26, no TCC gate on this API.

---

**Background:** A user questioned whether OpenWhispr avoids the Screen Recording TCC permission by using Accessibility instead. Research into the [OpenWhispr MIT source code](https://github.com/OpenWhispr/openwhispr) (an Electron/React app) produced a definitive answer.

**What OpenWhispr does (macOS):**

OpenWhispr ships a compiled Swift binary (`resources/macos-audio-tap.swift`) that uses `AudioHardwareCreateProcessTap` — a CoreAudio API introduced in **macOS 14.2**. The binary:
1. Creates a `CATapDescription` with `processes = []` (capture all system audio), `isExclusive = true`, `isMixdown = true`, `isPrivate = true`.
2. Creates a CoreAudio aggregate device backed by the tap.
3. Streams raw PCM (16-bit, 24 kHz mono) chunks to stdout.

The Electron main process (`src/helpers/audioTapManager.js`) spawns this binary as a child process, reads PCM chunks via stdout, and forwards them to the renderer for transcription.

**Permission model — the key uncertainty:** OpenWhispr caches the granted/denied status to a file (`~/.../userData/.system-audio-permission`) because there is no macOS API to check permission without actually trying to create a tap. Their entitlements file (`resources/mac/entitlements.mac.plist`) contains only standard Electron entitlements (`allow-jit`, `allow-unsigned-executable-memory`, `disable-library-validation`) plus `com.apple.security.device.audio-input` (microphone) — **no special audio-tap entitlement**. They also declare `NSAudioCaptureUsageDescription` in their Info.plist, not a Screen Recording usage string.

**However:** Developer forum research shows conflicting accounts of whether `AudioHardwareCreateProcessTap` with `CATapDescription` (macOS 14.2+) triggers Screen Recording TCC or its own consent mechanism. One credible source states non-sandboxed apps still need Screen Recording permission; another (including OpenWhispr's own architecture) implies an independent audio-capture dialog. This is **not yet verified by hands-on testing on macOS 26** and should be confirmed before investing in a port.

**Accessibility permission (user's question):** OpenWhispr appears in System Settings → Accessibility because of its **text injection** feature (auto-paste transcribed text into the focused app using `AXIsProcessTrustedWithOptions`). This is completely separate from audio capture.

**Relevance to Hush:**

| | Hush (current) | OpenWhispr |
|---|---|---|
| Audio API | ScreenCaptureKit | `AudioHardwareCreateProcessTap` (CoreAudio) |
| TCC category | Screen Recording (confirmed) | **None on macOS 26** (confirmed by probe) |
| macOS min | none specified | 14.2+ |
| Architecture | Rust native | Swift helper binary (spawned child process) |
| TCC cache / relaunch | Yes (mediaserverd cache) | No prompt → no cache issue |

**Cross-platform impact of a port:** Zero. The CoreAudio tap would be `#[cfg(target_os = "macos")]`-gated exactly like the current SCK code. Linux and Windows CI builds would be unaffected.

**Confirmed improvement for Hush (probe verified on macOS 26):**
- Eliminates the Screen Recording dialog entirely — no lock icon, no scary "record this computer's screen and audio" copy
- Likely avoids the `mediaserverd` TCC cache / process-relaunch issue (#579) since there is no TCC prompt to cache
- No regression on macOS version (Hush targets macOS 26+, well above the 14.2 minimum)

**Why not implemented yet:** The entire Hush audio stack is built on ScreenCaptureKit — the `AudioCapture` trait, `ScreenCaptureKit` crate, virtual device support, ring buffer, and pump are all SCK-centric. Switching would be a substantial Rust rewrite (likely a new `CoreAudioTap` impl behind the `AudioCapture` trait seam, plus shipping a pre-compiled Swift binary in the bundle). Implementation tracked in issue #585.

**Key files in OpenWhispr for reference:**
- `resources/macos-audio-tap.swift` — Swift binary; `AudioHardwareCreateProcessTap` usage
- `src/helpers/audioTapManager.js` — Electron main; spawns binary, caches permission status, streams chunks
- `src/utils/systemAudioAccess.ts` — defines `RendererSystemAudioStrategy` (`loopback` for Windows, `browser-portal` for Linux, `native` for macOS 14.2+ via CoreAudio tap)
- `src/types/electron.ts` — `SystemAudioStrategy` type (`"native" | "loopback" | "browser-portal" | "portal-helper" | "unsupported"`)
