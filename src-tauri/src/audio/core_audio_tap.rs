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

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{anyhow, Context, Result};
use rtrb::{Consumer, RingBuffer};

use super::{
    drain_consumer, log_overflow_if_set, AudioSession, AudioSource, CaptureFormat, CapturedAudio,
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
    consumer: Mutex<Consumer<f32>>,
    overflow_flag: Arc<AtomicBool>,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
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
        let stdout = guard
            .0
            .as_mut()
            .unwrap()
            .stdout
            .take()
            .ok_or_else(|| anyhow!("child stdout not captured"))?;
        let mut reader = BufReader::new(stdout);

        let mut hdr = [0u8; 12];
        reader
            .read_exact(&mut hdr)
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

        let format = CaptureFormat {
            sample_rate,
            channels: channels as u16,
        };

        // SPSC ring large enough for ~2 min of 48 kHz stereo (same ceiling as
        // the cpal path — see MAX_BUFFER_FRAMES in mod.rs).
        let capacity = MAX_BUFFER_FRAMES * channels as usize;
        // #612 candidate-2 diagnostic: log the channel count and ring
        // size so we can rule out an over-allocated ring when the tap
        // reports >2 channels (e.g. an aggregate device). Downstream
        // code mixes to mono before forwarding, so this is purely an
        // init-time footprint check.
        tracing::info!(
            "core-audio tap init: sr={} ch={} ring_capacity_samples={} (~{:.1} MB)",
            sample_rate,
            channels,
            capacity,
            (capacity * std::mem::size_of::<f32>()) as f64 / (1024.0 * 1024.0),
        );
        let (mut producer, consumer) = RingBuffer::new(capacity);
        let overflow_flag = Arc::new(AtomicBool::new(false));
        let overflow_writer = Arc::clone(&overflow_flag);
        let level_writer = Arc::clone(&level);

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
                        Ok(0) => break, // EOF — child exited, normal shutdown
                        Ok(n) => {
                            let total = tail + n;
                            let samples = total / 4;
                            for i in 0..samples {
                                let s = i * 4;
                                let sample =
                                    f32::from_le_bytes(chunk_buf[s..s + 4].try_into().unwrap());
                                level_writer.store(sample.abs().to_bits(), Ordering::Relaxed);
                                if producer.push(sample).is_err() {
                                    overflow_writer.store(true, Ordering::Relaxed);
                                }
                            }
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
                            break;
                        }
                    }
                }
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
                consumer: Mutex::new(consumer),
                overflow_flag,
                reader_thread: Mutex::new(Some(reader_thread)),
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
        let mut consumer = inner
            .consumer
            .lock()
            .map_err(|_| anyhow!("CoreAudio tap consumer lock poisoned"))?;
        let samples = drain_consumer(&mut consumer);
        log_overflow_if_set(&inner.overflow_flag);
        sink.extend_from_slice(&samples);
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

    // 2. Join reader thread — it exits once it sees EOF on the child's stdout.
    if let Ok(mut guard) = inner.reader_thread.lock() {
        if let Some(handle) = guard.take() {
            let _ = handle.join();
        }
    }

    // 3. Drain remaining samples from the ring.
    let samples = if let Ok(mut consumer) = inner.consumer.lock() {
        drain_consumer(&mut consumer)
    } else {
        Vec::new()
    };
    log_overflow_if_set(&inner.overflow_flag);

    Ok(samples)
}

// ── Path helper ───────────────────────────────────────────────────────────────

/// Resolve the CoreAudio tap binary path from the Tauri resource directory.
pub fn capture_binary_path(resource_dir: &Path) -> PathBuf {
    resource_dir
        .join("resources")
        .join("hush-audio-tap-capture")
}
