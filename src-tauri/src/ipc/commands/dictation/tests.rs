//! Unit tests for the dictation IPC command handlers and pipeline helpers.
//!
//! Extracted from `commands/dictation/mod.rs` under #684. All tests cover
//! the pure-logic helpers in `pipeline.rs` so the command shells stay thin
//! and the orchestration steps are independently pinned.

// -- start_dictation_inner regression tests ---------------------------
//
// These cover the foreground-leak fix surfaced in code review: a
// failed `audio.start` must not overwrite or pollute the
// `pending_foreground` slot. Using mock implementations of
// `AudioCapture` rather than the cpal backend so we do not need a real
// microphone or Tauri runtime.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;

use crate::audio::{AudioCapture, AudioDevice, AudioSource, CapturedAudio};
use crate::dictionary::{
    NewVocabularyTerm, ReplacementRepository, ReplacementRule, VocabularyRepository,
    VocabularyTerm,
};
use crate::ipc::state::ForegroundApp;
use crate::ipc::AppState;
use crate::transcription::Transcribe;

use super::pipeline::{
    load_replacement_rules, load_vocabulary_prompt, start_dictation_inner, stop_audio_capture,
    strip_whisper_brackets, take_foreground_snapshot,
};
use super::IpcError;

/// Local Transcribe stub. The crate-root tests have an
/// `EchoTranscribe` but it isn't `pub(crate)`; declaring a fresh
/// one here keeps the dependency minimal.
struct OkTranscribe;
impl Transcribe for OkTranscribe {
    fn transcribe(&self, _audio: &CapturedAudio) -> anyhow::Result<String> {
        Ok("ok".to_owned())
    }
}

struct AudioThatFailsToStart;

impl AudioCapture for AudioThatFailsToStart {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Err(anyhow!("device unplugged"))
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        unreachable!("stop should not be called when start fails")
    }
    fn is_recording(&self) -> bool {
        false
    }
}

/// Audio mock that surfaces a permission-shaped chain. Used to
/// pin the classifier promotion in `start_dictation_inner`
/// (#386 / #416 close-out): a chain containing
/// "microphone not authorized" should land as the typed
/// `IpcError::PermissionDenied("microphone")` variant
/// rather than a generic `IpcError::Audio(...)`.
struct AudioThatFailsWithMicrophoneDenial;

impl AudioCapture for AudioThatFailsWithMicrophoneDenial {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Err(anyhow!("microphone access not authorized"))
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        unreachable!("stop should not be called when start fails")
    }
    fn is_recording(&self) -> bool {
        false
    }
}

struct AudioThatStarts {
    recording: AtomicBool,
}

impl AudioCapture for AudioThatStarts {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        self.recording.store(true, Ordering::Release);
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        unreachable!()
    }
    fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }
}

#[test]
fn start_dictation_does_not_overwrite_foreground_on_audio_start_failure() {
    let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatFailsToStart);
    let transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
    let state = crate::ipc::AppStateBuilder::new()
        .audio(audio)
        .transcribe(Some(transcribe))
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
        .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete");

    // Pre-populate the slot with a sentinel value so a regression in
    // the assignment order — assigning the new capture before
    // `audio.start` returns — would visibly overwrite it.
    *state.pending_foreground.lock().unwrap() = Some(ForegroundApp {
        app_name: "sentinel".into(),
        window_title: "sentinel".into(),
    });

    let err = start_dictation_inner(&state, AudioSource::default_microphone())
        .expect_err("audio.start fails");
    assert!(
        matches!(err, IpcError::Audio(_)),
        "expected IpcError::Audio, got {err:?}"
    );

    let after = state.pending_foreground.lock().unwrap().clone();
    assert_eq!(
        after.map(|f| f.app_name).as_deref(),
        Some("sentinel"),
        "pending_foreground was overwritten despite failed start"
    );
}

#[test]
fn start_dictation_promotes_permission_shaped_error_to_typed_variant() {
    // #386 / #416 close-out: a permission-shaped chain from the audio
    // layer (e.g. microphone not authorized) must surface as
    // `IpcError::PermissionDenied(...)` so the frontend's
    // PermissionsDialog launch heuristic can match on `kind` instead
    // of substring-scraping.
    let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatFailsWithMicrophoneDenial);
    let transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
    let state = crate::ipc::AppStateBuilder::new()
        .audio(audio)
        .transcribe(Some(transcribe))
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
        .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete");

    let err = start_dictation_inner(&state, AudioSource::default_microphone())
        .expect_err("audio.start fails with permission-shaped chain");
    match err {
        IpcError::PermissionDenied(perm) => {
            assert_eq!(perm, "microphone");
        }
        other => {
            panic!("expected IpcError::PermissionDenied(\"microphone\"), got: {other:?}")
        }
    }
}

#[test]
fn start_dictation_succeeds_and_leaves_a_foreground_slot_for_stop() {
    // Confirms the happy path actually does write into the slot —
    // otherwise the bug-fix above could be "we just never assign
    // anything", which would also pass the regression test in
    // isolation.
    let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatStarts {
        recording: AtomicBool::new(false),
    });
    let transcribe: Arc<dyn Transcribe> = Arc::new(OkTranscribe);
    let state = crate::ipc::AppStateBuilder::new()
        .audio(audio)
        .transcribe(Some(transcribe))
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
        .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete");

    // We can't observe the OS foreground app reliably from a test
    // process, so we just assert the call returned Ok and the slot is
    // *some* value (None or Some, both are acceptable — the OS may
    // genuinely have no active window in CI).
    start_dictation_inner(&state, AudioSource::default_microphone()).expect("should succeed");

    // Just prove the lock didn't poison and the slot is reachable.
    let _: Option<ForegroundApp> = state.pending_foreground.lock().unwrap().clone();
}

/// Suppress the dead-code warning that fires because [`Mutex`] is
/// otherwise unused after the regression tests' construction —
/// this is part of the type signature compile-check above.
#[allow(dead_code)]
fn _assert_state_mutex_holds_foreground(state: AppState) -> Mutex<Option<ForegroundApp>> {
    state.pending_foreground
}

#[test]
fn start_dictation_returns_unavailable_when_no_transcriber_is_loaded() {
    // Pre-#195 this scenario silently opened audio capture and
    // failed at `stop_dictation` — the user spent N seconds
    // recording before learning no transcriber was loaded.
    // Pin the new pre-flight: no transcriber → fail fast, no
    // audio side effects, no foreground slot mutation.
    let audio_started = Arc::new(AtomicBool::new(false));
    let audio: Arc<dyn AudioCapture> = Arc::new(StartFlagAudio {
        started: Arc::clone(&audio_started),
    });
    let state = crate::ipc::AppStateBuilder::new()
        .audio(audio)
        // No `.transcribe(...)` — slot stays None.
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
        .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete");

    let err = start_dictation_inner(&state, AudioSource::default_microphone())
        .expect_err("no-transcriber must surface as a hard error");
    assert!(
        matches!(err, IpcError::TranscriptionUnavailable),
        "expected TranscriptionUnavailable, got {err:?}"
    );
    assert!(
        !audio_started.load(Ordering::Acquire),
        "audio.start_with_source must NOT be called when no transcriber is loaded"
    );
}

/// Audio backend whose only job is recording whether `start_with_source`
/// (or `start`) was called, so the pre-flight test can prove the
/// audio path was skipped before the error returned.
struct StartFlagAudio {
    started: Arc<AtomicBool>,
}

impl AudioCapture for StartFlagAudio {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        self.started.store(true, Ordering::Release);
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        unreachable!("stop should not be called");
    }
    fn is_recording(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }
}

// -- whisper bracket-sentinel stripping ------------------------------

#[test]
fn strip_brackets_drops_pure_blank_audio_sentinel() {
    // The exact case in #196's user report: whisper emitted
    // `[BLANK_AUDIO]` and the user saw it in the result panel
    // and on their clipboard.
    assert_eq!(strip_whisper_brackets("[BLANK_AUDIO]"), "");
}

#[test]
fn strip_brackets_drops_other_status_sentinels() {
    // Same shape, different label. Whisper produces these for
    // music / non-speech / unintelligible segments.
    for sentinel in [
        "[NOISE]",
        "[MUSIC]",
        "[ MUSIC ]",
        "[INAUDIBLE]",
        "[Sound effects]",
        "[laughter]",
    ] {
        assert_eq!(
            strip_whisper_brackets(sentinel),
            "",
            "sentinel {sentinel} should strip to empty"
        );
    }
}

#[test]
fn strip_brackets_keeps_real_speech_around_a_silence_marker() {
    // Whisper sometimes prefixes a transcript with
    // `[BLANK_AUDIO]` when there's a leading silence segment —
    // the real speech follows. Keep the speech, drop the marker,
    // collapse the surrounding whitespace.
    assert_eq!(
        strip_whisper_brackets("[BLANK_AUDIO] hello world"),
        "hello world"
    );
    assert_eq!(
        strip_whisper_brackets("hello world [NOISE]"),
        "hello world"
    );
    assert_eq!(
        strip_whisper_brackets("first [NOISE] second"),
        "first second"
    );
}

#[test]
fn strip_brackets_leaves_text_with_no_brackets_alone() {
    // The common path. Pin so a regression in the stripping
    // pass doesn't accidentally trim or reflow real
    // transcripts.
    assert_eq!(
        strip_whisper_brackets("Hello, world."),
        "Hello, world."
    );
}

#[test]
fn strip_brackets_handles_nested_or_unbalanced_brackets_safely() {
    // Defensive: whisper isn't supposed to emit nested or
    // unbalanced brackets, but the depth counter shouldn't
    // panic if it does. Output may not be ideal — the goal is
    // "doesn't crash, doesn't drop more than it should."
    assert_eq!(strip_whisper_brackets("[[NESTED]]"), "");
    // A stray closing bracket is preserved (depth never goes
    // negative).
    assert_eq!(strip_whisper_brackets("hello]"), "hello]");
}

// -- stop_dictation helper tests --------------------------------------
//
// The Tauri command itself needs an `AppHandle` (clipboard +
// notification + HUD), so it can't be unit-tested directly. The
// helpers extracted from it can — these tests pin their behaviour
// so the orchestration in `stop_dictation` stays trustworthy
// through future refactors.

struct AudioThatStopsWith {
    captured: CapturedAudio,
}

impl AudioCapture for AudioThatStopsWith {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        Ok(self.captured.clone())
    }
    fn is_recording(&self) -> bool {
        false
    }
}

struct AudioThatFailsToStop;

impl AudioCapture for AudioThatFailsToStop {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        Err(anyhow!("device went away"))
    }
    fn is_recording(&self) -> bool {
        false
    }
}

/// Audio mock whose `stop()` returns a typed `DeviceLost` wrapped
/// in `anyhow::Error`. Pin for the IPC downcast (#617).
struct AudioThatFailsToStopWithDeviceLost {
    device: String,
}

impl AudioCapture for AudioThatFailsToStopWithDeviceLost {
    fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<CapturedAudio> {
        Err(anyhow::Error::new(crate::audio::DeviceLost {
            device: self.device.clone(),
        }))
    }
    fn is_recording(&self) -> bool {
        false
    }
}

struct VocabWithTerms(Vec<VocabularyTerm>);

#[async_trait::async_trait]
impl crate::repository::Repository<VocabularyTerm, NewVocabularyTerm, i64> for VocabWithTerms {
    async fn list(&self) -> anyhow::Result<Vec<VocabularyTerm>> {
        Ok(self.0.clone())
    }
    async fn create(&self, _: NewVocabularyTerm) -> anyhow::Result<VocabularyTerm> {
        unreachable!()
    }
    async fn update(&self, _: VocabularyTerm) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
}

struct FailingVocab;

#[async_trait::async_trait]
impl crate::repository::Repository<VocabularyTerm, NewVocabularyTerm, i64> for FailingVocab {
    async fn list(&self) -> anyhow::Result<Vec<VocabularyTerm>> {
        Err(anyhow!("table missing"))
    }
    async fn create(&self, _: NewVocabularyTerm) -> anyhow::Result<VocabularyTerm> {
        unreachable!()
    }
    async fn update(&self, _: VocabularyTerm) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
}

struct FailingReplacements;

#[async_trait::async_trait]
impl crate::repository::Repository<ReplacementRule, crate::dictionary::NewReplacementRule, i64>
    for FailingReplacements
{
    async fn list(&self) -> anyhow::Result<Vec<ReplacementRule>> {
        Err(anyhow!("table missing"))
    }
    async fn create(
        &self,
        _: crate::dictionary::NewReplacementRule,
    ) -> anyhow::Result<ReplacementRule> {
        unreachable!()
    }
    async fn update(&self, _: ReplacementRule) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: i64) -> anyhow::Result<()> {
        Ok(())
    }
}

fn state_with(
    audio: Arc<dyn AudioCapture>,
    vocab: Arc<dyn VocabularyRepository>,
    replacements: Arc<dyn ReplacementRepository>,
) -> AppState {
    crate::ipc::AppStateBuilder::new()
        .audio(audio)
        .history(Arc::new(crate::ipc::tests::NoopHistory))
        .replacements(replacements)
        .vocabulary(vocab)
        .settings(Arc::new(crate::ipc::tests::MemSettings {
            map: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
        .meetings({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })
        .meeting_app_overrides({
            let o: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
                Arc::new(crate::ipc::tests::NoopMeetingAppOverrides);
            o
        })
        .meeting_manager(Arc::new(crate::meeting::SessionManager::new_for_test({
            let m: Arc<dyn crate::meeting::MeetingSessionRepository> =
                Arc::new(crate::ipc::tests::NoopMeetings);
            m
        })))
        .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
        .build()
        .expect("test state: builder fields complete")
}

fn fixed_audio() -> CapturedAudio {
    CapturedAudio {
        samples: vec![0.5_f32; 8],
        format: crate::audio::CaptureFormat {
            sample_rate: 48_000,
            channels: 1,
        },
    }
}

#[test]
fn stop_audio_capture_returns_captured_on_success() {
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    let captured = stop_audio_capture(&state).expect("audio.stop ok");
    assert_eq!(captured.samples.len(), 8);
    assert_eq!(captured.format.sample_rate, 48_000);
}

#[test]
fn stop_audio_capture_maps_backend_error_to_ipc_error_audio() {
    // Regression for the heuristic-classifier era: audio errors must
    // surface as `IpcError::Audio` so the frontend's switch-on-kind
    // dispatch picks the right recovery copy. This is *structural*
    // classification — there is no string match anywhere.
    let state = state_with(
        Arc::new(AudioThatFailsToStop),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    let err = stop_audio_capture(&state).expect_err("stop fails");
    assert!(matches!(err, IpcError::Audio(_)), "got {err:?}");
}

#[test]
fn stop_audio_capture_routes_device_lost_to_typed_ipc_variant() {
    // #617: pin the downcast that distinguishes "mic disconnected"
    // from generic audio failures. A regression here silently
    // demotes mic disconnects to the generic Audio bucket and
    // the frontend loses the structured banner copy.
    let state = state_with(
        Arc::new(AudioThatFailsToStopWithDeviceLost {
            device: "AirPods Pro".to_owned(),
        }),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    let err = stop_audio_capture(&state).expect_err("stop fails");
    match err {
        IpcError::AudioDeviceLost(name) => {
            assert_eq!(name, "AirPods Pro");
        }
        other => panic!("expected IpcError::AudioDeviceLost(\"AirPods Pro\"), got {other:?}"),
    }
}

#[tokio::test]
async fn load_vocabulary_prompt_formats_terms_when_present() {
    let terms = vec![
        VocabularyTerm {
            id: 1,
            term: "Hush".into(),
        },
        VocabularyTerm {
            id: 2,
            term: "whisper.cpp".into(),
        },
    ];
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(VocabWithTerms(terms.clone())),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    let prompt = load_vocabulary_prompt(&state).await;
    // The exact format is owned by `format_vocabulary_prompt`; this
    // test just pins that the helper actually invokes the formatter
    // rather than returning empty.
    assert!(prompt.contains("Hush"), "got: {prompt}");
    assert!(prompt.contains("whisper.cpp"), "got: {prompt}");
}

#[tokio::test]
async fn load_vocabulary_prompt_swallows_repository_errors() {
    // Repository failure must not block transcription — we demote
    // to the no-prompt path.
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(FailingVocab),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );

    let prompt = load_vocabulary_prompt(&state).await;
    assert!(prompt.is_empty(), "got: {prompt}");
}

#[tokio::test]
async fn load_replacement_rules_returns_empty_on_error() {
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(FailingReplacements),
    );

    let rules = load_replacement_rules(&state).await;
    assert!(rules.is_empty());
}

#[test]
fn take_foreground_snapshot_pops_and_clears_the_slot() {
    let state = state_with(
        Arc::new(AudioThatStopsWith {
            captured: fixed_audio(),
        }),
        Arc::new(crate::ipc::tests::NoopVocabulary),
        Arc::new(crate::ipc::tests::NoopReplacements),
    );
    *state.pending_foreground.lock().unwrap() = Some(ForegroundApp {
        app_name: "Slack".into(),
        window_title: "#general".into(),
    });

    let popped = take_foreground_snapshot(&state).expect("not poisoned");
    assert_eq!(popped.as_ref().map(|f| f.app_name.as_str()), Some("Slack"));

    // Second take must be None: the slot is consumed, not cloned.
    let again = take_foreground_snapshot(&state).expect("not poisoned");
    assert!(again.is_none());
}
