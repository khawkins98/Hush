# Changelog

All notable changes to Hush will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

This block covers the post-v0.1.0 meeting-mode pivot work. Grouped
by PR so a reader can scan which features shipped in which change —
the unreleased queue grew long enough during the pivot scaffold that
a single flat list was hard to navigate.

### Added

#### Live partial-utterance rendering in the meeting panel (#108 PR4)

- The meeting transcript renders `currentPartials` (PR3) below the
  settled finals with an italic + reduced-opacity treatment plus a
  dashed border-left and an animated "…" indicator next to the
  timestamp. Partials revise in place as whisper firms up the
  trailing tail; once aged past the commit threshold the pump emits
  them as finals and the styling solidifies. `prefers-reduced-motion`
  kills the indicator pulse.
- `MeetingSessionsPanel.svelte` keys partials by `speakerLabel` (one
  per source) so a revision swaps text in place rather than
  re-mounting the row. The autoscroll effect tracks both the finals
  count AND a partial-content fingerprint (`label:text` joined),
  so the live tail keeps following whether the user spoke a new
  word or whisper revised the in-flight tail.
- The empty-state copy ("every 10 seconds") and the active-session
  line ("about every 10 seconds") are updated to match the
  streaming cadence ("within a few seconds"; "italicised lines are
  still firming up").
- The `speakerLabel` helper is now structural-typed
  (`{ speakerLabel: string | null }`) so it accepts both
  `PersistedUtterance` (finals) and `StreamingUtterance` (partials)
  without duplication.
- 1 new e2e test pins the partial-rendering shape: 1 final + 2
  partials → 3 rows, 2 with `utterance-partial` class, italic
  computed-style, "…" indicator visible. 10/10 meeting-panel e2e
  tests pass.

What this completes for #108: the user now sees text appear within
~3 s of speech (vs ~10 s pre-#108), revising in place as whisper
firms up the segments. The four-PR sequence (PR1 streaming trait /
PR2 drain_into / PR3 pump rewrite / PR4 partial UI) closes the
streaming-meeting-mode UX promise. Real-meeting smoke validation
of CPU + revision behaviour is the remaining open item.

#### Streaming meeting pump — partials in IPC (#108 PR3)

- The meeting pump no longer chunks-and-restarts. It opens one
  `StreamingTranscribeSession` per audio source at session start
  (PR1), drains each `AudioSession` on a 500 ms tick (PR2), feeds
  the drained samples into the corresponding streaming session, and
  dispatches returned utterances: **finals** to the database (the
  existing `MeetingSessionRepository::append_utterance` path),
  **partials** to a new in-memory partials store keyed by
  `(session_id, speaker_label)`. The previous 10 s `CHUNK_DURATION`
  constant is gone; new `PUMP_TICK` of 500 ms is the only timing
  knob the pump owns. Whisper inference cadence (the ~3 s "when
  does a partial revise" interval) is internal to the streaming
  session.
- `meeting_session_get` IPC response gains `currentPartials:
  Vec<Utterance>` — the in-flight partials for the active session,
  sorted alphabetically by `speakerLabel` so render order is stable
  across polls. Closed sessions always return an empty
  `currentPartials` array. PR4 adds the visual treatment that
  distinguishes partials from finals.
- `SessionManager` gains a `partials: Arc<RwLock<HashMap<i64,
  HashMap<String, Utterance>>>>` field plus a `current_partials_for(
  session_id) -> Vec<Utterance>` reader. `RwLock` because the IPC
  poll path (~1/s) reads while the pump tick (~2/s) writes —
  readers shouldn't block each other. `stop_manual` clears the
  partials map for the closing session belt-and-braces; the pump's
  `finish()` path also clears entries as it commits final tail
  utterances.
- `dispatch_utterances` is the new per-tick routing helper: a final
  for source S clears the matching partial entry **before** the DB
  append (so a concurrent poll between clear-and-append sees neither
  rather than both, avoiding a brief duplicate render). Partials
  for source S **replace** the prior entry — at most one partial
  per source at any time. Cross-source isolation (mic final does
  not clear system partial) is pinned by tests.
- Streaming inference runs on `tokio::task::spawn_blocking` so
  whisper.cpp doesn't block the tokio worker thread. The streaming
  session round-trips through the spawn (taken out → moved in →
  returned with utterances) so the pump retains ownership across
  ticks. A panic in the spawned closure leaves the slot `None` for
  the rest of the session — that source goes dark until the next
  start, but the others continue.
- 8 new meeting-manager tests cover the partials store + dispatch
  contract: empty-on-new-session, partial replaces partial,
  per-source independence, final clears matching partial AND
  persists row, final does not clear other source's partial,
  empty-final filtering, stop_manual clears partials. Total: 27
  meeting-mode tests; 204 lib tests with `--features whisper`.
- 6 e2e mocks updated to include `currentPartials: []` on the
  `meeting_session_get` shape. Frontend type-check passes.
- What's deliberately not here yet (tracked in #108 PR4): visual
  rendering of partials in the panel — they arrive in the poll
  response but aren't yet rendered with italic / opacity. A
  consumer that ignored `currentPartials` would observably behave
  the same as today (modulo the lower latency on finals).

#### Audio drain-into for streaming-pump capture (#108 PR2)

- `AudioSession::drain_into(sink, ...) -> Result<CaptureFormat>` lets
  the meeting pump (#108 PR3) pull samples from a live capture
  handle on a tight tick (~500 ms) without stopping the session —
  the keystone shape change for streaming. Default impl errors so
  legacy mocks surface a clear diagnostic; the cpal mic backend and
  the ScreenCaptureKit system-audio backend both override.
- The cpal mic override routes a new `Cmd::DrainBuffer` to the audio
  worker thread (where the buffer Arc lives), `mem::take`'s the
  accumulated samples, and replies with `(samples, format)`. The
  worker round-trip is microsecond-scale — the alternative
  (leaking the buffer Arc into the handle at start time) would have
  required restructuring `Cmd::Start`'s reply shape, an invasive
  change for a one-call-per-tick path. The cpal stream keeps writing
  into the now-empty buffer between drains.
- The SCK override calls a new public `ScreenCaptureKitSession::drain_buffer()`
  helper — same `mem::take` discipline, no stream stop. The
  callback's Arc clone of the buffer remains valid across the drain.
- `stop()` continues to consume the handle as before; `drain_into`
  takes `&self` so it composes with the pump's `Vec<Box<dyn AudioSession>>`
  without lifetime gymnastics.
- 12 new audio-tests pin the contract: default-impl errors with an
  actionable message, override appends to the caller's sink (does
  not replace), repeated calls only return new samples since the
  previous drain. Total: 193 unit tests with `--features whisper`.

#### Phase A2: macOS system-audio capture via ScreenCaptureKit (#105)

- The "System audio" entry in the source picker is no longer the
  "coming soon" placeholder on macOS — it now drives a real
  ScreenCaptureKit capture session. Selecting it before pressing
  the dictation hotkey routes `start_dictation` through the new
  `audio::screencapturekit::ScreenCaptureKitSession` instead of
  the cpal mic path; samples land in the same `Vec<f32>` shape the
  rest of the transcription pipeline already consumes, so whisper
  / model-swap / replacements / history all work unchanged.
- Compiled behind a `screencapturekit` feature flag and a
  `cfg(target_os = "macos")` gate. Default builds remain cpal-only
  to keep CI and Linux/Windows tests deterministic; release macOS
  builds opt in via `cargo build --features screencapturekit`.
- Capture format is 48 kHz stereo f32 PCM, matching what the OS
  mixer already runs internally — avoids a forced resample at
  capture time. Existing `downmix_to_mono` and the whisper-side
  resampler reduce to 16 kHz mono ahead of transcription, same
  path as cpal mic input.
- TCC bucket is **Screen Recording** (Apple bundles audio-from-
  display under that prompt even when you capture zero pixels).
  First call triggers the prompt automatically; the existing
  `MacosDiagnosticPanel` already covers Screen Recording in its
  reset sweep.
- `AudioCapture::supports_source(SystemAudio)` returns `true` on
  macOS-with-feature, `false` everywhere else, so the source
  picker continues to render the option as disabled with a
  "coming soon" affordance on Linux / Windows / feature-off
  builds. Linux PulseAudio monitor (#106) and Windows WASAPI
  loopback (#107) are tracked as separate PRs.
- Test compilation needs `DYLD_FALLBACK_LIBRARY_PATH` pointed at
  the Xcode Swift toolchain (`/Applications/Xcode.app/Contents/
  Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/
  macosx`) when run with `--features screencapturekit`. Production
  app bundles inherit the Swift runtime from the macOS dyld
  shared cache and need no special handling.

#### Phase C runtime: manual-start meeting sessions (#110)

- Meeting Mode goes live in manual-start mode. The user clicks
  "Start a session" in the panel; the backend opens a session row
  via the new `SessionManager`. They dictate with the existing
  hotkey / button flow; each `stop_dictation` transcript lands as
  an utterance under the active session in addition to the existing
  history insert. They click "Stop session"; the manager writes
  `ended_at` and clears the active-session pointer. The panel
  renders a live-status indicator with a pulsing dot while a
  session is in progress.
- New `crate::meeting::SessionManager` owns the in-memory
  `Mutex<Option<i64>>` for the active-session id. Manual-start
  only — auto-detect from foreground app is a follow-up. The
  manager's `append_if_active` returns `Ok(false)` when no session
  is active, so the dictation hot path's behaviour is observably
  unchanged when meeting mode isn't being used.
- New `crate::meeting::AppClassifier` with hardcoded defaults
  (Zoom, Teams, Meet, Discord, Slack-call → Meeting; YouTube,
  Spotify, Apple Music → Media; everything else → Other). Used to
  stamp `app_kind` on new sessions for the panel's coloured tag.
  Per-user overrides are deferred to #112.
- Three new IPC commands: `meeting_active_session` (read),
  `meeting_start_manual` (write), `meeting_stop_manual` (write).
- `MeetingSessionsPanel.svelte` grows Start / Stop buttons + an
  active-session indicator. The page refreshes the panel after
  each successful `stop_dictation` so newly-appended utterances
  appear in the timeline.
- 9 new Rust unit tests cover the manager's lifecycle (start
  rejects concurrent starts, stop errors when no session, append
  computes cumulative timestamps correctly) plus the classifier's
  default-table behaviour. Total: 169 unit tests.

What's deliberately **not** here yet (tracked in #110):

- Auto-detect from foreground app — manual-start is the safer
  first step because it never records a meeting the user didn't
  intend to record.
- Streaming partial utterances — each session captures one final
  utterance per `stop_dictation` call. Streaming partials wait on
  #108.
- System-audio capture per platform — without #105 / #106 / #107
  shipped, meeting mode captures via mic only (a single-speaker
  "personal meeting transcript" experience).

#### Phase C scaffold: meeting sessions data layer + UI panel (#113)

- Meeting Mode scaffold (Phase C foundation; refs #33 / #109).
  Lands the data layer + UI shell for the meeting-transcript
  surface that the design memo (`docs/system-audio-meeting-mode-proposal.md`)
  describes. **What's wired today:**
  - Migration 0002: `meeting_sessions` and `utterances` tables,
    plus FTS5 index over utterance text. Additive only —
    existing v0.1.0 databases migrate cleanly.
  - `crate::meeting::{MeetingSession, NewMeetingSession,
    PersistedUtterance, NewPersistedUtterance, MeetingAppKind}`
    types + `MeetingSessionRepository` trait (sibling to the
    other Repository-pattern repos post-#88) + SQLite impl.
  - Four new IPC commands: `meeting_sessions_list`,
    `meeting_session_get`, `meeting_session_delete`,
    `meeting_session_set_notes`.
  - `MeetingSessionsPanel.svelte` rendered at the bottom of the
    main page. Today shows a scaffolded "no sessions yet"
    placeholder that explicitly enumerates what's pending and
    links to the relevant tracking issues (#105 macOS, #106
    Linux, #107 Windows, #108 streaming, #110 session manager,
    #111 diarization). Permanent privacy line surfaced at the
    top of the panel.
  - 9 new Rust tests pin the SQLite impl's behaviour: create,
    list, idempotent close_session, atomic append_utterance with
    count bump, ordered list_utterances, set_notes round-trip,
    and FK cascade on delete.
#### Streaming transcription — sliding-window foundation (#108 PR1)

- `Transcribe` trait gains `start_stream(format, prompt)` returning a
  `Box<dyn StreamingTranscribeSession>` handle the meeting pump (PR3)
  feeds samples into on the audio drain cadence. The handle exposes
  `feed`, `drain`, and `finish` so the pump can pull partials + finals
  on a tight tick without stopping capture. Default impl errors —
  backends that opt in (the whisper-rs path, plus a future Parakeet
  ONNX backend) override `start_stream` AND override
  `supports_streaming` to return `true`. Non-streaming backends and
  test mocks keep their existing behaviour unchanged; the dictation
  hot path stays on `transcribe_with_prompt`.
- `WhisperTranscription` overrides `start_stream` to construct a
  `WhisperStreamingSession` that runs whisper.cpp on a rolling 30 s
  window every ~3 s of new audio, emitting partials for the trailing
  tail and finals for segments aged past an 8 s commit threshold. The
  policy state machine (`SlidingWindowState`) is whisper-agnostic and
  unit-tested with a scripted `WhisperLikeInferer` mock — 15 unit
  tests pin the diff/commit logic against synthetic segment streams
  (window growth, partial revision, commit-and-slide, long-silence
  failsafe, empty-segment filtering, dedup high-water mark). The
  whisper bridge is tested end-to-end via the existing fixture WAV
  (`tests/fixtures/jfk.wav`) under a new `streaming_fixture`
  integration test, gated by `HUSH_TEST_MODEL` like the existing
  `audio_fixture` smoke. Smoke run against the bundled JFK clip with
  `ggml-base.bin` produced 3 mid-stream partials and a final
  matching the canonical "ask not what your country can do for you"
  transcript.
- The whisper context is now held behind an `Arc<Mutex<...>>` instead
  of a bare `Mutex` so streaming sessions can hold their own clones
  and run inferences from the meeting pump's blocking pool without
  coupling to the original `&self` lifetime. The dictation hot path's
  `transcribe`/`transcribe_with_prompt` call sites continue to work
  unchanged — `lock()` is the same shape regardless of `Arc` wrapping.
- See `learnings.md` (2026-04-26 entry) for the design discussion of
  time-based commit vs stability-based commit, the in-memory partial
  vs DB-write trade-off (PR3 will surface partials via the
  `meeting_session_get` IPC, not a separate Tauri event), and a few
  whisper-rs API specifics learned in passing (segment timestamps in
  10ms units; `set_no_context(true)` for sliding-window; `Send` but
  `!Sync` `WhisperContext`).

#### Phase B foundation: streaming-transcription scaffold

- `stop_dictation` now invokes inference through the streaming
  entry point (`Transcribe::transcribe_chunks`) rather than the
  one-shot `transcribe_with_prompt`. Default-impl behaviour is
  byte-identical to before — the captured buffer is passed as a
  single chunk, the default impl produces one final utterance, the
  text reaches the clipboard exactly as it did pre-refactor — but
  the call site is now ready for a future Whisper-sliding-window or
  Parakeet backend that emits multiple partial utterances mid-
  recording. Non-final utterances are filtered out at this layer
  (they're for live UI updates in Phase C, not the dictation hot
  path's single clipboard write); a future PR forwards them via
  Tauri events when `supports_streaming()` is true. All 149 unit
  tests pass unchanged, confirming the refactor is observably
  identical.
- Streaming-transcription foundation (Phase B of the meeting-mode
  pivot, refs #33; design memo at
  `docs/system-audio-meeting-mode-proposal.md`). Adds the
  `Utterance` struct (`text`, `startedAtMs`, `endedAtMs`, `isFinal`,
  optional `speakerLabel`) — the unit a streaming backend emits and
  the row shape Phase C will persist into the meeting-sessions
  table. Extends the `Transcribe` trait with two new methods:
  `transcribe_chunks(chunks, format, prompt)` returning
  `Vec<Utterance>` (default impl is the one-shot fallback —
  concatenates chunks, calls `transcribe_with_prompt`, returns one
  `is_final = true` utterance spanning the recording), and
  `supports_streaming() -> bool` (default false). No behaviour
  change today — the dictation hot path still calls the one-shot
  `transcribe`. Future PRs landing Whisper sliding-window or
  Parakeet streaming override `transcribe_chunks` and flip the
  capability flag; the IPC layer will forward partial utterances
  to the meeting-mode UI when `supports_streaming` returns true.
  Six new unit tests pin the default behaviour (single final
  utterance, prompt forwarding, stereo duration arithmetic, empty-
  chunks safety, capability default, serde wire shape).
#### Phase A1: audio source picker (#98, #101)

- Audio source picker — first user-visible step of the system-audio +
  meeting-mode pivot (Phase A1, refs #33; design memo at
  `docs/system-audio-meeting-mode-proposal.md`). The mic dropdown is
  now a grouped `<select>` with two `<optgroup>`s: every input device
  under "Microphone", and a single "System audio" entry under "System
  audio". The system-audio option is rendered disabled with a
  "(coming soon — #33)" suffix until the per-platform backend ships.
  New IPC command `audio_list_sources` returns enriched listings
  including capability flags. `start_dictation` now takes a
  discriminated `AudioSource` argument (`{ kind: "microphone",
  deviceId }` or `{ kind: "system-audio" }`) instead of the bare
  device id; the IPC accepts `null` for the default-mic case so
  hotkey-triggered dictation stays one-click. `list_input_devices`
  is kept as a transitional alias for one release. Three new Rust
  tests pin the listing default impl, the override path, and the
  camelCase wire shape.
#### Phase A foundation (#96)

- `AudioSource` enum (`Microphone(Option<String>)` / `SystemAudio`)
  and `AudioCapture::start_with_source` trait method on the audio
  backend boundary (#96, foundation for #33). No behaviour change
  yet — the dictation hot path still calls `start(device_id)` —
  but downstream PRs that wire ScreenCaptureKit (macOS), WASAPI
  loopback (Windows), and PulseAudio monitor sources (Linux) now
  have a clean trait shape to slot into. Five new unit tests pin
  the default behaviour (Microphone forwards correctly, SystemAudio
  errors usefully, capability check defaults correct, serde wire
  shape round-trips). Refs the meeting-mode design memo at
  `docs/system-audio-meeting-mode-proposal.md`.

### Fixed

#### Round-7 review consolidation

- **Defensive guard against silent empty-clipboard.** `stop_dictation`
  filters utterances on `is_final`, then writes the concatenated text
  to the clipboard. Round-7 technical-quality reviewer caught a real
  failure mode: a future streaming backend that emits only partial
  utterances (and never a final) would slip through the filter as an
  empty string, and the user would get a clipboard with nothing in it
  with no error surfaced. Now we explicitly check for "utterances
  returned but none final" and surface it as
  `IpcError::Transcription` with a clear message. The default impl
  one-shot path always emits exactly one final, so this branch is
  only reachable for misbehaving overrides.
- **`app_kind_from_str` fails loud on unknown values** instead of
  silently defaulting to `Other`. Round-7 reviewer flagged the
  silent-default as data-corruption-masking — a rogue write of
  `"video-call"` would render as a generic "Other" session with no
  signal that anything was wrong. Now `FromRow` returns
  `sqlx::Error::Decode` with a descriptive message. A future variant
  added to `MeetingAppKind` is a deliberate code change that updates
  the match arm; the database is never expected to hold values the
  match doesn't cover.
- **`IpcError::MeetingSessions` variant added.** Meeting commands
  previously mapped errors to `IpcError::Settings` with a string
  prefix, drifting from the per-domain pattern (`History`,
  `Replacements`). Now the four meeting-session commands return their
  own kind (`meeting-sessions`) so the frontend can switch on the
  variant for tailored recovery copy when the streaming pump (#110)
  starts driving real writes.
- **First-run welcome modal pulled ahead of `Promise.all`.**
  Round-7 UX reviewer noted a real timing bug: when the first-run
  flag fetch raced against the parallel data fetches, a fresh-install
  user could see the no-model setup banner before the welcome modal
  landed — the modal explaining permissions appeared after the user
  had already started clicking around looking for the record button.
  Awaiting the flag synchronously makes the modal beat the rest of
  the UI to first paint. Cost: one extra IPC round-trip (cheap, a
  single SQLite read of a boolean).
- **Meeting panel placeholder reframed as product copy.** The earlier
  placeholder read like a GitHub-ticket summary ("Session manager —
  tracked in #110"). Round-7 UX reviewer caught the developer-y
  framing. Now the headline reads "Live meeting transcripts are
  coming soon" with a one-paragraph user-facing summary; the
  developer-facing tracking-issue list is preserved verbatim under a
  "Developer notes" `<details>` disclosure for readers who want to
  follow along.
- **Privacy line tightened.** The earlier framing leaked
  implementation trivia (the "30s ring buffer" detail) into a
  user-facing line. Now it leads with the user benefit ("Hush
  transcribes meeting audio live and never saves the audio itself")
  and moves the buffer detail into a "How it works" `<details>`
  disclosure for users who want the full mechanism.
- **PRD §5b "Meeting Mode (v1.x)" added.** The design memo from
  #93 had proposed adding this section; the pivot was actively
  shipping but the policy doc still described Hush as
  dictation-only. Documentation reviewer flagged. The PRD now
  carries the canonical "Meeting Mode v1.x" text, with §3 and §10
  tightened in lockstep.
- **Design memo status line updated.** The memo at
  `docs/system-audio-meeting-mode-proposal.md` still said "Draft for
  discussion. Not approved; not in the PRD yet." even after Phases
  A1, B foundation, and C scaffold had landed. Now reads "Approved
  direction; actively shipping" with the concrete phase status.

### Changed

- Removed unused `zip` dependency from Cargo.toml (#91). It was
  declared with an "Archive/export support" comment but no source
  file imported it; the only `zip` references in the codebase are
  `iter().zip()` calls from std. Removing it cuts 188 lines from
  Cargo.lock (zip pulled in a substantial transitive subtree: aes,
  bzip2-rs, deflate64, flate2, indexmap, lzma-rs, pbkdf2, zopfli,
  etc.) — meaningful build-time and binary-size savings, plus a
  smaller supply-chain surface to audit.
- `sha2` dependency upgraded 0.10 → 0.11 (#94). The 0.11 release
  dropped its `LowerHex` impl on the digest array returned by
  `finalize()` (the underlying type changed from `GenericArray` to
  `hybrid_array::Array`); replaced both `format!("{:x}", ...)`
  call sites in `transcription/download.rs` with a small inline
  `hex_encode` helper. No behaviour change for the user — the
  on-disk hex format is byte-identical to the prior `LowerHex`
  output.
- `active-win-pos-rs` dependency upgraded 0.8 → 0.10 (#95).
  Transparent — `get_active_window()`'s return type and the
  `ActiveWindow.app_name` / `.title` fields used in
  `capture_foreground` are unchanged. 0.10 is the line that has
  macOS 26 / Sequoia compatibility tweaks; staying on 0.8 risked
  foreground-detection drift on the project's primary target
  platform.

## [0.1.0] - 2026-04-26

First tagged release. Captures the M3-complete state of Hush —
end-to-end functional dictation on macOS 26 with history,
replacements, vocabulary, model picker, auto-download, first-run
welcome, recording HUD, and an in-app permission diagnostic.

### Added

- Bundled audio test fixture (#34, follow-up to part-a). The
  ~344 KB public-domain JFK "ask not what your country can do for
  you" clip (16 kHz mono PCM, lifted from whisper.cpp's
  `samples/jfk.wav`) now ships in `src-tauri/tests/fixtures/jfk.wav`
  and backs the default audio path of the integration test. A
  contributor with a model on disk can now run
  `HUSH_TEST_MODEL=/path/to/ggml-base.bin cargo test --features
  whisper --test audio_fixture -- --ignored` without staging an
  audio file separately. `HUSH_TEST_AUDIO` still overrides for
  contributors who want to point at a different clip. Whisper
  models stay out-of-repo (75 MB+ each); the model env var remains
  required.
- In-app macOS permission diagnostic and reset (#67). A collapsible
  section on the main page shows the bundle id, hint copy for
  Microphone and Input Monitoring, direct links to the relevant
  Privacy panes in System Settings, and a "Reset permissions" button
  that runs `tccutil reset` for the Microphone, ListenEvent (Input
  Monitoring), and Accessibility categories scoped to the Hush
  bundle id. Recovery path for the stuck-permission state previously
  documented only in `docs/macos-permissions.md`. The section is
  hidden entirely on non-macOS builds.
- Initial project scaffold: Tauri 2 + Svelte + TypeScript frontend, Rust backend.
- Rust module stubs: audio, transcription, hotkey, dictionary, history, db, ipc, updater.
- SQLite schema with FTS5 history index (migration 0001).
- Repository meta-files: README, CONTRIBUTING, CODE_OF_CONDUCT, SECURITY, learnings.md.
- CI workflow: cargo clippy, rustfmt check, cargo test on every push and PR.
- GitHub PR template and bug/feature issue templates.
- Cross-platform audio capture via `cpal`, behind an `AudioCapture`
  trait so OS-touching code can be mocked at the test seam. Captures
  at the device's native format and surfaces it alongside the
  samples; downmix and 16 kHz resampling happen at the transcription
  stage where format-mismatches are recoverable.
- Local Whisper transcription via `whisper-rs`, behind a `Transcribe`
  trait at the heavy-dep boundary. Gated behind the `whisper` Cargo
  feature because whisper.cpp needs cmake. Pure-logic linear
  resampler converts any captured sample rate to the 16 kHz mono
  Whisper expects before inference. Constructor takes a caller-
  provided GGUF model path; auto-download lands in #30.
- Three Tauri commands wire the dictation pipeline end-to-end:
  `list_input_devices`, `start_dictation`, `stop_dictation`. The stop
  command captures the foreground app at recording start (via
  `active-win-pos-rs`), writes the transcript to the system
  clipboard, and fires a "Ready to paste" notification. Errors are a
  tagged enum (`{ kind, message? }`) so the frontend dispatches
  recovery copy on `kind` rather than parsing free-form strings.
- Minimal Svelte dictation UI replaces the Tauri starter's "greet"
  placeholder. M2 ships button-driven recording first; the hotkey
  layer adds keyboard control on top.
- Toggle-record global hotkey via `tauri-plugin-global-shortcut`,
  default `CmdOrCtrl+Shift+Space` (overridable via
  `HUSH_TOGGLE_HOTKEY`). The handler emits a `hotkey:toggle` event
  and the frontend dispatches start vs. stop against its existing
  `recording` flag, keeping one source of truth for UI state.
- SQLite persistence via `sqlx`, wrapped in a `SqliteDatabase` that
  opens the database at a caller-provided path with WAL journal
  mode, `synchronous=NORMAL`, and per-connection foreign-key
  enforcement, then runs the embedded migrations from
  `src-tauri/migrations/`. An `open_in_memory` helper backs tests
  that need a real SQLite without touching the filesystem.
- Push-to-talk global hotkey via `rdev`, default `RightControl`
  (overridable via `HUSH_PTT_HOTKEY`). A dedicated thread runs the
  blocking `listen` loop and forwards key-down / key-up as
  `hotkey:ptt-press` / `hotkey:ptt-release` events. Closes the PTT
  half of #5. macOS prompts for Input Monitoring on first press;
  Linux requires X11 (Wayland support is compositor-dependent per
  PRD §10).
- History persistence: every successful transcription auto-inserts
  into a SQLite-backed history table via the `HistoryRepository`
  trait (sharing the sqlx pool). Tauri commands (`history_list`,
  `history_search`, `history_delete`, `history_count`) back a
  frontend history view with debounced FTS5 search, newest-first
  ordering, and per-row copy / delete. The `Transcribe` trait gained
  a `model_label()` so each row records which model produced its
  transcript.
- Post-transcription find/replace pipeline: pure-logic
  `apply_replacements()` plus a SQLite-backed `ReplacementRepository`.
  Rules are literal substrings, applied in `(sort_order, id)` order
  before the text reaches the clipboard. Tauri commands
  (`replacements_list`, `_create`, `_update`, `_delete`) back a
  frontend "Replacements" panel.
- Vocabulary prompt-biasing: user-managed terms are joined into the
  initial prompt Whisper's decoder sees, biasing recognition toward
  proper nouns and jargon. Backed by `VocabularyRepository` and four
  new IPC commands. The `Transcribe` trait gained a default-impl
  `transcribe_with_prompt` so non-Whisper backends can ignore the
  prompt without forcing every callsite to branch. Closes #6.
- Generic key-value settings persistence: `SettingsRepository` trait +
  SQLite impl backing the `settings` table. First consumer: the
  model picker's `selected_model_id`.
- Whisper model picker: static catalog of
  the five Whisper variants (tiny / base / small / medium / large-v3)
  with size, speed/accuracy ratings, and descriptions. Frontend
  card-grid section adopts the layout the user shared as the design
  reference (per-card name + size + bar-rated speed/accuracy +
  description + Default badge on the active card). Two new IPC
  commands: `model_list` and `model_select`. The transcriber
  resolution at startup now reads `selected_model_id` from settings
  and looks for the file in `<app-data>/models/<filename>`; falls
  back to the legacy `HUSH_MODEL_PATH` env var for the existing dev
  workflow. Hot-swap is intentionally not in this PR — selecting a
  new model writes the setting and prompts the user to restart.
  Auto-download is a follow-up.
- Whisper model auto-download. Pure-logic streaming downloader
  (`transcription::download`) with SHA-256 verification: bytes
  stream into a `.part` sibling file, hash computed on the fly,
  atomic rename on success, `.part` deleted on failure or cancel.
  Frontend picker grows per-card actions — Download, Cancel,
  Try-again-on-failure, Remove — with a CSS progress bar driven by
  Tauri events (`model:download-progress`, `model:download-done`,
  `model:download-failed`). Catalog gains `download_url` (Hugging
  Face mirror) and `sha256` (per-model, empty until a contributor
  verifies each hash — auto-download refuses to start with an empty
  hash and surfaces a friendly "configure manually for now" hint).
  Backend tests run against a local `wiremock` server; no real
  Hugging Face round-trips in CI. Closes #30.

- Audio test fixture (#34 part-a): an `#[ignore]`d integration test
  in `src-tauri/tests/audio_fixture.rs` that loads a WAV via
  `HUSH_TEST_AUDIO` (defaults to the bundled `tests/fixtures/jfk.wav`
  once #89 landed), runs it through the full transcription stack,
  and asserts the output contains configurable expected words. WAV
  parsing via `hound` (dev-dep only). Validates the auto-download +
  transcription path end-to-end once a contributor places a model.
  System-audio loopback variant stays open behind #33.

- First-run welcome modal (closes #22). Explains the permissions
  Hush needs — Microphone for cpal, Input Monitoring for the rdev
  PTT listener — and links to System Settings → Privacy & Security
  via a new `open_macos_privacy_pane` command on macOS. Persists
  dismissal in the settings table so the modal only shows on a
  fresh install. The OS prompts themselves still fire at app
  startup; the welcome's job is to explain what just happened,
  not to trigger anything new.
- **Bug fix surfaced during #22:** PR #42 added the
  `model_download` / `model_cancel_download` / `model_remove`
  Tauri commands but never registered them in `lib.rs`'s
  `generate_handler!` list. Frontend invokes would have failed at
  runtime. All three are now wired up.

- Recording HUD overlay scaffold (scaffold half of #21). A second
  Tauri window (label `hud`) shown while dictation is active:
  borderless, transparent, always-on-top, no taskbar entry.
  Renders a pulsing red dot + "Recording" label. Show/hide hooks
  into `start_dictation` / `stop_dictation` so the HUD tracks the
  audio stream's lifecycle, not the slower transcription that
  follows. The level-meter half (cpal callbacks compute RMS, audio
  thread → Tauri event → meter animation) lands as a follow-up.
- Recording HUD level meter (closes the level-meter half of #21).
  Per-callback RMS is computed in the cpal sample-conversion
  loop and published into a lock-free `Arc<AtomicU32>` (encoded
  as `f32::to_bits()`); a 30 Hz tokio task reads the latest value
  and emits an `audio:level` Tauri event. The HUD page
  (`src/routes/hud/+page.svelte`) listens, smooths the value with
  a fast-attack / slow-release envelope on `requestAnimationFrame`,
  and renders a soft red bar to the right of the "Recording" label.
  The `AudioCapture` trait gained a default-impl `current_level()`
  so non-cpal backends and test mocks inherit a no-op zero — the
  HUD bar simply idles for them.

### Changed

- **Refactor: extract generic `Repository<T, NewT, Id>` trait (#36).**
  Replaces the four near-identical CRUD declarations on
  `ReplacementRepository` and `VocabularyRepository` with one generic
  trait in `src-tauri/src/repository.rs`. Each domain trait is now a
  marker that aliases the generic under a domain-meaningful name plus
  a blanket impl, so concrete types implement the four CRUD methods
  exactly once. `HistoryRepository` deliberately stays standalone (its
  paginated `list`, plus `search` / `count` / no-`update` semantics
  don't fit a uniform shape), but its `insert` method was renamed to
  `create` for naming consistency with the rest of the repos. The
  `spawn_history_insert` helper became `spawn_history_create` to
  match. `SettingsRepository` stays its own trait — K/V semantics are
  genuinely different. Pure refactor; tests unchanged.
- **Refactor: `AppStateBuilder` replaces 7-arg constructor (#37).**
  `AppState::new(audio, transcribe, history, replacements,
  vocabulary, settings, models_dir)` was at the readable threshold
  and the next features (auto-download state, system-audio source,
  HUD service) would each add another positional parameter.
  Replaced with a builder: `AppStateBuilder::new().audio(…).…build()?`.
  `build()` returns a descriptive error naming the first missing
  required field, so a future test that forgets one fails loudly
  instead of by silent panic. `transcribe` stays optional. Pure
  refactor — same `AppState`, same fields, same call paths.
- **Refactor: split monolithic `+page.svelte` into per-section
  components (#40).** No behavior change; e2e suite stayed green
  through the move. The 2351-line page is now a 1080-line layout
  that imports seven focused components from `src/lib/`:
  `ControlsSection`, `ResultBlock`,
  `HistoryPanel`, `ReplacementsPanel`, `VocabularyPanel`,
  `ModelPickerPanel`, `MacosDiagnosticPanel`. Cross-cutting state
  (`recording`, `busy`, `Promise.all` mount, download-progress
  listeners) stays in the parent; each child takes data and callback
  props. Shared TypeScript types live in `src/lib/types.ts`. Per-panel
  styles moved into each component's own `<style>` block (Svelte
  scopes by default).
- **Hot-load on model select + honest "needs-download" notice.** The
  picker used to show "Saved. Restart Hush to use the new model"
  after every selection — including selections of undownloaded
  models, where restart wouldn't help (the file isn't there). New
  flow: `model_select` returns `{ loaded: bool }`. If the file is on
  disk, the backend hot-swaps the loaded transcriber via
  `AppState::swap_transcriber` (no restart) and the notice reads
  "✓ Loaded. Ready to record." If not, the notice reads "Saved as
  default — but this model isn't downloaded yet. Click Download on
  the card below to fetch it." Selection persists either way, so a
  user can pre-select Whisper Large v3, click Download, restart,
  and have it picked up. The `transcribe` field on `AppState` moved
  from `Option<Arc<dyn Transcribe>>` to `Mutex<Option<...>>` to
  support the swap; the dictation hot path acquires the lock briefly
  only to clone the inner Arc. Whisper GGUF parsing happens on a
  `spawn_blocking` task so the IPC handler doesn't hold the tokio
  runtime for the 50–500 ms load. Model cards in the picker are now
  uniformly clickable (previously only downloaded cards were); the
  markup branches were unified into a single `<button>` element.
- **README + PRD honesty pass on PTT and platform support.** README's
  Shipped list now separates toggle-record (works everywhere) from
  push-to-talk (Linux + Windows only by default; macOS opt-in, with
  the rdev/macOS-26 caveat called out and linked to issues #69 + #70).
  A new "Platform support — honest version" table notes that
  Linux + Windows are theoretically supported and CI-validated but
  not hands-on tested by the maintainer, and invites contributions
  and bug reports for those platforms. PRD §3 (Goals) and §9 (v1
  feature list) both updated with reality checks dated 2026-04-26 so
  the policy doc stops promising what the code can't currently
  deliver on macOS 26.
- **Default toggle hotkey changed from `⌘/Ctrl+Shift+Space` to
  `Ctrl+⌥/Alt+H`** (literal Control + Option/Alt + H — `⌃⌥H` on
  macOS). The previous default conflicted with macOS's character-
  picker chord on some configurations. `Ctrl+Shift+H` was
  considered but collides with Finder's "Go to Home folder"; the
  Ctrl+Alt family doesn't have any system bindings on macOS,
  Linux, or Windows for the `H` key, and sits in the same modifier-
  family VoiceInk uses (`⌃⌥V`) so users coming from a similar
  tool find it immediately reachable. Frontend hint card, README,
  STATUS, and the hotkey doc comment all updated in lockstep.
  Override via `HUSH_TOGGLE_HOTKEY` env var.
- **macOS permission troubleshooting docs.** New
  `docs/macos-permissions.md` covers the dev-build permission
  flakiness — why `cargo tauri dev` permissions aren't as sticky as
  signed-bundle permissions, the symptoms ("PTT silently does
  nothing", "transcript is empty / silence", "prompt attributes to
  Terminal"), and the `tccutil reset Microphone com.khawkins.hush` /
  `tccutil reset ListenEvent com.khawkins.hush` recipe to unstick
  them. Linked from `CONTRIBUTING.md` and the README docs table.
- **`npm run dev-cleanup` convenience script.** Kills stale
  processes left over from a hung `cargo tauri dev` run — the dev
  binary itself, Tauri's runner, Vite's dev server (port 1420 freed
  via `lsof -ti :1420`). Pass `--reset` to also `tccutil reset` the
  three macOS TCC entries (`Microphone`, `ListenEvent`,
  `Accessibility`) so the next launch re-prompts cleanly. Lives in
  `scripts/dev-cleanup.sh`; the `--reset` flag is macOS-only and
  no-ops elsewhere.
- **HUD polish — top-right placement, light-desktop contrast,
  screen-reader title.** Three round-4 reviewer items the a11y batch
  in #48 deferred:
  - HUD now positions itself top-right of the primary monitor on
    every show (40 logical-px margin, multi-monitor aware via
    `Window::primary_monitor`). Previously the OS picked the spot,
    which often centered the HUD over whatever the user was
    dictating into. Computing on every show — not once at startup —
    handles laptops moved between displays mid-session.
  - Light-desktop contrast: a `prefers-color-scheme: light` block
    bumps the dot's red glow from `0.55` to `0.9` opacity and flips
    the pill border to `rgba(0, 0, 0, 0.2)` so the indicator stays
    visible against a bright wallpaper. Pill background stays dark
    — it's the contrast carrier for the white text.
  - HUD window title changed from `"Hush HUD"` to `"Hush —
    Recording"` so screen readers announce something meaningful
    when the window is enumerated. Visible in some platform
    accessibility trees even though `skipTaskbar: true` is set.
- **`stop_dictation` decomposed (closes #38).** The Tauri command's body
  shrank from ~95 lines across 8 inline steps to a flat sequence of
  named helpers: `stop_audio_capture`, `load_vocabulary_prompt`,
  `load_replacement_rules`, `take_foreground_snapshot`,
  `write_to_clipboard`, `fire_ready_notification`,
  `spawn_history_insert`. Behaviour-preserving: every helper keeps the
  best-effort-vs-fatal distinction the inline code had (vocabulary,
  replacements, notification, history are best-effort with `tracing`
  logging; audio.stop, transcription, clipboard remain fatal). New
  helpers are independently unit-tested, including the structural
  audio-error → `IpcError::Audio` mapping that previously relied on
  `stop_dictation`'s shape.
- **M2 polish.** Visible recording and transcribing states (pulsing red
  dot + status text + window-title indicator), spinner during the
  Whisper inference window, and an in-app shortcuts hint card so the
  default hotkeys are discoverable without reading the README.
- **Friendlier error copy.** IPC errors are now mapped to recovery-
  oriented strings in the frontend rather than shown as raw `kind:
  message` pairs. The `transcription-unavailable` case in particular
  gives an actionable hint about `HUSH_MODEL_PATH` and the `whisper`
  feature.
- **Empty input-device list** now surfaces a platform-aware
  troubleshooting hint instead of silently disabling the start button.
- **Dark-mode error contrast** raised so the warning text passes WCAG
  AA on a dark background (was `#ffa0a0` on `#3a1a1a`, flagged as
  borderline by the UX review).
- `prefers-reduced-motion` honoured by the new pulse / spin animations.

### Fixed

- **Audio buffer take is timing-tolerant on stream cleanup.** Earlier
  versions failed with "audio buffer still shared after
  stream drop".** `stop_session` previously used `Arc::try_unwrap` to
  pull the captured samples out of `Arc<Mutex<Vec<f32>>>`, requiring
  *sole* Arc ownership. On some platforms cpal's stream cleanup is
  asynchronous — the callback's Arc clone can outlive the
  `drop(session.stream)` call by a beat — so `try_unwrap` would error
  on a successful recording and the user got "Microphone error: audio
  buffer still shared after stream drop. Try selecting a different
  input device." Replaced with a `lock()` + `mem::take`. Locking is
  correct regardless of how many Arc clones are alive: if a final
  callback is mid-write we wait the milliseconds it takes to finish;
  otherwise the lock is uncontended. The leftover Arc clones drop on
  their own as cpal finishes cleanup. Surfaced during hands-on
  testing on macOS 26 — the issue was likely always intermittent on
  some configurations but the user kept hitting it.
- **Model download wasn't actually reaching the file** (regression
  surfaced by user during hands-on testing of #41/#72). Hugging
  Face migrated large-file serving to their Xet content-addressed
  storage CDN, hosted on `cas-bridge.xethub.hf.co` — a subdomain of
  `hf.co`, not `huggingface.co`. The redirect-allowlist predicate
  added in #53 only allowed `huggingface.co` and its subdomains, so
  every model download died at the very first redirect with
  "redirect to host outside huggingface.co". Predicate now allows
  both HF-owned zones (`huggingface.co` and `hf.co`). Suffix-match
  trap is still defended (typo-squats like `myhf.co` and
  `hf.co.attacker.com` are unit-tested as rejected). Hop cap of 4
  unchanged.
- **Whisper transcription compiled in by default** (closes
  the silent-no-model bug surfaced in hands-on testing). Pre-fix:
  `npm run tauri dev` built without `--features whisper`, so the
  binary contained no Whisper loader code. Users could download a
  model successfully — the file landed on disk at the right path
  with the right SHA — but on the next launch the app reported
  "no transcription model is loaded" because `build_transcriber`
  was a `cfg`-gated stub returning `None`. The diagnostic looked
  identical to "user forgot to download" but had nothing to do with
  the user's actions. `whisper` is now a `default` Cargo feature.
  cmake is therefore mandatory at build time; the README's
  Prerequisites block is updated to call this out in bold. UI-only
  contributors who don't want cmake can opt out via the new
  `npm run tauri:ui-only` script (`--no-default-features`).
- **First-time-user flow: "Set up your first model" banner.** Two
  problems hit the user on the first launch with no model: (a) the
  prominent action surface was Start recording, which on click
  surfaced a stale error pointing at `HUSH_MODEL_PATH` and rebuilding
  with `--features whisper` — instructions for the M1 dev workflow,
  not the M3 user workflow; (b) the actual setup path (the model
  picker) was below the fold with no signpost. Replaced with: a
  prominent "Set up your first model" banner above the recording
  controls, shown only when `models.some(isDownloaded) === false`,
  with a "Choose a model" button that scrolls to the picker. Start
  recording is also disabled in that state with a clear hover/aria
  hint ("Choose a model first") rather than a click-then-error
  flow. The `transcription-unavailable` error copy is rewritten to
  point at the in-app picker instead of env vars, and the click-
  through still scrolls to the picker. Two new Playwright specs pin
  the banner-shown and banner-hidden cases; the existing
  `transcription-unavailable` spec now asserts the new copy and
  asserts the old `HUSH_MODEL_PATH` reference does *not* appear.
- **Model auto-download is functional end-to-end** (closes #41). The
  five Whisper variants in `transcription::catalog` shipped with
  empty `sha256` strings — the auto-download orchestrator's
  defence-in-depth gate refused to start a download without a
  verified hash, so every "Download" click returned the friendly
  "configure manually for now" message and required the user to
  curl the model themselves and place it in the app-data models
  directory. Hashes are now sourced from Hugging Face's git-LFS
  `oid` field (content-addressed, can't drift independently of the
  file content) for `ggml-tiny.bin`, `ggml-base.bin`,
  `ggml-small.bin`, `ggml-medium.bin`, `ggml-large-v3.bin`.
  `ggml-tiny` was independently verified by downloading and running
  `shasum -a 256` against the API value. The download orchestrator's
  empty-hash gate stays in place so a future catalog addition can't
  silently bypass SHA verification.
- **PTT crash on macOS 26+ contained** (closes the crash
  half of the rdev issue; native CGEventTap replacement tracked
  separately). rdev 0.5's CGEventTap callback unconditionally calls
  `TSMGetInputSourceProperty` from its listener thread to compute a
  Unicode key-name string we never read. macOS 26's TSM tightened
  its dispatch-queue assertions and now `dispatch_assert_queue_fail`s
  on the first modifier-key event — a hard `__builtin_trap` (SIGTRAP),
  not a Rust panic, so `catch_unwind` can't save it. Mitigation: PTT
  listener is now skipped on macOS by default, with `HUSH_PTT_ENABLE=1`
  to opt in for users on older macOS where rdev still works, and
  `HUSH_PTT_DISABLE=1` as the kill switch on every platform. Toggle
  hotkey (Tauri's plugin) and button-driven dictation are unaffected.
  Documented in `docs/macos-permissions.md`. The proper fix — a
  native CGEventTap that bypasses TSM — is a follow-up tracking
  issue.
- **HUD window transparency on macOS via `macos-private-api` (closes #62).** The
  HUD's `transparent: true` window flag was a no-op on macOS without
  Tauri's `macos-private-api` Cargo feature + the matching
  `macOSPrivateApi: true` app-config flag. Without those, the dark
  translucent pill the HUD CSS draws was sitting inside a solid
  default window — defeating the design. Both flags are now wired
  on; the dev startup warning ("The window is set to be transparent
  but the `macos-private-api` is not enabled") is gone. Tauri docs
  flag a possible App Store implication; not relevant to Hush's v1
  distribution plan, captured in `learnings.md` for future
  reference.
- **Welcome modal tagline copy.** Said "Two permissions worth knowing
  about before you start" but the modal renders three sections —
  Microphone, Input Monitoring, and a privacy footer that isn't a
  permission per se. Re-worded to "Here's what to know about
  permissions and privacy before you start." Polish-graded leftover
  from the round-4 reviewer pass on #48.
- **Updater plugin no longer panics on app launch.**
  `tauri-plugin-updater::Builder::new().build()` was registered in
  `lib.rs` without a corresponding `plugins.updater` block in
  `tauri.conf.json` (the plugin requires `pubkey` + `endpoints` to
  deserialise). On startup the plugin's deserialiser hit a `null`
  config and the whole app crashed before the main window appeared
  with `PluginInitialization("updater", "...invalid type: null,
  expected struct Config")`. The plugin registration is commented
  out until #10 wires the signing key and endpoints; the Cargo and
  npm deps stay in place so #10 lands as a single focused PR.
- **Welcome modal a11y batch (closes #48).** Round-4 reviewer
  flagged four issues on the recent welcome / model-picker work:
  - Modal had no Escape-key dismissal — keyboard-only users were
    locked into clicking "Got it". A window-level keydown listener,
    gated on `showFirstRun`, now handles Escape (and also persists
    dismissal via `mark_first_run_completed`, matching button
    behaviour).
  - No focus trap — Tab could escape behind the backdrop. The
    modal now traps Tab within its three buttons (cycle forward
    from "Got it" wraps to "Open Microphone settings"; Shift+Tab
    from the first wraps back). Auto-focus lands on the first
    action on open; focus restores to the previously-focused
    element on dismiss.
  - Download progress bar's `aria-valuemax` lied when the total
    size was unknown — fell back to `100` while `aria-valuenow`
    held the byte count, so a screen reader announced
    "3 percent" at 15 MB of an unknown-size file. Indeterminate
    state now omits `aria-valuenow` / `aria-valuemax` (per
    WAI-ARIA convention) and adds an `aria-valuetext` that
    matches what's drawn.
  - Retry-UX race — the optimistic "downloading" chip was set
    *before* the IPC call, so a synchronous failure (e.g.
    SHA-256 not configured) caused a brief flash of progress.
    The optimistic state now sets after the invoke resolves, so
    failure paths simply never show the chip.

  Two new Playwright specs pin the Escape and focus-trap
  behaviour; the previously `fixme`-marked Escape spec is now
  real and passing.

### Tests

- **`drain_buffer` regression tests** for the audio-stop fix in PR #77.
  The cpal stream itself can't be unit-tested (no audio device in CI),
  but the load-bearing race-prone bit — "take the captured samples
  out of the shared `Arc<Mutex<Vec<f32>>>` regardless of how many Arc
  clones cpal hasn't dropped yet" — is now extracted as a free
  function `drain_buffer` and unit-tested. Three cases pinned: take
  from a unique Arc, take while two extra Arc clones are alive
  (simulating the cpal-cleanup-still-in-flight case the user hit on
  macOS 26), and empty-buffer no-op. A future regression that
  reintroduces `Arc::try_unwrap` (or any strong-count-sensitive
  operation) on this path fails the second test.
- **Frontend e2e via Playwright + mocked Tauri IPC.** New
  `tests/e2e/` suite drives the SvelteKit dev server in
  `HUSH_E2E=1` mode — `vite.config.js` swaps
  `@tauri-apps/api/{core,event}` for in-tree stubs in
  `tests/e2e/setup/`, so the page renders in plain Chromium without
  Tauri's runtime. Tests configure per-spec `invoke` handlers and
  fire backend-emitted events via `installMocks(page, overrides)`
  and `fireEvent(page, name, payload)`. New CI job runs the suite on
  Linux. Three smoke tests cover: returning user does not see the
  welcome modal, fresh install does and dismisses it on "Got it",
  and `transcription-unavailable` errors surface the model-path
  recovery hint. A fourth test (`fixme`-marked) documents the
  welcome-modal-no-Escape regression flagged in #48 — it flips
  green automatically when that fix lands. Full-stack flows (HUD
  lifecycle, hotkey registration, real audio, real download) stay
  open behind #57 (tauri-driver path).

### Security

- **HUD window has its own scoped capability** (closes #50). The
  recording HUD's secondary Tauri window (label `hud`) was not in
  any capability file — Tauri 2 defaults unlisted windows to deny,
  meaning the HUD's `listen('audio:level')` call (and so the level
  meter that just landed) silently never fires. Added
  `src-tauri/capabilities/hud.json` granting `core:default` only —
  the HUD doesn't need clipboard, notification, or shortcut
  permissions, so leaving them off keeps the blast radius minimal
  if a future page somehow runs untrusted content.
- **Download client redirect policy is host-restricted** (closes the
  Critical half of #49). The shared reqwest client previously inherited
  reqwest's default `Policy::default()` (up to 10 redirects to *any*
  host); a BGP/DNS hijack of `huggingface.co` could redirect into an
  attacker-controlled origin. SHA-256 verification still catches a
  swapped file, but the bandwidth + latency leak to a non-HF host is
  avoidable. New policy: hop-cap 4, every hop must be `huggingface.co`
  or a subdomain. The host-allowlist predicate is unit-tested,
  including the `evilhuggingface.co` / suffix-match-trap case.
- **README + PRD privacy claims clarified** (Important half of #49).
  Previously the README said "no internet required" — true for
  transcription, false for the first-run model download. Both
  documents now disclose: transcription is fully on-device, no audio
  ever leaves the machine, and the only network traffic is the
  one-time model download from Hugging Face.
- **`tauri-plugin-shell` removed entirely.** Was registered in
  `lib.rs` and present as `@tauri-apps/plugin-shell` in `package.json`
  but never invoked — `open_macos_privacy_pane` uses
  `std::process::Command::new("open")` directly with hard-coded
  whitelisted URLs. Removing the unused plugin tightens the
  capabilities surface (no `shell:allow-execute` exposure), shrinks
  the dep tree, and removes a future-PR footgun (a contributor
  reaching for the plugin would now have to add it back deliberately).
  `@tauri-apps/plugin-opener` was already de-registered on the Rust
  side in PR #31; cleaned up the npm-side leftover at the same time.

### Fixed

- IPC `start_dictation` no longer overwrites the foreground-app slot
  when the underlying audio backend fails to start. Previously a
  failed start could leave a stale foreground snapshot visible to a
  subsequent `stop_dictation` call.
- IPC `stop_dictation` no longer routes errors via substring matching
  on a merged anyhow message (which could send a Whisper error
  mentioning "device" to the `audio` variant). Audio and
  transcription failures are now classified structurally at the call
  site.
- Internal mutex acquisition uses `?` with a typed
  `IpcError::Internal` variant instead of `.expect("…mutex")`, so a
  poisoned lock no longer panics a Tauri command (which can
  destabilise the renderer).

---

*First entry: Hush is a behavioural reimplementation of [VoiceInk](https://github.com/Beingpax/VoiceInk). No source code copied or referenced.*
