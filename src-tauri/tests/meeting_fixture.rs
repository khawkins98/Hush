//! End-to-end meeting pump test against the bundled WAV fixture.
//!
//! Exercises the full [`SessionManager`] → pump → streaming transcription
//! path through the [`AudioCapture`] seam using [`WavFileAudioCapture`].
//! Unlike `audio_fixture.rs` (which calls `WhisperTranscription::transcribe`
//! directly) this test exercises the `AudioSession::drain_into` path that
//! the meeting pump uses in production.
//!
//! ## What this test exercises
//!
//! 1. **`WavFileAudioCapture::start_session`** returns a [`WavFileAudioSession`]
//!    that serves pre-loaded samples in 500 ms chunks per `drain_into` tick.
//! 2. **`SessionManager::start_manual`** opens the audio session, creates the
//!    DB row, and spawns the pump task.
//! 3. **The pump loop** drains the session at 500 ms ticks, feeds samples into
//!    the streaming `WhisperTranscription` session, and persists final utterances.
//! 4. **`SessionManager::stop_manual`** cancels the pump, flushes the
//!    streaming tail, and persists remaining finals.
//! 5. **The session repo** is queried for persisted utterances; the test
//!    asserts that the expected words appear in the concatenated transcript.
//!
//! ## Why `#[ignore]`d by default
//!
//! Requires `HUSH_TEST_MODEL`, the `whisper` and `test-utils` Cargo features,
//! and `cmake` on the host. Run locally with:
//!
//! ```text
//! HUSH_TEST_MODEL=path/to/ggml-base.bin \
//! cargo test --features whisper,test-utils --test meeting_fixture -- --ignored --nocapture
//! ```
//!
//! `--nocapture` is recommended so per-tick log lines and the final
//! transcript are visible in the terminal.
//!
//! ## In-memory database
//!
//! All DB writes go to a [`SqliteDatabase::open_in_memory`] instance so the
//! test leaves no files on disk and can run in parallel with other fixture
//! tests.

#![cfg(all(feature = "whisper", feature = "test-utils"))]

use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;

use hush_lib::audio::file_source::WavFileAudioCapture;
use hush_lib::audio::AudioSource;
use hush_lib::db::SqliteDatabase;
use hush_lib::diarization::NoopDiarizer;
use hush_lib::events::NoopEventEmitter;
use hush_lib::ipc::TranscribeSlot;
use hush_lib::meeting::{
    MeetingSessionRepository, SessionManager, SqliteMeetingAppOverrideRepository,
    SqliteMeetingSessionRepository,
};
use hush_lib::transcription::{Transcribe, WhisperTranscription};

/// Resolve the model path (HUSH_TEST_MODEL env var). Returns `None` and prints
/// a skip message if unset or the file does not exist.
fn resolve_model_path() -> Option<PathBuf> {
    let p = match std::env::var("HUSH_TEST_MODEL") {
        Ok(v) => PathBuf::from(v),
        Err(_) => {
            eprintln!("skip: HUSH_TEST_MODEL not set; skipping meeting_fixture test");
            return None;
        }
    };
    if p.exists() {
        Some(p)
    } else {
        eprintln!(
            "skip: HUSH_TEST_MODEL → {} does not exist; skipping meeting_fixture test",
            p.display()
        );
        None
    }
}

fn bundled_jfk_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("jfk.wav")
}

fn audio_path() -> PathBuf {
    match std::env::var("HUSH_TEST_AUDIO") {
        Ok(v) => PathBuf::from(v),
        Err(_) => bundled_jfk_path(),
    }
}

#[test]
#[ignore] // Requires HUSH_TEST_MODEL; see module doc.
fn meeting_pump_transcribes_fixture_via_audiosession_seam() {
    let Some(model_path) = resolve_model_path() else {
        return;
    };
    let wav_path = audio_path();

    // Build fixture audio capture (~500 ms chunks to match PUMP_TICK).
    let audio = WavFileAudioCapture::from_wav(&wav_path, 500)
        .expect("load WAV fixture for meeting pump test");

    // Runtime needs a tokio handle; spin up a single-threaded runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    rt.block_on(async move {
        // In-memory DB + repositories — no files on disk.
        let db = Arc::new(
            SqliteDatabase::open_in_memory()
                .await
                .expect("open in-memory DB"),
        );
        let session_repo: Arc<dyn MeetingSessionRepository> =
            Arc::new(SqliteMeetingSessionRepository::new(Arc::clone(&db)));
        let override_repo: Arc<dyn hush_lib::meeting::MeetingAppOverrideRepository> =
            Arc::new(SqliteMeetingAppOverrideRepository::new(Arc::clone(&db)));

        // Load the whisper model and wire it into a TranscribeSlot.
        let transcriber: Arc<dyn Transcribe> =
            Arc::new(WhisperTranscription::new(&model_path).expect("load whisper model"));
        let transcribe_slot: TranscribeSlot =
            Arc::new(std::sync::Mutex::new(Some(Arc::clone(&transcriber))));

        let emitter = Arc::new(NoopEventEmitter) as Arc<dyn hush_lib::events::EventEmitter>;
        let diarize = Arc::new(NoopDiarizer) as Arc<dyn hush_lib::diarization::Diarize>;
        let mic_gain_db = Arc::new(AtomicU32::new(0f32.to_bits()));

        let manager = SessionManager::new(
            Arc::clone(&session_repo),
            Arc::new(audio),
            transcribe_slot,
            emitter,
            diarize,
            override_repo,
            mic_gain_db,
        );

        // Start a manual meeting session on the mic source.
        let session = manager
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("fixture-test".into()),
                None,
            )
            .await
            .expect("start_manual");

        eprintln!("meeting_fixture: session started (id={})", session.id);

        // Let the pump drain the fixture WAV. The JFK clip is ~11 s at 16 kHz;
        // at 500 ms ticks the pump processes ~22 ticks. Whisper needs several
        // chunks before it returns its first utterance. Wait 12 s for the pump
        // to drain the whole clip and flush finals.
        tokio::time::sleep(Duration::from_secs(12)).await;

        // Stop the session — flushes the streaming tail to DB.
        manager.stop_manual().await.expect("stop_manual");
        eprintln!("meeting_fixture: session stopped");

        // Query persisted utterances.
        let utterances = session_repo
            .list_utterances(session.id)
            .await
            .expect("list_utterances");

        let transcript: String = utterances
            .iter()
            .map(|u| u.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("meeting_fixture transcript: {transcript:?}");

        assert!(
            !transcript.is_empty(),
            "expected at least one utterance in the transcript; got empty string"
        );

        // The JFK clip always contains "country" with base+ models.
        let expected_word = std::env::var("HUSH_TEST_EXPECTED_WORDS")
            .ok()
            .and_then(|csv| {
                csv.split(',')
                    .map(|s| s.trim().to_lowercase())
                    .find(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "country".into());

        assert!(
            transcript.to_lowercase().contains(&expected_word),
            "expected transcript to contain {expected_word:?}; got: {transcript:?}",
        );
    });
}
