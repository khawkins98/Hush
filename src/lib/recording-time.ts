// Pure helpers for the recording-duration timer (#990).
//
// The Transcribe panel's timer must survive component remounts (tab
// navigation unmounts RecordPanel and remounts it on return) and must
// anchor to when the recording actually *started*, not when the
// component happened to mount. These helpers are side-effect-free so
// the resolution rules are unit-testable without a browser.
//
// Start-time resolution order (see `resolveRecordingStartMs`):
//   1. The dictation state module's `phase.startedAtMs` — module-level
//      state that survives remounts. Covers hotkey dictation and
//      manually-started (Record button) meetings.
//   2. The active meeting session's backend-persisted `startedAt`
//      (ISO string, polled into `meeting.activeDetail`). Covers
//      auto-detected meetings, where the dictation phase stays idle.
//
// The HUD solved the same anchoring problem via the `hud:state` event's
// `startedAtMs` field (#481), but an event push is lost when a component
// remounts after the event fired — RecordPanel needs this pull-based
// resolution instead.

/// Resolve the wall-clock ms timestamp the active recording started at,
/// or `null` when it cannot be determined (no recording, or the meeting
/// detail hasn't been polled in yet).
export function resolveRecordingStartMs(
  dictationStartedAtMs: number | null,
  meetingStartedAt: string | null | undefined,
): number | null {
  if (dictationStartedAtMs !== null) {
    return dictationStartedAtMs;
  }
  if (meetingStartedAt) {
    const parsed = Date.parse(meetingStartedAt);
    if (!Number.isNaN(parsed)) {
      return parsed;
    }
  }
  return null;
}

/// Format an elapsed duration as `MM:SS`, switching to `H:MM:SS` past
/// one hour. Negative inputs clamp to zero (clock skew between the
/// backend-persisted start and the frontend clock must never render
/// a negative timer).
export function formatElapsed(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const mm = minutes.toString().padStart(2, "0");
  const ss = seconds.toString().padStart(2, "0");
  if (hours > 0) return `${hours}:${mm}:${ss}`;
  return `${mm}:${ss}`;
}
