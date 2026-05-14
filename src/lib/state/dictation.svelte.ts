import { invoke } from "@tauri-apps/api/core";

// How long (ms) to hold the capture pipeline open after the user signals
// stop, so Whisper's in-flight chunk has time to accumulate the final word.
// Applied by all callers that represent "natural end of speech": PTT key-up,
// record-button tap, toggle hotkey stop, and command palette stop.
// 500 ms matches the conventional PTT trailing buffer used by voice apps
// (Discord, Mumble, etc.).
export const TRAILING_SILENCE_MS = 500;

import {
  formatErrorDisplay,
  isPermissionShapedError,
  type ErrorDisplay,
} from "$lib/errors";
import { joinUtterances } from "$lib/transcript-format";
import type {
  ActiveMeetingSession,
  AudioSourceListing,
  DictationResult,
  MeetingSession,
  MeetingSessionDetail,
  ModelCard,
} from "$lib/types";
import { audio } from "$lib/state/audio.svelte";
import { history } from "$lib/state/history.svelte";
import { meeting } from "$lib/state/meeting-sessions.svelte";

// ---------------------------------------------------------------------------
// Recording lifecycle state machine
// ---------------------------------------------------------------------------
// The five states form a linear flow with one branch and one recovery edge:
//
//   idle ──start──▶ starting ──IPC ok──▶ recording ──stop()──▶ stopping
//                      │                                             │
//                   IPC fail                              meeting_stop_manual ok
//                      │                                             │
//                      ▼                                        transcribing
//                     idle ◀──────────────────────────────────── (always)
//
// On stop failure: recovery queries meeting_active_session. If the session is
// gone → idle. If still live → restore to recording so the user can retry.

export type RecordMode = "dictation" | "meeting";

type RecordingPhase =
  | { tag: "idle" }
  | { tag: "starting" }
  | {
      tag: "recording";
      mode: RecordMode;
      // null for the start_dictation path (no meeting session ID).
      meetingId: number | null;
      startedAtMs: number;
    }
  | {
      tag: "stopping";
      mode: RecordMode;
      meetingId: number | null;
      startedAtMs: number;
    }
  | { tag: "transcribing" };

let phase = $state<RecordingPhase>({ tag: "idle" });
let result = $state<DictationResult | null>(null);
let error = $state<ErrorDisplay | null>(null);
let models = $state<ModelCard[]>([]);
let modelsLoaded = $state(false);
let appProfileNotice = $state<string | null>(null);
// Timer handle for the app-profile notice auto-dismiss. Not reactive — only
// used internally so the previous timer can be cancelled when a new notice
// arrives. Plain let is sufficient; $state would fire unnecessary updates.
let appProfileNoticeTimer: ReturnType<typeof setTimeout> | null = null;
let pendingPermissionsDialogIntro = $state<string | null>(null);

let recording = $derived(phase.tag === "recording");
let busy = $derived(
  phase.tag === "starting" ||
    phase.tag === "stopping" ||
    phase.tag === "transcribing",
);
// True only during the window between meeting_stop_manual returning and result
// hydration completing — previously this was always false because busy went
// false before the 350 ms setTimeout fired.
let transcribing = $derived(phase.tag === "transcribing");
let recordMode = $derived<RecordMode | null>(
  phase.tag === "recording" || phase.tag === "stopping" ? phase.mode : null,
);
let noModelInstalled = $derived(
  modelsLoaded && models.length > 0 && !models.some((m) => m.isDownloaded),
);
let activeModel = $derived(
  models.find((m) => m.isSelected && m.isDownloaded) ?? null,
);
// Cross-module derived recording-status flags. Centralised here because
// dictation.svelte.ts already imports audio and meeting; exporting from a
// single place prevents the same derivation from drifting across page,
// DictationSection, and palette (all three were previously independent).
let meetingOnlyActive = $derived(
  meeting.activeId !== null && !recording && !busy,
);
let screenRecordingLive = $derived(
  audio.findSystemAudio()?.isSupported ?? false,
);
let anyRecordingActive = $derived(recording || meetingOnlyActive);

export const dictation = {
  // ---- read-only derived state ----
  get recording() {
    return recording;
  },
  get busy() {
    return busy;
  },
  get transcribing() {
    return transcribing;
  },
  get recordMode() {
    return recordMode;
  },
  // ---- independently mutable state ----
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
  // ---- derived model helpers ----
  get noModelInstalled() {
    return noModelInstalled;
  },
  get activeModel() {
    return activeModel;
  },
  // ---- derived cross-module recording status ----
  // These are the canonical source for recording-state flags shared across
  // +page.svelte, DictationSection.svelte, and palette.svelte.ts.
  // All three previously maintained independent $derived definitions.
  get meetingOnlyActive() {
    return meetingOnlyActive;
  },
  get screenRecordingLive() {
    return screenRecordingLive;
  },
  get anyRecordingActive() {
    return anyRecordingActive;
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
  // ---- recording lifecycle ----
  //
  // start() uses the start_dictation / stop_dictation IPC pair.
  // That path applies vocabulary prompt biasing, replacements, and backend
  // clipboard write — features the meeting pump doesn't replicate. Used by
  // the toggle hotkey and PTT.
  //
  // startRecord() uses the meeting_start_manual / meeting_stop_manual pair.
  // Adds system-audio capture (when SCK is confirmed). Used by the UI button.
  async start() {
    if (phase.tag !== "idle") return;
    error = null;
    result = null;
    phase = { tag: "starting" };
    try {
      await invoke("start_dictation", { source: audio.selectedAsAudioSource() });
      phase = { tag: "recording", mode: "dictation", meetingId: null, startedAtMs: Date.now() };
    } catch (e) {
      error = formatErrorDisplay(e);
      phase = { tag: "idle" };
    }
  },
  async startRecord() {
    if (phase.tag !== "idle") return;
    error = null;
    result = null;
    phase = { tag: "starting" };
    const sourceShape = audio.selectedAsAudioSource();
    // Build the sources array for meeting_start_manual:
    // - No source selected → [] (backend rejects with no-device error, handled below)
    // - Microphone + Screen Recording live → mic + system-audio pair (multi-source meeting)
    // - Anything else (mic only, or system-audio only) → single source as-is
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
      phase = {
        tag: "recording",
        mode: isMultiSource ? "meeting" : "dictation",
        meetingId: session.id,
        startedAtMs: Date.now(),
      };
      meeting.activeId = session.id;
    } catch (e) {
      error = formatErrorDisplay(e);
      if (isMultiSource && isPermissionShapedError(e)) {
        pendingPermissionsDialogIntro =
          (error.headline ?? "Permission needed")
          + " — open System Settings below to grant access, then try Record again.";
      }
      phase = { tag: "idle" };
    }
  },
  async stop(trailingMs = 0) {
    // Phase guard: only the recording state can transition to stopping.
    // Replaces the old busy-flag re-entrancy guard — illegal transitions
    // are structurally impossible rather than defended at runtime.
    if (phase.tag !== "recording") return;
    const snapshot = {
      mode: phase.mode,
      meetingId: phase.meetingId,
      startedAtMs: phase.startedAtMs,
    };
    phase = { tag: "stopping", ...snapshot };
    try {
      // Trailing-silence buffer: hold the pipeline open so Whisper's
      // in-flight chunk can finish accumulating before teardown.
      if (trailingMs > 0) {
        await new Promise<void>((r) => setTimeout(r, trailingMs));
      }
      if (snapshot.meetingId !== null) {
        await _stopMeeting({ ...snapshot, meetingId: snapshot.meetingId });
      } else {
        await _stopDictation();
      }
    } catch (e) {
      error = formatErrorDisplay(e);
      if (snapshot.meetingId !== null) {
        // meeting_stop_manual failed — query the backend directly to decide
        // whether to restore recording state or clear it. Avoids the race
        // where meeting.activeId is stale after a failed refresh() (#552).
        const { active } = await invoke<ActiveMeetingSession>(
          "meeting_active_session",
        ).catch(() => ({ active: snapshot.meetingId }));
        if (active !== snapshot.meetingId) {
          // Session is gone on the backend — safe to clear UI state.
          meeting.activeId = null;
          phase = { tag: "idle" };
        } else {
          // Session still live — restore so the user can retry stop().
          phase = { tag: "recording", ...snapshot };
        }
      } else {
        phase = { tag: "idle" };
      }
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

// ---------------------------------------------------------------------------
// Private stop helpers — called only from dictation.stop()
// ---------------------------------------------------------------------------

// Handles the start_dictation / stop_dictation lifecycle. The result is
// returned synchronously from stop_dictation, so no transcribing phase is
// needed here — phase goes directly to idle.
async function _stopDictation(): Promise<void> {
  const dictResult = await invoke<DictationResult>("stop_dictation");
  result = dictResult;
  phase = { tag: "idle" };
  void history.refresh();
  if (meeting.activeId !== null) {
    void meeting.refresh();
  }
  // Fire-and-forget: confirm_permission nudges the TCC store so the
  // permission health panel reflects the most recent grant status. If it
  // fails (e.g. no microphone permission, or an older Tauri build), we
  // just warn — the main stop path has already completed successfully.
  void invoke("confirm_permission", { permission: "microphone" }).catch(
    (err) => {
      console.warn("[hush] confirm_permission(mic) failed", err);
    },
  );
}

// Handles the meeting_start_manual / meeting_stop_manual lifecycle.
// Transitions through transcribing while fetching the completed session
// detail — one fetch serves both clipboard copy and the result block.
async function _stopMeeting(snapshot: {
  mode: RecordMode;
  meetingId: number;
  startedAtMs: number;
}): Promise<void> {
  await invoke("meeting_stop_manual");
  // meeting_stop_manual awaits pump drain before returning, so the session
  // is fully finalised at this point — no setTimeout delay is needed.
  phase = { tag: "transcribing" };
  meeting.activeId = null;
  try {
    const detail = await invoke<MeetingSessionDetail>("meeting_session_get", {
      id: snapshot.meetingId,
    });
    const finals = (detail.utterances ?? []).filter((u) => u.isFinal);
    if (finals.length > 0) {
      const text = joinUtterances(finals, "\n\n");
      // Clipboard — one fetch serves both clipboard text and the result block.
      try {
        await navigator.clipboard.writeText(text);
        meeting.setNotice({
          kind: "success",
          message: "Copied to clipboard — full transcript also saved to History below.",
        });
      } catch {
        meeting.setNotice({
          kind: "failure",
          message:
            "Transcript saved to History — use the 'Copy transcript' button on the meeting row below.",
        });
      }
      // Result block for single-source dictation via the meeting path.
      if (snapshot.mode === "dictation") {
        result = {
          text,
          foreground: null,
          durationMs: Date.now() - snapshot.startedAtMs,
        };
      }
    }
  } catch (e) {
    console.warn("[hush] failed to hydrate result from meeting session", e);
  } finally {
    // Always clean up regardless of hydration success.
    phase = { tag: "idle" };
    void meeting.refresh();
    void history.refresh();
    void invoke("confirm_permission", { permission: "microphone" }).catch(
      (err) => {
        console.warn("[hush] confirm_permission(mic) failed", err);
      },
    );
  }
}
