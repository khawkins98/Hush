//! Cross-platform audio capture for offline dictation.
//!
//! Concept inspired by VoiceInk's AVFoundation-based audio capture.
//! Reimplemented from observed public behaviour; no source code referenced.
//! See §13.8 of the PRD.
//!
//! ## Responsibilities
//!
//! - Enumerate available input devices.
//! - Begin and end a capture session driven by the hotkey layer.
//! - Convert whatever native sample format the device hands us into f32 PCM.
//! - Surface the captured samples plus the format they were captured in.
//!
//! ## Design notes
//!
//! **Capture-at-native-format.** Whisper.cpp expects 16 kHz mono f32 PCM, but
//! consumer microphones almost universally prefer 44.1 or 48 kHz at 1–2
//! channels, and the platform backends (CoreAudio, WASAPI, ALSA/PulseAudio)
//! often refuse to honour an arbitrary sample-rate request. Rather than fight
//! the OS at capture time, this module captures at the device's preferred
//! format and surfaces both the samples and the format. Downmix and
//! resampling happen downstream where we can recover from a poor format
//! match without losing the buffer. See `learnings.md` (2026-04-25) for the
//! full reasoning.
//!
//! **Threading.** `cpal::Stream` is `!Send` on most platforms — its backing
//! audio thread must be polled from the thread that constructed it. The
//! cpal-backed prod impl ([`CpalAudioCapture`]) owns the stream on a
//! dedicated worker thread and drives it via a command channel. The public
//! API is `Send + Sync` and synchronous from the caller's perspective.
//!
//! **Test seam (PRD §13.5).** Consumers of audio capture depend on the
//! [`AudioCapture`] trait, never on [`CpalAudioCapture`] directly, so unit
//! tests of higher layers can substitute a deterministic mock without
//! pulling in `cpal` or a real device.
//!
//! ## File layout
//!
//! - This file: cross-platform types ([`CaptureFormat`], [`AudioSource`],
//!   [`AudioSession`], [`AudioCapture`]) plus shared helpers
//!   ([`MAX_BUFFER_FRAMES`], [`drain_consumer`], [`log_overflow_if_set`]).
//! - [`cpal`]: the cpal-backed prod impl (cross-platform mic capture).
//! - [`core_audio_tap`] (macOS only): system-audio capture via
//!   `AudioHardwareCreateProcessTap` (the Swift helper binary path).
//! - [`file_source`] (under `--features test-utils`): the
//!   `WavFileAudioCapture` deterministic file-backed test fixture.
//!
//! Trait tests live in `tests.rs` (peer of this file). The cpal-specific
//! tests live next to the cpal code in `cpal.rs`.

mod format;

mod cpal;

#[cfg(target_os = "macos")]
pub mod core_audio_tap;

#[cfg(feature = "test-utils")]
pub mod file_source;

#[cfg(test)]
mod tests;

pub use cpal::CpalAudioCapture;
pub use format::{apply_mic_gain, downmix_to_mono};

/// Defensive ceiling on the number of `f32` samples a single capture
/// buffer may hold. When this limit is reached the **oldest** samples
/// are evicted so the capture buffer acts as a circular window into
/// the most-recent audio (#827). An unbounded growth path (pump task
/// wedged, audio callback still firing) therefore can't OOM the
/// process, and the tail of a long dictation is preserved rather than
/// the head.
///
/// Sized for ~2 minutes of 48 kHz stereo audio = `48_000 * 2 * 120`
/// = 11.5M samples ≈ 46 MB. The meeting pump's normal-case window is
/// 10 s (drained then), so this cap is purely defensive — under the
/// typical drain-then-transcribe cycle it's never hit. A long-form
/// dictation session up to 2 minutes is also fine; anything past that
/// keeps the most-recent 2 minutes and discards older audio.
///
/// The cap is the same for both the cpal mic path and the macOS
/// CoreAudio tap path; both back into circular deques the callbacks push into.
pub(super) const MAX_BUFFER_FRAMES: usize = 48_000 * 2 * 120;

use std::collections::VecDeque;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// Sentinel attached to an `anyhow::Error` chain when the cpal
/// backend's error-callback fires `cpal::StreamError::DeviceNotAvailable`
/// — the device the user picked has been physically disconnected
/// (USB unplugged, AirPods walked out of range, webcam disabled).
///
/// IPC handlers downcast against this so the frontend can render a
/// targeted "microphone disconnected" message via
/// [`crate::ipc::commands::IpcError::AudioDeviceLost`] instead of the
/// generic `audio: …` bucket. Detection-and-surface only; the
/// auto-fallback policy half (#587 PR 2) lives in a future PR after
/// the silent-vs-prompted UX call is made.
#[derive(Debug, Clone)]
pub struct DeviceLost {
    /// Human-readable name of the lost device — same string the user
    /// saw in the source picker, captured at session start so it's
    /// available even after the device is gone.
    pub device: String,
}

impl std::fmt::Display for DeviceLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "audio device '{}' disconnected", self.device)
    }
}

impl std::error::Error for DeviceLost {}

/// Format of a captured audio buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureFormat {
    /// Samples per second, per channel.
    pub sample_rate: u32,
    /// Channel count. Samples in [`CapturedAudio::samples`] are interleaved
    /// in channel order when this is greater than 1.
    pub channels: u16,
}

/// Identifying information about an input device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    /// Stable identifier used to re-select the device across runs.
    ///
    /// `cpal` does not expose a backend-stable opaque ID, so we identify
    /// devices by name. This is good enough for the common case where users
    /// pick a device once in settings and we re-resolve it on next launch; if
    /// two devices share a name (e.g. two identical USB mics) the user will
    /// have to disambiguate by unplugging one. A future backend-specific
    /// identifier can replace this without changing the public type.
    pub id: String,
    /// Human-readable name shown in the settings UI.
    pub name: String,
    /// True if this device is the host's default input.
    pub is_default: bool,
}

/// What the user wants to capture from.
///
/// The dictation hot path always picks `Microphone`. Meeting Mode (#33) and
/// system-audio capture pick `SystemAudio` to record what's playing on the
/// speakers — Zoom calls, podcasts, anything routed through the OS mixer
/// rather than into a microphone.
///
/// Returning a discriminated source rather than overloading "device id"
/// means the audio backend can resolve each variant to a different
/// platform primitive (cpal input device for `Microphone`; CoreAudio
/// process tap on macOS / WASAPI loopback / PulseAudio monitor for
/// `SystemAudio`) without the caller having to know which path each
/// platform takes.
///
/// `serde` derives are present so this can flow over the IPC boundary —
/// the frontend's source picker dispatches on the `kind` tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "deviceId", rename_all = "kebab-case")]
pub enum AudioSource {
    /// Capture from a named microphone, or the system default if `None`.
    Microphone(Option<String>),
    /// Capture what's playing on the system's default audio output.
    /// Per-platform implementation lives behind the [`AudioCapture`] trait;
    /// not all platforms have shipped support yet — see
    /// [`AudioCapture::supports_system_audio`] for the capability check.
    SystemAudio,
}

impl AudioSource {
    /// Convenience constructor for the system default mic — the most
    /// common case at call sites.
    pub fn default_microphone() -> Self {
        AudioSource::Microphone(None)
    }

    /// Coarse kind label, suitable for logs and user-facing error
    /// messages without leaking the inner device id.
    ///
    /// `Debug`-printing the full enum (`AudioSource::Microphone(Some("Khawkins' AirPods"))`)
    /// surfaces user PII — bluetooth pairing names often include
    /// real names. The `kind_label` flattens to `"microphone"` /
    /// `"system-audio"` so structured-logging fields and IPC error
    /// strings stay generic.
    pub fn kind_label(&self) -> &'static str {
        match self {
            AudioSource::Microphone(_) => "microphone",
            AudioSource::SystemAudio => "system-audio",
        }
    }

    /// Short tag the persistence + dispatch layers use to label
    /// utterances and sessions by source: `"mic"` / `"system"`.
    /// Distinct from [`kind_label`] (for logs / error strings)
    /// because that flavour is wordy for in-line transcript
    /// metadata — the frontend's `sourceListLabel` maps
    /// `"mic"` → "Mic" and `"system"` → "System audio".
    ///
    /// **Internal-protocol invariant.** Single source of truth for:
    /// - `MeetingSession.sources` CSV (migration 0004, #244).
    /// - The source-derived `Utterance.speaker_label` fallback in
    ///   `meeting::manager::run_pump` and `dispatch_utterances`.
    ///
    /// All four sites must agree. #244 introduced a drift where
    /// the new sources column used `kind_label`'s long form while
    /// the dispatch sites used hand-rolled `"mic"` / `"system"`,
    /// so the frontend chip rendered the literal long-form
    /// strings through its default case. Routing every site
    /// through this method prevents the next reviewer from
    /// re-discovering the bug.
    pub fn speaker_tag(&self) -> &'static str {
        match self {
            AudioSource::Microphone(_) => "mic",
            AudioSource::SystemAudio => "system",
        }
    }
}

/// Frontend-facing listing of one audio source the user can pick from.
///
/// Flattens the `AudioDevice` + capability axes into a single list so
/// the source picker can render mic devices and the system-audio entry
/// uniformly. The `kind` tag mirrors [`AudioSource`]'s discriminator.
///
/// The `is_supported` flag distinguishes "can be picked right now" from
/// "exists in the catalog but the backend hasn't shipped it yet". Mic
/// devices always set `is_supported = true` (every cpal-supported
/// platform has mic capture). The `SystemAudio` listing reports the
/// backend's [`AudioCapture::supports_system_audio`] return value, so a
/// platform that hasn't shipped its system-audio path yet shows the
/// option as disabled with a "coming soon" affordance instead of letting
/// the user pick it and hit a runtime error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSourceListing {
    /// Discriminated kind: `"microphone"` or `"system-audio"`.
    /// `kebab-case` matches the `AudioSource` serde tag.
    #[serde(rename = "kind")]
    pub kind: AudioSourceKind,
    /// Stable identifier. For mic devices: the device name (cpal does
    /// not expose a backend-stable id). For system audio: the literal
    /// string `"system"` — there's only ever one system-audio source
    /// per host, so a fixed id is enough.
    pub id: String,
    /// Human-readable name shown in the picker.
    pub name: String,
    /// True if this is the host's default for its kind.
    pub is_default: bool,
    /// True if the backend can actually start a capture session
    /// against this source. Mic devices are always supported; the
    /// system-audio entry mirrors [`AudioCapture::supports_system_audio`].
    pub is_supported: bool,
}

/// Discriminator for [`AudioSourceListing`]. Kept as a separate enum
/// rather than reusing [`AudioSource`] because the listing carries a
/// device id alongside the kind in distinct fields (rather than wrapped
/// in the variant) — easier for the frontend to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioSourceKind {
    Microphone,
    SystemAudio,
}

/// Captured audio plus the format it was recorded in.
#[derive(Debug, Clone)]
pub struct CapturedAudio {
    /// Channel-interleaved f32 PCM samples, normalised to `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
    /// Format of `samples`. The transcription layer uses this to drive
    /// downmix and (eventually) resampling before handing data to whisper.
    pub format: CaptureFormat,
}

/// Handle owning the lifecycle of one capture session.
///
/// Returned by [`AudioCapture::start_session`]. Multiple handles may be
/// alive concurrently when the underlying backend supports parallel
/// capture (the [`CpalAudioCapture`] backend does — one mic via cpal
/// alongside one system-audio via the macOS CoreAudio tap, which is the
/// canonical shape the meeting pump uses to capture both sides of a
/// Zoom call).
///
/// # Single-stop discipline
///
/// `stop` consumes the boxed handle (`self: Box<Self>`), so a
/// double-stop is a compile-time error rather than a runtime check.
/// The pump (PR2) relies on this — the cancellation path takes the
/// handle out of an `Option<Box<dyn AudioSession>>` slot exactly once
/// and the type system prevents a second drop racing with the drain.
///
/// # Why a separate trait from `AudioCapture`
///
/// The legacy [`AudioCapture::start_with_source`] / [`AudioCapture::stop`]
/// pair models capture as a singleton operation on the backend itself.
/// That fits the dictation hot path (one source, one transcript,
/// short burst) but doesn't compose for meeting capture, where the
/// pump needs to track several concurrent sources independently.
/// Promoting the session to its own object lets the caller hold N of
/// them — one per source — and stop each on its own cadence.
pub trait AudioSession: Send + Sync {
    /// The source this session is capturing from. Inspected by the
    /// pump so the per-source utterance dispatch can tag each chunk
    /// with the originating mic / system-audio entry.
    fn source(&self) -> &AudioSource;

    /// Latest RMS level for *this* session, in roughly `[0.0, 1.0]`.
    /// Default returns `0.0` for backends that don't track per-session
    /// levels (every test mock today). The cpal-backed mic session
    /// and the CoreAudio-tap-backed system-audio session both
    /// override; the HUD's level pump reads whichever session is
    /// currently active.
    fn current_level(&self) -> f32 {
        0.0
    }

    /// Drain whatever samples have accumulated in the underlying
    /// capture buffer **without stopping the session**. The pump
    /// (post-#108) calls this on its tight tick (~500 ms) to feed
    /// samples into a streaming inference session that needs to keep
    /// receiving fresh audio between drains.
    ///
    /// `sink` is the destination buffer; the implementation appends
    /// samples to it. Returns the [`CaptureFormat`] the samples were
    /// captured in (rate + channel count) so the caller can resample
    /// without re-querying state. The returned format is identical to
    /// what [`Self::stop`] would have surfaced for the same session.
    ///
    /// Default impl errors — backends opt in. The cpal mic backend and
    /// the CoreAudio-tap system-audio backend both override; test
    /// mocks that aren't wired into the streaming pump can leave the
    /// default to surface a clear "no streaming support" message at
    /// the call site.
    ///
    /// **Why an out-parameter, not a return value.** The pump owns
    /// one persistent buffer per source and reuses it across drains
    /// to avoid the realloc tail of growing a fresh `Vec` every tick.
    /// Appending into a caller-owned buffer is the cheaper shape
    /// (single capacity-extend per drain at most). A hypothetical
    /// `fn drain(&self) -> Vec<f32>` would force a fresh allocation
    /// per tick and a copy at the call site.
    fn drain_into(&self, sink: &mut Vec<f32>) -> Result<CaptureFormat> {
        let _ = sink;
        Err(anyhow!(
            "drain_into is not implemented for this AudioSession backend; \
             override the method to opt into streaming-pump capture (used by the \
             meeting pump's continuous drain cadence)"
        ))
    }

    /// Stop capture and drain the buffer. Consumes the handle so
    /// the type system rules out double-drains.
    fn stop(self: Box<Self>) -> Result<CapturedAudio>;
}

/// Trait at the OS boundary. Higher layers (IPC, transcription pipeline)
/// depend only on this trait so OS-touching code can be mocked at the seam.
pub trait AudioCapture: Send + Sync {
    /// Enumerate input devices known to the host.
    fn list_input_devices(&self) -> Result<Vec<AudioDevice>>;

    /// Begin capturing from `device_id`, or the system default if `None`.
    ///
    /// Returns immediately; samples accumulate on a background thread until
    /// [`AudioCapture::stop`] is called. Returns an error if a recording is
    /// already in progress, the named device cannot be found, or the host
    /// refuses to open an input stream on it.
    ///
    /// Equivalent to [`AudioCapture::start_with_source`] with
    /// `AudioSource::Microphone(device_id.map(str::to_owned))`. Kept as a
    /// distinct method so existing call sites and tests continue to work
    /// without churn while the system-audio variant is rolled out
    /// incrementally per platform.
    fn start(&self, device_id: Option<&str>) -> Result<()>;

    /// Begin capturing from `source` — microphone or system audio.
    ///
    /// Default impl dispatches `Microphone` to [`AudioCapture::start`] and
    /// errors on `SystemAudio` with a message that names the platform.
    /// Backends that support system-audio capture override this method
    /// AND override [`AudioCapture::supports_system_audio`] to return
    /// `true` for the appropriate sources.
    ///
    /// Why a default impl: most existing impls (the cpal backend, all
    /// test mocks) have no system-audio support today. The default
    /// keeps them compiling without making every implementor reach for
    /// boilerplate `Err(...)` arms.
    fn start_with_source(&self, source: AudioSource) -> Result<()> {
        match source {
            AudioSource::Microphone(device_id) => self.start(device_id.as_deref()),
            AudioSource::SystemAudio => Err(anyhow!(
                "system audio capture is not yet implemented on this platform — tracked under #106 (Linux) / #107 (Windows)"
            )),
        }
    }

    /// Begin capturing `source` and return a handle that owns the
    /// session's lifecycle. Multiple handles may be alive concurrently
    /// when the backend supports parallel capture (the
    /// [`CpalAudioCapture`] backend does — one mic via cpal + one
    /// system-audio via the macOS CoreAudio tap, which is the canonical
    /// shape the meeting pump uses to capture both sides of a Zoom call).
    ///
    /// The default impl errors so existing mocks (which only need
    /// the singleton [`AudioCapture::start_with_source`] /
    /// [`AudioCapture::stop`] API for the dictation hot path) keep
    /// compiling unchanged. Backends that participate in the meeting
    /// pump override.
    ///
    /// # Why a separate API from `start_with_source`
    ///
    /// `start_with_source` is the dictation hot path: short burst,
    /// one source, one transcript, write to clipboard, done. The
    /// meeting pump needs a different shape — long-running, multiple
    /// sources concurrently, periodic chunk drains — and trying to
    /// layer it on top of the singleton API would force every
    /// backend to track per-source state internally. The handle-
    /// based API moves that state into the handle itself, where it
    /// composes naturally with the pump's
    /// `Vec<Box<dyn AudioSession>>`.
    fn start_session(&self, source: AudioSource) -> Result<Box<dyn AudioSession>> {
        let _ = source;
        Err(anyhow!(
            "start_session is not implemented for this AudioCapture backend; \
             override the method to opt into handle-based parallel capture \
             (used by the meeting pump for mic + system-audio in parallel)"
        ))
    }

    /// Flat list of every source the user can choose from in the
    /// frontend's picker — every mic device plus the system-audio
    /// entry, with capability flags so the picker can disable
    /// not-yet-supported options instead of letting the user pick
    /// them and hit a start-time error.
    ///
    /// Default impl combines [`AudioCapture::list_input_devices`]
    /// with the capability-check methods. Backends that need to
    /// surface platform-specific richness (multiple system-audio
    /// sources, per-app audio capture) override.
    fn list_audio_sources(&self) -> Result<Vec<AudioSourceListing>> {
        let mut listings: Vec<AudioSourceListing> = self
            .list_input_devices()?
            .into_iter()
            .map(|d| AudioSourceListing {
                kind: AudioSourceKind::Microphone,
                id: d.id,
                name: d.name,
                is_default: d.is_default,
                is_supported: true,
            })
            .collect();

        // Always surface a single system-audio entry, even on
        // platforms where the backend doesn't yet support it. The
        // frontend renders it as disabled in that state with a
        // "coming soon" affordance — the user knows the feature
        // exists in concept and where to look for it once it ships
        // (Linux: #106, Windows: #107). Hiding it would be more
        // confusing than showing-disabled because the design memo
        // already mentions it as in-flight work.
        listings.push(AudioSourceListing {
            kind: AudioSourceKind::SystemAudio,
            id: "system".to_owned(),
            name: "System audio".to_owned(),
            is_default: false,
            is_supported: self.supports_system_audio(),
        });

        Ok(listings)
    }

    /// Whether this backend can capture from `source`.
    ///
    /// Used by the IPC layer to populate the frontend's source picker
    /// (the user sees "System audio" disabled with a "coming soon"
    /// affordance on platforms whose backend still returns `false`)
    /// rather than letting the user pick an option that errors at start
    /// time.
    ///
    /// Default returns `true` for `Microphone` (every backend has at
    /// least mic capture, even mocks) and `false` for `SystemAudio`.
    /// Backends override when they implement a new source.
    fn supports_source(&self, source: &AudioSource) -> bool {
        matches!(source, AudioSource::Microphone(_))
    }

    /// Convenience check used by frontend to decide whether to show the
    /// system-audio option at all. Equivalent to
    /// `self.supports_source(&AudioSource::SystemAudio)`.
    fn supports_system_audio(&self) -> bool {
        self.supports_source(&AudioSource::SystemAudio)
    }

    /// Stop capturing and return the accumulated samples.
    ///
    /// Returns an error if no recording is in progress.
    fn stop(&self) -> Result<CapturedAudio>;

    /// True if a recording is currently in progress.
    fn is_recording(&self) -> bool;

    /// Latest RMS level computed by the most recent capture callback,
    /// roughly in `[0.0, 1.0]`. Drives the HUD level meter (#21).
    /// Default returns `0.0` — non-cpal backends and test mocks
    /// inherit a no-op level so the HUD's bar simply stays at idle
    /// for them. Implementations that *do* compute a level should
    /// return `0.0` while not recording so the meter idles cleanly
    /// across start/stop cycles.
    fn current_level(&self) -> f32 {
        0.0
    }
}

// -- Shared helpers --------------------------------------------------------
//
// Used by both the cpal worker (`audio/cpal.rs`) and the macOS
// CoreAudio tap (`audio/core_audio_tap.rs`). Kept here rather than
// duplicated so any future circular-eviction policy change happens once.

/// Push `samples` into a circular capture buffer, evicting the oldest
/// samples from the front when the buffer is at capacity. Batches the
/// entire slice under one lock acquisition so the callback (cpal) or
/// reader thread (CoreAudio tap) holds the lock for the minimum
/// possible time.
///
/// Pre-#827: the buffer was an `rtrb` SPSC ring whose `Producer` would
/// silently drop new samples when full, losing the tail of long
/// dictation sessions. The circular-deque approach keeps the most-recent
/// audio — the semantics the module-level `MAX_BUFFER_FRAMES` comment
/// always claimed ("drops the oldest") but `rtrb` couldn't deliver.
pub(super) fn push_samples_circular(
    buf: &Mutex<VecDeque<f32>>,
    samples: &[f32],
    max_frames: usize,
) {
    let mut guard = buf.lock().unwrap_or_else(|p| p.into_inner());
    for &s in samples {
        if guard.len() >= max_frames {
            guard.pop_front(); // evict oldest to make room for newest
        }
        guard.push_back(s);
    }
}

/// Drain the entire contents of the capture buffer into a fresh `Vec<f32>`.
/// Called from `Cmd::Stop` (one-shot, full drain) and `Cmd::DrainBuffer`
/// (per-tick drain on the meeting pump). Lock contention is negligible —
/// the audio callback holds the same lock only for the duration of pushing
/// a single callback's worth of samples (~milliseconds at most).
///
/// Returns an empty `Vec` when the buffer is empty — the "user pressed
/// Stop almost immediately" path. Never errors: `Mutex` poisoning is
/// treated as a recoverable state by calling `into_inner()`.
pub(super) fn drain_buffer(buf: &Mutex<VecDeque<f32>>) -> Vec<f32> {
    let mut guard = buf.lock().unwrap_or_else(|p| p.into_inner());
    guard.drain(..).collect()
}

/// Compute the RMS level for one audio callback buffer.
///
/// Both the cpal microphone path and the CoreAudio tap reader path
/// use this helper so the level meter values are on the same scale
/// and the HUD waveform doesn't show divergent amplitudes between
/// sources (#822).
pub(super) fn rms_from_sum_sq(sum_sq: f32, n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        (sum_sq / n as f32).sqrt()
    }
}
