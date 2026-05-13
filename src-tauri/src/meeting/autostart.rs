//! Auto-start mode for Meeting Mode (#112).
//!
//! `MeetingAutostartMode` is the user's configured preference for whether
//! Hush should start a meeting session automatically. It is persisted as a
//! kebab-case string under
//! [`crate::settings::keys::MEETING_AUTOSTART_MODE`] and encoded into an
//! atomic `u8` in [`crate::ipc::RuntimeFlags`] for lock-free reads from the
//! async meeting-detection task.
//!
//! ## Detection mechanism
//!
//! v1 used a 3-second foreground-app polling loop (`autostart_poller.rs`).
//! #665 replaced it with a CoreAudio HAL event-driven monitor
//! (`mic_camera_monitor.rs`): when any input device transitions from idle to
//! active, the detection task evaluates the frontmost app and starts a
//! session if the mode is `Always`.
//!
//! ## What's deliberately NOT here
//!
//! - **`"ask"` mode.** Reserved in the wire format; treated as `Off` until
//!   the prompt UI ships.
//! - **Permission pre-check.** If the mic is denied, `start_manual` fails
//!   and surfaces a warning. A pre-emptive check is a future nice-to-have.

use serde::{Deserialize, Serialize};

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
    Off,
    /// Auto-start whenever a Meeting-classified app is frontmost
    /// and the mic activates, no prompt. Most useful for users who
    /// always-record meetings (interviewers, researchers).
    Always,
}

impl MeetingAutostartMode {
    /// Parse the persisted settings row into the enum. Absent
    /// rows fall through to `Always` (the default). Any
    /// unrecognised value falls through to `Always` rather than
    /// silently disabling a feature the user expects to work.
    pub fn from_setting(raw: Option<&str>) -> Self {
        match raw {
            Some("always") | None => Self::Always,
            Some("off") => Self::Off,
            // Reserved for the prompt UI; for now treat it as Always.
            Some("ask") => Self::Always,
            _ => Self::Always,
        }
    }

    /// Encode the enum to the on-disk string used by the settings row.
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Always => "always",
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
            MeetingAutostartMode::Always
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
        // Always for now.
        assert_eq!(
            MeetingAutostartMode::from_setting(Some("ask")),
            MeetingAutostartMode::Always
        );
        // Unknown row falls back to Always (the default).
        assert_eq!(
            MeetingAutostartMode::from_setting(Some("garbage")),
            MeetingAutostartMode::Always
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
}
