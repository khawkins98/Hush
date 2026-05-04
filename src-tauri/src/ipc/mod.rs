//! Tauri IPC layer — exposes the dictation pipeline to the frontend.
//!
//! Concept inspired by VoiceInk's hotkey-driven recording loop.
//! Reimplemented from observed public behaviour; no source code referenced.
//! See §13.8 of the PRD.
//!
//! ## Responsibilities
//!
//! - Hold the application's long-lived service handles (audio capture,
//!   transcription, history, replacements, vocabulary, settings, HTTP)
//!   inside [`AppState`], constructed once at startup and shared across
//!   Tauri command handlers via `tauri::State<AppState>`.
//! - Expose Tauri command handlers as thin wrappers that pull state and
//!   call into the underlying repository / capture / transcription
//!   modules. Orchestration of the dictation hot path lives in
//!   `commands::stop_dictation`, which delegates per-step to the
//!   helper functions in the same file (`load_vocabulary_prompt`,
//!   `load_replacement_rules`, `take_foreground_snapshot`,
//!   `spawn_history_create`, etc.).
//! - Capture the foreground app at the moment recording starts so the
//!   focused-app metadata is preserved even if Hush's own window grabs
//!   focus during the recording.
//!
//! ## Test seam (PRD §13.5)
//!
//! The orchestration is split into:
//!
//! - [`run_pipeline`] — pure(-ish) function that takes an `&dyn AudioCapture`
//!   plus an `&dyn Transcribe`, runs the audio→transcription path, trims,
//!   and returns the text. No Tauri or OS dep. Unit-tested with mock
//!   implementations of both traits.
//! - [`commands`] — thin Tauri command wrappers that pull state, call
//!   `run_pipeline`, and perform side effects (clipboard write, notification,
//!   foreground capture). Manual smoke checklists in `tests/manual/` cover
//!   these because they need a real Tauri app.

pub mod commands;
pub mod events;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::transcription::download::CancelHandle;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audio::{AudioCapture, CpalAudioCapture};
use crate::db::SqliteDatabase;
use crate::dictionary::{
    ReplacementRepository, SqliteReplacementRepository, SqliteVocabularyRepository,
    VocabularyRepository,
};
use crate::history::{HistoryRepository, SqliteHistoryRepository};
use crate::settings::{SettingsRepository, SqliteSettingsRepository};
use crate::transcription::Transcribe;

/// Hot-swappable transcriber slot, shared by reference between
/// [`AppState`] and [`crate::meeting::SessionManager`]. The inner
/// `Option` is `None` until a model is loaded; the outer `Arc` lets
/// the meeting pump observe model hot-swaps via the same mutex
/// `model_select` writes through. Aliased for type-complexity
/// hygiene (clippy) and so the type is easy to grep for at every
/// hand-off point.
pub type TranscribeSlot = Arc<Mutex<Option<Arc<dyn Transcribe>>>>;

// Re-exports kept light: the command functions are referred to by their
// `commands::` path inside `generate_handler!` (Tauri's macro looks up a
// hidden `__cmd__<name>` sibling, which a `pub use` does not carry).

/// Hard cap on how many redirects the download client follows before
/// erroring out. Hugging Face's `/resolve/main/` path is observed to
/// redirect at most twice (`huggingface.co` →
/// `cas-bridge.xethub.hf.co` → a signed URL still on the same Xet
/// CDN); four leaves headroom for a future re-architecture.
const MAX_DOWNLOAD_REDIRECTS: usize = 4;

/// Predicate for the redirect-policy closure: returns `true` iff
/// `host` is in one of Hugging Face's owned DNS zones. Both
/// `huggingface.co` and `hf.co` are HF-owned; the Xet content-
/// addressed storage that HF migrated large-file serving to in 2025
/// lives on `cas-bridge.xethub.hf.co`, which is a subdomain of
/// `hf.co` not `huggingface.co`. We need to allow the `hf.co` zone
/// or the model-download redirect chain dies — see PR #74 for the
/// regression that surfaced this.
///
/// Pulled out so the host-allowlist logic is unit-testable —
/// `reqwest::redirect::Attempt` has no public constructor, so the
/// closure as a whole is not, but this small predicate is the
/// load-bearing security check.
///
/// Care taken on the suffix match: `.huggingface.co` and `.hf.co`
/// (with leading dot) so a typo-squat like `evilhuggingface.co` or
/// `myhf.co` does not match.
fn is_huggingface_host(host: Option<&str>) -> bool {
    match host {
        Some(h) => {
            h == "huggingface.co"
                || h.ends_with(".huggingface.co")
                || h == "hf.co"
                || h.ends_with(".hf.co")
        }
        None => false,
    }
}

/// Outcome of the model-download redirect predicate, broken out
/// from the reqwest closure so the policy is unit-testable
/// (`reqwest::redirect::Attempt` has no public constructor — the
/// closure as a whole is not testable, but this is).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RedirectDecision {
    Follow,
    /// Static reasons rather than `Error<String>` so each branch
    /// matches against a `&'static str` in tests without
    /// stringifying.
    Stop(&'static str),
}

/// Pure logic behind the model-download redirect closure (#258).
///
/// Allows a hop when EITHER the destination is on an HF host OR
/// the immediately-previous URL was on an HF host. The second
/// clause covers HF → signed-CDN chains (S3, Cloudflare R2, etc.)
/// that surface when HF routes large-file serving through a
/// third-party object store. The signed URL itself isn't an HF
/// host, but the user trusts HF to redirect them to one — same
/// trust shape browsers use.
///
/// Only HTTPS is ever followed; an http:// destination is
/// rejected even from an HF origin (downgrade defence).
///
/// Caps at `MAX_DOWNLOAD_REDIRECTS` regardless of host trust.
pub(crate) fn redirect_decision(
    previous: &[reqwest::Url],
    destination: &reqwest::Url,
) -> RedirectDecision {
    if previous.len() >= MAX_DOWNLOAD_REDIRECTS {
        return RedirectDecision::Stop("too many redirects");
    }
    if destination.scheme() != "https" {
        return RedirectDecision::Stop("redirect to non-HTTPS scheme");
    }
    let dest_is_hf = is_huggingface_host(destination.host_str());
    let previous_is_hf = previous
        .last()
        .map(|u| is_huggingface_host(u.host_str()))
        .unwrap_or(false);
    if dest_is_hf || previous_is_hf {
        RedirectDecision::Follow
    } else {
        RedirectDecision::Stop(
            "redirect from non-HF host to non-HF host (signed-URL chain not extending HF origin)",
        )
    }
}

/// Snapshot of which application was in the foreground when dictation
/// started. Captured so the resulting history row records "you were
/// dictating into Slack / Notion / Mail" rather than "you were dictating
/// into Hush" (which is what `active-win-pos-rs` would report once the user
/// focuses our window to press the start button).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForegroundApp {
    pub app_name: String,
    pub window_title: String,
}

/// User-data repositories grouped behind a single [`AppState`] field.
///
/// The four bundled here share a single shape — `Arc<dyn …Repository>`
/// — and a single lifecycle: read/written by the dictation + history +
/// meeting flows, hot-mocked behind the trait seam in tests. Bundling
/// them keeps `AppState`'s top-level field count bounded as new
/// repositories land (e.g. future diarization summaries beyond the
/// current speaker-label column).
///
/// `settings` is intentionally NOT in this struct. It has a different
/// access pattern: read at boot to drive transcriber / PTT / autostart
/// state, written through a small fixed set of commands, observed
/// indirectly by the model hot-swap path. Mixing it with the user-data
/// repos would obscure that special role and force every consumer to
/// depend on the bundle when they only need settings.
///
/// `meeting_manager` (the stateful session lifecycle owner) also stays
/// flat — it isn't a `Repository` shape, it owns mutable in-memory
/// state, and meeting commands routinely need both `data.meetings`
/// (the row store) and `meeting_manager` (the live session) in the
/// same call.
pub struct DataServices {
    /// Dictation transcript history. CRUD + FTS5 search.
    pub history: Arc<dyn HistoryRepository>,
    /// User-defined find/replace rules applied to every transcript.
    pub replacements: Arc<dyn ReplacementRepository>,
    /// User-defined vocabulary terms threaded through the Whisper
    /// initial prompt for proper-noun bias.
    pub vocabulary: Arc<dyn VocabularyRepository>,
    /// Meeting Mode session row storage (refs #33 / #109).
    /// Read-side handle — browsing / deleting sessions reads from
    /// this. The write-side ([`AppState::meeting_manager`]) is the
    /// stateful owner that opens / closes sessions and appends
    /// utterances.
    pub meetings: Arc<dyn crate::meeting::MeetingSessionRepository>,
    /// Per-app classifier overrides (#112/#192). The Settings panel
    /// reads/writes through these IPC commands; the
    /// `SessionManager` reads at every session start so edits take
    /// effect without an app restart.
    pub meeting_app_overrides: Arc<dyn crate::meeting::MeetingAppOverrideRepository>,
}

/// Long-lived application state, registered with `tauri::Builder::manage`.
///
/// Ownership rules:
/// - `audio` is `Arc<dyn AudioCapture>` so a future hotkey layer can hold
///   its own clone and call `start`/`stop` without going through a
///   Tauri command.
/// - `transcribe` is `Option<Arc<dyn Transcribe>>` because the production
///   backend is gated behind the `whisper` Cargo feature *and* requires a
///   model path. When either is absent the `stop_dictation` command returns
///   [`commands::IpcError::TranscriptionUnavailable`] rather than crashing.
/// - `data` bundles the four user-data repositories — see [`DataServices`]
///   for the grouping rationale. Each is `Arc<dyn …Repository>` so the IPC
///   layer can hold handles without knowing about the SQLite-specific impl;
///   tests swap in deterministic mocks at the trait seam.
/// - `pending_foreground` is captured on `start_dictation` and taken on
///   `stop_dictation`. The `Mutex` is for `&self` interior mutability; it
///   is never contended on the hot path because dictation is fundamentally
///   serial.
pub struct AppState {
    pub audio: Arc<dyn AudioCapture>,
    /// Wrapped in a `Mutex` so `model_select` can hot-swap the loaded
    /// transcriber at runtime without restarting the app. The lock is
    /// held for the duration of one of two operations only: a clone
    /// of the inner `Arc` (microseconds, on the dictation hot path) or
    /// a wholesale replacement (only when the user picks a new model
    /// in the picker). No async work happens inside the lock — the
    /// model file load is done on a `spawn_blocking` task and only
    /// the resulting `Arc` is moved into the lock. See
    /// `swap_transcriber` for the swap path; `stop_dictation` for the
    /// read path.
    ///
    /// Wrapped in `Arc` so a hot-swap from `model_select` writes
    /// through to the dictation hot path (`stop_dictation`) on the
    /// next call without needing to rebuild `AppState`.
    ///
    /// Split from [`Self::transcribe_meeting`] under #248 so the
    /// dictation one-shot inference and the meeting pump's
    /// streaming inference don't contend on a single
    /// `Mutex<WhisperContext>`. Both slots load the same GGUF; the
    /// underlying weights are mmap'd, so the marginal RAM cost of
    /// the second context is near zero (just two `WhisperContext`
    /// structs on the heap).
    pub transcribe: TranscribeSlot,
    /// Meeting-pump transcribe slot. Owns its own
    /// `WhisperTranscription` instance distinct from
    /// [`Self::transcribe`] so a chunk-tick inference and a
    /// concurrent dictation `stop` don't queue up behind one
    /// `Mutex<WhisperContext>` (#248). Cloned into
    /// `SessionManager` at startup; `model_select` writes both
    /// slots in lockstep so the user-visible model stays
    /// consistent across the two paths.
    pub transcribe_meeting: TranscribeSlot,
    /// Speaker diarization seam (#111). Tags meeting utterances
    /// with per-speaker labels. Production wires
    /// [`crate::diarization::FlagGatedDiarizer`] which routes to
    /// [`crate::diarization::onnx::OnnxDiarizer`] when the
    /// Speakers toggle is on and the wespeaker model is loaded,
    /// else [`crate::diarization::NoopDiarizer`]. The builder
    /// default falls back to plain `NoopDiarizer` for tests.
    pub diarize: Arc<dyn crate::diarization::Diarize>,
    /// Persistent user-data repositories bundled together. See
    /// [`DataServices`] for why these four group naturally and why
    /// `settings` stays separate.
    pub data: DataServices,
    pub settings: Arc<dyn SettingsRepository>,
    /// Meeting Mode session lifecycle owner (#110 manual-start MVP).
    /// Holds an in-memory pointer to the active session id so the
    /// IPC layer's `stop_dictation` path can route transcripts into
    /// the active session as utterances. `Arc` because the manager
    /// outlives any single command call and is shared across the
    /// `meeting_*` handlers and `stop_dictation`.
    pub meeting_manager: Arc<crate::meeting::SessionManager>,
    /// Directory the model picker scans for downloaded GGUF files
    /// (`<app_data>/models/`). Stored on AppState rather than
    /// re-resolving it on every IPC call so the picker has a single
    /// source of truth and tests can override it.
    pub models_dir: PathBuf,
    /// HTTP client shared across all model downloads. Cheap to clone;
    /// holds connection-pool state internally. One per app keeps
    /// keep-alive working across consecutive downloads (e.g. Tiny
    /// then Base on first launch).
    pub http: reqwest::Client,
    /// Cache for the manual "Check for updates" probe (#333).
    /// GitHub's unauthenticated API rate limit is 60 req/h/IP; a
    /// shared corporate NAT or an impatient user clicking the
    /// Settings → About button can collectively burn that limit
    /// fast. We cache the last successful result for 15 minutes —
    /// well below the rate-limit window, well above the spam-click
    /// threshold. The frontend's `updateChecking` flag covers the
    /// in-flight case; this covers the back-to-back case.
    pub last_update_check: Mutex<Option<(std::time::Instant, crate::updater::UpdateCheckResult)>>,
    /// Cancel handles for in-flight downloads, keyed by model id.
    /// Inserted by `model_download` when it spawns a task; the cancel
    /// command flips the handle's flag; the spawned task removes its
    /// own entry on completion. Wrapped in `Arc` so the spawned task
    /// can hold a clone independently of the live `AppState` —
    /// previously the cleanup code reached back through
    /// `AppHandle::try_state` which forced the cancel-handle cleanup
    /// onto a code path that requires a real Tauri runtime
    /// (untestable, per #315).
    pub downloads: Arc<Mutex<HashMap<String, CancelHandle>>>,
    /// Serialises the SCK auto-confirm probe inside
    /// `get_permission_health` (#386). Without this, two near-
    /// simultaneous calls (Settings tab open + window-focus +
    /// startup probe firing concurrently) each `spawn_blocking`
    /// the `validate_screen_recording_capability` Cocoa round-
    /// trip and each stamp the settings row. The stamp is
    /// idempotent (last writer wins, both write near-identical
    /// timestamps), so it's wasted work rather than corruption —
    /// but the Cocoa round-trip is single-digit ms each and the
    /// frontend may issue several focus-driven refreshes in
    /// quick succession.
    ///
    /// `tokio::sync::Mutex` (not `std::sync::Mutex`) so the
    /// guard can be held across `.await` boundaries — the probe
    /// is `spawn_blocking().await` and the stamp is `set().await`.
    pub sck_probe_lock: tokio::sync::Mutex<()>,
    pub pending_foreground: Mutex<Option<ForegroundApp>>,
    /// User's chosen PTT key combo, hot-swappable via
    /// `ptt_set_combo`. The listener thread reads through this
    /// `RwLock` on every event so a Settings UI change takes effect
    /// without restarting the listener (rdev::listen has no clean
    /// stop API; we don't want to bounce the thread on every edit).
    /// Initialised from settings DB at boot, falls back to the
    /// platform default. See `crate::hotkey::ptt::PttCombo`.
    pub ptt_combo: Arc<std::sync::RwLock<crate::hotkey::ptt::PttCombo>>,
    /// Whether PTT is currently active. The listener observes
    /// every keyboard event regardless, but only emits press/release
    /// to the frontend when this flag is true. The IPC `ptt_set_config`
    /// command flips it for in-session toggles (no listener restart
    /// needed). At boot, this mirrors the persisted value and the
    /// env-var override.
    ///
    /// First-time opt-in: when the user enables PTT in a session
    /// that started with it off, `ptt_set_config` calls
    /// `register_ptt_listener` to spawn the listener thread on
    /// demand (which fires the macOS Input Monitoring prompt). The
    /// `ptt_listener_spawned` latch makes that idempotent.
    pub ptt_active: Arc<std::sync::atomic::AtomicBool>,
    /// Latch tracking whether the rdev listener thread has been
    /// spawned in this session. The spawn is idempotent — re-calling
    /// `register_ptt_listener` after the thread is up returns
    /// without spawning a second one. Used by `ptt_set_config` to
    /// start the listener on demand when the user toggles Enabled
    /// for the first time, so first-time opt-in doesn't require an
    /// app restart.
    pub ptt_listener_spawned: Arc<std::sync::atomic::AtomicBool>,
    /// Hot-swappable diarizer slot (#301). The `FlagGatedDiarizer`
    /// constructed in `build_default` holds an `Arc::clone` of
    /// this slot; the IPC `download_diarizer_model` path replaces
    /// the inner Arc after a successful wespeaker download so the
    /// new `OnnxDiarizer` takes effect on the next meeting tick
    /// — no app restart required.
    pub diarize_slot: crate::diarization::DiarizeSlot,
    /// User-facing flags that mirror Settings rows — see
    /// [`RuntimeFlags`] for the full per-field rationale. Grouped
    /// into a substruct (#431) so `AppState` stays readable.
    pub runtime_flags: RuntimeFlags,
}

/// User-facing runtime flags that mirror Settings rows (#431).
///
/// Each entry is an `Arc<Atomic*>` so the read-side hot paths
/// (dictation start, meeting pump tick, foreground poller, etc.)
/// can do lock-free reads, and the IPC `set_*` commands flip the
/// flag in place — the loaded `WhisperTranscription` and the
/// active `SessionManager` clone the same `Arc` at construction
/// time, so a Settings change is observable on the next inference
/// call / pump tick / poller tick without any reload.
///
/// Grouped into a substruct so [`AppState`]'s field count stays
/// readable. Pre-#431 these six atomics sat alongside the IPC
/// concerns (downloads map, http client, model slots, etc.) and
/// the audit flagged the resulting 22-field flat struct as harder
/// to navigate than its lock-typology warranted. The grouping is
/// purely organisational; the field types and Arc-sharing
/// semantics are unchanged from the pre-refactor shape.
pub struct RuntimeFlags {
    /// Whether the recording HUD overlay should appear during
    /// dictation / meeting capture. Read by `start_dictation` /
    /// meeting-pump start paths to decide whether to call
    /// `crate::hud::show`. Stored as an `AtomicBool` so the sync
    /// hot path can read it without locking; flipped by the
    /// `set_hud_enabled` IPC command, which also persists to the
    /// `hud_enabled` settings row.
    pub hud_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// Whether to play short audio cues at the recording-start
    /// and transcription-complete transitions (#292; cross-
    /// platform synthesis added in #446). Default off; opt-in
    /// via Settings → General → Audio cues. Stored as an
    /// `AtomicBool` so the dictation hot path reads without
    /// locking. Mirrored on the `sound_cues_enabled` settings row.
    pub sound_cues_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// Per-event sub-toggles (#463) read at the two cue callsites in
    /// `start_dictation_inner` and `stop_dictation`. The hot path
    /// fires the cue only when `master && sub` is true. Both default
    /// to `true` so users who already opted into the master toggle
    /// keep hearing both events; turning the master off short-
    /// circuits both regardless of these.
    pub sound_cue_start_enabled: Arc<std::sync::atomic::AtomicBool>,
    pub sound_cue_complete_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// User-chosen Meeting-Mode auto-start mode (Off / Always).
    /// Read by the foreground poller every tick; flipped by the
    /// `set_meeting_autostart_mode` IPC. Encoded as an
    /// `AtomicU8` (`0 = Off`, `1 = Always`) for lock-free reads.
    pub meeting_autostart_mode: Arc<std::sync::atomic::AtomicU8>,
    /// Whether speaker diarization should label utterances during
    /// meeting capture. Read by the meeting pump's dispatch path so
    /// the toggle takes effect on the next chunk without restarting
    /// the session. Stored as an `AtomicBool` so the read is lock-
    /// free; flipped by `set_diarization_enabled`, which also writes
    /// the `diarization_enabled` settings row.
    pub diarization_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// Whisper inference thread count (#255). Settings → General
    /// slider writes through the `set_inference_threads` IPC; the
    /// loaded `WhisperTranscription` shares this same Arc via
    /// `shared_inference_threads()` so a slider change is
    /// observable on the next inference call without a model
    /// reload. `AtomicI32` matches `whisper-rs`'s `set_n_threads`
    /// signature.
    pub inference_threads: Arc<std::sync::atomic::AtomicI32>,
    /// Microphone gain in dB (#531). Settings → General slider
    /// writes through `set_mic_gain_db`; the loaded
    /// `WhisperTranscription` and the meeting pump share this
    /// same Arc so a slider change takes effect on the next
    /// inference chunk without a model reload. Stored as
    /// `AtomicU32` holding `f32` bits (no `AtomicF32` in std).
    pub mic_gain_db: Arc<std::sync::atomic::AtomicU32>,
    /// Whether the LaunchAgent re-register on startup (#271)
    /// failed for a reason that needs the user's attention
    /// (read-only home, fs permission issue). Set by `lib.rs::run`
    /// when `autolaunch().enable()` returns Err. Read by Settings
    /// → General to show a "path is stale" warning row (#317).
    /// Cleared when `retry_autostart_registration` succeeds.
    pub autostart_path_stale: Arc<std::sync::atomic::AtomicBool>,
}

/// Encode [`crate::meeting::MeetingAutostartMode`] into the
/// `AppState::meeting_autostart_mode` atomic. Centralised so the
/// boot path, the IPC writer, and the poller reader all agree on
/// the byte value.
pub(crate) fn encode_autostart_mode(mode: crate::meeting::MeetingAutostartMode) -> u8 {
    match mode {
        crate::meeting::MeetingAutostartMode::Off => 0,
        crate::meeting::MeetingAutostartMode::Always => 1,
    }
}

/// Decode the byte stored in `AppState::meeting_autostart_mode`
/// back into the enum. Unknown bytes (a future variant a stale
/// build doesn't recognise) fall back to `Off` — the safer
/// default.
pub(crate) fn decode_autostart_mode(byte: u8) -> crate::meeting::MeetingAutostartMode {
    match byte {
        1 => crate::meeting::MeetingAutostartMode::Always,
        _ => crate::meeting::MeetingAutostartMode::Off,
    }
}

/// Builder for [`AppState`].
///
/// Replaces a 7-positional-arg constructor whose call sites read like an
/// unlabelled tuple. Each `.field(value)` call is self-documenting at the
/// call site, and adding a future required field (e.g. a download-state
/// service or a system-audio source) becomes a one-method addition rather
/// than a breaking-arg-list change at every caller.
///
/// `transcribe` is `Option<Arc<dyn Transcribe>>` rather than required
/// because the production backend is gated behind the `whisper` feature
/// AND a loaded model — both legitimately absent on a fresh install. The
/// rest are required; [`AppStateBuilder::build`] returns an error naming
/// the missing field if any of them are unset, which is more useful than
/// a panic when (e.g.) a future test refactor accidentally forgets one.
#[derive(Default)]
pub struct AppStateBuilder {
    audio: Option<Arc<dyn AudioCapture>>,
    transcribe: Option<Arc<dyn Transcribe>>,
    /// Pre-built `Arc<Mutex<...>>` for the transcribe slot. Set via
    /// [`AppStateBuilder::transcribe_arc`] when the caller (the
    /// production wiring in `build_default`) needs to share the same
    /// Arc with the meeting pump. When unset, `build` wraps
    /// [`Self::transcribe`] in a fresh Arc — the hot-swap surface
    /// stays inside `AppState` only.
    transcribe_arc: Option<TranscribeSlot>,
    /// Pre-built Arc for the meeting-pump slot. See [`AppState::transcribe_meeting`]
    /// (#248). When unset, `build` creates a fresh empty slot — fine
    /// for tests that don't drive the meeting pump.
    transcribe_meeting_arc: Option<TranscribeSlot>,
    diarize: Option<Arc<dyn crate::diarization::Diarize>>,
    history: Option<Arc<dyn HistoryRepository>>,
    replacements: Option<Arc<dyn ReplacementRepository>>,
    vocabulary: Option<Arc<dyn VocabularyRepository>>,
    settings: Option<Arc<dyn SettingsRepository>>,
    meetings: Option<Arc<dyn crate::meeting::MeetingSessionRepository>>,
    meeting_app_overrides: Option<Arc<dyn crate::meeting::MeetingAppOverrideRepository>>,
    meeting_manager: Option<Arc<crate::meeting::SessionManager>>,
    models_dir: Option<PathBuf>,
    ptt_combo: Option<crate::hotkey::ptt::PttCombo>,
    ptt_active: Option<bool>,
    hud_enabled: Option<bool>,
    sound_cues_enabled: Option<bool>,
    sound_cue_start_enabled: Option<bool>,
    sound_cue_complete_enabled: Option<bool>,
    meeting_autostart_mode: Option<crate::meeting::MeetingAutostartMode>,
    diarization_enabled: Option<bool>,
    /// Pre-built `Arc<AtomicBool>` for the diarization-enabled
    /// flag. Set via [`AppStateBuilder::diarization_enabled_arc`]
    /// when the production wiring (`build_default`) needs to
    /// share the same Arc with the meeting pump's
    /// [`crate::diarization::FlagGatedDiarizer`]. When unset,
    /// `build` constructs a fresh Arc seeded from
    /// [`Self::diarization_enabled`].
    diarization_enabled_arc: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Pre-built [`crate::diarization::DiarizeSlot`] for hot-swap
    /// support (#301). Set via
    /// [`AppStateBuilder::diarize_slot`] when the production
    /// wiring needs to share the same slot with the
    /// `FlagGatedDiarizer` so the post-download swap propagates.
    /// When unset, `build` constructs a fresh slot seeded with a
    /// `NoopDiarizer` — fine for tests that don't exercise the
    /// download / swap path.
    diarize_slot: Option<crate::diarization::DiarizeSlot>,
    /// Pre-built shared thread-count atomic (#255). Set via
    /// [`AppStateBuilder::inference_threads_arc`] when
    /// `build_default` wants to share the loaded
    /// `WhisperTranscription`'s atomic with the IPC writer. When
    /// unset, `build` constructs a fresh Arc seeded from the
    /// default thread count — fine for tests.
    inference_threads_arc: Option<Arc<std::sync::atomic::AtomicI32>>,
    /// Pre-built shared mic-gain atomic (#531). Set via
    /// [`AppStateBuilder::mic_gain_db_arc`] when `build_default`
    /// wants to share the loaded `WhisperTranscription`'s atomic
    /// with the IPC writer and the meeting pump. When unset,
    /// `build` constructs a fresh Arc at 0.0 dB — fine for tests.
    mic_gain_db_arc: Option<Arc<std::sync::atomic::AtomicU32>>,
}

impl AppStateBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn audio(mut self, audio: Arc<dyn AudioCapture>) -> Self {
        self.audio = Some(audio);
        self
    }

    /// Optional. `None` means "no transcriber loaded yet"; the IPC layer
    /// surfaces [`commands::IpcError::TranscriptionUnavailable`] for
    /// dictation calls while in this state.
    pub fn transcribe(mut self, transcribe: Option<Arc<dyn Transcribe>>) -> Self {
        self.transcribe = transcribe;
        self
    }

    /// Hand the builder the pre-built `Arc<Mutex<...>>` so the meeting
    /// pump can hold the same Arc and observe model hot-swaps. When
    /// supplied, the builder uses this directly instead of wrapping
    /// `transcribe()` in a fresh Arc.
    pub fn transcribe_arc(mut self, transcribe: TranscribeSlot) -> Self {
        self.transcribe_arc = Some(transcribe);
        self
    }

    /// Hand the builder the pre-built meeting-pump slot. Production
    /// (#248) loads a second `WhisperTranscription` instance and
    /// wires it here so `SessionManager` reads from a slot
    /// independent of the dictation one. Tests can leave this
    /// unset; `build` then constructs an empty slot.
    pub fn transcribe_meeting_arc(mut self, transcribe: TranscribeSlot) -> Self {
        self.transcribe_meeting_arc = Some(transcribe);
        self
    }

    /// Optional. Defaults to [`crate::diarization::NoopDiarizer`] —
    /// the meeting pump's existing source-derived `"mic"` /
    /// `"system"` labels survive. Override to wire D1 / D2.
    pub fn diarize(mut self, diarize: Arc<dyn crate::diarization::Diarize>) -> Self {
        self.diarize = Some(diarize);
        self
    }

    pub fn history(mut self, history: Arc<dyn HistoryRepository>) -> Self {
        self.history = Some(history);
        self
    }

    pub fn replacements(mut self, replacements: Arc<dyn ReplacementRepository>) -> Self {
        self.replacements = Some(replacements);
        self
    }

    pub fn vocabulary(mut self, vocabulary: Arc<dyn VocabularyRepository>) -> Self {
        self.vocabulary = Some(vocabulary);
        self
    }

    pub fn settings(mut self, settings: Arc<dyn SettingsRepository>) -> Self {
        self.settings = Some(settings);
        self
    }

    pub fn meeting_manager(mut self, mgr: Arc<crate::meeting::SessionManager>) -> Self {
        self.meeting_manager = Some(mgr);
        self
    }

    pub fn meetings(mut self, meetings: Arc<dyn crate::meeting::MeetingSessionRepository>) -> Self {
        self.meetings = Some(meetings);
        self
    }

    pub fn meeting_app_overrides(
        mut self,
        overrides: Arc<dyn crate::meeting::MeetingAppOverrideRepository>,
    ) -> Self {
        self.meeting_app_overrides = Some(overrides);
        self
    }

    pub fn models_dir(mut self, models_dir: PathBuf) -> Self {
        self.models_dir = Some(models_dir);
        self
    }

    pub fn ptt_combo(mut self, combo: crate::hotkey::ptt::PttCombo) -> Self {
        self.ptt_combo = Some(combo);
        self
    }

    pub fn ptt_active(mut self, active: bool) -> Self {
        self.ptt_active = Some(active);
        self
    }

    pub fn hud_enabled(mut self, enabled: bool) -> Self {
        self.hud_enabled = Some(enabled);
        self
    }

    pub fn sound_cues_enabled(mut self, enabled: bool) -> Self {
        self.sound_cues_enabled = Some(enabled);
        self
    }

    pub fn sound_cue_start_enabled(mut self, enabled: bool) -> Self {
        self.sound_cue_start_enabled = Some(enabled);
        self
    }

    pub fn sound_cue_complete_enabled(mut self, enabled: bool) -> Self {
        self.sound_cue_complete_enabled = Some(enabled);
        self
    }

    pub fn meeting_autostart_mode(mut self, mode: crate::meeting::MeetingAutostartMode) -> Self {
        self.meeting_autostart_mode = Some(mode);
        self
    }

    pub fn diarization_enabled(mut self, enabled: bool) -> Self {
        self.diarization_enabled = Some(enabled);
        self
    }

    /// Set the pre-built `Arc<AtomicBool>` that the FlagGatedDiarizer
    /// already holds. The AppState's `diarization_enabled` field
    /// becomes that same Arc, so the IPC `set_diarization_enabled`
    /// path flips both views with one atomic store.
    pub fn diarization_enabled_arc(mut self, arc: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.diarization_enabled_arc = Some(arc);
        self
    }

    /// Set the pre-built shared thread-count atomic (#255).
    /// `build_default` cloned this out of the just-loaded
    /// `WhisperTranscription::shared_inference_threads()`, so the
    /// AppState field, the IPC writer, and the transcriber all
    /// read/write through the same atomic.
    pub fn inference_threads_arc(mut self, arc: Arc<std::sync::atomic::AtomicI32>) -> Self {
        self.inference_threads_arc = Some(arc);
        self
    }

    /// Set the pre-built shared mic-gain atomic (#531).
    /// `build_default` clones this out of the loaded
    /// `WhisperTranscription::shared_mic_gain_db()` so the
    /// AppState field, the IPC writer, the dictation transcriber,
    /// and the meeting pump all read/write through the same atomic.
    pub fn mic_gain_db_arc(mut self, arc: Arc<std::sync::atomic::AtomicU32>) -> Self {
        self.mic_gain_db_arc = Some(arc);
        self
    }

    /// Set the pre-built [`crate::diarization::DiarizeSlot`] (#301).
    /// The `FlagGatedDiarizer` holds an `Arc::clone` of the same
    /// slot, so the IPC `download_diarizer_model` path can
    /// hot-swap the inner diarizer post-download.
    pub fn diarize_slot(mut self, slot: crate::diarization::DiarizeSlot) -> Self {
        self.diarize_slot = Some(slot);
        self
    }

    /// Construct the [`AppState`], or return a descriptive error naming
    /// the first required field that wasn't set.
    pub fn build(self) -> Result<AppState> {
        Ok(AppState {
            audio: self
                .audio
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: audio not set"))?,
            transcribe: self
                .transcribe_arc
                .unwrap_or_else(|| Arc::new(Mutex::new(self.transcribe))),
            transcribe_meeting: self
                .transcribe_meeting_arc
                .unwrap_or_else(|| Arc::new(Mutex::new(None))),
            diarize: self
                .diarize
                .unwrap_or_else(|| Arc::new(crate::diarization::NoopDiarizer)),
            data: DataServices {
                history: self
                    .history
                    .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: history not set"))?,
                replacements: self
                    .replacements
                    .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: replacements not set"))?,
                vocabulary: self
                    .vocabulary
                    .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: vocabulary not set"))?,
                meetings: self
                    .meetings
                    .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: meetings not set"))?,
                meeting_app_overrides: self.meeting_app_overrides.ok_or_else(|| {
                    anyhow::anyhow!("AppStateBuilder: meeting_app_overrides not set")
                })?,
            },
            settings: self
                .settings
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: settings not set"))?,
            meeting_manager: self
                .meeting_manager
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: meeting_manager not set"))?,
            models_dir: self
                .models_dir
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: models_dir not set"))?,
            http: reqwest::Client::builder()
                // Whisper-large-v3 is ~3 GB; ten-minute timeout is on
                // the optimistic side of "any reasonable home
                // connection". Real fix is resumable downloads, but
                // that's out of scope for this PR.
                .timeout(std::time::Duration::from_secs(600))
                .user_agent(concat!("hush/", env!("CARGO_PKG_VERSION")))
                // Redirect policy is host-restricted, not just hop-
                // capped. The default `Policy::default()` follows up
                // to 10 redirects to *any* host — a BGP/DNS hijack of
                // huggingface.co could redirect to an arbitrary server
                // and we'd transfer bytes there before the SHA-256
                // verification rejects them. SHA still catches a
                // swapped file, but the bandwidth + latency leak to
                // the attacker's host is avoidable.
                //
                // We allow up to four hops (HF's `/resolve/main/`
                // typically goes huggingface.co → cdn-lfs.huggingface.co
                // → a signed URL on the same CDN; four leaves headroom
                // for a future re-architecture).
                //
                // Browser-like trust model (#258): a hop is allowed
                // if EITHER its destination is on an HF host OR the
                // immediately-previous URL was on an HF host. The
                // second clause covers HF → S3-signed-URL chains
                // that surface when HF routes large-file serving
                // through a third-party CDN. Without it we'd reject
                // the perfectly-legitimate "HF told us to fetch the
                // file from this signed AWS URL" hop and the
                // download dies with no clear user-facing reason.
                //
                // Only HTTPS is ever followed — an http:// hop from
                // anywhere is rejected, including from an HF host.
                // Defends against a downgrade attack via a
                // (hypothetical) compromised HF redirect.
                .redirect(reqwest::redirect::Policy::custom(
                    |attempt| match redirect_decision(attempt.previous(), attempt.url()) {
                        RedirectDecision::Follow => attempt.follow(),
                        RedirectDecision::Stop(reason) => attempt.error(reason),
                    },
                ))
                .build()
                .expect("reqwest client should always build with default config"),
            downloads: Arc::new(Mutex::new(HashMap::new())),
            sck_probe_lock: tokio::sync::Mutex::new(()),
            pending_foreground: Mutex::new(None),
            last_update_check: Mutex::new(None),
            ptt_combo: Arc::new(std::sync::RwLock::new(self.ptt_combo.unwrap_or_else(
                || crate::hotkey::ptt::PttCombo::single(crate::hotkey::ptt::DEFAULT_PTT_KEY),
            ))),
            ptt_active: Arc::new(std::sync::atomic::AtomicBool::new(
                self.ptt_active.unwrap_or(false),
            )),
            ptt_listener_spawned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            diarize_slot: self.diarize_slot.unwrap_or_else(|| {
                Arc::new(std::sync::RwLock::new(
                    Arc::new(crate::diarization::NoopDiarizer)
                        as Arc<dyn crate::diarization::Diarize>,
                ))
            }),
            runtime_flags: RuntimeFlags {
                hud_enabled: Arc::new(std::sync::atomic::AtomicBool::new(
                    self.hud_enabled.unwrap_or(true),
                )),
                sound_cues_enabled: Arc::new(std::sync::atomic::AtomicBool::new(
                    self.sound_cues_enabled.unwrap_or(false),
                )),
                sound_cue_start_enabled: Arc::new(std::sync::atomic::AtomicBool::new(
                    self.sound_cue_start_enabled.unwrap_or(true),
                )),
                sound_cue_complete_enabled: Arc::new(std::sync::atomic::AtomicBool::new(
                    self.sound_cue_complete_enabled.unwrap_or(true),
                )),
                meeting_autostart_mode: Arc::new(std::sync::atomic::AtomicU8::new(
                    encode_autostart_mode(
                        self.meeting_autostart_mode
                            .unwrap_or(crate::meeting::MeetingAutostartMode::Off),
                    ),
                )),
                diarization_enabled: self.diarization_enabled_arc.unwrap_or_else(|| {
                    Arc::new(std::sync::atomic::AtomicBool::new(
                        self.diarization_enabled.unwrap_or(false),
                    ))
                }),
                inference_threads: self
                    .inference_threads_arc
                    .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicI32::new(4))),
                mic_gain_db: self
                    .mic_gain_db_arc
                    .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicU32::new(0f32.to_bits()))),
                autostart_path_stale: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        })
    }
}

// `AppHandleMeetingEventEmitter` was the production glue between the
// meeting module's `MeetingEventEmitter` trait and `tauri::AppHandle::emit`.
// Both went away in #431: the meeting module now consumes
// `crate::events::EventEmitter` directly, and the production wrapper is
// `crate::ipc::events::TauriEventEmitter` (constructed below in the
// `SessionManager::new` call site).

/// Parse the persisted [`crate::settings::keys::HUD_ENABLED`] row
/// into a bool. Wire encoding lives in [`crate::settings::codec`];
/// this helper layers on the per-key absent-row default of `true`
/// (the HUD is on by default — first-time users benefit from the
/// visual cue that the mic is hot, and a corrupted row shouldn't
/// silently turn it off either).
pub(crate) fn parse_hud_enabled_setting(raw: Option<String>) -> bool {
    crate::settings::codec::decode_bool(raw.as_deref()).unwrap_or(true)
}

/// Parse the persisted [`crate::settings::keys::SOUND_CUES_ENABLED`]
/// row (#292). Wire encoding lives in [`crate::settings::codec`];
/// per-key default is `false` (audio cues are off by default —
/// they'd be intrusive in shared spaces / meeting rooms, opt-in is
/// the right shape).
pub(crate) fn parse_sound_cues_setting(raw: Option<String>) -> bool {
    crate::settings::codec::decode_bool(raw.as_deref()).unwrap_or(false)
}

/// Parse a per-event sound-cue sub-toggle row (#463). Absent /
/// unparseable rows fall back to `true` — the per-event toggles
/// are scoped beneath the master row, so the safe default is
/// "fire everything the master allows" rather than silently
/// dropping a cue the user opted into via the master switch.
pub(crate) fn parse_sound_cue_sub_setting(raw: Option<String>) -> bool {
    crate::settings::codec::decode_bool(raw.as_deref()).unwrap_or(true)
}

/// Parse the persisted [`crate::settings::keys::DIARIZATION_ENABLED`]
/// row (#111).
///
/// **Pre-#478**: per-key default was `false` because the wespeaker
/// model wasn't downloaded out of the box and triggering a download
/// at first-record-time would have surprised the user. **Post-#478**
/// the wespeaker model ships alongside the Whisper model in the
/// first-run download flow, so by the time `diarization_enabled` is
/// read the model is on disk (or first-run was explicitly skipped /
/// the wespeaker download failed best-effort, in which case the
/// `FlagGatedDiarizer` falls through to `NoopDiarizer` automatically
/// — no behavioural difference for the recording).
///
/// Default flips to `true`. Existing users who explicitly toggled
/// the setting OFF have a `"false"` row in the settings table; the
/// `decode_bool` step preserves that, so their preference survives
/// the upgrade. The `unwrap_or(true)` only fires for absent /
/// corrupted rows — i.e. fresh installs and new users.
pub(crate) fn parse_diarization_enabled_setting(raw: Option<String>) -> bool {
    crate::settings::codec::decode_bool(raw.as_deref()).unwrap_or(true)
}

/// Parse the persisted [`crate::settings::keys::INFERENCE_THREADS`]
/// row into an `i32` clamped to the band whisper.cpp accepts (#255).
/// Absent or unparseable rows fall back to
/// `DEFAULT_INFERENCE_THREADS` so existing installs see no behaviour
/// change until the user touches the slider.
pub(crate) fn parse_inference_threads_setting(raw: Option<String>) -> i32 {
    #[cfg(feature = "whisper")]
    let default_threads = crate::transcription::DEFAULT_INFERENCE_THREADS;
    #[cfg(not(feature = "whisper"))]
    let default_threads: i32 = 4;
    let parsed = raw
        .as_deref()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(default_threads);
    #[cfg(feature = "whisper")]
    {
        parsed.clamp(
            crate::transcription::MIN_INFERENCE_THREADS,
            crate::transcription::MAX_INFERENCE_THREADS,
        )
    }
    #[cfg(not(feature = "whisper"))]
    {
        parsed.clamp(1, 16)
    }
}

/// Parse and clamp a stored mic gain dB string. Returns 0.0 (unity) when
/// the setting is absent or unparseable; clamps to `[0.0, 20.0]` otherwise.
pub(crate) fn parse_mic_gain_db_setting(raw: Option<String>) -> f32 {
    raw.as_deref()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or(0.0)
        .clamp(0.0, 20.0)
}

/// Build the "inner" diarizer for the FlagGatedDiarizer (#111).
///
/// When the `diarization-onnx` feature is built in AND the wespeaker
/// model file is present at `models_dir/<filename>`, returns an
/// `OnnxDiarizer`. Otherwise (feature off, model not downloaded
/// yet, or load failure) returns `NoopDiarizer` so the boot path
/// stays resilient — a user without the model file still gets a
/// working app, just with source-only labels.
///
/// Errors loading the model are logged at `warn` level and treated
/// as "fall back to Noop" — same as missing-file.
fn build_diarizer_inner(_models_dir: &Path) -> Arc<dyn crate::diarization::Diarize> {
    #[cfg(feature = "diarization-onnx")]
    {
        let model_path =
            _models_dir.join(crate::diarization::catalog::WESPEAKER_RESNET34_LM_FILENAME);
        if model_path.exists() {
            match crate::diarization::onnx::OnnxDiarizer::new(&model_path) {
                Ok(d) => {
                    tracing::info!(
                        path = %model_path.display(),
                        "diarization: loaded OnnxDiarizer (wespeaker)"
                    );
                    return Arc::new(d);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %model_path.display(),
                        "diarization: OnnxDiarizer load failed; falling back to Noop"
                    );
                }
            }
        } else {
            tracing::info!(
                path = %model_path.display(),
                "diarization: model file not found; using NoopDiarizer (manual download required)"
            );
        }
    }
    Arc::new(crate::diarization::NoopDiarizer)
}

impl AppState {
    /// Build the state used in production: the cpal audio backend, the
    /// SQLite-backed history repository at `db_path`, plus (when the
    /// `whisper` feature is enabled and `HUSH_MODEL_PATH` points at a
    /// readable GGUF file) a whisper transcriber loaded from that path.
    ///
    /// Why `db_path` is a parameter: the platform app-data directory is
    /// only resolvable from a Tauri `App` / `AppHandle`, which doesn't
    /// exist until `setup` runs. The caller in `lib.rs::run` does the
    /// resolution and hands us the path, so this function stays trivially
    /// testable.
    ///
    /// Why the env var still exists alongside the picker: the
    /// `HUSH_MODEL_PATH` path is a power-user / CI override. The
    /// picker (Settings → Model) is the primary mechanism for end
    /// users; the resolved selection persists in the settings DB,
    /// and the env var wins when both are set so a developer can
    /// pin a specific GGUF for the session without disturbing the
    /// user's saved choice.
    pub async fn build_default(
        app: tauri::AppHandle,
        db_path: &Path,
        models_dir: PathBuf,
    ) -> Result<Self> {
        let audio: Arc<dyn AudioCapture> = Arc::new(CpalAudioCapture::new());

        let db = SqliteDatabase::open(db_path)
            .await
            .with_context(|| format!("open database at {}", db_path.display()))?;
        let db = Arc::new(db);

        let history: Arc<dyn HistoryRepository> =
            Arc::new(SqliteHistoryRepository::new(Arc::clone(&db)));
        let replacements: Arc<dyn ReplacementRepository> =
            Arc::new(SqliteReplacementRepository::new(Arc::clone(&db)));
        let vocabulary: Arc<dyn VocabularyRepository> =
            Arc::new(SqliteVocabularyRepository::new(Arc::clone(&db)));
        let settings: Arc<dyn SettingsRepository> =
            Arc::new(SqliteSettingsRepository::new(Arc::clone(&db)));
        let meetings: Arc<dyn crate::meeting::MeetingSessionRepository> = Arc::new(
            crate::meeting::SqliteMeetingSessionRepository::new(Arc::clone(&db)),
        );
        let meeting_app_overrides: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
            Arc::new(crate::meeting::SqliteMeetingAppOverrideRepository::new(db));
        // Inference thread count (#255). Read the persisted value
        // from settings, clamp to the supported range, build the
        // shared Arc that AppState + WhisperTranscription + the IPC
        // writer all read/write through. Constructed *before*
        // `build_transcriber` so the loaded model points at this
        // exact Arc, not a fresh one.
        let inference_threads_initial = parse_inference_threads_setting(
            settings
                .get(crate::settings::keys::INFERENCE_THREADS)
                .await
                .ok()
                .flatten(),
        );
        let inference_threads_arc =
            Arc::new(std::sync::atomic::AtomicI32::new(inference_threads_initial));

        // Mic gain (#531). Same Arc-sharing pattern as inference_threads:
        // built before `build_transcriber` so the loaded models and the
        // meeting pump all point at this exact Arc. Default 0.0 dB (unity).
        let mic_gain_db_initial = parse_mic_gain_db_setting(
            settings
                .get(crate::settings::keys::MIC_GAIN_DB)
                .await
                .ok()
                .flatten(),
        );
        let mic_gain_db_arc =
            Arc::new(std::sync::atomic::AtomicU32::new(mic_gain_db_initial.to_bits()));

        // Resolve which transcriber to load at startup. Order:
        //   1. settings → `selected_model_id` → `<models_dir>/<filename>`
        //   2. legacy `HUSH_MODEL_PATH` env var (M1/M2 dev workflow)
        //   3. None — IPC surfaces `TranscriptionUnavailable`.
        // Step 1 resolves the M3 picker; step 2 keeps the existing dev
        // setup working until a user actually opens the picker once.
        //
        // Loaded twice so the dictation hot path and the meeting pump
        // each own a private `WhisperContext` and don't contend on a
        // single mutex (#248). Marginal RAM cost is small — `whisper-rs`
        // mmap's the GGUF, so the two contexts share the underlying
        // weights on disk. Both instances share the same
        // `inference_threads_arc` so the slider in Settings (#255)
        // takes effect on every inference call regardless of slot.
        let transcribe_dictation =
            build_transcriber(&settings, &models_dir, &inference_threads_arc, &mic_gain_db_arc)
                .await;
        let transcribe_meeting =
            build_transcriber(&settings, &models_dir, &inference_threads_arc, &mic_gain_db_arc)
                .await;

        // Wrap each instance in its own `Arc<Mutex<...>>` so
        // `model_select` can hot-swap independently. SessionManager
        // gets the meeting slot only — it never needs to read or
        // write the dictation slot, and vice versa.
        let transcribe_shared = Arc::new(Mutex::new(transcribe_dictation));
        let transcribe_meeting_shared = Arc::new(Mutex::new(transcribe_meeting));
        let event_emitter: Arc<dyn crate::events::EventEmitter> =
            Arc::new(crate::ipc::events::TauriEventEmitter::new(app.clone()));
        // Diarization wiring (#111, post-#310):
        //
        // 1. Read the persisted `diarization_enabled` flag and
        //    build an `Arc<AtomicBool>` so the IPC `set_*` path
        //    and the FlagGatedDiarizer share the same view.
        // 2. If the `diarization-onnx` feature is built in AND
        //    the wespeaker .onnx file is present in models_dir,
        //    instantiate `OnnxDiarizer`. Otherwise use Noop.
        // 3. Wrap the inner diarizer in FlagGatedDiarizer with
        //    Noop as the off-state fallback. Settings → toggle
        //    flips routing between the two without restart.
        //
        // The model-on-disk gate keeps the boot path resilient:
        // a user who hasn't downloaded the model yet still gets
        // a working app (just with source-only labels).
        let diarization_enabled_initial = parse_diarization_enabled_setting(
            settings
                .get(crate::settings::keys::DIARIZATION_ENABLED)
                .await
                .ok()
                .flatten(),
        );
        let diarization_enabled_arc = Arc::new(std::sync::atomic::AtomicBool::new(
            diarization_enabled_initial,
        ));
        // Hot-swappable inner-diarizer slot (#301). Owned by
        // AppState; cloned into the FlagGatedDiarizer below; cloned
        // again into the IPC `download_diarizer_model` writer so a
        // post-download swap propagates to the meeting pump on the
        // next tick — no restart needed.
        let diarize_inner_initial = build_diarizer_inner(&models_dir);
        let diarize_slot: crate::diarization::DiarizeSlot =
            Arc::new(std::sync::RwLock::new(diarize_inner_initial));
        let diarize_fallback: Arc<dyn crate::diarization::Diarize> =
            Arc::new(crate::diarization::NoopDiarizer);
        let diarize: Arc<dyn crate::diarization::Diarize> =
            Arc::new(crate::diarization::FlagGatedDiarizer::new(
                Arc::clone(&diarization_enabled_arc),
                Arc::clone(&diarize_slot),
                Arc::clone(&diarize_fallback),
            ));
        let meeting_manager = Arc::new(crate::meeting::SessionManager::new(
            Arc::clone(&meetings),
            Arc::clone(&audio),
            Arc::clone(&transcribe_meeting_shared),
            event_emitter,
            Arc::clone(&diarize),
            Arc::clone(&meeting_app_overrides),
            Arc::clone(&mic_gain_db_arc),
        ));

        // Restore the user's persisted PTT combo, if any. Falls back
        // to the platform default single-key (RightMeta on macOS,
        // RightControl elsewhere) when no value is stored. Parse
        // failures fall back too — a corrupt settings row shouldn't
        // brick the listener.
        let ptt_combo = match settings.get(crate::settings::keys::PTT_COMBO).await {
            Ok(Some(raw)) => crate::hotkey::ptt::parse_ptt_combo(&raw).unwrap_or_else(|e| {
                tracing::warn!(error = %e, raw = %raw, "stored PTT combo failed to parse; using default");
                crate::hotkey::ptt::PttCombo::single(crate::hotkey::ptt::DEFAULT_PTT_KEY)
            }),
            _ => crate::hotkey::ptt::PttCombo::single(crate::hotkey::ptt::DEFAULT_PTT_KEY),
        };

        // Persisted PTT-enabled flag. Env vars take precedence so
        // power users / CI can hard-force the listener regardless of
        // the persisted value:
        //   - HUSH_PTT_DISABLE=1 → off
        //   - HUSH_PTT_ENABLE=1  → on
        //   - otherwise: settings DB → persisted value
        //   - otherwise: platform default (true everywhere now that
        //     the macOS-26 abort is fixed by pinning rdev to
        //     fufesou's fork). The Input Monitoring TCC prompt fires
        //     on first listener spawn — same as it would when the
        //     user later flipped the toggle in Settings — so the
        //     onboarding cost is "one extra prompt at first launch"
        //     in exchange for the toggle hotkey + PTT both working
        //     out of the box. Pre-#55 this was opt-out on macOS
        //     because rdev::listen aborted; that constraint no
        //     longer applies.
        let ptt_active = match settings.get(crate::settings::keys::PTT_ENABLED).await {
            Ok(Some(raw)) => raw == "true",
            _ => true,
        };

        // HUD on by default. Absent / unparseable settings rows fall
        // through to the default rather than silently turning the
        // HUD off — first-time users benefit from the visual cue
        // that the mic is hot.
        let hud_enabled = parse_hud_enabled_setting(
            settings
                .get(crate::settings::keys::HUD_ENABLED)
                .await
                .ok()
                .flatten(),
        );

        // Audio cues — off by default (#292). Reads the same
        // settings table the IPC commands write through.
        let sound_cues_enabled = parse_sound_cues_setting(
            settings
                .get(crate::settings::keys::SOUND_CUES_ENABLED)
                .await
                .ok()
                .flatten(),
        );

        // Per-event sub-toggles (#463). Default true — see
        // `parse_sound_cue_sub_setting` for the reasoning.
        let sound_cue_start_enabled = parse_sound_cue_sub_setting(
            settings
                .get(crate::settings::keys::SOUND_CUE_START_ENABLED)
                .await
                .ok()
                .flatten(),
        );
        let sound_cue_complete_enabled = parse_sound_cue_sub_setting(
            settings
                .get(crate::settings::keys::SOUND_CUE_COMPLETE_ENABLED)
                .await
                .ok()
                .flatten(),
        );

        // Meeting auto-start mode. Off by default; absent or
        // garbage rows fall through to Off (the safer default —
        // a corrupted row should not silently make the mic
        // spontaneously turn on).
        let meeting_autostart_mode = crate::meeting::MeetingAutostartMode::from_setting(
            settings
                .get(crate::settings::keys::MEETING_AUTOSTART_MODE)
                .await
                .ok()
                .flatten()
                .as_deref(),
        );

        AppStateBuilder::new()
            .audio(audio)
            .transcribe_arc(transcribe_shared)
            .transcribe_meeting_arc(transcribe_meeting_shared)
            .diarize(diarize)
            .history(history)
            .replacements(replacements)
            .vocabulary(vocabulary)
            .settings(settings)
            .meetings(meetings)
            .meeting_app_overrides(meeting_app_overrides)
            .meeting_manager(meeting_manager)
            .models_dir(models_dir)
            .ptt_combo(ptt_combo)
            .ptt_active(ptt_active)
            .hud_enabled(hud_enabled)
            .sound_cues_enabled(sound_cues_enabled)
            .sound_cue_start_enabled(sound_cue_start_enabled)
            .sound_cue_complete_enabled(sound_cue_complete_enabled)
            .meeting_autostart_mode(meeting_autostart_mode)
            .diarization_enabled_arc(diarization_enabled_arc)
            .diarize_slot(diarize_slot)
            .inference_threads_arc(inference_threads_arc)
            .mic_gain_db_arc(mic_gain_db_arc)
            .build()
    }
}

impl AppState {
    /// Hot-swap the loaded transcriber across both the dictation and
    /// meeting slots (#248).
    ///
    /// Called from `model_select` after the user picks a model that
    /// has a downloaded file on disk. The lock is acquired only after
    /// the (potentially-slow) GGUF load completes on a blocking task,
    /// so the dictation hot path is never blocked on disk I/O.
    ///
    /// Both slots receive instances constructed from the same GGUF
    /// path so the user-visible model stays consistent — the split
    /// is purely about avoiding `Mutex<WhisperContext>` contention
    /// between the two inference paths, not about running different
    /// models per path.
    ///
    /// Returns the previous dictation-slot value so the caller can
    /// diagnose "did we actually swap something?" if it cares; the
    /// previous meeting-slot value is dropped on the floor.
    pub fn swap_transcriber(
        &self,
        new_dictation: Option<Arc<dyn Transcribe>>,
        new_meeting: Option<Arc<dyn Transcribe>>,
    ) -> Result<Option<Arc<dyn Transcribe>>> {
        let mut dictation_guard = self
            .transcribe
            .lock()
            .map_err(|_| anyhow::anyhow!("transcribe mutex poisoned"))?;
        let prev = std::mem::replace(&mut *dictation_guard, new_dictation);
        drop(dictation_guard);
        let mut meeting_guard = self
            .transcribe_meeting
            .lock()
            .map_err(|_| anyhow::anyhow!("transcribe_meeting mutex poisoned"))?;
        *meeting_guard = new_meeting;
        Ok(prev)
    }
}

/// Try to load the GGUF for a single catalog model id. Returns `None`
/// if the model isn't in the catalog, the file isn't on disk, or the
/// `whisper` Cargo feature is off. Returns an error if the file is on
/// disk but `WhisperTranscription::new` fails — the caller decides
/// whether to surface that to the user or silently fall through.
///
/// Pulled out as its own function so `model_select` can hot-load a
/// specific model without going through the full startup-time
/// fallback chain in [`build_transcriber`] (which also tries the
/// legacy `HUSH_MODEL_PATH` env var, irrelevant once the user is
/// driving the picker).
#[cfg_attr(not(feature = "whisper"), allow(unused_variables))]
pub fn load_transcriber_for_model(
    model_id: &str,
    models_dir: &Path,
    inference_threads: &Arc<std::sync::atomic::AtomicI32>,
    mic_gain_db: &Arc<std::sync::atomic::AtomicU32>,
) -> Result<Option<Arc<dyn Transcribe>>> {
    #[cfg(feature = "whisper")]
    {
        use crate::transcription::catalog;

        let Some(meta) = catalog::find_by_id(model_id) else {
            return Ok(None);
        };
        let path = models_dir.join(&meta.filename);
        if !path.exists() {
            return Ok(None);
        }
        let transcriber = crate::transcription::WhisperTranscription::new(&path)
            .with_context(|| format!("load whisper model {} from {}", meta.id, path.display()))?
            .with_inference_threads(Arc::clone(inference_threads))
            .with_mic_gain_db(Arc::clone(mic_gain_db));
        tracing::info!(
            model_id = %meta.id,
            path = %path.display(),
            "hot-loaded whisper model"
        );
        Ok(Some(Arc::new(transcriber) as Arc<dyn Transcribe>))
    }

    #[cfg(not(feature = "whisper"))]
    {
        let _ = inference_threads;
        let _ = mic_gain_db;
        Ok(None)
    }
}

/// Resolve the active transcriber backend. Pulled out so a test or a
/// future "reload model" command can call it without rebuilding the
/// rest of `AppState`.
#[cfg_attr(not(feature = "whisper"), allow(unused_variables))]
async fn build_transcriber(
    settings: &Arc<dyn SettingsRepository>,
    models_dir: &Path,
    inference_threads: &Arc<std::sync::atomic::AtomicI32>,
    mic_gain_db: &Arc<std::sync::atomic::AtomicU32>,
) -> Option<Arc<dyn Transcribe>> {
    #[cfg(feature = "whisper")]
    {
        use crate::settings::keys;
        use crate::transcription::catalog;

        // 1) Settings-driven path: model id → catalog → models_dir.
        if let Ok(Some(id)) = settings.get(keys::SELECTED_MODEL_ID).await {
            if let Some(meta) = catalog::find_by_id(&id) {
                let path = models_dir.join(&meta.filename);
                if path.exists() {
                    match crate::transcription::WhisperTranscription::new(&path) {
                        Ok(t) => {
                            tracing::info!(
                                model_id = %meta.id,
                                path = %path.display(),
                                "loaded selected whisper model"
                            );
                            return Some(Arc::new(
                                t.with_inference_threads(Arc::clone(inference_threads))
                                    .with_mic_gain_db(Arc::clone(mic_gain_db)),
                            ) as Arc<dyn Transcribe>);
                        }
                        Err(e) => {
                            tracing::error!(
                                error = ?e,
                                path = %path.display(),
                                "selected model failed to load; falling back"
                            );
                        }
                    }
                } else {
                    tracing::warn!(
                        model_id = %id,
                        path = %path.display(),
                        "selected model file is missing; falling back"
                    );
                }
            } else {
                tracing::warn!(
                    model_id = %id,
                    "selected model id is not in the catalog; falling back"
                );
            }
        }

        // 2) Legacy dev path. Removed once the picker is mature enough
        //    that we can ask users to migrate.
        if let Ok(path) = std::env::var("HUSH_MODEL_PATH") {
            let path = std::path::PathBuf::from(path);
            match crate::transcription::WhisperTranscription::new(&path) {
                Ok(t) => {
                    tracing::info!(path = %path.display(), "loaded HUSH_MODEL_PATH whisper model");
                    return Some(
                        Arc::new(
                            t.with_inference_threads(Arc::clone(inference_threads))
                                .with_mic_gain_db(Arc::clone(mic_gain_db)),
                        ) as Arc<dyn Transcribe>,
                    );
                }
                Err(e) => {
                    tracing::error!(
                        error = ?e,
                        path = %path.display(),
                        "HUSH_MODEL_PATH failed to load"
                    );
                }
            }
        }

        None
    }

    #[cfg(not(feature = "whisper"))]
    {
        // Without the `whisper` feature there is no production
        // transcriber. The IPC layer surfaces `TranscriptionUnavailable`.
        let _ = mic_gain_db;
        None
    }
}

/// Pure orchestration function — exposed so unit tests can exercise the
/// audio→transcription path with mocked implementations of both traits,
/// without needing a Tauri runtime, an audio device, or a real Whisper
/// model. The Tauri command wrapper handles the OS side effects on top.
pub fn run_pipeline(
    audio: &dyn AudioCapture,
    transcribe: &dyn Transcribe,
) -> anyhow::Result<String> {
    let captured = audio.stop()?;
    let raw = transcribe.transcribe(&captured)?;
    Ok(raw.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioDevice, CaptureFormat, CapturedAudio};
    use anyhow::anyhow;

    /// Mock that returns a fixed [`CapturedAudio`] from `stop`. `start` and
    /// `is_recording` keep just enough state for tests to assert on.
    struct MockAudio {
        captured: CapturedAudio,
        recording: std::sync::atomic::AtomicBool,
    }

    impl MockAudio {
        fn new(captured: CapturedAudio) -> Self {
            Self {
                captured,
                recording: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    impl AudioCapture for MockAudio {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![AudioDevice {
                id: "mock".into(),
                name: "Mock Mic".into(),
                is_default: true,
            }])
        }
        fn start(&self, _device_id: Option<&str>) -> anyhow::Result<()> {
            self.recording
                .store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            self.recording
                .store(false, std::sync::atomic::Ordering::Release);
            Ok(self.captured.clone())
        }
        fn is_recording(&self) -> bool {
            self.recording.load(std::sync::atomic::Ordering::Acquire)
        }
    }

    struct EchoTranscribe {
        text: String,
    }

    impl Transcribe for EchoTranscribe {
        fn transcribe(&self, _audio: &CapturedAudio) -> anyhow::Result<String> {
            Ok(self.text.clone())
        }
    }

    struct FailingTranscribe;

    impl Transcribe for FailingTranscribe {
        fn transcribe(&self, _audio: &CapturedAudio) -> anyhow::Result<String> {
            Err(anyhow!("model exploded"))
        }
    }

    fn fake_audio() -> CapturedAudio {
        CapturedAudio {
            samples: vec![0.0_f32; 4],
            format: CaptureFormat {
                sample_rate: 48_000,
                channels: 1,
            },
        }
    }

    #[test]
    fn run_pipeline_trims_whitespace_from_model_output() {
        let audio = MockAudio::new(fake_audio());
        let transcribe = EchoTranscribe {
            text: "  hello world\n".into(),
        };
        let text = run_pipeline(&audio, &transcribe).unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn run_pipeline_propagates_audio_stop_failure() {
        struct BrokenAudio;
        impl AudioCapture for BrokenAudio {
            fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
                Ok(vec![])
            }
            fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
                Ok(())
            }
            fn stop(&self) -> anyhow::Result<CapturedAudio> {
                Err(anyhow!("device went away"))
            }
            fn is_recording(&self) -> bool {
                false
            }
        }

        let err = run_pipeline(&BrokenAudio, &EchoTranscribe { text: "x".into() })
            .unwrap_err()
            .to_string();
        assert!(err.contains("device went away"), "got: {err}");
    }

    #[test]
    fn run_pipeline_propagates_transcription_failure() {
        let audio = MockAudio::new(fake_audio());
        let err = run_pipeline(&audio, &FailingTranscribe)
            .unwrap_err()
            .to_string();
        assert!(err.contains("model exploded"), "got: {err}");
    }

    /// Tiny mock for unit tests that need an `Arc<dyn HistoryRepository>`
    /// in `AppState` but don't exercise its methods. Pinning the count to
    /// `0` keeps surface minimal — tests that need real behaviour against
    /// the SQLite-backed impl call the repository directly.
    pub(crate) struct NoopHistory;

    #[async_trait::async_trait]
    impl HistoryRepository for NoopHistory {
        async fn create(&self, _: crate::history::NewHistoryEntry) -> anyhow::Result<i64> {
            Ok(0)
        }
        async fn list(&self, _: i64, _: i64) -> anyhow::Result<Vec<crate::history::HistoryEntry>> {
            Ok(vec![])
        }
        async fn search(
            &self,
            _: &str,
            _: i64,
            _: i64,
        ) -> anyhow::Result<Vec<crate::history::HistoryEntry>> {
            Ok(vec![])
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn clear(&self) -> anyhow::Result<i64> {
            Ok(0)
        }
        async fn count(&self) -> anyhow::Result<i64> {
            Ok(0)
        }
        async fn get_stats(&self) -> anyhow::Result<crate::history::DictationStats> {
            Ok(crate::history::DictationStats::default())
        }
    }

    /// Tiny mock that returns an empty rules list so the dictation
    /// pipeline behaves as if no replacements are configured. Tests that
    /// need actual replacement behaviour use the SQLite-backed repo
    /// directly rather than mocking the trait.
    pub(crate) struct NoopReplacements;

    #[async_trait::async_trait]
    impl
        crate::repository::Repository<
            crate::dictionary::ReplacementRule,
            crate::dictionary::NewReplacementRule,
            i64,
        > for NoopReplacements
    {
        async fn list(&self) -> anyhow::Result<Vec<crate::dictionary::ReplacementRule>> {
            Ok(vec![])
        }
        async fn create(
            &self,
            _: crate::dictionary::NewReplacementRule,
        ) -> anyhow::Result<crate::dictionary::ReplacementRule> {
            unreachable!("mock does not exercise create")
        }
        async fn update(&self, _: crate::dictionary::ReplacementRule) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Same shape as [`NoopReplacements`] / [`NoopHistory`] — empty
    /// list so the dictation pipeline behaves as if no vocab terms are
    /// configured. Tests that need real behaviour against the SQLite-
    /// backed repo call it directly.
    pub(crate) struct NoopVocabulary;

    #[async_trait::async_trait]
    impl
        crate::repository::Repository<
            crate::dictionary::VocabularyTerm,
            crate::dictionary::NewVocabularyTerm,
            i64,
        > for NoopVocabulary
    {
        async fn list(&self) -> anyhow::Result<Vec<crate::dictionary::VocabularyTerm>> {
            Ok(vec![])
        }
        async fn create(
            &self,
            _: crate::dictionary::NewVocabularyTerm,
        ) -> anyhow::Result<crate::dictionary::VocabularyTerm> {
            unreachable!("mock does not exercise create")
        }
        async fn update(&self, _: crate::dictionary::VocabularyTerm) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Noop meeting-session repository — returns empty lists, eats
    /// inserts. Tests that exercise the IPC layer don't need real
    /// persistence here today; the streaming pump that actually
    /// writes to it lands in #110.
    pub(crate) struct NoopMeetings;

    #[async_trait::async_trait]
    impl
        crate::repository::Repository<
            crate::meeting::MeetingSession,
            crate::meeting::NewMeetingSession,
            i64,
        > for NoopMeetings
    {
        async fn list(&self) -> anyhow::Result<Vec<crate::meeting::MeetingSession>> {
            Ok(vec![])
        }
        async fn create(
            &self,
            _: crate::meeting::NewMeetingSession,
        ) -> anyhow::Result<crate::meeting::MeetingSession> {
            unreachable!("mock does not exercise create")
        }
        async fn update(&self, _: crate::meeting::MeetingSession) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl crate::meeting::MeetingSessionRepository for NoopMeetings {
        async fn close_session(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn append_utterance(
            &self,
            _: crate::meeting::NewPersistedUtterance,
        ) -> anyhow::Result<crate::meeting::PersistedUtterance> {
            unreachable!("mock does not exercise append_utterance")
        }
        async fn list_utterances(
            &self,
            _: i64,
        ) -> anyhow::Result<Vec<crate::meeting::PersistedUtterance>> {
            Ok(vec![])
        }
        async fn set_notes(&self, _: i64, _: Option<String>) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_by_id(
            &self,
            _: i64,
        ) -> anyhow::Result<Option<crate::meeting::MeetingSession>> {
            Ok(None)
        }
        async fn list_open_sessions(&self) -> anyhow::Result<Vec<crate::meeting::MeetingSession>> {
            Ok(vec![])
        }
        async fn search_sessions(
            &self,
            _: &str,
        ) -> anyhow::Result<Vec<crate::meeting::MeetingSession>> {
            Ok(vec![])
        }
    }

    /// Test mock for the meeting-app overrides repo (#112). Returns
    /// an empty list so the classifier falls through to the static
    /// defaults — same behaviour the pre-#112 IPC layer exhibited.
    pub(crate) struct NoopMeetingAppOverrides;

    #[async_trait::async_trait]
    impl crate::meeting::MeetingAppOverrideRepository for NoopMeetingAppOverrides {
        async fn list(&self) -> anyhow::Result<Vec<crate::meeting::MeetingAppOverride>> {
            Ok(vec![])
        }
        async fn upsert(
            &self,
            _: crate::meeting::NewMeetingAppOverride,
        ) -> anyhow::Result<crate::meeting::MeetingAppOverride> {
            unreachable!("mock does not exercise upsert")
        }
        async fn set_profile(
            &self,
            _: &str,
            _: Option<&str>,
            _: Option<&str>,
        ) -> anyhow::Result<crate::meeting::MeetingAppOverride> {
            unreachable!("mock does not exercise set_profile")
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// In-memory settings store backed by a HashMap. Lighter than
    /// spinning up a SQLite for tests that just need to round-trip a
    /// few keys.
    pub(crate) struct MemSettings {
        pub map: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    #[async_trait::async_trait]
    impl SettingsRepository for MemSettings {
        async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
            Ok(self.map.lock().unwrap().get(key).cloned())
        }
        async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
            self.map
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_owned());
            Ok(())
        }
        async fn remove(&self, key: &str) -> anyhow::Result<()> {
            self.map.lock().unwrap().remove(key);
            Ok(())
        }
    }

    pub(crate) fn mock_state() -> AppState {
        AppStateBuilder::new()
            .audio(Arc::new(MockAudio::new(fake_audio())))
            .history(Arc::new(NoopHistory))
            .replacements(Arc::new(NoopReplacements))
            .vocabulary(Arc::new(NoopVocabulary))
            .settings(Arc::new(MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .meetings({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> = Arc::new(NoopMeetings);
                m
            })
            .meeting_app_overrides({
                let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                    Arc::new(NoopMeetingAppOverrides);
                o
            })
            .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
                let m: Arc<dyn crate::meeting::MeetingSessionRepository> = Arc::new(NoopMeetings);
                m
            })))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("mock_state: builder fields complete")
    }

    #[test]
    fn appstate_can_be_constructed_with_no_transcriber() {
        // Mirrors the runtime path where `--features whisper` is off or
        // `HUSH_MODEL_PATH` is unset: the app boots, device enumeration
        // works, and the IPC layer surfaces `TranscriptionUnavailable` on
        // stop. We just check construction here; the unavailable behaviour
        // is exercised by the `commands` module's runtime path.
        let state = mock_state();
        assert!(state.transcribe.lock().unwrap().is_none());
    }

    #[test]
    fn swap_transcriber_replaces_the_inner_arc_and_returns_previous() {
        // Round-7 architecture reviewer flagged that `swap_transcriber`
        // (called from `model_select` when the user picks a new model
        // with a downloaded file) had no test coverage. Pin the
        // observable contract: the new value lands inside the Mutex,
        // and the previous value is returned so the caller can drop
        // it explicitly if it cares to. A future change that
        // accidentally swaps in an async lock or a different replacement
        // strategy would fail this.

        struct StubTranscriber {
            label: &'static str,
        }
        impl crate::transcription::Transcribe for StubTranscriber {
            fn transcribe(&self, _: &crate::audio::CapturedAudio) -> anyhow::Result<String> {
                Ok(String::new())
            }
            fn model_label(&self) -> String {
                self.label.to_owned()
            }
        }

        let state = mock_state();
        // mock_state() leaves both slots = None (no model loaded).
        assert!(state.transcribe.lock().unwrap().is_none());
        assert!(state.transcribe_meeting.lock().unwrap().is_none());

        let first_d: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "first" });
        let first_m: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "first" });
        let prev = state
            .swap_transcriber(Some(first_d), Some(first_m))
            .expect("first swap succeeds");
        assert!(prev.is_none(), "previous was None (mock_state baseline)");

        // Now confirm the swap actually landed in both slots.
        {
            let guard = state.transcribe.lock().unwrap();
            assert_eq!(
                guard.as_ref().map(|t| t.model_label()),
                Some("first".to_owned()),
                "new transcriber readable from the dictation slot"
            );
        }
        {
            let guard = state.transcribe_meeting.lock().unwrap();
            assert_eq!(
                guard.as_ref().map(|t| t.model_label()),
                Some("first".to_owned()),
                "new transcriber readable from the meeting slot"
            );
        }

        // Second swap returns the first one as the "previous" value
        // (from the dictation slot — the meeting-slot prev is dropped
        // on the floor as documented).
        let second_d: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "second" });
        let second_m: Arc<dyn Transcribe> = Arc::new(StubTranscriber { label: "second" });
        let prev = state
            .swap_transcriber(Some(second_d), Some(second_m))
            .expect("second swap succeeds");
        assert_eq!(
            prev.map(|t| t.model_label()),
            Some("first".to_owned()),
            "previous dictation value returned to caller"
        );
        assert_eq!(
            state
                .transcribe
                .lock()
                .unwrap()
                .as_ref()
                .map(|t| t.model_label()),
            Some("second".to_owned()),
            "second dictation transcriber landed"
        );
        assert_eq!(
            state
                .transcribe_meeting
                .lock()
                .unwrap()
                .as_ref()
                .map(|t| t.model_label()),
            Some("second".to_owned()),
            "second meeting transcriber landed"
        );

        // Swap to None to confirm the unload path works for both slots.
        let prev = state
            .swap_transcriber(None, None)
            .expect("clear swap succeeds");
        assert_eq!(prev.map(|t| t.model_label()), Some("second".to_owned()));
        assert!(state.transcribe.lock().unwrap().is_none());
        assert!(state.transcribe_meeting.lock().unwrap().is_none());
    }

    #[test]
    fn appstate_builder_errors_descriptively_on_missing_required_field() {
        // Round-7 architecture reviewer flagged that the builder's
        // self-documenting error messages had no test coverage. A future
        // refactor that "fixed" the error message wording (or stopped
        // ok_or_else'ing entirely) would silently regress the developer
        // experience of "I forgot a field — the error tells me which one."
        // Spot-check one required field. The message format ("audio not
        // set") is part of the developer-facing contract — pin it.
        let result = AppStateBuilder::new().build();
        // AppState doesn't implement Debug, so we can't use
        // `expect_err`; match on the Result instead.
        let err = match result {
            Ok(_) => panic!("empty builder must error, got Ok"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("audio not set"),
            "error must name the first missing required field; got: {msg}"
        );
    }

    #[test]
    fn huggingface_host_predicate_accepts_apex_and_subdomains() {
        // Pin the load-bearing security check: the download redirect
        // policy treats these as in-zone. Both `huggingface.co` and
        // `hf.co` are HF-owned; the Xet CDN that HF migrated large-
        // file serving to in 2025 lives on the `hf.co` zone (e.g.
        // `cas-bridge.xethub.hf.co`), not `huggingface.co`.
        assert!(is_huggingface_host(Some("huggingface.co")));
        assert!(is_huggingface_host(Some("cdn-lfs.huggingface.co")));
        assert!(is_huggingface_host(Some("cdn-lfs-us-1.huggingface.co")));
        assert!(is_huggingface_host(Some("hf.co")));
        assert!(is_huggingface_host(Some("xethub.hf.co")));
        assert!(is_huggingface_host(Some("cas-bridge.xethub.hf.co")));
    }

    #[test]
    fn huggingface_host_predicate_rejects_typosquats_and_lookalikes() {
        // Regression for "ends_with" naivety: `evilhuggingface.co`
        // (no leading dot) is not in zone but a sloppy `ends_with`
        // without the dot would accept it. The predicate must also
        // reject obvious off-domain hosts.
        assert!(!is_huggingface_host(Some("evilhuggingface.co")));
        assert!(!is_huggingface_host(Some("huggingface.co.attacker.com")));
        assert!(!is_huggingface_host(Some("attacker.com")));
        // hf.co-zone equivalents of the same trap.
        assert!(!is_huggingface_host(Some("myhf.co")));
        assert!(!is_huggingface_host(Some("hf.co.attacker.com")));
        assert!(!is_huggingface_host(Some("")));
        assert!(!is_huggingface_host(None));
    }

    /// Helper: build a `reqwest::Url` for the redirect tests below.
    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).expect("test URL parses")
    }

    #[test]
    fn redirect_decision_allows_hop_within_hf_zone() {
        // Common case: huggingface.co → cas-bridge.xethub.hf.co.
        let prev = vec![url("https://huggingface.co/foo")];
        let dest = url("https://cas-bridge.xethub.hf.co/bar");
        assert_eq!(redirect_decision(&prev, &dest), RedirectDecision::Follow);
    }

    #[test]
    fn redirect_decision_allows_hf_to_signed_cdn() {
        // The whole reason this PR exists (#258): HF redirects to
        // a signed AWS / Cloudflare URL outside the HF zone.
        let prev = vec![
            url("https://huggingface.co/foo"),
            url("https://cas-bridge.xethub.hf.co/bar"),
        ];
        let dest = url("https://hf-cdn.s3.amazonaws.com/weights.gguf?X-Amz-Signature=abc123");
        assert_eq!(redirect_decision(&prev, &dest), RedirectDecision::Follow);
    }

    #[test]
    fn redirect_decision_allows_first_hop_hf_to_signed_cdn() {
        // Single-hop variant: HF immediately redirects to the
        // signed URL with no in-zone intermediary.
        let prev = vec![url("https://huggingface.co/resolve/main/foo.gguf")];
        let dest = url("https://r2-signed.cloudflarestorage.com/x?sig=abc");
        assert_eq!(redirect_decision(&prev, &dest), RedirectDecision::Follow);
    }

    #[test]
    fn redirect_decision_blocks_chain_extension_from_signed_url() {
        // After we've hopped to a signed CDN URL, that URL's host
        // is no longer trusted to redirect us further. If the CDN
        // tries to send us to attacker.com, deny.
        let prev = vec![
            url("https://huggingface.co/foo"),
            url("https://hf-cdn.s3.amazonaws.com/x?sig=abc"),
        ];
        let dest = url("https://attacker.com/evil.gguf");
        match redirect_decision(&prev, &dest) {
            RedirectDecision::Stop(reason) => {
                assert!(
                    reason.contains("non-HF host"),
                    "non-HF → non-HF should be blocked, got: {reason}"
                );
            }
            d => panic!("expected Stop, got {d:?}"),
        }
    }

    #[test]
    fn redirect_decision_blocks_http_downgrade() {
        // Defence-in-depth: an HF host telling us to downgrade
        // to plain http:// is rejected, not followed. We don't
        // trust HF (or anyone) to send us cleartext.
        let prev = vec![url("https://huggingface.co/foo")];
        let dest = url("http://huggingface.co/foo"); // http not https
        match redirect_decision(&prev, &dest) {
            RedirectDecision::Stop(reason) => {
                assert!(reason.contains("non-HTTPS"), "got: {reason}");
            }
            d => panic!("expected Stop for http://, got {d:?}"),
        }
    }

    #[test]
    fn redirect_decision_caps_at_max_redirects() {
        // The hop-count cap fires before host checks so a chain
        // that's legitimate at every hop still terminates.
        let prev: Vec<reqwest::Url> = (0..MAX_DOWNLOAD_REDIRECTS)
            .map(|i| url(&format!("https://huggingface.co/hop-{i}")))
            .collect();
        let dest = url("https://huggingface.co/final");
        match redirect_decision(&prev, &dest) {
            RedirectDecision::Stop(reason) => {
                assert!(reason.contains("too many"), "got: {reason}");
            }
            d => panic!("expected Stop for over-cap, got {d:?}"),
        }
    }

    #[test]
    fn redirect_decision_blocks_non_hf_origin() {
        // Unlikely path (the request started at HF and reqwest
        // wouldn't manufacture a fresh non-HF origin), but pin it
        // anyway: zero-length previous + non-HF destination is a
        // straight reject.
        let prev: Vec<reqwest::Url> = vec![];
        let dest = url("https://attacker.com/evil.gguf");
        match redirect_decision(&prev, &dest) {
            RedirectDecision::Stop(_) => {}
            d => panic!("expected Stop for empty-prev + non-HF, got {d:?}"),
        }
    }

    #[test]
    fn parse_hud_enabled_setting_handles_all_branches() {
        // Absent row → on. First-time users must see the HUD even
        // before they have ever touched the toggle.
        assert!(parse_hud_enabled_setting(None));
        // Literal "false" → off. The only string that turns it off.
        assert!(!parse_hud_enabled_setting(Some("false".into())));
        // Literal "true" → on.
        assert!(parse_hud_enabled_setting(Some("true".into())));
        // Anything else falls through to on. Defends against a
        // settings-table corruption that scribbled garbage into
        // the row — silently turning the HUD off for that user
        // would be worse than silently re-enabling.
        assert!(parse_hud_enabled_setting(Some("garbage".into())));
        assert!(parse_hud_enabled_setting(Some("".into())));
        assert!(parse_hud_enabled_setting(Some("True".into())));
        assert!(parse_hud_enabled_setting(Some("FALSE".into())));
    }

    #[test]
    fn parse_diarization_enabled_setting_handles_all_branches() {
        // Absent row → on (#478 default flip). The wespeaker model
        // is bundled into the first-run download flow, so by the
        // time this is read on a fresh install the model is on
        // disk; existing users with an explicit `"false"` row
        // keep their preference (the round-trip below pins that).
        assert!(parse_diarization_enabled_setting(None));
        // Literal "true" → on.
        assert!(parse_diarization_enabled_setting(Some("true".into())));
        // Literal "false" → off. Critical: an existing user who
        // explicitly toggled diarization OFF before #478 has
        // exactly this row, and the upgrade must respect it.
        assert!(!parse_diarization_enabled_setting(Some("false".into())));
        // Anything else falls through to the absent-row default
        // (now `true`) — same fallthrough policy other settings
        // (`hud_enabled`) use for corrupted rows.
        assert!(parse_diarization_enabled_setting(Some("garbage".into())));
        assert!(parse_diarization_enabled_setting(Some("".into())));
        assert!(parse_diarization_enabled_setting(Some("True".into())));
        assert!(parse_diarization_enabled_setting(Some("1".into())));
    }

    #[test]
    fn autostart_mode_round_trips_through_atomic_encoding() {
        use crate::meeting::MeetingAutostartMode;
        // Every defined variant must encode + decode back to itself.
        for mode in [MeetingAutostartMode::Off, MeetingAutostartMode::Always] {
            let byte = encode_autostart_mode(mode);
            assert_eq!(decode_autostart_mode(byte), mode, "round-trip for {mode:?}");
        }
    }

    #[test]
    fn autostart_mode_decode_falls_back_to_off_for_unknown_bytes() {
        use crate::meeting::MeetingAutostartMode;
        // A future variant added to the enum but not yet known to a
        // stale build (or a corrupted atomic from some unforeseen
        // path) must read as `Off` — the safer default. Nobody wants
        // their mic to spontaneously turn on because of a byte the
        // decoder didn't recognise.
        for byte in [2u8, 3, 7, 42, 99, 255] {
            assert_eq!(
                decode_autostart_mode(byte),
                MeetingAutostartMode::Off,
                "unknown byte {byte} should decode to Off"
            );
        }
    }
}
