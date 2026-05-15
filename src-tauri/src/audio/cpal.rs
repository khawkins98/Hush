//! cpal-backed microphone capture (the prod implementation of [`super::AudioCapture`]).
//!
//! Owns a single dedicated worker thread that holds the `cpal::Stream`
//! object — `Stream` is `!Send` on most platforms, so the worker model
//! avoids cross-thread issues by construction. Callers interact via the
//! `Cmd` channel; the public API is [`CpalAudioCapture`].
//!
//! The macOS-specific [`super::core_audio_tap::CoreAudioTapSession`] is
//! plugged in via the `cat_session` slot for the "legacy singleton"
//! `start_with_source` path and via direct construction in the
//! handle-based `start_session` path. See the trait docs in
//! [`super`] for the dual-API rationale.
//!
//! Extracted from `audio/mod.rs` under #597 (item 5) so a future Linux
//! (#106) / Windows (#107) audio backend can land as a peer file
//! rather than doubling the size of `mod.rs`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamError, SupportedStreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};

#[cfg(target_os = "macos")]
use super::core_audio_tap;
use super::{
    drain_consumer, log_overflow_if_set, AudioCapture, AudioDevice, AudioSession, AudioSource,
    CaptureFormat, CapturedAudio, DeviceLost, MAX_BUFFER_FRAMES,
};

/// `legacy_source` sentinel values (stored in `AtomicU8`).
const LEGACY_IDLE: u8 = 0;
const LEGACY_MIC: u8 = 1;
const LEGACY_SYS: u8 = 2;

pub struct CpalAudioCapture {
    /// Wrapped in a [`Mutex`] because [`mpsc::Sender`] is `Send` but `!Sync`,
    /// and we need `&self` access from multiple threads through the trait.
    cmd_tx: Mutex<mpsc::Sender<Cmd>>,
    /// Reference count of active capture sessions. The legacy
    /// singleton path (cpal mic via worker, macOS system audio via
    /// `cat_session` slot) plus the handle-based
    /// [`AudioCapture::start_session`] paths all increment this on
    /// start and decrement on stop.
    ///
    /// Modelled as a count rather than a bool so parallel mic +
    /// system-audio capture (the meeting pump's canonical config)
    /// reports `is_recording() == true` while either is in flight,
    /// without the two paths racing on a shared bool. The legacy
    /// hot path still treats it as a binary "any capture active",
    /// which works because the count is monotonically positive.
    active_sessions: Arc<AtomicU32>,
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
    /// State machine for the legacy singleton `start_with_source` / `stop`
    /// path. Prevents a race where mic + system-audio can both be started
    /// via separate `start_with_source` calls, leaving `stop()` only able
    /// to clear one of them (#903).
    ///
    /// Values: `LEGACY_IDLE` | `LEGACY_MIC` | `LEGACY_SYS`.
    /// CAS-guarded in `start_with_source` so a second call while non-idle
    /// returns an error. `stop()` swaps back to `LEGACY_IDLE` and dispatches
    /// to exactly the backend that was started.
    legacy_source: AtomicU8,
    /// Active CoreAudio tap session for system-audio capture (#600).
    /// Lives outside the cpal worker because the tap delivers samples on
    /// its own reader thread — there is no Stream object to babysit
    /// from a !Send-bound thread. Mutex<Option<...>> mirrors the
    /// "either nothing, or one in-flight" shape of the cpal session.
    #[cfg(target_os = "macos")]
    cat_session: Mutex<Option<core_audio_tap::CoreAudioTapSession>>,
    /// Path to the Tauri resource directory. Used to locate
    /// `resources/hush-audio-tap-capture` when starting a system-audio
    /// session via the CoreAudio tap path.
    #[cfg(target_os = "macos")]
    resource_dir: std::path::PathBuf,
}

/// Commands sent from the public API into the audio worker thread.
enum Cmd {
    ListDevices(mpsc::Sender<Result<Vec<AudioDevice>>>),
    Start {
        device_id: Option<String>,
        reply: mpsc::Sender<Result<()>>,
    },
    Stop(mpsc::Sender<Result<CapturedAudio>>),
    /// Take everything currently in the active session's buffer
    /// without stopping the session — the streaming pump's tick.
    /// Reply carries `(samples, format)` if a session is active or
    /// an error if not. Empty `Vec` is a normal reply (tick fired
    /// before the audio callback wrote anything new); the caller
    /// just appends nothing and waits for the next tick.
    DrainBuffer(mpsc::Sender<Result<(Vec<f32>, CaptureFormat)>>),
    Shutdown,
}

#[allow(clippy::new_without_default)]
impl CpalAudioCapture {
    /// Spawn the audio worker thread and return a handle.
    ///
    /// Allocating the thread up-front (rather than on first `start`) keeps
    /// the latency between hotkey-press and first sample bounded, since the
    /// thread is already alive and blocked on `recv`.
    pub fn new(#[cfg(target_os = "macos")] resource_dir: std::path::PathBuf) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let active_sessions = Arc::new(AtomicU32::new(0));
        let level = Arc::new(AtomicU32::new(0_f32.to_bits()));
        let worker_flag = Arc::clone(&active_sessions);
        let worker_level = Arc::clone(&level);

        let worker = thread::Builder::new()
            .name("hush-audio".into())
            .spawn(move || worker_loop(cmd_rx, worker_flag, worker_level))
            .expect("failed to spawn audio worker thread");

        Self {
            cmd_tx: Mutex::new(cmd_tx),
            active_sessions,
            level,
            worker: Some(worker),
            legacy_source: AtomicU8::new(LEGACY_IDLE),
            #[cfg(target_os = "macos")]
            cat_session: Mutex::new(None),
            #[cfg(target_os = "macos")]
            resource_dir,
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
        // The cpal worker rejects a second mic Start while its
        // singleton mic Session is occupied. Mic + SCK in parallel
        // is fine — different backends, different singletons — so
        // we no longer block on the SCK slot here. The pump (PR2)
        // exercises this combination as its canonical config.
        let device_id = device_id.map(str::to_owned);
        self.dispatch(|reply| Cmd::Start { device_id, reply })
    }

    fn start_with_source(&self, source: AudioSource) -> Result<()> {
        match source {
            AudioSource::Microphone(device_id) => {
                // CAS IDLE → MIC; reject if another legacy session is active (#903).
                self.legacy_source
                    .compare_exchange(
                        LEGACY_IDLE,
                        LEGACY_MIC,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .map_err(|_| anyhow!("a legacy audio session is already in progress"))?;
                match self.start(device_id.as_deref()) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        self.legacy_source.store(LEGACY_IDLE, Ordering::Release);
                        Err(e)
                    }
                }
            }
            #[cfg(target_os = "macos")]
            AudioSource::SystemAudio => {
                // CAS IDLE → SYS; reject if another legacy session is active (#903).
                self.legacy_source
                    .compare_exchange(
                        LEGACY_IDLE,
                        LEGACY_SYS,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .map_err(|_| anyhow!("a legacy audio session is already in progress"))?;
                let mut guard = self
                    .cat_session
                    .lock()
                    .map_err(|_| anyhow!("cat session lock poisoned"))?;
                if guard.is_some() {
                    self.legacy_source.store(LEGACY_IDLE, Ordering::Release);
                    return Err(anyhow!("system-audio capture already in progress"));
                }
                match core_audio_tap::CoreAudioTapSession::start(
                    &self.resource_dir,
                    Arc::clone(&self.active_sessions),
                    Arc::clone(&self.level),
                ) {
                    Ok(session) => {
                        *guard = Some(session);
                        Ok(())
                    }
                    Err(e) => {
                        self.legacy_source.store(LEGACY_IDLE, Ordering::Release);
                        Err(e)
                    }
                }
            }
            #[cfg(not(target_os = "macos"))]
            AudioSource::SystemAudio => Err(anyhow!(
                "system audio capture is not yet implemented on this platform — tracked under #106 (Linux) / #107 (Windows)"
            )),
        }
    }

    fn supports_source(&self, source: &AudioSource) -> bool {
        match source {
            AudioSource::Microphone(_) => true,
            AudioSource::SystemAudio => cfg!(target_os = "macos"),
        }
    }

    fn stop(&self) -> Result<CapturedAudio> {
        // Swap to IDLE atomically so we know which backend to stop.
        // AcqRel: we need to observe any stores done by start_with_source
        // (Acquire) and make our swap visible before active_sessions
        // can read 0 (Release).
        let prior = self.legacy_source.swap(LEGACY_IDLE, Ordering::AcqRel);
        match prior {
            LEGACY_SYS => {
                #[cfg(target_os = "macos")]
                {
                    let mut guard = self
                        .cat_session
                        .lock()
                        .map_err(|_| anyhow!("cat session lock poisoned"))?;
                    if let Some(session) = guard.take() {
                        // stop() decrements active_sessions unconditionally inside
                        // CoreAudioTapSession::stop — #555 pattern preserved.
                        return Box::new(session).stop();
                    }
                }
                Err(anyhow!(
                    "legacy system-audio session was marked active but cat_session was empty"
                ))
            }
            // LEGACY_MIC or LEGACY_IDLE (backward-compat: callers that use
            // stop() without a matching start_with_source, e.g. tests).
            _ => self.dispatch(Cmd::Stop),
        }
    }

    fn is_recording(&self) -> bool {
        // True while any capture session — legacy singleton or
        // handle-based — is in flight. Acquire ordering pairs with the
        // Release on each fetch_add / fetch_sub so a `true` reading
        // happens-after the corresponding start.
        self.active_sessions.load(Ordering::Acquire) > 0
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

    fn start_session(&self, source: AudioSource) -> Result<Box<dyn AudioSession>> {
        match source {
            AudioSource::Microphone(device_id) => {
                // Dispatches to the same cpal worker the legacy
                // `start` path uses; the worker rejects if its
                // singleton mic Session slot is already occupied.
                // That mutual-exclusion is fine — only one mic
                // capture per process at a time is what cpal
                // supports, and the meeting pump's two-source
                // config is mic + SCK, not mic + mic.
                // Snapshot the sender BEFORE dispatching Start so a
                // post-Start failure (cmd_tx mutex poisoning) doesn't
                // leave us with an incremented refcount and no way
                // to send the matching Cmd::Stop. With the clone in
                // hand we can issue a rollback Stop on any
                // construction failure path.
                let cmd_tx = self
                    .cmd_tx
                    .lock()
                    .map_err(|_| anyhow!("audio command channel lock poisoned"))?
                    .clone();
                let device_id_owned = device_id.clone();
                self.dispatch::<()>(|reply| Cmd::Start {
                    device_id: device_id_owned,
                    reply,
                })?;
                Ok(Box::new(CpalMicSessionHandle {
                    source: AudioSource::Microphone(device_id),
                    cmd_tx: Some(cmd_tx),
                    level: Arc::clone(&self.level),
                }))
            }
            #[cfg(target_os = "macos")]
            AudioSource::SystemAudio => {
                // Independent CoreAudio tap session owned by the handle.
                // Doesn't touch `cat_session` (the legacy hot-path slot), so
                // the dictation hot path's SystemAudio capture and the meeting
                // pump's SystemAudio capture don't race on the same slot.
                let session = core_audio_tap::CoreAudioTapSession::start(
                    &self.resource_dir,
                    Arc::clone(&self.active_sessions),
                    Arc::clone(&self.level),
                )?;
                // active_sessions already incremented inside start(); the
                // CoreAudioTapSession takes ownership of the decrement in its
                // stop()/Drop path.
                Ok(Box::new(session) as Box<dyn AudioSession>)
            }
            #[cfg(not(target_os = "macos"))]
            AudioSource::SystemAudio => Err(anyhow!(
                "system audio capture is not yet implemented on this platform — tracked under #106 (Linux) / #107 (Windows)"
            )),
        }
    }
}

/// Handle returned by [`CpalAudioCapture::start_session`] for a
/// microphone source. Owns the right to send a `Cmd::Stop` to the
/// cpal worker on drop / explicit stop.
///
/// The worker thread keeps the actual `Session` (the cpal stream +
/// buffer); this handle is just a typed permission slip that issues
/// the stop command and receives the drained samples back.
///
/// `cmd_tx` is `Option` so the explicit `stop()` path can `take()`
/// it and the `Drop` impl can detect "already stopped" — a single
/// stop guarantee is what makes the resource accounting on
/// `active_sessions` symmetric.
struct CpalMicSessionHandle {
    source: AudioSource,
    cmd_tx: Option<mpsc::Sender<Cmd>>,
    level: Arc<AtomicU32>,
}

impl AudioSession for CpalMicSessionHandle {
    fn source(&self) -> &AudioSource {
        &self.source
    }
    fn current_level(&self) -> f32 {
        // The worker writes the latest RMS into the shared atomic
        // on every callback. The handle just reads it; there is no
        // per-handle filtering today (a pump running mic + SCK in
        // parallel sees the most-recent reading from either path),
        // which is fine for the HUD's single-bar meter.
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
    fn drain_into(&self, sink: &mut Vec<f32>) -> Result<CaptureFormat> {
        // Round-trip via Cmd::DrainBuffer because the cpal Session
        // (which holds the buffer Arc) lives on the worker thread.
        // The mpsc round-trip is microsecond-scale; the alternative
        // — leaking the buffer Arc into the handle at start-time —
        // would require expanding Cmd::Start's reply shape, an
        // invasive change for a one-call-per-tick path.
        let cmd_tx = self
            .cmd_tx
            .as_ref()
            .ok_or_else(|| anyhow!("mic session already stopped; drain_into unavailable"))?;
        let (tx, rx) = mpsc::channel::<Result<(Vec<f32>, CaptureFormat)>>();
        cmd_tx
            .send(Cmd::DrainBuffer(tx))
            .map_err(|_| anyhow!("audio worker thread has exited"))?;
        let (mut samples, format) = rx
            .recv()
            .map_err(|_| anyhow!("audio worker dropped reply channel"))??;
        sink.extend_from_slice(&samples);
        // Zeroize the local copy: the Vec's backing allocation survives
        // until the next collection cycle and may contain PCM data.
        {
            use zeroize::Zeroize;
            samples.zeroize();
        }
        Ok(format)
    }
    fn stop(mut self: Box<Self>) -> Result<CapturedAudio> {
        let cmd_tx = self
            .cmd_tx
            .take()
            .ok_or_else(|| anyhow!("mic session already stopped"))?;
        let (tx, rx) = mpsc::channel::<Result<CapturedAudio>>();
        cmd_tx
            .send(Cmd::Stop(tx))
            .map_err(|_| anyhow!("audio worker thread has exited"))?;
        rx.recv()
            .map_err(|_| anyhow!("audio worker dropped reply channel"))?
    }
}

impl Drop for CpalMicSessionHandle {
    fn drop(&mut self) {
        // Implicit-drop path: the handle is dropped without an
        // explicit `stop()` (panic in the pump task, runtime
        // shutdown, manager Drop, …). Best-effort stop so the cpal
        // worker's singleton mic Session slot is released; without
        // this the mic stream stays live until the worker thread
        // exits, and a subsequent capture session sees
        // "recording already in progress" forever.
        //
        // Drop must be fast — we don't wait for the reply. The
        // worker's Cmd::Stop handler decrements active_sessions
        // even when the reply channel is dropped on the receiver
        // side, so the refcount stays consistent.
        if let Some(cmd_tx) = self.cmd_tx.take() {
            let (tx, _rx) = mpsc::channel::<Result<CapturedAudio>>();
            if let Err(e) = cmd_tx.send(Cmd::Stop(tx)) {
                tracing::warn!(
                    error = ?e,
                    "cpal mic session Cmd::Stop failed during Drop (worker likely exited)"
                );
            }
        }
    }
}

/// State held by the worker thread for the duration of a single recording.
struct Session {
    /// Kept alive for the duration of capture. Dropping it stops the stream.
    /// We do not read from it after construction; the underlying callback
    /// writes directly into the ring's producer half (which the callback
    /// closure owns).
    stream: Stream,
    format: CaptureFormat,
    /// Human-readable device name captured at session start (#587).
    /// Kept here because cpal's `StreamError` callback fires after the
    /// device is gone and `device.name()` would no longer be callable.
    /// Surfaced through [`DeviceLost`] when the worker detects a
    /// disconnect.
    device_name: String,
    /// Consumer end of the SPSC ring (#55). The callback (writer) owns
    /// the matching `Producer` inside its closure; this `Consumer`
    /// stays on the worker thread and is the only reader. Single-
    /// owner on each side — `rtrb` enforces this at compile time
    /// (`Producer` / `Consumer` are `!Sync`), which is also why this
    /// field is held by value rather than `Arc`.
    consumer: Consumer<f32>,
    /// One-shot flag the callback flips when `producer.push` returns
    /// `PushError::Full`. The worker logs once per drain cycle when
    /// it observes `true` and resets the flag, so a sustained
    /// overflow doesn't spam the log every callback. `Relaxed`
    /// because the message is purely informational; the dropped
    /// samples are already lost regardless of when the worker
    /// notices.
    overflow_flag: Arc<AtomicBool>,
    /// Latched flag the cpal `error_callback` flips when it sees
    /// [`StreamError::DeviceNotAvailable`] (#587). Read on every
    /// `Cmd::DrainBuffer` and on `Cmd::Stop` so the IPC layer can
    /// route the typed [`DeviceLost`] error to the frontend instead
    /// of a generic "audio: …" message. `Arc<AtomicBool>` because
    /// the error callback runs on the cpal-owned audio thread and
    /// the worker thread reads it on each drain.
    device_lost_flag: Arc<AtomicBool>,
}

fn worker_loop(
    cmd_rx: mpsc::Receiver<Cmd>,
    active_sessions: Arc<AtomicU32>,
    level: Arc<AtomicU32>,
) {
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
                match start_cpal_session(&host, device_id.as_deref(), Arc::clone(&level)) {
                    Ok(s) => {
                        // Release ordering pairs with Acquire in `is_recording()`.
                        // fetch_add returns the previous value; if it was 0 we
                        // just transitioned from "no captures" to "one capture",
                        // which is what the legacy `is_recording` bool tracked.
                        active_sessions.fetch_add(1, Ordering::Release);
                        session = Some(s);
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Cmd::Stop(reply) => {
                match session.take() {
                    Some(s) => {
                        let result = stop_cpal_session(s);
                        // Decrement only on success-or-attempt path: we held
                        // a session, so a corresponding fetch_add happened
                        // on the matching Start. fetch_sub here pairs with it.
                        active_sessions.fetch_sub(1, Ordering::Release);
                        // Only zero the HUD level if no other capture path
                        // is currently running. Otherwise an in-flight SCK
                        // session would see its meter blanked while it's
                        // still actively writing samples.
                        if active_sessions.load(Ordering::Acquire) == 0 {
                            level.store(0_f32.to_bits(), Ordering::Relaxed);
                        }
                        let _ = reply.send(result);
                    }
                    None => {
                        // No session to drain; don't touch the refcount —
                        // a no-op stop must not underflow the counter.
                        let _ = reply.send(Err(anyhow!("no recording in progress")));
                    }
                }
            }
            Cmd::DrainBuffer(reply) => {
                // Like Stop, but leaves the session in place so the
                // cpal stream keeps writing samples post-drain. The
                // pump (#108 PR3) calls this on a tight tick.
                match session.as_mut() {
                    Some(s) => {
                        // Device-disconnect detection (#587). The
                        // cpal error callback sets this flag when the
                        // selected input goes away. Surface as a
                        // typed [`DeviceLost`] so the meeting pump
                        // (PR 3 of #587) — and any future caller of
                        // drain_into — can route the disconnect
                        // through `IpcError::AudioDeviceLost`
                        // instead of mistaking the empty-ring drain
                        // for a regular tick. Drained samples are
                        // dropped on this path: for a session that
                        // ended because the device walked away, the
                        // partial audio isn't worth more than the
                        // typed failure signal.
                        if s.device_lost_flag.load(Ordering::Relaxed) {
                            let _ = reply.send(Err(anyhow::Error::new(DeviceLost {
                                device: s.device_name.clone(),
                            })));
                            continue;
                        }
                        let samples = drain_consumer(&mut s.consumer);
                        log_overflow_if_set(&s.overflow_flag);
                        let _ = reply.send(Ok((samples, s.format)));
                    }
                    None => {
                        let _ = reply.send(Err(anyhow!(
                            "no recording in progress; cannot drain buffer"
                        )));
                    }
                }
            }
            Cmd::Shutdown => break,
        }
    }
}

fn list_devices(host: &cpal::Host) -> Result<Vec<AudioDevice>> {
    // On macOS 14+, CoreAudio input device enumeration triggers the TCC
    // microphone prompt even for passive listing — not just when recording
    // starts. Guard here so cold-start source listing (called from the
    // frontend's onMount before the user has taken any recording action)
    // stays side-effect-free. The caller gets an empty device list until
    // the user grants mic access; `list_audio_sources` still appends the
    // system-audio entry, which doesn't require mic permission.
    #[cfg(target_os = "macos")]
    if crate::permissions::microphone_status() != crate::permissions::PermissionStatus::Granted {
        return Ok(vec![]);
    }

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

fn start_cpal_session(
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

    // Capture the device name once, here at start, so it's available
    // for the [`DeviceLost`] error path (#587) — by the time the
    // cpal error callback fires `DeviceNotAvailable`, calling
    // `device.name()` is racy at best and may reflect the new
    // default-input rather than the lost one.
    let device_name = device
        .name()
        .unwrap_or_else(|_| "<unknown input device>".to_owned());

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

    // Pre-allocate the SPSC ring at the existing buffer cap so the
    // overflow behaviour matches the pre-#55 Mutex<Vec<f32>> path
    // (overflow at the same threshold). Sized in samples; at 48 kHz
    // stereo f32 this is the same MAX_BUFFER_FRAMES the ad-hoc
    // ceiling used. The whole capacity is allocated once at session
    // start — no realloc inside the realtime callback.
    let (producer, consumer) = RingBuffer::<f32>::new(MAX_BUFFER_FRAMES);
    let overflow_flag = Arc::new(AtomicBool::new(false));
    let device_lost_flag = Arc::new(AtomicBool::new(false));
    let stream = build_input_stream(
        &device,
        &supported,
        producer,
        Arc::clone(&overflow_flag),
        Arc::clone(&device_lost_flag),
        level,
    )?;
    stream.play().context("start input stream")?;

    Ok(Session {
        stream,
        format,
        device_name,
        consumer,
        overflow_flag,
        device_lost_flag,
    })
}

fn stop_cpal_session(mut session: Session) -> Result<CapturedAudio> {
    // Pause first so no further callbacks can land while we drain the
    // ring. Dropping the stream alone is technically sufficient on
    // every backend we currently target, but `pause()` makes the
    // intent obvious and is cheap on the human-paced control plane.
    let _ = session.stream.pause();
    drop(session.stream);

    let samples = drain_consumer(&mut session.consumer);
    log_overflow_if_set(&session.overflow_flag);

    // Device-disconnect detection (#587). The cpal error callback
    // sets the flag when the host fires `DeviceNotAvailable`; we
    // surface it here as a typed [`DeviceLost`] error so the IPC
    // layer can route to `IpcError::AudioDeviceLost` instead of the
    // generic "audio: …" bucket. Whatever samples we drained pre-
    // disconnect are dropped — for a recording that ended because
    // the user's mic walked away, the partial audio isn't useful
    // and surfacing the typed failure is the right shape.
    if session.device_lost_flag.load(Ordering::Relaxed) {
        return Err(anyhow::Error::new(DeviceLost {
            device: session.device_name,
        }));
    }

    Ok(CapturedAudio {
        samples,
        format: session.format,
    })
}

fn build_input_stream(
    device: &cpal::Device,
    supported: &SupportedStreamConfig,
    producer: Producer<f32>,
    overflow_flag: Arc<AtomicBool>,
    device_lost_flag: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
) -> Result<Stream> {
    let config: cpal::StreamConfig = supported.config();

    // cpal hands samples back in the device's native format. We
    // convert to f32 inside the callback so the rest of the pipeline
    // only ever deals with one type. The integer paths are exhaustive
    // over what cpal 0.15 exposes today; an unknown variant is treated
    // as a hard error rather than a silent fallback so we notice when
    // cpal adds a new format.
    //
    // The `Producer` is moved into the closure (single-owner — no
    // `Clone` impl), so each match arm gets its own move + a fresh
    // overflow_flag clone. The level Arc is the same shape as before.
    // The error callback is built once and shared (Clone on the Arc),
    // so each sample-format arm sets up the same DeviceNotAvailable →
    // device_lost_flag detection.
    let make_error_callback =
        |flag: Arc<AtomicBool>| move |err: StreamError| stream_error_callback(&flag, err);
    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            let mut prod = producer;
            let overflow = Arc::clone(&overflow_flag);
            let lvl = Arc::clone(&level);
            device.build_input_stream(
                &config,
                move |data: &[f32], _| push_samples(&mut prod, &overflow, data, |s| *s, &lvl),
                make_error_callback(Arc::clone(&device_lost_flag)),
                None,
            )
        }
        SampleFormat::I16 => {
            let mut prod = producer;
            let overflow = Arc::clone(&overflow_flag);
            let lvl = Arc::clone(&level);
            device.build_input_stream(
                &config,
                move |data: &[i16], _| push_samples(&mut prod, &overflow, data, i16_to_f32, &lvl),
                make_error_callback(Arc::clone(&device_lost_flag)),
                None,
            )
        }
        SampleFormat::U16 => {
            let mut prod = producer;
            let overflow = Arc::clone(&overflow_flag);
            let lvl = Arc::clone(&level);
            device.build_input_stream(
                &config,
                move |data: &[u16], _| push_samples(&mut prod, &overflow, data, u16_to_f32, &lvl),
                make_error_callback(Arc::clone(&device_lost_flag)),
                None,
            )
        }
        other => return Err(anyhow!("unsupported cpal sample format: {other:?}")),
    }
    .context("build cpal input stream")?;

    Ok(stream)
}

/// Push a callback's worth of samples into the SPSC ring and
/// publish the per-callback RMS to the level meter.
///
/// The audio callback runs on a realtime-ish thread; it must not
/// allocate, block, or lock anything that could be contended on a
/// higher-priority thread. `rtrb::Producer::push` is wait-free and
/// allocation-free; on a full ring it returns `PushError::Full`,
/// which we treat as overflow recovery: drop the sample, set the
/// overflow flag (the worker logs once per drain), continue the
/// loop. RMS is computed in the same single pass that converts and
/// pushes — no extra allocation, no second iteration.
///
/// Pre-#55, the buffer was a `Mutex<Vec<f32>>` that locked briefly
/// per callback. That worked but violated the realtime-audio
/// discipline (priority inversion if the worker thread was preempted
/// while holding the lock). The current shape honours the discipline
/// at the cost of dropping NEWER samples on overflow rather than
/// OLDER — both are "the consumer wedged" recovery behaviours and
/// the user-visible difference is negligible.
fn push_samples<T: Copy>(
    producer: &mut Producer<f32>,
    overflow_flag: &AtomicBool,
    data: &[T],
    convert: impl Fn(&T) -> f32,
    level: &AtomicU32,
) {
    let mut sum_sq = 0.0_f32;
    let mut overflowed = false;
    for sample in data {
        let f = convert(sample);
        sum_sq += f * f;
        if producer.push(f).is_err() {
            overflowed = true;
            // Continue the loop so RMS still reflects the full
            // callback worth of audio even when the ring is full —
            // the level meter shouldn't go silent during overflow.
        }
    }
    if overflowed {
        // `Relaxed` is enough — the worker reads on a human-paced
        // drain tick and the message is purely informational.
        overflow_flag.store(true, Ordering::Relaxed);
    }
    if !data.is_empty() {
        let rms = rms_from_sum_sq(sum_sq, data.len());
        // `Relaxed`: each callback writes the latest reading; the HUD
        // pump reads independently and can tolerate a stale value for
        // one 33 ms tick. There is no other field that needs to be
        // observed alongside the level.
        level.store(rms.to_bits(), Ordering::Relaxed);
    }
}

/// RMS from a pre-computed sum-of-squares plus the sample count.
/// Pulled out as a free function so the level-meter math can be
/// Thin alias so the call-sites in this module keep the same name while
/// the implementation lives in the shared `super::rms_from_sum_sq` helper
/// (#822 — both audio paths now share one definition).
fn rms_from_sum_sq(sum_sq: f32, n: usize) -> f32 {
    super::rms_from_sum_sq(sum_sq, n)
}

/// cpal stream `error_callback` — runs on the audio thread when the
/// stream encounters a host-level fault.
///
/// Sets `device_lost_flag` on [`StreamError::DeviceNotAvailable`]
/// (#587) so the worker's drain / stop paths surface a typed
/// [`DeviceLost`] error to the IPC layer. Other variants
/// (`BackendSpecific` etc.) remain logged-and-continue per the
/// existing realtime-thread discipline: this callback runs on a
/// realtime-ish thread, so heavy work or anything fallible would
/// risk the audio backend's invariants.
///
/// Free function rather than a method so the callback's closure
/// shape stays small — cpal's `error_callback` parameter is
/// `FnMut(StreamError) + Send + 'static` and a single
/// per-stream closure that calls into this helper compiles cleanly
/// across all three sample-format arms.
fn stream_error_callback(device_lost_flag: &AtomicBool, err: StreamError) {
    match err {
        StreamError::DeviceNotAvailable => {
            // Latch the flag — the worker reads it on every drain
            // tick and on Cmd::Stop. `Relaxed` is fine: the flag
            // is independent of any other shared state, the worker
            // tolerates a one-tick delay (~500 ms) before observing
            // the flip, and an over-strong ordering would only buy
            // an unhelpful "this drain saw it slightly sooner" win.
            device_lost_flag.store(true, Ordering::Relaxed);
            tracing::warn!(
                "cpal stream error: DeviceNotAvailable — selected input device disconnected"
            );
        }
        other => {
            tracing::error!(error = ?other, "audio input stream error");
        }
    }
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
    fn push_samples_handles_full_ring_by_dropping_and_setting_overflow() {
        // Sized for exactly two samples — the third push fails and
        // sets the overflow flag. Pre-#55 this scenario dropped
        // OLDER samples (FIFO eviction); post-#55 it drops NEWER
        // ones because rtrb is realtime-safe and doesn't allow the
        // producer to evict committed samples. Both behaviours are
        // overflow-recovery; the test pins the new contract.
        let (mut p, _c) = RingBuffer::<f32>::new(2);
        let overflow = AtomicBool::new(false);
        let level = AtomicU32::new(0);

        push_samples(&mut p, &overflow, &[1.0_f32, 2.0, 3.0], |s| *s, &level);

        // 2 samples landed, the 3rd dropped — overflow flag set.
        assert!(overflow.load(Ordering::Relaxed));
    }

    #[test]
    fn push_samples_publishes_rms_even_when_ring_is_full() {
        // Regression guard: the level meter must not go silent
        // during overflow. The HUD's pulsing dot is the user's
        // primary "is the mic actually live?" signal — making it
        // freeze on an overflow would surface as "the mic stopped
        // working" even though audio is still being captured (just
        // dropped at the ring boundary).
        let (mut p, _c) = RingBuffer::<f32>::new(1);
        let overflow = AtomicBool::new(false);
        let level = AtomicU32::new(0);
        push_samples(&mut p, &overflow, &[0.5_f32, 0.5, 0.5, 0.5], |s| *s, &level);
        let observed_rms = f32::from_bits(level.load(Ordering::Relaxed));
        // RMS of four 0.5s is 0.5 exactly.
        assert!((observed_rms - 0.5).abs() < 1e-6, "rms {observed_rms}");
    }
}
