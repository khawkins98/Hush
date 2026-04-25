# Learnings Log

Engineering decision log for Hush. Append-only, dated entries. Captures dependency choices, platform quirks, false starts, and anything future contributors would benefit from knowing.

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
