//! Optional audio cues for the dictation hot path (#292).
//!
//! Two transition moments matter most for the PTT workflow where
//! the user's eyes are on the *target app*, not on Hush:
//!
//! - **Recording starts** — mic is hot, safe to speak.
//! - **Transcription complete** — clipboard is ready, safe to paste.
//!
//! Both fire short macOS system sounds (Tink and Glass respectively)
//! via `NSSound soundNamed:`. Default-off; users opt in via the
//! Settings → General → Audio cues toggle, mirrored on
//! `AppState::sound_cues_enabled` (an `AtomicBool`) for sync hot-
//! path reads. Output respects the system volume + Do Not Disturb
//! automatically (NSSound delegates to CoreAudio).
//!
//! Non-macOS is a no-op for now. Linux / Windows users would
//! benefit from the same affordance, but the cross-platform sound-
//! playback story isn't worth a new dep until those platforms have
//! hands-on test coverage. Adding `rodio` or similar later is a
//! single-file extension to this module.

#[cfg(target_os = "macos")]
mod imp {
    use objc2::class;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;

    /// Play a named macOS system sound by `NSSound soundNamed:`.
    /// Recognised names live in `/System/Library/Sounds/` (Tink,
    /// Glass, Pop, Ping, …). The lookup is case-sensitive on
    /// modern macOS.
    pub fn play(name: &str) {
        // SAFETY: classic AppKit pattern — `NSSound soundNamed:`
        // returns an autoreleased `NSSound *` (or nil if the
        // name doesn't resolve). We retain via the class +
        // messaging shape and don't escape the pointer beyond
        // this scope. `play` is non-blocking; the framework
        // queues the sound on its own thread.
        unsafe {
            let ns_name = NSString::from_str(name);
            let cls = class!(NSSound);
            let sound: *mut AnyObject = msg_send![cls, soundNamed: &*ns_name];
            if sound.is_null() {
                tracing::debug!(
                    name,
                    "audio_cues: NSSound soundNamed: returned nil; skipping"
                );
                return;
            }
            let _: bool = msg_send![sound, play];
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn play(_name: &str) {
        // No-op on non-macOS; see module-doc.
    }
}

/// Wire-format constants for the two cue points. Centralised so a
/// future refactor that swaps which built-in sound is used (e.g.
/// "Pop" instead of "Tink") only needs to change them here.
pub const CUE_RECORDING_START: &str = "Tink";
pub const CUE_TRANSCRIPTION_READY: &str = "Glass";

/// Play a cue sound. No-op when `enabled` is false; the caller
/// passes `state.sound_cues_enabled.load(Ordering::Relaxed)` so the
/// hot path doesn't have to branch.
pub fn play_if_enabled(enabled: bool, name: &str) {
    if !enabled {
        return;
    }
    imp::play(name);
}
