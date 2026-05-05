import { invoke } from "@tauri-apps/api/core";

import {
  formatErrorDisplay,
  isPermissionShapedError,
  type ErrorDisplay,
} from "$lib/errors";
import { joinUtterances } from "$lib/transcript-format";
import type {
  AudioSourceListing,
  DictationResult,
  MeetingSession,
  MeetingSessionDetail,
  ModelCard,
} from "$lib/types";
import { audio } from "$lib/state/audio.svelte";
import { history } from "$lib/state/history.svelte";
import { meeting } from "$lib/state/meeting-sessions.svelte";

let recording = $state(false);
let busy = $state(false);
let result = $state<DictationResult | null>(null);
let error = $state<ErrorDisplay | null>(null);
let recordMode = $state<"dictation" | "meeting" | null>(null);
let lastMeetingId = $state<number | null>(null);
let lastRecordingStartedAtMs = $state<number | null>(null);
let models = $state<ModelCard[]>([]);
let modelsLoaded = $state(false);
let appProfileNotice = $state<string | null>(null);
let appProfileNoticeTimer = $state<ReturnType<typeof setTimeout> | null>(null);
let pendingPermissionsDialogIntro = $state<string | null>(null);

let transcribing = $derived(busy && !recording && !!result === false);
let noModelInstalled = $derived(
  modelsLoaded && models.length > 0 && !models.some((m) => m.isDownloaded),
);
let activeModel = $derived(
  models.find((m) => m.isSelected && m.isDownloaded) ?? null,
);

export const dictation = {
  get recording() {
    return recording;
  },
  set recording(val: boolean) {
    recording = val;
  },
  get busy() {
    return busy;
  },
  set busy(val: boolean) {
    busy = val;
  },
  get result() {
    return result;
  },
  set result(val: DictationResult | null) {
    result = val;
  },
  get error() {
    return error;
  },
  set error(val: ErrorDisplay | null) {
    error = val;
  },
  get recordMode() {
    return recordMode;
  },
  set recordMode(val: "dictation" | "meeting" | null) {
    recordMode = val;
  },
  get lastMeetingId() {
    return lastMeetingId;
  },
  set lastMeetingId(val: number | null) {
    lastMeetingId = val;
  },
  get lastRecordingStartedAtMs() {
    return lastRecordingStartedAtMs;
  },
  set lastRecordingStartedAtMs(val: number | null) {
    lastRecordingStartedAtMs = val;
  },
  get models() {
    return models;
  },
  set models(val: ModelCard[]) {
    models = val;
  },
  get modelsLoaded() {
    return modelsLoaded;
  },
  set modelsLoaded(val: boolean) {
    modelsLoaded = val;
  },
  get appProfileNotice() {
    return appProfileNotice;
  },
  set appProfileNotice(val: string | null) {
    appProfileNotice = val;
  },
  get pendingPermissionsDialogIntro() {
    return pendingPermissionsDialogIntro;
  },
  set pendingPermissionsDialogIntro(val: string | null) {
    pendingPermissionsDialogIntro = val;
  },
  get transcribing() {
    return transcribing;
  },
  get noModelInstalled() {
    return noModelInstalled;
  },
  get activeModel() {
    return activeModel;
  },
  async loadSources() {
    try {
      const sources = await invoke<AudioSourceListing[]>("audio_list_sources");
      audio.setSources(sources);
      const mics = sources.filter((s) => s.kind === "microphone");
      const def = mics.find((s) => s.isDefault) ?? mics[0];
      if (def) {
        audio.selected = def.id;
        audio.meetingMicId = def.id;
      }
      const sys = sources.find((s) => s.kind === "system-audio");
      audio.meetingIncludeSystemAudio = sys?.isSupported ?? false;
    } catch (e) {
      error = formatErrorDisplay(e);
    } finally {
      audio.sourcesLoaded = true;
    }
  },
  async start() {
    error = null;
    result = null;
    busy = true;
    try {
      await invoke("start_dictation", { source: audio.selectedAsAudioSource() });
      recording = true;
      recordMode = "dictation";
    } catch (e) {
      error = formatErrorDisplay(e);
    } finally {
      busy = false;
    }
  },
  async startRecord(screenRecordingLive: boolean) {
    error = null;
    result = null;
    busy = true;
    const sourceShape = audio.selectedAsAudioSource();
    const sources =
      sourceShape === null
        ? []
        : sourceShape.kind === "microphone" && screenRecordingLive
          ? [
              { kind: "microphone", deviceId: sourceShape.deviceId },
              { kind: "system-audio" },
            ]
          : [sourceShape];
    const isMultiSource = sources.length > 1;
    try {
      const session = await invoke<MeetingSession>("meeting_start_manual", {
        sources,
        appName: null,
      });
      recording = true;
      recordMode = isMultiSource ? "meeting" : "dictation";
      lastMeetingId = session.id;
      lastRecordingStartedAtMs = Date.now();
      meeting.activeId = session.id;
      if (isMultiSource) {
        void invoke("confirm_permission", {
          permission: "screen-recording",
        }).catch((err) => {
          console.warn("[hush] confirm_permission(screen-recording) failed", err);
        });
      }
    } catch (e) {
      error = formatErrorDisplay(e);
      if (isMultiSource && isPermissionShapedError(e)) {
        pendingPermissionsDialogIntro =
          (error.headline ?? "Screen Recording permission needed")
          + " — open System Settings below to grant access, then try Record again.";
      }
    } finally {
      busy = false;
    }
  },
  async stop() {
    busy = true;
    const meetingId = lastMeetingId;
    const modeAtStop = recordMode;
    try {
      if (meetingId !== null) {
        await invoke("meeting_stop_manual");
        recording = false;
        recordMode = null;
        meeting.activeId = null;
        lastMeetingId = null;
        setTimeout(() => void meeting.refresh(), 300);
        setTimeout(() => void history.refresh(), 300);
        const populateResult = modeAtStop === "dictation";
        const recordedAt = lastRecordingStartedAtMs;
        lastRecordingStartedAtMs = null;
        setTimeout(async () => {
          await meeting.copyToClipboard(meetingId);
          if (populateResult) {
            try {
              const detail = await invoke<MeetingSessionDetail>(
                "meeting_session_get",
                { id: meetingId },
              );
              const finals = (detail.utterances ?? []).filter((u) => u.isFinal);
              if (finals.length > 0) {
                const text = joinUtterances(finals, "\n\n");
                const durationMs =
                  recordedAt !== null ? Date.now() - recordedAt : null;
                result = { text, foreground: null, durationMs };
              }
            } catch (e) {
              console.warn("[hush] failed to populate result block", e);
            }
          }
        }, 350);
      } else {
        result = await invoke<DictationResult>("stop_dictation");
        recording = false;
        recordMode = null;
        setTimeout(() => void history.refresh(), 150);
        if (meeting.activeId !== null) {
          setTimeout(() => void meeting.refresh(), 200);
        }
      }
      void invoke("confirm_permission", { permission: "microphone" }).catch(
        (err) => {
          console.warn("[hush] confirm_permission(mic) failed", err);
        },
      );
    } catch (e) {
      error = formatErrorDisplay(e);
      recording = false;
      recordMode = null;
    } finally {
      busy = false;
    }
  },
  async refreshModels() {
    try {
      models = await invoke<ModelCard[]>("model_list");
    } catch (e) {
      console.warn("[hush] model_list failed on main window", e);
    } finally {
      modelsLoaded = true;
    }
  },
  handleModelLoaded() {
    if (error?.actionKey === "open-model-settings") {
      error = null;
    }
    void dictation.refreshModels();
  },
  async onAppProfileActivated(payload: {
    appName: string;
    preferredAudioSource: string | null;
    preferredModelId: string | null;
  }) {
    if (recording) return;

    if (payload.preferredAudioSource !== null) {
      const target = audio.sources.find((s) => s.id === payload.preferredAudioSource);
      if (target) {
        audio.selected = target.id;
      } else {
        console.warn(
          `[hush] app:profile-activated for ${payload.appName}: source ${payload.preferredAudioSource} not in current list`,
        );
      }
    }

    if (payload.preferredModelId !== null) {
      try {
        await invoke("model_select", { id: payload.preferredModelId });
        await dictation.refreshModels();
      } catch (e) {
        console.warn(`[hush] app:profile-activated model_select failed`, e);
      }
    }

    appProfileNotice = `Switched to ${payload.appName} profile.`;
    if (appProfileNoticeTimer !== null) {
      clearTimeout(appProfileNoticeTimer);
    }
    appProfileNoticeTimer = setTimeout(() => {
      appProfileNotice = null;
      appProfileNoticeTimer = null;
    }, 3000);
  },
  clearAppProfileNotice() {
    appProfileNotice = null;
    if (appProfileNoticeTimer !== null) {
      clearTimeout(appProfileNoticeTimer);
      appProfileNoticeTimer = null;
    }
  },
  clearPendingPermissionsDialog() {
    pendingPermissionsDialogIntro = null;
  },
  cleanup() {
    if (appProfileNoticeTimer !== null) {
      clearTimeout(appProfileNoticeTimer);
      appProfileNoticeTimer = null;
    }
  },
};
