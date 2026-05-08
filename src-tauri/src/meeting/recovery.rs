//! Per-source recovery state for the meeting pump's device-loss /
//! reconnect machine.
//!
//! Extracted from `pump.rs` (#655) so the state transitions and the
//! enum that drives them live together rather than scattered through
//! a 700-line function body.

use crate::audio::AudioSource;

/// Per-source recovery state for the fallback / reconnect machine in
/// [`super::pump::run_pump`].
#[derive(Debug, Clone)]
pub(super) enum SourceRecoveryState {
    /// Source is capturing normally.
    Active,
    /// DeviceLost occurred and the pump is now capturing from a
    /// different (fallback) device. Holds the original source info so
    /// the reconnect watcher can swap back when the original returns.
    Fallback {
        original_source: AudioSource,
        original_device_name: String,
    },
    /// DeviceLost occurred and no fallback was available. The pump is
    /// NOT capturing for this source. Holds the original info so the
    /// reconnect watcher can reopen it when the device is replugged.
    LostAwaitingReconnect {
        original_source: AudioSource,
        original_device_name: String,
    },
    /// Permanently dead (non-device-loss failure, or SystemAudio
    /// disconnect). The reconnect watcher ignores this state.
    Dead,
}

impl SourceRecoveryState {
    /// Returns a clone of the original source + device name if this
    /// state is eligible for a reconnect attempt. Returns `None` for
    /// `Active` and `Dead`.
    ///
    /// Returns owned data (not references) so callers can mutate
    /// `recovery_states[i]` in the same scope after extracting the
    /// reconnect target — a common pattern in the reconnect watcher
    /// where we need to call `open_source_handle` and then immediately
    /// transition to `Active`.
    pub(super) fn reconnect_target(&self) -> Option<(AudioSource, String)> {
        match self {
            SourceRecoveryState::Fallback {
                original_source,
                original_device_name,
            }
            | SourceRecoveryState::LostAwaitingReconnect {
                original_source,
                original_device_name,
            } => Some((original_source.clone(), original_device_name.clone())),
            SourceRecoveryState::Active | SourceRecoveryState::Dead => None,
        }
    }
}
