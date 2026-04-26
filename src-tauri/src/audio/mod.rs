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
//! audio thread must be polled from the thread that constructed it. We
//! therefore own the stream on a dedicated worker thread and drive it via a
//! command channel. The public API is `Send + Sync` and synchronous from the
//! caller's perspective.
//!
//! **Test seam (PRD §13.5).** Consumers of audio capture depend on the
//! [`AudioCapture`] trait, never on [`CpalAudioCapture`] directly, so unit
//! tests of higher layers can substitute a deterministic mock without
//! pulling in `cpal` or a real device.

mod format;

#[cfg(all(target_os = "macos", feature = "screencapturekit"))]
mod screencapturekit;

pub use format::downmix_to_mono;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamError, SupportedStreamConfig};
use serde::{Deserialize, Serialize};

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
/// platform primitive (cpal input device for `Microphone`; ScreenCaptureKit
/// / WASAPI loopback / PulseAudio monitor for `SystemAudio`) without the
/// caller having to know which path each platform takes.
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
/// platform that hasn't shipped ScreenCaptureKit / WASAPI loopback /
/// PulseAudio monitor support yet shows the option as disabled with a
/// "coming soon" affordance instead of letting the user pick it and
/// hit a runtime error.
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
                "system audio capture is not yet implemented on this platform — see #33 for the per-OS roadmap"
            )),
        }
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
        // (issue #33). Hiding it would be more confusing than
        // showing-disabled because the design memo + roadmap
        // already mention it as in-flight work.
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

// -- cpal backend ----------------------------------------------------------

/// Production audio backend, wrapping `cpal`.
///
/// Owns a worker thread that holds the `cpal::Stream` (which is `!Send` on
/// most platforms). Public methods send commands to the worker over an
/// `mpsc` channel and block on a one-shot reply. The control plane (start,
/// stop, list-devices) is human-paced — the lock and channel hops cost
/// microseconds and never run on the audio callback thread.
pub struct CpalAudioCapture {
    /// Wrapped in a [`Mutex`] because [`mpsc::Sender`] is `Send` but `!Sync`,
    /// and we need `&self` access from multiple threads through the trait.
    cmd_tx: Mutex<mpsc::Sender<Cmd>>,
    /// Cheap, lock-free read of "is something recording right now?".
    /// Updated by the worker, read by the public API.
    is_recording: Arc<AtomicBool>,
    /// Latest RMS level, encoded as `f32::to_bits()`. Written by the cpal
    /// callback at audio-callback rate (~100 Hz at 48 kHz / 480-frame
    /// callbacks), read by the HUD level pump at ~30 Hz.
    ///
    /// `Relaxed` ordering is the load-bearing invariant here, and the
    /// reasoning is worth spelling out so a future change doesn't
    /// "tighten" it without understanding why it's safe today:
    ///
    /// 1. **The level field is independent.** No other shared state
    ///    needs to be observed alongside it — there's no "if level >
    ///    threshold AND state == X" guard, no state-machine that
    ///    depends on level transitions. Each store is meaningful on
    ///    its own.
    /// 2. **A stale read is acceptable.** The HUD pump consumes whatever
    ///    value is in the atomic at tick time and renders one frame
    ///    of the level meter. Showing "the previous frame's level"
    ///    for one ~30 ms tick is invisible to a human.
    /// 3. **No happens-before relationship is needed** with anything else
    ///    in the codebase — Acquire/Release would only matter if a
    ///    reader needed to observe other writes that happened on the
    ///    callback thread before this store, and no such other writes
    ///    exist on the path.
    ///
    /// If level ever becomes load-bearing for a state machine
    /// (e.g. "stop dictation if RMS < X for 2s" voice-activity
    /// detection), upgrade to Acquire/Release pairs at that point —
    /// the new dependency would be the new ordering requirement.
    /// Cleared back to `0.0` on stop so the meter idles cleanly.
    level: Arc<AtomicU32>,
    /// Joined on drop. Wrapped in [`Option`] so [`Drop`] can take ownership.
    worker: Option<JoinHandle<()>>,
    /// Active ScreenCaptureKit session for system-audio capture (#105).
    /// Lives outside the cpal worker because SCK delivers samples on
    /// its own libdispatch queue — there is no Stream object to babysit
    /// from a !Send-bound thread. Mutex<Option<...>> mirrors the
    /// "either nothing, or one in-flight" shape of the cpal session.
    #[cfg(all(target_os = "macos", feature = "screencapturekit"))]
    sck_session: Mutex<Option<screencapturekit::ScreenCaptureKitSession>>,
}

/// Commands sent from the public API into the audio worker thread.
enum Cmd {
    ListDevices(mpsc::Sender<Result<Vec<AudioDevice>>>),
    Start {
        device_id: Option<String>,
        reply: mpsc::Sender<Result<()>>,
    },
    Stop(mpsc::Sender<Result<CapturedAudio>>),
    Shutdown,
}

impl CpalAudioCapture {
    /// Spawn the audio worker thread and return a handle.
    ///
    /// Allocating the thread up-front (rather than on first `start`) keeps
    /// the latency between hotkey-press and first sample bounded, since the
    /// thread is already alive and blocked on `recv`.
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let is_recording = Arc::new(AtomicBool::new(false));
        let level = Arc::new(AtomicU32::new(0_f32.to_bits()));
        let worker_flag = Arc::clone(&is_recording);
        let worker_level = Arc::clone(&level);

        let worker = thread::Builder::new()
            .name("hush-audio".into())
            .spawn(move || worker_loop(cmd_rx, worker_flag, worker_level))
            .expect("failed to spawn audio worker thread");

        Self {
            cmd_tx: Mutex::new(cmd_tx),
            is_recording,
            level,
            worker: Some(worker),
            #[cfg(all(target_os = "macos", feature = "screencapturekit"))]
            sck_session: Mutex::new(None),
        }
    }

    /// Send a command and block on its reply. Centralised so every public
    /// method gets the same lock / channel-error handling.
    fn dispatch<T>(&self, make_cmd: impl FnOnce(mpsc::Sender<Result<T>>) -> Cmd) -> Result<T> {
        let (tx, rx) = mpsc::channel::<Result<T>>();
        let cmd = make_cmd(tx);
        self.cmd_tx
            .lock()
            .map_err(|_| anyhow!("audio command channel lock poisoned"))?
            .send(cmd)
            .map_err(|_| anyhow!("audio worker thread has exited"))?;
        rx.recv()
            .map_err(|_| anyhow!("audio worker dropped reply channel"))?
    }
}

impl Default for CpalAudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CpalAudioCapture {
    fn drop(&mut self) {
        // Best-effort shutdown. If the channel is already closed the worker
        // has exited on its own — nothing to do. We deliberately do not
        // unwrap the join: a panic in the worker should not poison the
        // application shutdown path.
        if let Ok(tx) = self.cmd_tx.lock() {
            let _ = tx.send(Cmd::Shutdown);
        }
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl AudioCapture for CpalAudioCapture {
    fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
        self.dispatch(Cmd::ListDevices)
    }

    fn start(&self, device_id: Option<&str>) -> Result<()> {
        // Refuse to start a mic capture while an SCK system-audio
        // session is in flight. Only one capture path at a time —
        // is_recording is the cross-path source of truth, but checking
        // the SCK slot directly gives a clearer error message.
        #[cfg(all(target_os = "macos", feature = "screencapturekit"))]
        {
            let guard = self
                .sck_session
                .lock()
                .map_err(|_| anyhow!("sck session lock poisoned"))?;
            if guard.is_some() {
                return Err(anyhow!(
                    "system-audio capture already in progress; stop it first"
                ));
            }
        }
        let device_id = device_id.map(str::to_owned);
        self.dispatch(|reply| Cmd::Start { device_id, reply })
    }

    fn start_with_source(&self, source: AudioSource) -> Result<()> {
        match source {
            AudioSource::Microphone(device_id) => self.start(device_id.as_deref()),
            #[cfg(all(target_os = "macos", feature = "screencapturekit"))]
            AudioSource::SystemAudio => {
                if self.is_recording() {
                    return Err(anyhow!("recording already in progress"));
                }
                let mut guard = self
                    .sck_session
                    .lock()
                    .map_err(|_| anyhow!("sck session lock poisoned"))?;
                if guard.is_some() {
                    return Err(anyhow!(
                        "system-audio capture already in progress"
                    ));
                }
                let session = screencapturekit::ScreenCaptureKitSession::start(
                    Arc::clone(&self.level),
                )?;
                *guard = Some(session);
                self.is_recording.store(true, Ordering::Release);
                Ok(())
            }
            #[cfg(not(all(target_os = "macos", feature = "screencapturekit")))]
            AudioSource::SystemAudio => Err(anyhow!(
                "system audio capture is not yet implemented on this platform — see #33 for the per-OS roadmap"
            )),
        }
    }

    fn supports_source(&self, source: &AudioSource) -> bool {
        match source {
            AudioSource::Microphone(_) => true,
            AudioSource::SystemAudio => {
                cfg!(all(target_os = "macos", feature = "screencapturekit"))
            }
        }
    }

    fn stop(&self) -> Result<CapturedAudio> {
        // SCK path first: if a system-audio session is active, drain
        // it and skip the cpal worker round-trip entirely. Order
        // matters — we must clear the SCK slot before dropping the
        // is_recording flag, so a concurrent start() call can't see
        // a "not recording" state while the SCK session is still
        // mid-stop.
        #[cfg(all(target_os = "macos", feature = "screencapturekit"))]
        {
            let mut guard = self
                .sck_session
                .lock()
                .map_err(|_| anyhow!("sck session lock poisoned"))?;
            if let Some(session) = guard.take() {
                let format = session.format();
                let samples = session.stop()?;
                self.is_recording.store(false, Ordering::Release);
                self.level.store(0_f32.to_bits(), Ordering::Relaxed);
                return Ok(CapturedAudio { samples, format });
            }
        }
        self.dispatch(Cmd::Stop)
    }

    fn is_recording(&self) -> bool {
        // Acquire ordering so a `true` reading happens-after the worker's
        // store, ensuring the corresponding stream is actually live.
        self.is_recording.load(Ordering::Acquire)
    }

    fn current_level(&self) -> f32 {
        // Gate on `is_recording`: the level field is only cleared on stop,
        // but `is_recording` flips at the same point. Reading the flag
        // first lets a future change to the meter (e.g. fade-out instead
        // of hard-zero) live entirely in the consumer without changing
        // the storage discipline here.
        if self.is_recording() {
            f32::from_bits(self.level.load(Ordering::Relaxed))
        } else {
            0.0
        }
    }
}

/// State held by the worker thread for the duration of a single recording.
struct Session {
    /// Kept alive for the duration of capture. Dropping it stops the stream.
    /// We do not need to read from it after construction; the underlying
    /// callback writes directly into [`Self::buffer`].
    stream: Stream,
    format: CaptureFormat,
    /// Shared with the cpal callback. The callback locks briefly, the worker
    /// drains it on stop. See the comment in [`append_samples`] for why a
    /// short-held mutex is acceptable here.
    buffer: Arc<Mutex<Vec<f32>>>,
}

fn worker_loop(cmd_rx: mpsc::Receiver<Cmd>, is_recording: Arc<AtomicBool>, level: Arc<AtomicU32>) {
    // The cpal host is created on the worker thread to avoid any chance of
    // cross-thread state: some backends keep thread-locals pointing back at
    // the host that constructed them.
    let host = cpal::default_host();
    let mut session: Option<Session> = None;

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Cmd::ListDevices(reply) => {
                let _ = reply.send(list_devices(&host));
            }
            Cmd::Start { device_id, reply } => {
                if session.is_some() {
                    let _ = reply.send(Err(anyhow!("recording already in progress")));
                    continue;
                }
                match start_session(&host, device_id.as_deref(), Arc::clone(&level)) {
                    Ok(s) => {
                        // Release ordering pairs with Acquire in `is_recording()`.
                        is_recording.store(true, Ordering::Release);
                        session = Some(s);
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Cmd::Stop(reply) => {
                let result = match session.take() {
                    Some(s) => stop_session(s),
                    None => Err(anyhow!("no recording in progress")),
                };
                // Always clear the flag, even on error: a failed stop should
                // not leave us stuck pretending we're still recording.
                is_recording.store(false, Ordering::Release);
                // Reset the level so the HUD's meter idles cleanly between
                // sessions instead of holding the last RMS reading until
                // the next start.
                level.store(0_f32.to_bits(), Ordering::Relaxed);
                let _ = reply.send(result);
            }
            Cmd::Shutdown => break,
        }
    }
}

fn list_devices(host: &cpal::Host) -> Result<Vec<AudioDevice>> {
    // Capture the default device's name once so we can flag it in the list.
    // We use the name as the comparison key because that is also our public
    // identifier; see the doc comment on [`AudioDevice::id`].
    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    let devices = host
        .input_devices()
        .context("failed to enumerate input devices")?;
    let mut out = Vec::new();
    for device in devices {
        // A device that fails to report its name is unusable from the UI; we
        // skip it rather than surface a synthetic ID we cannot round-trip.
        let Ok(name) = device.name() else { continue };
        let is_default = default_name.as_deref() == Some(name.as_str());
        out.push(AudioDevice {
            id: name.clone(),
            name,
            is_default,
        });
    }
    Ok(out)
}

fn start_session(
    host: &cpal::Host,
    device_id: Option<&str>,
    level: Arc<AtomicU32>,
) -> Result<Session> {
    let device = match device_id {
        Some(id) => host
            .input_devices()
            .context("enumerate input devices")?
            .find(|d| d.name().map(|n| n == id).unwrap_or(false))
            .ok_or_else(|| anyhow!("input device '{id}' not found"))?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device available"))?,
    };

    // `default_input_config` returns the format the OS thinks the device is
    // happiest at. Picking it (rather than negotiating a 16 kHz mono config
    // ourselves) maximises the chance the stream actually opens. See the
    // module-level "Capture-at-native-format" note.
    let supported = device
        .default_input_config()
        .context("query default input config")?;

    let format = CaptureFormat {
        sample_rate: supported.sample_rate().0,
        channels: supported.channels(),
    };

    let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let stream = build_input_stream(&device, &supported, Arc::clone(&buffer), level)?;
    stream.play().context("start input stream")?;

    Ok(Session {
        stream,
        format,
        buffer,
    })
}

fn stop_session(session: Session) -> Result<CapturedAudio> {
    // Pause first so no further callbacks can land while we move the buffer
    // out of the Arc. Dropping the stream alone is technically sufficient on
    // every backend we currently target, but `pause()` makes the intent
    // obvious and is cheap on the human-paced control plane.
    let _ = session.stream.pause();
    drop(session.stream);

    let samples = drain_buffer(&session.buffer)?;

    Ok(CapturedAudio {
        samples,
        format: session.format,
    })
}

fn build_input_stream(
    device: &cpal::Device,
    supported: &SupportedStreamConfig,
    buffer: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
) -> Result<Stream> {
    let config: cpal::StreamConfig = supported.config();

    // cpal hands samples back in the device's native format. We convert to
    // f32 inside the callback so the rest of the pipeline only ever deals
    // with one type. The integer paths are exhaustive over what cpal 0.15
    // exposes today; an unknown variant is treated as a hard error rather
    // than a silent fallback so we notice when cpal adds a new format.
    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            let buf = Arc::clone(&buffer);
            let lvl = Arc::clone(&level);
            device.build_input_stream(
                &config,
                move |data: &[f32], _| append_samples(&buf, data, |s| *s, &lvl),
                log_stream_error,
                None,
            )
        }
        SampleFormat::I16 => {
            let buf = Arc::clone(&buffer);
            let lvl = Arc::clone(&level);
            device.build_input_stream(
                &config,
                move |data: &[i16], _| append_samples(&buf, data, i16_to_f32, &lvl),
                log_stream_error,
                None,
            )
        }
        SampleFormat::U16 => {
            let buf = Arc::clone(&buffer);
            let lvl = Arc::clone(&level);
            device.build_input_stream(
                &config,
                move |data: &[u16], _| append_samples(&buf, data, u16_to_f32, &lvl),
                log_stream_error,
                None,
            )
        }
        other => return Err(anyhow!("unsupported cpal sample format: {other:?}")),
    }
    .context("build cpal input stream")?;

    Ok(stream)
}

/// Append a callback's worth of samples to the shared buffer and publish
/// the per-callback RMS to the level meter.
///
/// The audio callback runs on a real-time-ish thread; it must not block for
/// long. Locking the mutex is acceptable because the only other lock holder
/// is the worker thread, and only on stop, by which point callbacks have
/// already been paused. RMS is computed in the same single pass that
/// converts and pushes samples — no extra allocation, no second iteration.
/// If profiling later shows contention we can swap in an SPSC ring buffer
/// (e.g. `rtrb`) without changing the public API.
fn append_samples<T: Copy>(
    buffer: &Mutex<Vec<f32>>,
    data: &[T],
    convert: impl Fn(&T) -> f32,
    level: &AtomicU32,
) {
    // A poisoned mutex here means another thread panicked while holding it.
    // Recovering the inner buffer is preferable to panicking the audio
    // thread, which on some backends would tear down the whole process.
    let mut buf = match buffer.lock() {
        Ok(b) => b,
        Err(poisoned) => poisoned.into_inner(),
    };
    buf.reserve(data.len());
    let mut sum_sq = 0.0_f32;
    for sample in data {
        let f = convert(sample);
        sum_sq += f * f;
        buf.push(f);
    }
    if !data.is_empty() {
        let rms = rms_from_sum_sq(sum_sq, data.len());
        // `Relaxed`: each callback writes the latest reading; the HUD
        // pump reads independently and can tolerate a stale value for one
        // 33 ms tick. There is no other field that needs to be observed
        // alongside the level.
        level.store(rms.to_bits(), Ordering::Relaxed);
    }
}

/// Take the captured samples out of the shared buffer, leaving the
/// mutex's inner `Vec` empty. Pulled out as its own free function so
/// the regression that surfaced in PR #77 — `Arc::try_unwrap` fails
/// when cpal's stream cleanup is asynchronous and the callback's
/// Arc clone outlives `drop(stream)` — has unit-test coverage. The
/// real cpal stream is impossible to construct in a unit test (it
/// needs a real audio device), but the load-bearing piece is just
/// "can we get the samples out when other Arc clones are alive?",
/// which `lock + mem::take` answers correctly regardless of clone
/// count.
///
/// Locking is correct under all the timings we care about:
/// - Uncontended (the common case post-`stream.pause()`): immediate.
/// - Contended by a final in-flight callback: the lock waits the
///   few-ms append to finish, then we take.
/// - Multiple Arc clones outstanding: irrelevant — the lock doesn't
///   care about Arc strong count, only mutex ownership.
fn drain_buffer(buffer: &Arc<Mutex<Vec<f32>>>) -> Result<Vec<f32>> {
    let mut guard = buffer
        .lock()
        .map_err(|_| anyhow!("audio buffer mutex poisoned"))?;
    Ok(std::mem::take(&mut *guard))
}

/// RMS from a pre-computed sum-of-squares plus the sample count.
/// Pulled out as a free function so the level-meter math can be
/// unit-tested without spinning up a real cpal stream — the callback
/// itself stays a one-line call into this helper.
fn rms_from_sum_sq(sum_sq: f32, n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        (sum_sq / n as f32).sqrt()
    }
}

fn log_stream_error(err: StreamError) {
    tracing::error!(error = ?err, "audio input stream error");
}

fn i16_to_f32(s: &i16) -> f32 {
    // Symmetric scaling: divide by `i16::MAX` so a full-scale negative sample
    // (`i16::MIN` = -32768) maps to slightly past -1.0, which we leave as-is
    // rather than clamping. Whisper handles values just outside [-1, 1] fine
    // and clamping would introduce a one-sample DC bias.
    *s as f32 / i16::MAX as f32
}

fn u16_to_f32(s: &u16) -> f32 {
    // cpal models U16 as unsigned-PCM with 0x8000 = silence. Shift to
    // signed-centered, then scale into [-1.0, 1.0].
    (*s as f32 - 32768.0) / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_conversion_endpoints() {
        // Spot-check the integer-to-f32 conversions at their extremes; this
        // is the kind of off-by-one that is silent until a recording sounds
        // wrong, so it is worth pinning down.
        assert!((i16_to_f32(&i16::MAX) - 1.0).abs() < 1e-6);
        assert!(i16_to_f32(&0).abs() < 1e-6);
        assert!(i16_to_f32(&i16::MIN) < -0.999);
    }

    #[test]
    fn u16_conversion_endpoints() {
        assert!((u16_to_f32(&u16::MAX) - 0.99997).abs() < 1e-3);
        assert!(u16_to_f32(&32768).abs() < 1e-6);
        assert!((u16_to_f32(&0) + 1.0).abs() < 1e-6);
    }

    /// Compile-time check that the trait is object-safe. If this ever fails
    /// to compile, a higher layer cannot store an `Arc<dyn AudioCapture>`,
    /// which is how the IPC layer plugs in either the cpal backend or a
    /// test mock.
    #[test]
    fn audio_capture_trait_is_object_safe() {
        fn _assert_object_safe(_: &dyn AudioCapture) {}
    }

    #[test]
    fn rms_of_silence_is_zero() {
        // All-zero buffer must produce a zero level so the HUD's
        // meter idles cleanly while the user is between words.
        let n = 480; // typical 10 ms callback at 48 kHz mono
        let sum_sq = 0.0;
        assert!(rms_from_sum_sq(sum_sq, n).abs() < 1e-7);
    }

    #[test]
    fn rms_of_full_scale_signal_is_one() {
        // A buffer of all-±1 samples has sum-of-squares == n, so RMS
        // is exactly 1.0. Pinned because the HUD's bar boost (×4) is
        // calibrated against this scale — if the math drifts the
        // meter would saturate at the wrong amplitude.
        let n = 480;
        let sum_sq = n as f32; // each sample squared is 1.0
        assert!((rms_from_sum_sq(sum_sq, n) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rms_handles_empty_buffer_without_panicking() {
        // An empty data slice on a callback (rare, but cpal does not
        // forbid it) must not divide by zero.
        assert_eq!(rms_from_sum_sq(0.0, 0), 0.0);
    }

    #[test]
    fn default_current_level_is_zero_for_mocks() {
        // Default trait method backs every non-cpal implementation
        // (test mocks, future Parakeet adapter); the HUD treats 0.0
        // as idle, so this is the value mocks are expected to surface.
        struct Stub;
        impl AudioCapture for Stub {
            fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
                Ok(vec![])
            }
            fn start(&self, _: Option<&str>) -> Result<()> {
                Ok(())
            }
            fn stop(&self) -> Result<CapturedAudio> {
                Ok(CapturedAudio {
                    samples: vec![],
                    format: CaptureFormat {
                        sample_rate: 16_000,
                        channels: 1,
                    },
                })
            }
            fn is_recording(&self) -> bool {
                false
            }
        }
        assert_eq!(Stub.current_level(), 0.0);
    }

    // -- AudioSource + start_with_source default impl -------------------
    //
    // The default `start_with_source` impl dispatches `Microphone` to
    // `start` and errors on `SystemAudio`. These tests pin both arms so
    // a future trait change that "tightens" the default doesn't silently
    // break a backend that's relying on it.

    /// Mock that records the device id passed to `start` so we can
    /// assert the default `start_with_source` actually forwards it.
    struct RecordingMic {
        last_device_id: std::sync::Mutex<Option<Option<String>>>,
    }
    impl RecordingMic {
        fn new() -> Self {
            Self {
                last_device_id: std::sync::Mutex::new(None),
            }
        }
    }
    impl AudioCapture for RecordingMic {
        fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, device_id: Option<&str>) -> Result<()> {
            *self.last_device_id.lock().unwrap() = Some(device_id.map(str::to_owned));
            Ok(())
        }
        fn stop(&self) -> Result<CapturedAudio> {
            Ok(CapturedAudio {
                samples: vec![],
                format: CaptureFormat {
                    sample_rate: 16_000,
                    channels: 1,
                },
            })
        }
        fn is_recording(&self) -> bool {
            false
        }
    }

    #[test]
    fn start_with_source_microphone_default_forwards_to_start_with_none() {
        let mic = RecordingMic::new();
        mic.start_with_source(AudioSource::default_microphone())
            .unwrap();
        assert_eq!(*mic.last_device_id.lock().unwrap(), Some(None));
    }

    #[test]
    fn start_with_source_microphone_with_id_forwards_the_id() {
        // Pins the unwrap path: the wrapped `Option<String>` is unpacked
        // back to `Option<&str>` for the legacy `start` signature.
        // A future change that drops the inner unwrap would silently
        // pass `Some("None")` or similar.
        let mic = RecordingMic::new();
        mic.start_with_source(AudioSource::Microphone(Some("usb-mic".to_owned())))
            .unwrap();
        assert_eq!(
            *mic.last_device_id.lock().unwrap(),
            Some(Some("usb-mic".to_owned()))
        );
    }

    #[test]
    fn start_with_source_system_audio_default_returns_error_naming_the_gap() {
        // The default impl must surface a clear error rather than
        // silently falling back to mic — that would let a frontend
        // pick "System audio" and unknowingly record the wrong source.
        let mic = RecordingMic::new();
        let err = mic
            .start_with_source(AudioSource::SystemAudio)
            .expect_err("default impl errors for SystemAudio");
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("system audio"),
            "error should name what's missing; got: {msg}"
        );
        // And critically: the legacy `start` was NOT called.
        assert_eq!(*mic.last_device_id.lock().unwrap(), None);
    }

    #[test]
    fn supports_source_default_is_microphone_only() {
        // Default impl says yes to every Microphone source, no to
        // SystemAudio. Pinned so a future trait change that flips a
        // default to "everything supported" can't accidentally make
        // the frontend's source picker offer SystemAudio on a backend
        // that hasn't actually shipped it.
        let mic = RecordingMic::new();
        assert!(mic.supports_source(&AudioSource::default_microphone()));
        assert!(mic.supports_source(&AudioSource::Microphone(Some("any".to_owned()))));
        assert!(!mic.supports_source(&AudioSource::SystemAudio));
        assert!(!mic.supports_system_audio());
    }

    #[test]
    fn list_audio_sources_includes_each_input_device_plus_system_audio_entry() {
        struct ThreeMics;
        impl AudioCapture for ThreeMics {
            fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
                Ok(vec![
                    AudioDevice {
                        id: "Built-in".into(),
                        name: "Built-in".into(),
                        is_default: true,
                    },
                    AudioDevice {
                        id: "USB-C".into(),
                        name: "USB-C".into(),
                        is_default: false,
                    },
                    AudioDevice {
                        id: "Bluetooth".into(),
                        name: "Bluetooth".into(),
                        is_default: false,
                    },
                ])
            }
            fn start(&self, _: Option<&str>) -> Result<()> {
                Ok(())
            }
            fn stop(&self) -> Result<CapturedAudio> {
                Ok(CapturedAudio {
                    samples: vec![],
                    format: CaptureFormat {
                        sample_rate: 16_000,
                        channels: 1,
                    },
                })
            }
            fn is_recording(&self) -> bool {
                false
            }
        }

        let listings = ThreeMics.list_audio_sources().unwrap();
        // Three mics + one system-audio entry = four listings.
        assert_eq!(listings.len(), 4);

        let mics: Vec<_> = listings
            .iter()
            .filter(|l| l.kind == AudioSourceKind::Microphone)
            .collect();
        assert_eq!(mics.len(), 3);
        assert!(mics.iter().all(|l| l.is_supported));
        // is_default copies through from AudioDevice.
        assert_eq!(
            mics.iter().filter(|l| l.is_default).count(),
            1,
            "exactly one mic should be the default"
        );

        let system: Vec<_> = listings
            .iter()
            .filter(|l| l.kind == AudioSourceKind::SystemAudio)
            .collect();
        assert_eq!(system.len(), 1, "exactly one system-audio entry");
        // Default `supports_system_audio` returns false; the listing
        // mirrors that so the frontend renders it disabled.
        assert!(!system[0].is_supported);
        assert_eq!(system[0].id, "system");
        // System-audio listing is never marked is_default — there's
        // exactly one, "default" doesn't apply, and the frontend
        // shouldn't auto-pick it on first run.
        assert!(!system[0].is_default);
    }

    #[test]
    fn list_audio_sources_marks_system_audio_supported_when_backend_overrides() {
        // Pin the override path: a backend that ships system-audio
        // returns true from supports_system_audio() and therefore
        // surfaces it as is_supported=true to the frontend, which
        // would render it as a selectable option rather than disabled.
        struct WithSystemAudio;
        impl AudioCapture for WithSystemAudio {
            fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
                Ok(vec![])
            }
            fn start(&self, _: Option<&str>) -> Result<()> {
                Ok(())
            }
            fn stop(&self) -> Result<CapturedAudio> {
                Ok(CapturedAudio {
                    samples: vec![],
                    format: CaptureFormat {
                        sample_rate: 16_000,
                        channels: 1,
                    },
                })
            }
            fn is_recording(&self) -> bool {
                false
            }
            fn supports_source(&self, source: &AudioSource) -> bool {
                matches!(
                    source,
                    AudioSource::Microphone(_) | AudioSource::SystemAudio
                )
            }
        }
        let listings = WithSystemAudio.list_audio_sources().unwrap();
        let sys = listings
            .iter()
            .find(|l| l.kind == AudioSourceKind::SystemAudio)
            .unwrap();
        assert!(sys.is_supported);
    }

    #[test]
    fn audio_source_listing_serde_uses_camel_case_for_frontend_consumption() {
        // The frontend's TypeScript definition uses isDefault,
        // isSupported, deviceId-style camelCase. Pin the wire shape so
        // a future Rust-side rename fails loud rather than silently
        // breaking the picker.
        let listing = AudioSourceListing {
            kind: AudioSourceKind::Microphone,
            id: "Built-in".into(),
            name: "Built-in".into(),
            is_default: true,
            is_supported: true,
        };
        let json = serde_json::to_string(&listing).unwrap();
        assert!(json.contains(r#""isDefault":true"#), "got: {json}");
        assert!(json.contains(r#""isSupported":true"#), "got: {json}");
        assert!(json.contains(r#""kind":"microphone""#), "got: {json}");

        let sys_listing = AudioSourceListing {
            kind: AudioSourceKind::SystemAudio,
            id: "system".into(),
            name: "System audio".into(),
            is_default: false,
            is_supported: false,
        };
        let sys_json = serde_json::to_string(&sys_listing).unwrap();
        assert!(
            sys_json.contains(r#""kind":"system-audio""#),
            "got: {sys_json}"
        );
    }

    #[test]
    fn audio_source_serde_round_trips() {
        // The IPC boundary serialises this enum; round-tripping pins
        // the wire shape (`{ kind: "microphone" | "system-audio",
        // deviceId: ... }`) so the frontend's TypeScript discriminated
        // union stays in lock-step.
        let mic = AudioSource::Microphone(Some("usb-mic".to_owned()));
        let mic_default = AudioSource::default_microphone();
        let sys = AudioSource::SystemAudio;

        let mic_json = serde_json::to_string(&mic).unwrap();
        let mic_default_json = serde_json::to_string(&mic_default).unwrap();
        let sys_json = serde_json::to_string(&sys).unwrap();

        assert!(
            mic_json.contains(r#""kind":"microphone""#),
            "got: {mic_json}"
        );
        assert!(
            mic_json.contains(r#""deviceId":"usb-mic""#),
            "got: {mic_json}"
        );
        assert!(
            mic_default_json.contains(r#""kind":"microphone""#),
            "got: {mic_default_json}"
        );
        assert!(
            sys_json.contains(r#""kind":"system-audio""#),
            "got: {sys_json}"
        );

        assert_eq!(serde_json::from_str::<AudioSource>(&mic_json).unwrap(), mic);
        assert_eq!(
            serde_json::from_str::<AudioSource>(&sys_json).unwrap(),
            AudioSource::SystemAudio
        );
    }

    // -- drain_buffer regression tests -----------------------------------
    //
    // PR #77 fixed a real bug surfaced in hands-on testing: stop_session
    // used Arc::try_unwrap to take the buffer Vec, requiring sole Arc
    // ownership. On macOS 26 (and apparently other platforms), cpal's
    // stream cleanup is asynchronous — the callback closure's Arc clone
    // can outlive drop(session.stream) by a beat — so try_unwrap
    // sporadically failed on perfectly-good recordings with "audio buffer
    // still shared after stream drop." The fix swapped to lock + mem::take.
    //
    // These tests pin the new behaviour: drain_buffer must succeed
    // regardless of how many Arc clones are still alive at call time.
    // The unit-test coverage matters because the cpal stream itself is
    // impossible to construct without a real audio device, so the
    // race-prone bit lives entirely in the buffer-take path now. A
    // future regression that puts try_unwrap (or any
    // strong-count-sensitive operation) back fails these tests.

    #[test]
    fn drain_buffer_takes_contents_when_arc_is_unique() {
        let buffer = Arc::new(Mutex::new(vec![1.0_f32, 2.0, 3.0]));
        let samples = drain_buffer(&buffer).expect("drain succeeds with unique Arc");
        assert_eq!(samples, vec![1.0_f32, 2.0, 3.0]);
        // Mutex's interior is now an empty Vec; the Arc itself is still
        // valid for any other holders that haven't dropped yet.
        assert!(buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn drain_buffer_succeeds_with_outstanding_arc_clones() {
        // Simulates the cpal-cleanup-still-in-flight case that broke
        // try_unwrap. Multiple Arc clones outstanding; drain must still
        // produce the recording's samples, not error.
        let buffer = Arc::new(Mutex::new(vec![1.0_f32, 2.0, 3.0]));
        let cpal_closure_clone = Arc::clone(&buffer);
        let another_clone = Arc::clone(&buffer);

        let samples = drain_buffer(&buffer).expect("drain succeeds despite extra Arc clones");
        assert_eq!(samples, vec![1.0_f32, 2.0, 3.0]);

        // The other clones still see the (now-empty) buffer through their
        // shared Arc — proving lock-and-take did not require sole Arc
        // ownership. These would have errored under the pre-PR-#77
        // try_unwrap implementation.
        assert!(cpal_closure_clone.lock().unwrap().is_empty());
        assert!(another_clone.lock().unwrap().is_empty());
    }

    #[test]
    fn drain_buffer_returns_empty_for_empty_buffer() {
        // The "user pressed Stop almost immediately" path. Drain returns
        // an empty Vec rather than erroring; the transcription stack will
        // surface a more useful error downstream if the silence matters.
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let samples = drain_buffer(&buffer).expect("drain succeeds on empty buffer");
        assert!(samples.is_empty());
    }
}
