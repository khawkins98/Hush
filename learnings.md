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

- `open_macos_privacy_pane(target)` is a Tauri command rather than the frontend invoking the URL via `tauri-plugin-shell` because the shell plugin's capability config doesn't whitelist `x-apple.systempreferences:` schemes by default and adding it would broaden permissions further than needed. The command takes an enum-shaped string (`microphone` / `input-monitoring` / `accessibility`) and rejects anything else, so a frontend bug can't pivot it into an arbitrary `open` launcher.
- The flag is just a settings-table row (`first_run_completed=true`), not a typed wrapper. Reuses the K/V infra; one new command per get/set.
- The welcome renders on **all** platforms, not just macOS. Linux / Windows users see the explanation copy and click "Got it"; the deep-link buttons no-op via the cfg-gated backend command. Adding platform-specific gating would require a new `host_platform` command or pulling in `@tauri-apps/plugin-os`; not worth the cost when the welcome content is mostly relevant everywhere (Microphone permission exists on every platform).

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

**App Store implication.** Tauri's docs flag that `macos-private-api` uses Apple private APIs that may complicate App Store review. Hush's v1 distribution plan (PRD §11) is direct distribution, not App Store, so this is irrelevant today. If the project ever pursues App Store distribution, the HUD's transparency design needs to be revisited — either accept solid-background HUD rendering on the App Store target (lossless via `cfg!(target_os = "macos")` config gating) or use only public APIs.

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
