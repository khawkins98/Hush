//! Bundle-id → [`MeetingAppKind`] lookup (#431).
//!
//! Lifted out of [`super::manager`] under #431 to take a clean
//! ~180-LOC chunk out of the manager's mega-module — the table is
//! purely declarative and entirely independent of the lifecycle
//! state machine, so it sits comfortably in its own peer file
//! without any visibility / coupling concerns.
//!
//! The classifier drives two things:
//! 1. **`app_kind` row tag** stamped on new sessions (informational,
//!    drives the panel's coloured tag in the frontend).
//! 2. **Auto-start gate** in `run_meeting_detection_task` (#665):
//!    the frontmost app's kind is one of the six guards in
//!    `evaluate_mic_state` — only `MeetingAppKind::Meeting` unblocks
//!    an automatic session start.
//!
//! Per-user overrides (Phase E, [#112]) will write entries into the
//! settings table that this struct reads on construction. Today
//! the table is empty; the defaults are the only signal.
//!
//! [#112]: https://github.com/khawkins98/Hush/issues/112

use super::MeetingAppKind;

/// Bundle-id → [`MeetingAppKind`] lookup. See module docs for
/// rationale + override semantics.
pub struct AppClassifier {
    /// Future: replace with `HashMap` once the entry list grows
    /// past ~20. v1 stays linear because the default table is small
    /// and the per-classify cost is irrelevant.
    entries: Vec<(&'static str, MeetingAppKind)>,
    /// User-supplied overrides loaded from
    /// [`super::MeetingAppOverrideRepository`] (#112). Consulted
    /// before the static `entries` table — an override row with the
    /// same `app_name` as a default wins.
    ///
    /// Snapshot at construction time. Edits to the override table
    /// from the Settings panel don't propagate live; the next
    /// session start reads a fresh snapshot. Live propagation would
    /// need an event-driven invalidation, which the manual-start
    /// session lifecycle doesn't justify yet.
    overrides: Vec<(String, MeetingAppKind)>,
}

impl AppClassifier {
    /// Hardcoded defaults. Each entry matches what
    /// `active-win-pos-rs::get_active_window().app_name` returns on
    /// the corresponding platform — macOS prefers reverse-DNS bundle
    /// ids, Linux returns the process / app name, and Windows
    /// returns the executable basename (with `.exe`). To cover all
    /// three OSes the table lists every variant of an app
    /// explicitly: matching is exact-string, no normalisation, so
    /// "Zoom" on Linux and "Zoom.exe" on Windows must each be its
    /// own entry. Locale variants (e.g. "Microsoft Teams (work or
    /// school)") only land here if active-win actually returns them
    /// in shipped builds — covering every translation is unbounded.
    pub fn default_table() -> Self {
        Self {
            entries: vec![
                // ---- Meeting / video-call apps ----
                // Auto-start (when that policy lands) defaults to
                // "ask" for these.
                //
                // Zoom
                ("zoom.us", MeetingAppKind::Meeting),
                ("us.zoom.xos", MeetingAppKind::Meeting), // macOS bundle
                ("Zoom", MeetingAppKind::Meeting),        // Linux / display name
                ("Zoom Meetings", MeetingAppKind::Meeting),
                ("zoom", MeetingAppKind::Meeting), // Linux process
                ("Zoom.exe", MeetingAppKind::Meeting), // Windows
                ("zoom.exe", MeetingAppKind::Meeting),
                // Microsoft Teams
                ("Microsoft Teams", MeetingAppKind::Meeting),
                ("com.microsoft.teams2", MeetingAppKind::Meeting), // macOS bundle
                ("Microsoft Teams (work or school)", MeetingAppKind::Meeting),
                ("ms-teams", MeetingAppKind::Meeting),
                ("ms-teams.exe", MeetingAppKind::Meeting), // Windows
                ("Teams.exe", MeetingAppKind::Meeting),
                ("teams-for-linux", MeetingAppKind::Meeting), // unofficial Linux client
                // Google Meet (largely browser-based, but a few
                // PWAs / wrappers exist).
                ("Google Meet", MeetingAppKind::Meeting),
                ("Meet", MeetingAppKind::Meeting),
                // Discord
                ("Discord", MeetingAppKind::Meeting),
                ("com.hnc.Discord", MeetingAppKind::Meeting), // macOS bundle
                ("discord", MeetingAppKind::Meeting),         // Linux process
                ("Discord.exe", MeetingAppKind::Meeting),     // Windows
                // Slack
                ("Slack", MeetingAppKind::Meeting),
                ("com.tinyspeck.slackmacgap", MeetingAppKind::Meeting), // macOS bundle
                ("slack", MeetingAppKind::Meeting),                     // Linux process
                ("slack.exe", MeetingAppKind::Meeting),                 // Windows
                ("Slack.exe", MeetingAppKind::Meeting),
                // Webex
                ("Webex", MeetingAppKind::Meeting),
                ("Cisco Webex Meetings", MeetingAppKind::Meeting),
                ("com.cisco.webexmeetingsapp", MeetingAppKind::Meeting), // macOS bundle
                ("webex", MeetingAppKind::Meeting),
                ("Webex.exe", MeetingAppKind::Meeting),
                ("CiscoCollabHost.exe", MeetingAppKind::Meeting),
                // Skype (legacy but still in active use, especially
                // for international calls).
                ("Skype", MeetingAppKind::Meeting),
                ("skype", MeetingAppKind::Meeting),
                ("Skype.exe", MeetingAppKind::Meeting),
                // GoTo / GoToMeeting
                ("GoToMeeting", MeetingAppKind::Meeting),
                ("GoToMeeting.exe", MeetingAppKind::Meeting),
                ("GoTo", MeetingAppKind::Meeting),
                // BlueJeans (Verizon)
                ("BlueJeans", MeetingAppKind::Meeting),
                ("BlueJeans.exe", MeetingAppKind::Meeting),
                // Loom (async video — not a live call but the
                // recording surface is the same)
                ("Loom", MeetingAppKind::Meeting),
                ("com.loom.desktop", MeetingAppKind::Meeting), // macOS bundle
                ("Loom.exe", MeetingAppKind::Meeting),
                // FaceTime (macOS / iOS native video call)
                ("FaceTime", MeetingAppKind::Meeting),
                ("com.apple.FaceTime", MeetingAppKind::Meeting), // macOS bundle
                // Tuple (pair-programming video call)
                ("Tuple", MeetingAppKind::Meeting),
                ("app.tuple.app", MeetingAppKind::Meeting), // macOS bundle
                // Around (spatial video call)
                ("Around", MeetingAppKind::Meeting),
                ("co.around.Around", MeetingAppKind::Meeting), // macOS bundle
                // Microsoft Teams (classic / legacy bundle ID)
                ("com.microsoft.teams", MeetingAppKind::Meeting), // macOS bundle (classic)
                // ---- Media apps ----
                // Auto-start (when shipped) defaults to "no" for
                // these — most users don't want a YouTube watch-
                // party transcribed by accident.
                //
                // YouTube (typically a browser tab; PWA / wrappers
                // included for completeness)
                ("YouTube", MeetingAppKind::Media),
                // Spotify
                ("Spotify", MeetingAppKind::Media),
                ("com.spotify.client", MeetingAppKind::Media), // macOS bundle
                ("spotify", MeetingAppKind::Media),            // Linux process
                ("Spotify.exe", MeetingAppKind::Media),
                // Apple Music / iTunes (macOS) and the legacy iTunes
                // on Windows.
                ("Apple Music", MeetingAppKind::Media),
                ("Music", MeetingAppKind::Media),
                ("iTunes", MeetingAppKind::Media),
                ("iTunes.exe", MeetingAppKind::Media),
                ("Podcasts", MeetingAppKind::Media),
                // Apple TV desktop on macOS — sound is system
                // audio, the surfaced app is "TV".
                ("TV", MeetingAppKind::Media),
                // VLC — cross-platform default media player
                ("VLC", MeetingAppKind::Media),
                ("VLC media player", MeetingAppKind::Media),
                ("vlc", MeetingAppKind::Media),
                ("vlc.exe", MeetingAppKind::Media),
                // Plex / Plexamp
                ("Plex", MeetingAppKind::Media),
                ("Plex.exe", MeetingAppKind::Media),
                ("plexamp", MeetingAppKind::Media),
                ("Plexamp", MeetingAppKind::Media),
            ],
            overrides: Vec::new(),
        }
    }

    /// Construct with a user-override snapshot loaded from the
    /// repository. The override list is checked before the static
    /// defaults, so a row with the same `app_name` as a default
    /// wins.
    pub fn with_overrides(overrides: Vec<(String, MeetingAppKind)>) -> Self {
        let mut classifier = Self::default_table();
        classifier.overrides = overrides;
        classifier
    }

    /// Read-only view of the built-in `(app_name, kind)` table —
    /// the curated list in `default_table()`, **excluding** any
    /// user overrides. Order matches the source order in
    /// `default_table()` (curated by app) so callers can group
    /// adjacent rows by display name without re-sorting.
    ///
    /// Used by the Settings → Meeting → App Classification panel
    /// (#320) to show users what's already covered, so they
    /// don't add redundant overrides.
    pub fn default_entries(&self) -> Vec<(String, MeetingAppKind)> {
        self.entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect()
    }

    pub fn classify(&self, app_name: &str) -> MeetingAppKind {
        // User overrides win over defaults — even when an override
        // explicitly maps an app the table classifies as Meeting to
        // Other (the way to ignore an app the defaults catch).
        for (key, kind) in &self.overrides {
            if key == app_name {
                return *kind;
            }
        }
        for (key, kind) in &self.entries {
            if *key == app_name {
                return *kind;
            }
        }
        MeetingAppKind::Other
    }
}
