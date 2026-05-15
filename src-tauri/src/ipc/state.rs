//! Long-lived application state — `AppState`, `AppStateBuilder`'s
//! collaborators, and the production startup helpers.
//!
//! Extracted from `ipc/mod.rs` under #597 (item 6). No behaviour change.
//!
//! - Types: [`ForegroundApp`], [`DataServices`], [`AppState`],
//!   [`RuntimeFlags`].
//! - Production constructor: `AppState::build_default` (delegates to
//!   [`super::builder::AppStateBuilder`] for the explicit-field shape).
//! - Hot-swap: `AppState::swap_transcriber` for the model-picker path.
//! - Internal helpers: [`encode_autostart_mode`] /
//!   [`decode_autostart_mode`] (autostart-mode wire encoding),
//!   `parse_*_setting` (per-setting decoders + per-setting defaults),
//!   `build_diarizer_inner` (production diarizer construction).
//!
//! The explicit-builder pattern lives in [`super::builder`]; the
//! redirect-policy + transcriber-loading + pure pipeline runner lives
//! in [`super::pipeline`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use tauri::Manager;

use crate::audio::{AudioCapture, CpalAudioCapture};
use crate::db::SqliteDatabase;
use crate::dictionary::{
    ReplacementRepository, SqliteReplacementRepository, SqliteVocabularyRepository,
    VocabularyRepository,
};
use crate::history::{HistoryRepository, SqliteHistoryRepository};
use crate::settings::{SettingsRepository, SqliteSettingsRepository};
use crate::transcription::download::CancelHandle;
use crate::transcription::Transcribe;

use super::builder::AppStateBuilder;
use super::pipeline::build_transcriber;

/// Hot-swappable transcriber slot, shared by reference between
/// [`AppState`] and [`crate::meeting::SessionManager`]. The inner
/// `Option` is `None` until a model is loaded; the outer `Arc` lets
/// the meeting pump observe model hot-swaps via the same mutex
/// `model_select` writes through. Aliased for type-complexity
/// hygiene (clippy) and so the type is easy to grep for at every
/// hand-off point.
pub type TranscribeSlot = Arc<Mutex<Option<Arc<dyn Transcribe>>>>;

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
    /// Serialises concurrent callers to `check_for_updates_inner` so only one
    /// network probe is in flight at a time (#876). A caller that arrives while
    /// another is probing will wait here, then re-check `last_update_check` and
    /// return the cached result without issuing a duplicate request.
    pub update_check_inflight: Arc<tokio::sync::Mutex<()>>,
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
    /// In-app debug log console (#532). Holds the ring buffer of
    /// the last 200 backend `tracing` events. The tracing layer
    /// is registered before the Tauri builder in `run()` and
    /// forwarded here. The `get_log_entries` IPC command drains
    /// a snapshot so the frontend can catch up to the live stream.
    pub debug_log: crate::debug_log::DebugLogState,
    /// User-facing flags that mirror Settings rows — see
    /// [`RuntimeFlags`] for the full per-field rationale. Grouped
    /// into a substruct (#431) so `AppState` stays readable.
    pub runtime_flags: RuntimeFlags,
    /// Per-phase wall-clock timings captured during
    /// [`AppState::build_default`] (#584 Angle 1). Populated at
    /// boot, never mutated thereafter — `Vec` rather than
    /// `Mutex<Vec>` because the IPC reader (`get_startup_timings`)
    /// only ever sees a finished list. Surfaced to the Debug tab
    /// so contributors can spot startup-time regressions without
    /// reaching for Instruments.
    pub startup_timings: Vec<crate::ipc::commands::system::StartupPhase>,
    /// Error string from the Ctrl+⌥+H toggle-hotkey registration attempt
    /// (#904). `None` means registration succeeded; `Some(msg)` means it
    /// failed and the user needs to take action (e.g. grant Input
    /// Monitoring, dismiss a conflicting app). Written once at boot by
    /// `register_hotkeys`; the IPC `get_toggle_hotkey_status` reads it.
    pub hotkey_toggle_error: Mutex<Option<String>>,
}

/// User-facing runtime flags that mirror Settings rows (#431).
///
/// Each entry is an `Arc<Atomic*>` so the read-side hot paths
/// (dictation start, meeting pump tick, HAL detection task, etc.)
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
    /// Read by the CoreAudio HAL detection task on every device-state
    /// event; flipped by the `set_meeting_autostart_mode` IPC.
    /// Encoded as an `AtomicU8` (`0 = Off`, `1 = Always`) for lock-free reads.
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
        // Reject NaN/±Inf: f32::clamp on a NaN receiver returns NaN
        // (neither bound fires), poisoning every captured sample (#841).
        .filter(|v| v.is_finite())
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
    /// Helper used by `build_default` to read a settings key at startup.
    /// Returns `None` on a DB error and logs a warning so the failure is
    /// visible in logs without aborting startup (#842).
    async fn startup_setting(
        settings: &dyn crate::settings::SettingsRepository,
        key: &'static str,
    ) -> Option<String> {
        settings.get(key).await.unwrap_or_else(|e| {
            tracing::warn!(
                key,
                error = %e,
                "startup: settings read failed; using default"
            );
            None
        })
    }

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
        debug_log: crate::debug_log::DebugLogState,
    ) -> Result<Self> {
        let t_start = std::time::Instant::now();
        // Per-phase timing trace (#584 Angle 1). Populated below at
        // each existing `tracing::info!` checkpoint so the IPC
        // surface and the log lines share one source of truth. The
        // closure captures `t_start` so the `elapsed_ms` field
        // matches what gets logged at the same point.
        let mut startup_timings: Vec<crate::ipc::commands::system::StartupPhase> = Vec::new();
        let mut record_phase = |name: &str| {
            startup_timings.push(crate::ipc::commands::system::StartupPhase {
                name: name.to_owned(),
                elapsed_ms: t_start.elapsed().as_millis() as u64,
            });
        };
        tracing::info!("app state: build_default started");
        #[cfg(target_os = "macos")]
        let resource_dir = app
            .path()
            .resource_dir()
            .context("resolve Tauri resource directory")?;
        let audio: Arc<dyn AudioCapture> = Arc::new(CpalAudioCapture::new(
            #[cfg(target_os = "macos")]
            resource_dir,
        ));

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
        record_phase("database and repositories");
        tracing::info!(
            elapsed_ms = t_start.elapsed().as_millis(),
            "app state: database and repositories ready"
        );
        // Inference thread count (#255). Read the persisted value
        // from settings, clamp to the supported range, build the
        // shared Arc that AppState + WhisperTranscription + the IPC
        // writer all read/write through. Constructed *before*
        // `build_transcriber` so the loaded model points at this
        // exact Arc, not a fresh one.
        let inference_threads_initial = parse_inference_threads_setting(
            Self::startup_setting(settings.as_ref(), crate::settings::keys::INFERENCE_THREADS)
                .await,
        );
        let inference_threads_arc =
            Arc::new(std::sync::atomic::AtomicI32::new(inference_threads_initial));

        // Mic gain (#531). Same Arc-sharing pattern as inference_threads:
        // built before `build_transcriber` so the loaded models and the
        // meeting pump all point at this exact Arc. Default 0.0 dB (unity).
        let mic_gain_db_initial = parse_mic_gain_db_setting(
            Self::startup_setting(settings.as_ref(), crate::settings::keys::MIC_GAIN_DB).await,
        );
        let mic_gain_db_arc = Arc::new(std::sync::atomic::AtomicU32::new(
            mic_gain_db_initial.to_bits(),
        ));

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
        let transcribe_dictation;
        let transcribe_meeting;
        // Load both whisper contexts in parallel (#561). The two loads are
        // independent and each blocks while mmapping the model file;
        // running them concurrently halves the sequential cost. Each
        // `build_transcriber` call wraps `WhisperTranscription::new` in
        // `spawn_blocking`, so both blocking loads start before either one
        // completes and run on separate tokio blocking threads.
        (transcribe_dictation, transcribe_meeting) = tokio::join!(
            build_transcriber(
                &settings,
                &models_dir,
                &inference_threads_arc,
                &mic_gain_db_arc,
            ),
            build_transcriber(
                &settings,
                &models_dir,
                &inference_threads_arc,
                &mic_gain_db_arc,
            ),
        );

        record_phase("whisper contexts (parallel load)");
        tracing::info!(
            elapsed_ms = t_start.elapsed().as_millis(),
            "app state: whisper contexts loaded"
        );

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
            Self::startup_setting(
                settings.as_ref(),
                crate::settings::keys::DIARIZATION_ENABLED,
            )
            .await,
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
        record_phase("diarizer init");
        tracing::info!(
            elapsed_ms = t_start.elapsed().as_millis(),
            "app state: diarizer ready"
        );
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
            Ok(None) => crate::hotkey::ptt::PttCombo::single(crate::hotkey::ptt::DEFAULT_PTT_KEY),
            Err(e) => {
                tracing::warn!(error = %e, "startup: PTT_COMBO read failed; using default");
                crate::hotkey::ptt::PttCombo::single(crate::hotkey::ptt::DEFAULT_PTT_KEY)
            }
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
            Ok(None) => true,
            Err(e) => {
                tracing::warn!(error = %e, "startup: PTT_ENABLED read failed; defaulting to enabled");
                true
            }
        };

        // HUD on by default. Absent / unparseable settings rows fall
        // through to the default rather than silently turning the
        // HUD off — first-time users benefit from the visual cue
        // that the mic is hot.
        let hud_enabled = parse_hud_enabled_setting(
            Self::startup_setting(settings.as_ref(), crate::settings::keys::HUD_ENABLED).await,
        );

        // Audio cues — off by default (#292). Reads the same
        // settings table the IPC commands write through.
        let sound_cues_enabled = parse_sound_cues_setting(
            Self::startup_setting(settings.as_ref(), crate::settings::keys::SOUND_CUES_ENABLED)
                .await,
        );

        // Per-event sub-toggles (#463). Default true — see
        // `parse_sound_cue_sub_setting` for the reasoning.
        let sound_cue_start_enabled = parse_sound_cue_sub_setting(
            Self::startup_setting(
                settings.as_ref(),
                crate::settings::keys::SOUND_CUE_START_ENABLED,
            )
            .await,
        );
        let sound_cue_complete_enabled = parse_sound_cue_sub_setting(
            Self::startup_setting(
                settings.as_ref(),
                crate::settings::keys::SOUND_CUE_COMPLETE_ENABLED,
            )
            .await,
        );

        // Meeting auto-start mode. Off by default; absent or
        // garbage rows fall through to Off (the safer default —
        // a corrupted row should not silently make the mic
        // spontaneously turn on).
        let meeting_autostart_mode = crate::meeting::MeetingAutostartMode::from_setting(
            Self::startup_setting(
                settings.as_ref(),
                crate::settings::keys::MEETING_AUTOSTART_MODE,
            )
            .await
            .as_deref(),
        );

        // Final phase covers everything between the diarizer init and
        // the AppState compose (settings reads, autostart-mode decode,
        // PTT setup, hud / sound-cue flag init). Cheap on every host
        // I've measured but still worth a checkpoint so a future
        // regression in any of those settings reads is visible.
        record_phase("settings + flag wiring");

        let state = AppStateBuilder::new()
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
            .debug_log(debug_log)
            .startup_timings(startup_timings)
            .build();
        tracing::info!(
            elapsed_ms = t_start.elapsed().as_millis(),
            "app state: build_default complete"
        );
        state
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
