//! macOS system-audio capture via CoreAudio process tap (#600).
//!
//! Replaces the ScreenCaptureKit path with `AudioHardwareCreateProcessTap`
//! (available since macOS 14.2, confirmed no Screen Recording TCC on macOS 26 —
//! see `learnings.md` 2026-05-xx).  The tap is implemented in a small Swift
//! helper binary (`resources/hush-audio-tap-capture`) that writes a 12-byte
//! header followed by continuous f32 LE interleaved PCM to stdout.  This
//! module spawns that binary, reads the header, then streams samples from the
//! child's stdout into an `rtrb` ring that the meeting-pump drains per tick.
//!
//! ## Wire protocol
//!
//! ```text
//! bytes  0–3   "HUSH" magic (0x48 55 53 48)
//! bytes  4–7   sample_rate  as u32 little-endian
//! bytes  8–11  channel_count as u32 little-endian
//! bytes 12..   interleaved f32 LE PCM — continuous until SIGTERM / exit
//! ```
//!
//! ## Stopping
//!
//! [`CoreAudioTapSession::stop`] sends SIGTERM to the child and waits up to
//! 1 s for a clean exit before falling back to `kill()`.  The reader thread
//! detects `UnexpectedEof` when the child exits and shuts itself down.

use std::collections::VecDeque;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use super::{
    drain_buffer, push_samples_circular, AudioSession, AudioSource, CaptureFormat, CapturedAudio,
    MAX_BUFFER_FRAMES,
};

// ── Public struct ────────────────────────────────────────────────────────────

/// Live system-audio capture session backed by the CoreAudio tap binary.
///
/// Created by [`CoreAudioTapSession::start`]; dropped or explicitly stopped
/// via [`AudioSession::stop`].  `inner` is `Option` so the explicit stop path
/// can `take()` it without a double-stop on `Drop`.
pub struct CoreAudioTapSession {
    pub(super) source: AudioSource,
    pub(super) inner: Option<TapInner>,
    pub(super) active_sessions: Arc<AtomicU32>,
    pub(super) level: Arc<AtomicU32>,
}

/// Heap-allocated state owned by a live tap session.
pub(super) struct TapInner {
    pub(super) format: CaptureFormat,
    child: Mutex<Option<Child>>,
    /// Shared circular capture buffer (#827). The reader thread (writer) and
    /// the drain/stop paths (reader) share this Arc. The VecDeque evicts
    /// oldest samples when at capacity, preserving the most-recent audio.
    buffer: Arc<Mutex<VecDeque<f32>>>,
    /// Set to `true` by the reader thread when the helper process exits or
    /// errors — lets `drain_into` surface an error once the buffer is exhausted
    /// so the pump emits `meeting:source-failed` instead of recording silence.
    reader_exited: Arc<AtomicBool>,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
    /// Receives `()` when the reader thread is about to return.  Used by
    /// `stop_inner` to wait for clean exit with a bounded timeout instead of
    /// blocking indefinitely on `handle.join()` (#864).
    reader_done_rx: Mutex<Option<mpsc::Receiver<()>>>,
}

// ── Construction ─────────────────────────────────────────────────────────────

impl CoreAudioTapSession {
    /// Spawn the CoreAudio tap binary and return a ready session.
    ///
    /// `resource_dir` is `AppHandle::path().resource_dir()` — the binary is
    /// expected at `<resource_dir>/resources/hush-audio-tap-capture`.
    pub fn start(
        resource_dir: &Path,
        active_sessions: Arc<AtomicU32>,
        level: Arc<AtomicU32>,
    ) -> Result<Self> {
        let binary = capture_binary_path(resource_dir);

        if !binary.exists() {
            return Err(anyhow!(
                "CoreAudio tap binary not found at {}",
                binary.display()
            ));
        }

        let child = Command::new(&binary)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // tap diagnostic messages → app stderr/log
            .spawn()
            .with_context(|| format!("spawn {}", binary.display()))?;

        // RAII guard: if any step below fails, kill + reap the child so the
        // tap and aggregate device it installed are cleaned up before we return.
        // Disarmed at the end of the function via `guard.0.take()`.
        struct KillGuard(Option<Child>);
        impl Drop for KillGuard {
            fn drop(&mut self) {
                if let Some(mut c) = self.0.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }
        let mut guard = KillGuard(Some(child));

        // Read the 12-byte protocol header before handing off to the
        // background reader thread.  The Swift binary writes the header before
        // starting `engine.start()`, so this read will unblock promptly once
        // the tap is established.  If the binary exits before writing the header
        // the pipe EOF propagates here as `UnexpectedEof`; the KillGuard above
        // ensures the child is reaped even on these early-return paths.
        //
        // A 5 s deadline guards against a hung tap binary blocking app startup
        // forever (#859).  On timeout the KillGuard fires, killing the child
        // (which causes the spawned thread to see EOF and exit cleanly).
        let stdout = guard
            .0
            .as_mut()
            .unwrap()
            .stdout
            .take()
            .ok_or_else(|| anyhow!("child stdout not captured"))?;
        let mut reader = BufReader::new(stdout);

        let (hdr_tx, hdr_rx) =
            mpsc::channel::<anyhow::Result<([u8; 12], BufReader<std::process::ChildStdout>)>>();
        thread::Builder::new()
            .name("hush-cat-hdr".into())
            .spawn(move || {
                let mut hdr = [0u8; 12];
                let result = reader
                    .read_exact(&mut hdr)
                    .map(|_| (hdr, reader))
                    .map_err(anyhow::Error::from);
                let _ = hdr_tx.send(result);
            })
            .context("failed to spawn hush-cat-hdr thread")?;

        let (hdr, mut reader) = hdr_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("timed out waiting for audio tap binary protocol header (5 s)"))?
            .context("read protocol header from audio tap binary")?;

        if &hdr[0..4] != b"HUSH" {
            return Err(anyhow!(
                "audio tap binary sent unexpected magic: {:?}",
                &hdr[0..4]
            ));
        }
        let sample_rate = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
        let channels = u32::from_le_bytes(hdr[8..12].try_into().unwrap());

        if sample_rate == 0 || channels == 0 {
            return Err(anyhow!(
                "audio tap binary reported degenerate format: sr={sample_rate} ch={channels}"
            ));
        }
        // Guard against a tap binary reporting a pathological channel count
        // that would silently truncate when cast to the u16 stored in
        // CaptureFormat — the ring-buffer capacity (below) was already using
        // the pre-cast u32, so the two values would diverge. Any real audio
        // device tops out at far fewer than 65535 channels; reject early.
        if channels > u16::MAX as u32 {
            return Err(anyhow!(
                "audio tap binary reported implausible channel count: {channels}"
            ));
        }

        let format = CaptureFormat {
            sample_rate,
            channels: channels as u16, // safe: guarded above
        };

        // SPSC ring sized to MAX_BUFFER_FRAMES (48 kHz × 2 ch × 120 s samples —
        // same constant and same capacity the cpal path uses; see mod.rs).
        // Do NOT multiply by `channels` here: the constant is already in samples
        // (not frames), so multiplying again would double-allocate for stereo (#929).
        let capacity = MAX_BUFFER_FRAMES;
        // #612 candidate-2 diagnostic: log the channel count and buffer
        // size so we can rule out an over-allocated buffer when the tap
        // reports >2 channels (e.g. an aggregate device). Downstream
        // code mixes to mono before forwarding, so this is purely an
        // init-time footprint check.
        tracing::info!(
            "core-audio tap init: sr={} ch={} buffer_capacity_samples={} (~{:.1} MB)",
            sample_rate,
            channels,
            capacity,
            (capacity * std::mem::size_of::<f32>()) as f64 / (1024.0 * 1024.0),
        );
        let buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(capacity)));
        let reader_buffer = Arc::clone(&buffer);
        let reader_exited = Arc::new(AtomicBool::new(false));
        let reader_exited_writer = Arc::clone(&reader_exited);
        let level_writer = Arc::clone(&level);

        // Channel used by the reader thread to signal its exit so stop_inner
        // can bound the wait with recv_timeout instead of blocking on join (#864).
        let (reader_done_tx, reader_done_rx) = mpsc::channel::<()>();

        let reader_thread = thread::Builder::new()
            .name("hush-cat-reader".into())
            .spawn(move || {
                // Read in 4096-byte chunks (~1024 f32 samples per iteration)
                // rather than one sample at a time — reduces read overhead by
                // ~250× at 48 kHz stereo.  A `tail` counter tracks the 0–3
                // bytes of a partial f32 straddling a read boundary.
                let mut chunk_buf = [0u8; 4096];
                let mut tail = 0usize;

                loop {
                    match reader.read(&mut chunk_buf[tail..]) {
                        Ok(0) => {
                            // EOF — child exited. Signal drain_into so it can
                            // surface an error once the buffer is exhausted (#910).
                            reader_exited_writer.store(true, Ordering::Release);
                            break;
                        }
                        Ok(n) => {
                            let total = tail + n;
                            let samples = total / 4;
                            let mut sum_sq = 0.0_f32;
                            let mut converted = Vec::with_capacity(samples);
                            for i in 0..samples {
                                let s = i * 4;
                                let sample =
                                    f32::from_le_bytes(chunk_buf[s..s + 4].try_into().unwrap());
                                sum_sq += sample * sample;
                                converted.push(sample);
                            }
                            push_samples_circular(&reader_buffer, &converted, MAX_BUFFER_FRAMES);
                            // Store RMS for this chunk so the level meter matches
                            // the cpal mic path (which also computes RMS per
                            // callback). Storing peak-abs (the old approach) read
                            // ~40% higher than mic at the same loudness (#822).
                            level_writer.store(
                                crate::audio::rms_from_sum_sq(sum_sq, samples).to_bits(),
                                Ordering::Relaxed,
                            );
                            tail = total - samples * 4;
                            if tail > 0 {
                                let carried = samples * 4;
                                chunk_buf.copy_within(carried..carried + tail, 0);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "CoreAudio tap reader: stdout read error; stopping"
                            );
                            reader_exited_writer.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
                // Notify stop_inner that this thread is done (#864).
                let _ = reader_done_tx.send(());
            })
            .context("failed to spawn hush-cat-reader thread")?;

        // All fallible operations succeeded — disarm the kill guard and store
        // the child in TapInner.
        let child = guard.0.take().unwrap();

        active_sessions.fetch_add(1, Ordering::Release);

        Ok(Self {
            source: AudioSource::SystemAudio,
            inner: Some(TapInner {
                format,
                child: Mutex::new(Some(child)),
                buffer,
                reader_exited,
                reader_thread: Mutex::new(Some(reader_thread)),
                reader_done_rx: Mutex::new(Some(reader_done_rx)),
            }),
            active_sessions,
            level,
        })
    }
}

// ── AudioSession impl ─────────────────────────────────────────────────────────

impl AudioSession for CoreAudioTapSession {
    fn source(&self) -> &AudioSource {
        &self.source
    }

    fn current_level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    fn drain_into(&self, sink: &mut Vec<f32>) -> Result<CaptureFormat> {
        let inner = self.inner.as_ref().ok_or_else(|| {
            anyhow!("CoreAudio tap session already stopped; drain_into unavailable")
        })?;
        let mut samples = drain_buffer(&inner.buffer);
        sink.extend_from_slice(&samples);
        // Zeroize before drop: same discipline as the cpal drain_into path.
        {
            use zeroize::Zeroize;
            samples.zeroize();
        }
        // Surface a helper-exit error once the buffer is fully drained so the
        // pump can emit meeting:source-failed instead of recording silence (#910).
        if sink.is_empty() && inner.reader_exited.load(Ordering::Acquire) {
            return Err(anyhow!(
                "CoreAudio tap helper exited; system audio capture stopped"
            ));
        }
        Ok(inner.format)
    }

    fn stop(mut self: Box<Self>) -> Result<CapturedAudio> {
        let inner = self
            .inner
            .take()
            .ok_or_else(|| anyhow!("CoreAudio tap session already stopped"))?;

        let format = inner.format;
        let result = stop_inner(inner);

        self.active_sessions.fetch_sub(1, Ordering::Release);
        if self.active_sessions.load(Ordering::Acquire) == 0 {
            self.level.store(0_f32.to_bits(), Ordering::Relaxed);
        }

        Ok(CapturedAudio {
            samples: result?,
            format,
        })
    }
}

impl Drop for CoreAudioTapSession {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            if let Err(e) = stop_inner(inner) {
                tracing::warn!(error = ?e, "CoreAudio tap session stop failed during Drop");
            }
            self.active_sessions.fetch_sub(1, Ordering::Release);
            if self.active_sessions.load(Ordering::Acquire) == 0 {
                self.level.store(0_f32.to_bits(), Ordering::Relaxed);
            }
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Gracefully stop a `TapInner`: SIGTERM the child, join the reader thread,
/// drain any remaining samples from the ring.
fn stop_inner(inner: TapInner) -> Result<Vec<f32>> {
    // 1. Terminate child: SIGTERM, then 1 s poll, then SIGKILL fallback.
    if let Ok(mut guard) = inner.child.lock() {
        if let Some(ref mut child) = *guard {
            let pid = child.id() as i32;
            // SAFETY: `pid` is a live child PID obtained from `Child::id()`.
            unsafe { libc::kill(pid, libc::SIGTERM) };

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        thread::sleep(std::time::Duration::from_millis(50));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
    }

    // 2. Wait for reader thread — bounded by 3 s so a stuck thread doesn't
    //    block app shutdown forever (#864).  After step 1 above the child is
    //    already dead, so the reader will see EOF and signal done imminently.
    if let Ok(mut guard) = inner.reader_done_rx.lock() {
        if let Some(rx) = guard.take() {
            if rx.recv_timeout(Duration::from_secs(3)).is_err() {
                tracing::warn!(
                    "CoreAudio tap reader thread did not exit within 3 s; \
                     detaching — samples already drained from buffer"
                );
            }
        }
    }
    // Drop the JoinHandle — detaches the thread if it's somehow still running.
    if let Ok(mut guard) = inner.reader_thread.lock() {
        let _ = guard.take();
    }

    // 3. Drain remaining samples from the buffer.
    let samples = drain_buffer(&inner.buffer);

    Ok(samples)
}

// ── Path helper ───────────────────────────────────────────────────────────────

/// Resolve the CoreAudio tap binary path from the Tauri resource directory.
pub fn capture_binary_path(resource_dir: &Path) -> PathBuf {
    resource_dir
        .join("resources")
        .join("hush-audio-tap-capture")
}
