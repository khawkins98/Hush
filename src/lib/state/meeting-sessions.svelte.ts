import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { MeetingCopyNotice } from "$lib/MeetingSection.svelte";
import {
  formatErrorDisplay,
  type ErrorDisplay,
} from "$lib/errors";
import { Events } from "$lib/events";
import { joinUtterances } from "$lib/transcript-format";
import type {
  ActiveMeetingSession,
  AudioSource,
  MeetingSession,
  MeetingSessionDetail,
} from "$lib/types";
import { audio } from "$lib/state/audio.svelte";

let meetingSessions = $state<MeetingSession[]>([]);
let meetingSessionsLoaded = $state(false);
let meetingSessionsError = $state<ErrorDisplay | null>(null);
let meetingActiveId = $state<number | null>(null);
let meetingActiveDetail = $state<MeetingSessionDetail | null>(null);
let meetingBusy = $state(false);
let meetingCopyNotice = $state<MeetingCopyNotice | null>(null);
let pendingPermissionsDialogIntro = $state<string | null>(null);
/// Source-failed banner text set when the backend emits
/// `meeting:source-failed` during an active session (#533).
/// Cleared when the session ends or the user dismisses it.
let meetingSourceFailedNotice = $state<string | null>(null);
/// Append-failed banner text set when the backend emits
/// `dictation:meeting-append-failed` (#696). Fires when a
/// transcription completed but the utterance couldn't be written
/// to the active meeting session. The transcript still landed on
/// the clipboard; this warns the user the session log is incomplete.
/// Cleared when the session ends or the user dismisses it.
let meetingAppendFailedNotice = $state<string | null>(null);
/// Latest-wins guard for `meeting.refresh()`. Incremented on every
/// call so a stale response from a previous in-flight request can't
/// overwrite state already set by a newer one.
let meetingRefreshSeq = 0;
/// Latest-wins guard for `meeting.refreshActiveDetail()`. Same pattern as
/// `meetingRefreshSeq` — rapid session-switching can produce an in-flight
/// detail fetch from the previous session that should not overwrite the
/// already-resolved detail for the current one.
let meetingActiveDetailSeq = 0;
/// Current search query mirror. Kept in sync by history.setSearchQuery()
/// so meeting.refresh() uses the right filter without importing history.
let meetingSearchQuery = "";

export const meeting = {
  get sessions() {
    return meetingSessions;
  },
  set sessions(val: MeetingSession[]) {
    meetingSessions = val;
  },
  get sessionsLoaded() {
    return meetingSessionsLoaded;
  },
  set sessionsLoaded(val: boolean) {
    meetingSessionsLoaded = val;
  },
  get error() {
    return meetingSessionsError;
  },
  set error(val: ErrorDisplay | null) {
    meetingSessionsError = val;
  },
  get activeId() {
    return meetingActiveId;
  },
  set activeId(val: number | null) {
    meetingActiveId = val;
  },
  get activeDetail() {
    return meetingActiveDetail;
  },
  set activeDetail(val: MeetingSessionDetail | null) {
    meetingActiveDetail = val;
  },
  get busy() {
    return meetingBusy;
  },
  set busy(val: boolean) {
    meetingBusy = val;
  },
  get copyNotice() {
    return meetingCopyNotice;
  },
  set copyNotice(val: MeetingCopyNotice | null) {
    meetingCopyNotice = val;
  },
  get pendingPermissionsDialogIntro() {
    return pendingPermissionsDialogIntro;
  },
  set pendingPermissionsDialogIntro(val: string | null) {
    pendingPermissionsDialogIntro = val;
  },
  get sourceFailedNotice() {
    return meetingSourceFailedNotice;
  },
  set sourceFailedNotice(val: string | null) {
    meetingSourceFailedNotice = val;
  },
  get appendFailedNotice() {
    return meetingAppendFailedNotice;
  },
  set appendFailedNotice(val: string | null) {
    meetingAppendFailedNotice = val;
  },
  /** Keep the local search-query mirror in sync. Called by
   *  history.setSearchQuery() so this module doesn't need to import
   *  history (breaking the previous circular dependency). */
  set searchQuery(val: string) {
    meetingSearchQuery = val;
  },
  async refresh() {
    meetingRefreshSeq += 1;
    const seq = meetingRefreshSeq;
    try {
      // Use the cheaper list IPC when no search query is active — avoids
      // spinning up the FTS5 engine on every idle poll.
      const sessionsP = meetingSearchQuery
        ? invoke<MeetingSession[]>("meeting_sessions_search", {
            query: meetingSearchQuery,
          })
        : invoke<MeetingSession[]>("meeting_sessions_list");
      const [sessions, active] = await Promise.all([
        sessionsP,
        invoke<ActiveMeetingSession>("meeting_active_session"),
      ]);
      // Discard if a newer refresh already completed.
      if (seq !== meetingRefreshSeq) return;
      meetingSessions = sessions;
      meetingActiveId = active.active;
      meetingSessionsError = null;
    } catch (e) {
      if (seq !== meetingRefreshSeq) return;
      meetingSessionsError = formatErrorDisplay(e);
    } finally {
      meetingSessionsLoaded = true;
    }
  },
  async refreshActiveDetail(id: number) {
    meetingActiveDetailSeq += 1;
    const seq = meetingActiveDetailSeq;
    try {
      const detail = await invoke<MeetingSessionDetail>("meeting_session_get", { id });
      // Discard if a newer refreshActiveDetail already completed.
      if (seq !== meetingActiveDetailSeq) return;
      meetingActiveDetail = detail;
    } catch (e) {
      if (seq !== meetingActiveDetailSeq) return;
      // Transient poll failures are logged but not surfaced — the next
      // poll cycle will self-heal and we don't want to clobber the
      // session-list error field with an ephemeral detail-fetch blip.
      console.warn("refreshActiveDetail failed:", e);
    }
  },
  clearActiveDetail() {
    meetingActiveDetail = null;
  },
  async deleteSession(session: MeetingSession) {
    try {
      await invoke("meeting_session_delete", { id: session.id });
      meetingSessions = meetingSessions.filter((s) => s.id !== session.id);
    } catch (e) {
      meetingSessionsError = formatErrorDisplay(e);
    }
  },
  async loadSessionDetail(id: number): Promise<MeetingSessionDetail> {
    try {
      const detail = await invoke<MeetingSessionDetail>("meeting_session_get", {
        id,
      });
      meetingSessionsError = null;
      return detail;
    } catch (e) {
      meetingSessionsError = formatErrorDisplay(e);
      throw e;
    }
  },
  async copyToClipboard(id: number): Promise<void> {
    try {
      const detail = await invoke<MeetingSessionDetail>("meeting_session_get", {
        id,
      });
      const finals = detail.utterances.filter((u) => u.isFinal);
      if (finals.length === 0) {
        return;
      }
      const joined = joinUtterances(finals, "\n\n");
      await navigator.clipboard.writeText(joined);
      meeting.setNotice({
        kind: "success",
        message:
          "Copied to clipboard — full transcript also saved to History below.",
      });
    } catch (err) {
      console.warn(
        "[hush] meeting transcript clipboard copy failed; user can export from History",
        err,
      );
      meeting.setNotice({
        kind: "failure",
        message:
          "Copy failed — the transcript is still in History. Try using the Export button on the row instead.",
      });
    }
  },
  setNotice(notice: MeetingCopyNotice | null) {
    meetingCopyNotice = notice;
  },
  async startSession() {
    meetingBusy = true;
    try {
      const sources: AudioSource[] = [];
      if (audio.meetingMicId !== null) {
        sources.push({ kind: "microphone", deviceId: audio.meetingMicId });
      }
      const sys = audio.findSystemAudio();
      if (audio.meetingIncludeSystemAudio && sys?.isSupported) {
        sources.push({ kind: "system-audio" });
      }
      if (sources.length === 0) {
        meetingSessionsError = {
          headline: "No audio sources selected",
          hint: "Pick at least one source (microphone or system audio) before starting a session.",
        };
        return;
      }
      await invoke("meeting_start_manual", { sources, appName: null });
      await meeting.refresh();
    } catch (e) {
      meetingSessionsError = formatErrorDisplay(e);
    } finally {
      meetingBusy = false;
    }
  },
  async stopSession() {
    meetingBusy = true;
    try {
      await invoke("meeting_stop_manual");
      await meeting.refresh();
      // Clear any banners — the session is done.
      meetingSourceFailedNotice = null;
      meetingAppendFailedNotice = null;
    } catch (e) {
      meetingSessionsError = formatErrorDisplay(e);
    } finally {
      meetingBusy = false;
    }
  },
  clearPendingPermissionsDialog() {
    pendingPermissionsDialogIntro = null;
  },
  /// Register the three meeting-session Tauri event listeners and
  /// return a cleanup function. Call from `AppLifecycle.svelte`'s
  /// `onMount`; call the returned cleanup from `onDestroy`.
  ///
  /// Owned here (rather than in `AppLifecycle`) because all three
  /// events update only meeting state — keeping them co-located with
  /// the state they mutate means fewer files to touch if the event
  /// shapes change.
  ///
  /// `AudioDeviceLost` / `AudioDeviceRestored` remain in
  /// `AppLifecycle` because they update BOTH `audio.inputDeviceName`
  /// AND `meeting.sourceFailedNotice` across two state modules.
  async initSessionListeners(): Promise<() => void> {
    const unlistenStarted = await listen<{ sessionId: number }>(
      Events.MeetingSessionStarted,
      (e) => {
        meeting.activeId = e.payload.sessionId;
        void meeting.refresh();
      },
    );

    const unlistenSourceFailed = await listen<{
      sessionId: number;
      sourceKind: string;
      reason: string;
      deviceLost: boolean;
    }>(Events.MeetingSourceFailed, (e) => {
      console.debug(
        "[MeetingSourceFailed]",
        e.payload.sourceKind,
        e.payload.reason,
        "deviceLost:",
        e.payload.deviceLost,
        "sessionId:",
        e.payload.sessionId,
      );
      const label =
        e.payload.sourceKind === "mic" ? "Microphone" : "System audio";
      // Mirrors the multi-source detection in startSession().
      const wasMultiSource =
        audio.meetingMicId !== null &&
        audio.meetingIncludeSystemAudio &&
        audio.findSystemAudio()?.isSupported === true;
      const otherSourceLabel =
        e.payload.sourceKind === "mic" ? "system audio" : "microphone";

      let verb: string;
      if (e.payload.deviceLost) {
        verb = wasMultiSource
          ? `disconnected — ${otherSourceLabel} still recording`
          : "disconnected — recording stopped";
      } else if (e.payload.reason.includes("at session start")) {
        verb = "couldn't start";
      } else {
        verb = "stopped transcribing";
      }
      meeting.sourceFailedNotice = `${label} ${verb}.`;
    });

    const unlistenEnded = await listen<{ sessionId: number }>(
      Events.MeetingSessionEnded,
      (e) => {
        // Clear activeId if it still matches the session that just ended.
        // Guards against a race where the user stopped manually (already
        // cleared activeId) before this event arrives (#799).
        if (meeting.activeId === e.payload.sessionId) {
          meeting.activeId = null;
          void meeting.refresh();
        }
      },
    );

    const unlistenAppendFailed = await listen<{ error: string }>(
      Events.DictationMeetingAppendFailed,
      (e) => {
        console.warn("[DictationMeetingAppendFailed]", e.payload.error);
        meeting.appendFailedNotice =
          "A transcription couldn't be saved to your meeting session. The text is still on your clipboard.";
      },
    );

    return () => {
      unlistenStarted();
      unlistenSourceFailed();
      unlistenEnded();
      unlistenAppendFailed();
    };
  },
};
