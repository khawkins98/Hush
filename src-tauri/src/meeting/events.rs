//! Meeting-module event payloads and emission helpers.
//!
//! Centralizes the wire shapes passed through [`crate::events::EventEmitter`]
//! so lifecycle and pump code share one source of truth for event names and
//! payload fields.

use serde::Serialize;

use crate::events::EventEmitter;

fn emit_payload<T: Serialize>(event_emitter: &dyn EventEmitter, event: &str, payload: &T) {
    match serde_json::to_value(payload) {
        Ok(value) => event_emitter.emit_json(event, value),
        Err(error) => {
            tracing::warn!(
                error = ?error,
                event = %event,
                "EventEmitter: payload serialization failed; event dropped"
            );
        }
    }
}

/// Fired when the meeting pump drops a per-source capture path mid-session
/// (TCC revoke, device unplug, inference panic). Without this signal the
/// panel keeps showing "recording from mic + system audio" while one of
/// those sources has silently gone dead.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MeetingSourceFailedPayload<'a> {
    pub session_id: i64,
    pub source_kind: &'a str,
    pub reason: &'a str,
    /// `true` when the failure came from `audio::DeviceLost` — the user's
    /// mic / AirPods disconnected mid-session or vanished during pre-warm.
    /// Lets the frontend branch on a typed flag instead of substring-
    /// matching `reason`, which #617 flagged as fragile to backend wording
    /// changes.
    pub device_lost: bool,
}

/// Tauri event name the pump fires when [`MeetingSourceFailedPayload`] is
/// the wire body. Centralized so the frontend's listener
/// (`Events.MeetingSourceFailed`) and the backend emit sites can't drift.
pub(super) const MEETING_SOURCE_FAILED_EVENT: &str = "meeting:source-failed";

/// Payload emitted by [`crate::meeting::SessionManager::start_manual`] when a
/// new session opens successfully (both manual button-press and HAL
/// auto-start paths). Centralized so the frontend's listener
/// (`Events.MeetingSessionStarted`) and every backend emit site stay in sync.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MeetingSessionStartedPayload {
    pub session_id: i64,
}

/// Tauri event name for [`MeetingSessionStartedPayload`]. Matches the
/// TypeScript constant `Events.MeetingSessionStarted` in `events.ts`.
pub(super) const MEETING_SESSION_STARTED_EVENT: &str = "meeting:session-started";

/// Fired when the pump fails to persist a finished utterance to the DB.
/// Re-uses the `dictation:meeting-append-failed` wire name so the existing
/// frontend banner listener in `meeting-sessions.svelte.ts` picks it up
/// without a new event registration.  Payload: `{ error: String }`.
pub(super) const MEETING_APPEND_FAILED_EVENT: &str = "dictation:meeting-append-failed";

/// Payload for [`MEETING_APPEND_FAILED_EVENT`].
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MeetingAppendFailedPayload {
    pub error: String,
}

pub(super) fn emit_utterance_append_failed(event_emitter: &dyn EventEmitter, error: &str) {
    emit_payload(
        event_emitter,
        MEETING_APPEND_FAILED_EVENT,
        &MeetingAppendFailedPayload {
            error: error.to_owned(),
        },
    );
}

/// Fired when a mic source is lost mid-session and the pump has switched to
/// the system default or has no fallback. Payload: [`AudioDeviceLostPayload`].
pub(super) const AUDIO_DEVICE_LOST_EVENT: &str = "audio:device-lost";

/// Fired when the original mic is detected on replug and the pump has swapped
/// back. Payload: [`AudioDeviceRestoredPayload`].
pub(super) const AUDIO_DEVICE_RESTORED_EVENT: &str = "audio:device-restored";

/// Payload for [`AUDIO_DEVICE_LOST_EVENT`].
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AudioDeviceLostPayload<'a> {
    pub session_id: i64,
    pub source_kind: &'a str,
    pub lost_device: &'a str,
    /// `Some` when fallback succeeded (name of the device now recording).
    pub new_device: Option<&'a str>,
}

/// Payload for [`AUDIO_DEVICE_RESTORED_EVENT`].
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AudioDeviceRestoredPayload<'a> {
    pub session_id: i64,
    pub source_kind: &'a str,
    pub restored_device: &'a str,
}

pub(super) fn emit_meeting_source_failed(
    event_emitter: &dyn EventEmitter,
    session_id: i64,
    source_kind: &str,
    reason: &str,
    device_lost: bool,
) {
    emit_payload(
        event_emitter,
        MEETING_SOURCE_FAILED_EVENT,
        &MeetingSourceFailedPayload {
            session_id,
            source_kind,
            reason,
            device_lost,
        },
    );
}

/// Fired when the meeting pump finishes (normal stop, auto-stop, or error).
/// Lets the frontend clear `meeting.activeId` even when the stop was backend-
/// driven (device failure, auto-stop) and no explicit `stopSession()` call
/// was made from the UI (#799).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MeetingSessionEndedPayload {
    pub session_id: i64,
}

/// Tauri event name for [`MeetingSessionEndedPayload`]. Matches the
/// TypeScript constant `Events.MeetingSessionEnded` in `events.ts`.
pub(super) const MEETING_SESSION_ENDED_EVENT: &str = "meeting:session-ended";

pub(super) fn emit_meeting_session_ended(event_emitter: &dyn EventEmitter, session_id: i64) {
    emit_payload(
        event_emitter,
        MEETING_SESSION_ENDED_EVENT,
        &MeetingSessionEndedPayload { session_id },
    );
}

pub(super) fn emit_meeting_session_started(event_emitter: &dyn EventEmitter, session_id: i64) {
    emit_payload(
        event_emitter,
        MEETING_SESSION_STARTED_EVENT,
        &MeetingSessionStartedPayload { session_id },
    );
}

pub(super) fn emit_audio_device_lost(
    event_emitter: &dyn EventEmitter,
    session_id: i64,
    source_kind: &str,
    lost_device: &str,
    new_device: Option<&str>,
) {
    emit_payload(
        event_emitter,
        AUDIO_DEVICE_LOST_EVENT,
        &AudioDeviceLostPayload {
            session_id,
            source_kind,
            lost_device,
            new_device,
        },
    );
}

pub(super) fn emit_audio_device_restored(
    event_emitter: &dyn EventEmitter,
    session_id: i64,
    source_kind: &str,
    restored_device: &str,
) {
    emit_payload(
        event_emitter,
        AUDIO_DEVICE_RESTORED_EVENT,
        &AudioDeviceRestoredPayload {
            session_id,
            source_kind,
            restored_device,
        },
    );
}
