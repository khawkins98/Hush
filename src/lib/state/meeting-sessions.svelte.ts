import { invoke } from "@tauri-apps/api/core";

import type { MeetingCopyNotice } from "$lib/MeetingSection.svelte";
import {
  formatErrorDisplay,
  type ErrorDisplay,
} from "$lib/errors";
import { joinUtterances } from "$lib/transcript-format";
import type {
  ActiveMeetingSession,
  AudioSource,
  MeetingSession,
  MeetingSessionDetail,
} from "$lib/types";
import { audio } from "$lib/state/audio.svelte";
import { history } from "$lib/state/history.svelte";

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
  async refresh() {
    try {
      const [sessions, active] = await Promise.all([
        invoke<MeetingSession[]>("meeting_sessions_search", {
          query: history.historyQuery,
        }),
        invoke<ActiveMeetingSession>("meeting_active_session"),
      ]);
      meetingSessions = sessions;
      meetingActiveId = active.active;
      meetingSessionsError = null;
    } catch (e) {
      meetingSessionsError = formatErrorDisplay(e);
    } finally {
      meetingSessionsLoaded = true;
    }
  },
  async refreshActiveDetail(id: number) {
    try {
      meetingActiveDetail = await invoke<MeetingSessionDetail>(
        "meeting_session_get",
        { id },
      );
    } catch (e) {
      meetingSessionsError = formatErrorDisplay(e);
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
      // Clear any source-failed banner — the session is done.
      meetingSourceFailedNotice = null;
    } catch (e) {
      meetingSessionsError = formatErrorDisplay(e);
    } finally {
      meetingBusy = false;
    }
  },
  clearPendingPermissionsDialog() {
    pendingPermissionsDialogIntro = null;
  },
};
