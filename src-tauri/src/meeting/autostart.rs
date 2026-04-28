//! Auto-start lifecycle for Meeting Mode.
//!
//! Manual-start has shipped since #110: the user clicks Start in
//! the Meetings panel and a session opens. Auto-start watches the
//! foreground app (#112's open piece) and opens a session
//! automatically when a Meeting-classified app comes to focus.
//!
//! ## Why a poller, not a notification subscription
//!
//! macOS exposes `NSWorkspace.didActivateApplicationNotification`
//! which would let us react instantly to focus changes; Linux and
//! Windows have rough equivalents. None of those plumb through
//! `active-win-pos-rs` cleanly, and the cost of a 3 s tick is
//! negligible — `get_active_window()` is a single OS call. A
//! per-platform notification glue is a worthwhile follow-up
//! (smoother UX, lower wake-up rate) but not a v1 blocker.
//!
//! ## Decision logic
//!
//! The poller's job each tick is purely classification:
//! - Snapshot the foreground app.
//! - Run it through the [`AppClassifier`].
//! - Compare with the previous tick's verdict.
//!
//! [`AutostartDecision::should_start`] is a free function that
//! takes the previous and current verdicts plus the user's mode
//! and returns whether to call `start_manual`. Tests pin every
//! transition combo without spinning up a tokio task.
//!
//! ## What's deliberately NOT here in v1
//!
//! - **Auto-stop on app blur.** Users alt-tab during meetings; a
//!   short-window blur shouldn't end the session. Auto-stop will
//!   need a "5 minutes away from the meeting app" debounce, which
//!   has its own state machine. Manual stop only for now.
//! - **`"ask"` mode.** The wire format reserves the value but the
//!   poller treats it as `"off"` until the prompt UI ships.
//! - **Permission pre-check.** If mic is denied, `start_manual`
//!   will fail and surface a tracing warning. A pre-emptive check
//!   that suppresses the start before the failure is a
//!   nice-to-have.

use serde::{Deserialize, Serialize};

use super::MeetingAppKind;

/// User-facing auto-start mode.
///
/// `serde` derives use kebab-case so the wire shape matches the
/// frontend's discriminated union convention. The on-disk
/// representation stored under
/// [`crate::settings::keys::MEETING_AUTOSTART_MODE`] is the
/// same kebab-case string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeetingAutostartMode {
    /// Never auto-start. The user clicks Start manually each time.
    /// This is the default for new installs — auto-recording the
    /// mic without the user explicitly opting in is the kind of
    /// surprise that costs trust.
    Off,
    /// Auto-start whenever a Meeting-classified app focuses, no
    /// prompt. Most useful for users who always-record meetings
    /// (interviewers, researchers).
    Always,
}

impl MeetingAutostartMode {
    /// Parse the persisted settings row into the enum. Absent
    /// rows or any unrecognised value fall through to `Off` — the
    /// safer default. A garbage row should not silently make the
    /// mic spontaneously turn on.
    pub fn from_setting(raw: Option<&str>) -> Self {
        match raw {
            Some("always") => Self::Always,
            // Reserved for the prompt UI; for now treat it as Off.
            Some("ask") => Self::Off,
            _ => Self::Off,
        }
    }

    /// Encode the enum to the on-disk string used by the settings
    /// row.
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Always => "always",
        }
    }
}

/// Decision the poller takes on each foreground transition.
///
/// `Start { app_name }` carries the focused app's name so the
/// caller passes it through to `start_manual` without re-querying
/// `active-win-pos-rs`. `Ignore` covers every other case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutostartDecision {
    Start { app_name: String },
    Ignore,
}

impl AutostartDecision {
    /// Decide whether the poller should auto-start a session given
    /// the previous tick's verdict, the current tick's verdict,
    /// and the user's mode.
    ///
    /// We start only on a transition into a Meeting verdict.
    /// Steady-state (Meeting on both ticks) is silent — otherwise
    /// every poll while a meeting app stays focused would
    /// repeatedly call `start_manual`.
    ///
    /// `session_active` is the in-memory flag the manager exposes
    /// via `active_session_id().is_some()`. If a session is
    /// already open we never auto-start; the user still owns the
    /// stop button.
    pub fn decide(
        previous: Option<MeetingAppKind>,
        current: MeetingAppKind,
        current_app_name: &str,
        mode: MeetingAutostartMode,
        session_active: bool,
    ) -> AutostartDecision {
        if mode == MeetingAutostartMode::Off {
            return AutostartDecision::Ignore;
        }
        if session_active {
            return AutostartDecision::Ignore;
        }
        // Only start on a transition INTO Meeting. Steady-state
        // Meeting → Meeting is silent.
        let was_meeting = matches!(previous, Some(MeetingAppKind::Meeting));
        if current == MeetingAppKind::Meeting && !was_meeting {
            AutostartDecision::Start {
                app_name: current_app_name.to_owned(),
            }
        } else {
            AutostartDecision::Ignore
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_setting_handles_all_branches() {
        assert_eq!(
            MeetingAutostartMode::from_setting(None),
            MeetingAutostartMode::Off
        );
        assert_eq!(
            MeetingAutostartMode::from_setting(Some("off")),
            MeetingAutostartMode::Off
        );
        assert_eq!(
            MeetingAutostartMode::from_setting(Some("always")),
            MeetingAutostartMode::Always
        );
        // "ask" is reserved for the future prompt UI; treat as
        // off for now — never silently start without an explicit
        // opt-in mode.
        assert_eq!(
            MeetingAutostartMode::from_setting(Some("ask")),
            MeetingAutostartMode::Off
        );
        // Garbage row falls back to Off (the safer default).
        assert_eq!(
            MeetingAutostartMode::from_setting(Some("garbage")),
            MeetingAutostartMode::Off
        );
    }

    #[test]
    fn as_setting_round_trips_through_from_setting() {
        for mode in [MeetingAutostartMode::Off, MeetingAutostartMode::Always] {
            assert_eq!(
                MeetingAutostartMode::from_setting(Some(mode.as_setting())),
                mode
            );
        }
    }

    #[test]
    fn decide_off_mode_never_starts() {
        // Even on a transition that would otherwise start, Off
        // mode is silent.
        let d = AutostartDecision::decide(
            Some(MeetingAppKind::Other),
            MeetingAppKind::Meeting,
            "Zoom",
            MeetingAutostartMode::Off,
            false,
        );
        assert_eq!(d, AutostartDecision::Ignore);
    }

    #[test]
    fn decide_always_starts_on_transition_into_meeting() {
        let d = AutostartDecision::decide(
            Some(MeetingAppKind::Other),
            MeetingAppKind::Meeting,
            "Zoom",
            MeetingAutostartMode::Always,
            false,
        );
        assert_eq!(
            d,
            AutostartDecision::Start {
                app_name: "Zoom".into(),
            }
        );
    }

    #[test]
    fn decide_always_starts_on_first_tick_meeting() {
        // No previous verdict (first poll) and a Meeting-classified
        // app: still a transition into Meeting, so start. Defends
        // against "user launches with Zoom already focused".
        let d = AutostartDecision::decide(
            None,
            MeetingAppKind::Meeting,
            "Zoom",
            MeetingAutostartMode::Always,
            false,
        );
        assert_eq!(
            d,
            AutostartDecision::Start {
                app_name: "Zoom".into(),
            }
        );
    }

    #[test]
    fn decide_steady_state_meeting_is_silent() {
        // Meeting → Meeting on consecutive ticks: don't re-start
        // every poll while the app stays focused.
        let d = AutostartDecision::decide(
            Some(MeetingAppKind::Meeting),
            MeetingAppKind::Meeting,
            "Zoom",
            MeetingAutostartMode::Always,
            false,
        );
        assert_eq!(d, AutostartDecision::Ignore);
    }

    #[test]
    fn decide_skips_when_session_is_already_active() {
        // A session is already open — don't try to open another
        // one. The user manually started something earlier.
        let d = AutostartDecision::decide(
            Some(MeetingAppKind::Other),
            MeetingAppKind::Meeting,
            "Zoom",
            MeetingAutostartMode::Always,
            true,
        );
        assert_eq!(d, AutostartDecision::Ignore);
    }

    #[test]
    fn decide_media_or_other_does_not_start() {
        for kind in [MeetingAppKind::Media, MeetingAppKind::Other] {
            let d = AutostartDecision::decide(
                Some(MeetingAppKind::Other),
                kind,
                "Spotify",
                MeetingAutostartMode::Always,
                false,
            );
            assert_eq!(d, AutostartDecision::Ignore, "for kind {kind:?}");
        }
    }
}
