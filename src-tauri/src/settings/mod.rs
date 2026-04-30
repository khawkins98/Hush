//! Generic key-value settings persisted in SQLite.
//!
//! Backs the `settings` table from migration 0001 (`key TEXT PRIMARY KEY,
//! value TEXT NOT NULL`). Higher layers (e.g. the model picker, hotkey
//! rebind UI) read and write their own keys directly — there is no
//! per-setting typed wrapper because every setting we ship is a small
//! number of strings or stringly-encoded values, and a typed wrapper for
//! each would balloon the surface for no real safety win.
//!
//! ## Why a generic K/V store rather than a structured config file
//!
//! - Migrating a structured `config.toml` between versions requires
//!   careful schema versioning. SQLite already gives us versioning via
//!   migrations, and rows can be added/removed without touching old
//!   keys.
//! - Reading settings concurrently with other db work (history insert,
//!   etc.) is naturally serialised by the WAL pool we already share.
//! - The first non-trivial setting — the model picker's
//!   `selected_model_id` — is a single string. Bringing in `serde` or
//!   `figment` for it would be premature.
//!
//! ## Test seam (PRD §13.5)
//!
//! Higher layers depend on the [`SettingsRepository`] trait, never on
//! [`SqliteSettingsRepository`] directly, so unit tests of consumers
//! can substitute a deterministic mock without touching SQLite.

pub mod sqlite;

pub use sqlite::SqliteSettingsRepository;

use anyhow::Result;
use async_trait::async_trait;

/// Settings keys consumed by the app. Centralising them so a typo in
/// one call site doesn't silently miss a stored value (the next call
/// would just see `None` and behave as if nothing was set).
pub mod keys {
    /// ID of the model the user picked in the model picker. Format is
    /// the catalog id (e.g. `whisper-base`); resolution to a filesystem
    /// path happens in the transcription module's setup.
    pub const SELECTED_MODEL_ID: &str = "selected_model_id";

    /// Marks that the macOS first-run welcome flow has been shown and
    /// dismissed. Stored as the literal string `"true"` once set; any
    /// other value (including absent) means "show the welcome on
    /// next launch". Per-platform behaviour: only the macOS frontend
    /// reads this — Linux/Windows never check.
    pub const FIRST_RUN_COMPLETED: &str = "first_run_completed";

    /// Whether the PTT listener should run. Stored as `"true"` /
    /// `"false"`; absent means "platform default" (true on Linux /
    /// Windows, false on macOS so the Input Monitoring prompt only
    /// fires when the user opts in). Settings UI flips this; the env
    /// vars `HUSH_PTT_ENABLE` / `HUSH_PTT_DISABLE` still work as
    /// hard overrides for power users / dev workflows.
    pub const PTT_ENABLED: &str = "ptt_enabled";

    /// User's chosen PTT key combination. Stored as a `+`-separated
    /// list of `PttKey` names (e.g. `RightMeta` or
    /// `RightMeta+RightShift`). Absent means "platform default
    /// single key" (RightMeta on macOS, RightControl elsewhere). All
    /// keys in the combo must be held simultaneously to trigger PTT.
    pub const PTT_COMBO: &str = "ptt_combo";

    /// Whether the recording HUD overlay should appear during
    /// dictation / meeting capture. Stored as `"true"` / `"false"`;
    /// absent means "show the HUD" (the default — first-time users
    /// benefit from the visual confirmation that the mic is hot).
    /// Power users who'd rather not see the floating pill can flip
    /// this off in Settings → General.
    pub const HUD_ENABLED: &str = "hud_enabled";

    /// Boolean stored as `"1"` / `"0"`. Whether to play short
    /// macOS system sounds at the recording-start and
    /// transcription-complete transitions (#292). Absent or
    /// `"0"` means silent — the default. Distinct from
    /// `HUD_ENABLED` because some users want visual feedback
    /// (or none) but can't have audio (shared office, meeting
    /// room, focus mode).
    pub const SOUND_CUES_ENABLED: &str = "sound_cues_enabled";

    /// Auto-start mode for Meeting Mode. The foreground poller
    /// uses this to decide what to do when a Meeting-classified
    /// app focuses. Stored as one of:
    /// - `"off"` — never auto-start (the default; user starts
    ///   every session manually).
    /// - `"always"` — auto-start a session the moment a Meeting-
    ///   classified app focuses; no prompt.
    ///
    /// Future: `"ask"` once the prompt UI ships. Absent /
    /// unparseable values fall back to `"off"` — the safer
    /// default; nobody wants their mic to spontaneously turn on
    /// because of a bad settings row.
    pub const MEETING_AUTOSTART_MODE: &str = "meeting_autostart_mode";

    /// Whether speaker diarization should run on meeting transcripts.
    /// Stored as `"true"` / `"false"`; absent means "off" — the safer
    /// default until the ONNX model + download pipeline lands in PR-B
    /// of #111. The foundation PR ships the user-visible plumbing
    /// (this setting key + IPC + UI toggle) so the model-download
    /// follow-up can flip the default without churn. When off, the
    /// existing source-derived `"mic"` / `"system"` labels stand in
    /// for proper speaker IDs.
    pub const DIARIZATION_ENABLED: &str = "diarization_enabled";
}

/// Repository trait at the storage boundary.
///
/// `Send + Sync` so the IPC layer holds an `Arc<dyn SettingsRepository>`
/// across async Tauri commands; object-safe via `async-trait` for parity
/// with the other repositories in the codebase.
#[async_trait]
pub trait SettingsRepository: Send + Sync {
    /// Read a single setting. Returns `None` if the key has never been
    /// written rather than treating "absent" as a value, so callers can
    /// distinguish "user never set this" from "user set this to empty".
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Write (or overwrite) a setting. The store has no notion of
    /// types — values are persisted verbatim and the caller is
    /// responsible for any serialisation.
    async fn set(&self, key: &str, value: &str) -> Result<()>;

    /// Remove a setting. No-op if `key` does not exist, mirroring the
    /// other repository delete contracts.
    async fn remove(&self, key: &str) -> Result<()>;
}
