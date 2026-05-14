<!--
  App-level lifecycle component. Extracts from +page.svelte (#685):
    - Tauri event listeners (hotkey toggle, PTT, menu navigation,
      model download, app profile, meeting source failures, audio
      device events, meeting session start)
    - PTT state machine
    - Meeting-session poll
    - Permission side-effects (source-reload on first grant,
      permissions-dialog triggers from dictation / meeting state)
    - Platform detection and first-run flag check

  State that callers need is exposed via $bindable() props so
  +page.svelte stays a pure layout. AppLifecycle holds no markup.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { platform } from "@tauri-apps/plugin-os";
  import { onDestroy, onMount } from "svelte";

  import { Events } from "$lib/events";
  import type { PermissionStatuses, PermissionsHealth } from "$lib/types";
  import { audio } from "$lib/state/audio.svelte";
  import { dictation, TRAILING_SILENCE_MS } from "$lib/state/dictation.svelte";
  import { history } from "$lib/state/history.svelte";
  import { meeting } from "$lib/state/meeting-sessions.svelte";
  import type { SettingsTab } from "$lib/SettingsPanel.svelte";
  import { nav } from "$lib/state/nav.svelte";

  type Props = {
    /// Resolved platform — true when running on macOS. Set during onMount
    /// from `@tauri-apps/plugin-os`'s `platform()` call.
    isMacOS: boolean;
    /// First-run wizard visibility. Set to true if the backend flag is
    /// unset; parent clears it via dismissFirstRun().
    showFirstRun: boolean;
    /// Current permission probe result (mic / screen / input-monitoring).
    /// Read by the permission-source-reload effect.
    permStatuses: PermissionStatuses | null;
    /// Permissions-recovery dialog visibility. Written by the dictation /
    /// meeting pending-dialog effects.
    showPermissionsDialog: boolean;
    /// Optional intro copy for the recovery dialog. Cleared on dismiss.
    permissionsDialogIntro: string | undefined;
    /// Keyboard handler to register on `window` during onMount. Passed
    /// by the parent so handleGlobalKeydown can close over paletteOpen.
    onGlobalKeydown: (e: KeyboardEvent) => void;
    // permissionHealth / macosCapable / staleBannerDismissed are owned
    // and bound by PermissionHealthSection / the page template; they are
    // not needed here.
  };

  let {
    isMacOS = $bindable(false),
    showFirstRun = $bindable(false),
    permStatuses = $bindable<PermissionStatuses | null>(null),
    showPermissionsDialog = $bindable(false),
    permissionsDialogIntro = $bindable<string | undefined>(undefined),
    onGlobalKeydown,
  }: Props = $props();

  // Capture once at init so addEventListener/removeEventListener always
  // use the same reference, even if the prop were to change.
  // svelte-ignore state_referenced_locally
  const _keydownHandler = onGlobalKeydown;

  // --- Listener handles ---
  let unlistenToggle: UnlistenFn | null = null;
  let unlistenPttPress: UnlistenFn | null = null;
  let unlistenPttRelease: UnlistenFn | null = null;
  let unlistenMenuGoto: UnlistenFn | null = null;
  let unlistenSettingsGoto: UnlistenFn | null = null;
  let unlistenDownloadDone: UnlistenFn | null = null;
  let unlistenAppProfileActivated: UnlistenFn | null = null;
  let unlistenMeetingSourceFailed: UnlistenFn | null = null;
  let unlistenAudioDeviceLost: UnlistenFn | null = null;
  let unlistenAudioDeviceRestored: UnlistenFn | null = null;
  let unlistenMeetingSessionStarted: UnlistenFn | null = null;
  let unlistenMeetingAppendFailed: UnlistenFn | null = null;

  // PTT state machine.
  //
  // Minimum hold time (ms) before a PTT press is treated as intentional.
  // Taps shorter than this are discarded as accidental. 100 ms is below
  // deliberate-hold perception but clears OS key-bounce artifacts and
  // rapid accidental taps.
  const PTT_MIN_HOLD_MS = 100;
  // Whether the PTT key is physically down right now. Used by the timer
  // callback to avoid starting a recording after the key was already
  // released, and to detect the stuck-recording race (key released while
  // start IPC was in-flight).
  let pttIsDown = false;
  // True only when PTT itself started the current recording. Guards the
  // release handler from stopping a recording that was started by the UI
  // button or the toggle hotkey.
  let pttOwnedRecording = false;
  // Non-null while the minimum-hold guard timer is running (before we
  // commit to starting a recording). Cancelled on early key-up.
  let pttPressTimer: ReturnType<typeof setTimeout> | null = null;

  let meetingActivePollHandle: ReturnType<typeof setInterval> | null = null;

  // When mic permission transitions to granted but we have no mic devices
  // yet (the async TCC race on first-launch: the user grants permission
  // inside the first-run wizard and the PermissionHealthSection poll
  // catches it before our source list has re-enumerated), reload sources.
  // The guard on audio.sources prevents a perpetual reload loop: once
  // loadSources() succeeds with permission granted it populates sources
  // with at least one microphone entry and the condition becomes false.
  $effect(() => {
    if (
      permStatuses?.microphone === "granted" &&
      audio.sources.filter((s) => s.kind === "microphone").length === 0
    ) {
      void dictation.loadSources();
    }
  });

  $effect(() => {
    if (dictation.pendingPermissionsDialogIntro !== null) {
      permissionsDialogIntro = dictation.pendingPermissionsDialogIntro;
      showPermissionsDialog = true;
      dictation.clearPendingPermissionsDialog();
    }
  });

  $effect(() => {
    if (meeting.pendingPermissionsDialogIntro !== null) {
      permissionsDialogIntro = meeting.pendingPermissionsDialogIntro;
      showPermissionsDialog = true;
      meeting.clearPendingPermissionsDialog();
    }
  });

  // Poll for the active meeting's live transcript while it is in flight.
  // This stays in a lifecycle component so the interval is owned by
  // component lifecycle.
  $effect(() => {
    if (meetingActivePollHandle !== null) {
      clearInterval(meetingActivePollHandle);
      meetingActivePollHandle = null;
    }
    const id = meeting.activeId;
    if (id === null) {
      meeting.clearActiveDetail();
      return;
    }
    void meeting.refreshActiveDetail(id);
    meetingActivePollHandle = setInterval(() => {
      void meeting.refreshActiveDetail(id);
    }, 3000);
    return () => {
      if (meetingActivePollHandle !== null) {
        clearInterval(meetingActivePollHandle);
        meetingActivePollHandle = null;
      }
    };
  });

  onMount(async () => {
    try {
      isMacOS = (await platform()) === "macos";
    } catch (e) {
      console.warn("[hush] platform() probe failed; defaulting to non-macOS glyph", e);
    }

    // Check the first-run flag BEFORE the Promise.all — round-7 UX
    // reviewer caught a real timing bug.
    try {
      const done = await invoke<boolean>("get_first_run_completed");
      if (!done) showFirstRun = true;
    } catch (e) {
      console.error("get_first_run_completed failed:", e);
    }

    // Register before the initial refresh so that an auto-start event
    // that fires in the narrow window between refresh and listener
    // setup is never lost. The listener immediately sets `meeting.activeId`
    // from the payload (shows the Stop button without a round-trip wait)
    // and then calls `meeting.refresh()` for the full session list.
    unlistenMeetingSessionStarted = await listen<{ sessionId: number }>(
      Events.MeetingSessionStarted,
      (e) => {
        meeting.activeId = e.payload.sessionId;
        void meeting.refresh();
      },
    );

    await Promise.all([
      dictation.loadSources(),
      history.refresh(),
      dictation.refreshModels(),
      meeting.refresh(),
    ]);

    unlistenToggle = await listen(Events.HotkeyToggle, () => {
      if (dictation.busy || meeting.busy) return;
      if (dictation.recording) void dictation.stop(TRAILING_SILENCE_MS);
      else if (meeting.activeId !== null) void meeting.stopSession();
      else void dictation.start();
    });

    unlistenMenuGoto = await listen<string>(Events.MenuGotoSection, (e) => {
      const payload = e.payload;
      nav.activeSection =
        payload === "meetings" || payload === "history"
          ? "history"
          : "dictation";
    });

    unlistenSettingsGoto = await listen<string>(Events.SettingsGotoTab, (e) => {
      nav.openSettingsTab(e.payload as SettingsTab | "about");
    });

    unlistenDownloadDone = await listen<{ id: string }>(
      Events.ModelDownloadDone,
      () => {
        void dictation.refreshModels();
      },
    );

    unlistenAppProfileActivated = await listen<{
      appName: string;
      preferredAudioSource: string | null;
      preferredModelId: string | null;
    }>(Events.AppProfileActivated, (e) => {
      void dictation.onAppProfileActivated(e.payload);
    });

    unlistenPttPress = await listen(Events.HotkeyPttPress, () => {
      pttIsDown = true;
      if (dictation.busy || dictation.recording) return;
      // Ignore key-repeat events while the hold timer is already running.
      if (pttPressTimer !== null) return;
      pttPressTimer = setTimeout(() => {
        pttPressTimer = null;
        // Key may have been released before the timer fired (short tap).
        if (!pttIsDown || dictation.busy || dictation.recording) return;
        void dictation.start().then(() => {
          if (!dictation.recording || dictation.busy) return;
          if (pttIsDown) {
            // Normal case: key still held — mark this recording as PTT-owned
            // so the release handler knows it's allowed to stop it.
            pttOwnedRecording = true;
          } else {
            // Stuck-recording race: key was released while start IPC was
            // in-flight. Release handler saw busy=true and skipped stop().
            void dictation.stop(TRAILING_SILENCE_MS);
          }
        });
      }, PTT_MIN_HOLD_MS);
    });
    unlistenPttRelease = await listen(Events.HotkeyPttRelease, () => {
      pttIsDown = false;
      if (pttPressTimer !== null) {
        // Key released before the minimum hold elapsed — treat as an
        // accidental tap. Cancel the timer; nothing starts or stops.
        clearTimeout(pttPressTimer);
        pttPressTimer = null;
        return;
      }
      // If start() is in-flight (busy=true, recording=false), the
      // post-start callback above will call stop() once it resolves.
      // Only stop if PTT itself started this recording — don't interrupt
      // a UI-button or toggle-hotkey recording on PTT key release.
      if (!pttOwnedRecording || !dictation.recording || dictation.busy) return;
      pttOwnedRecording = false;
      void dictation.stop(TRAILING_SILENCE_MS);
    });

    // Surface per-source transcription failures as a banner (#533).
    // The backend emits this at session start (source failed to open)
    // and mid-session (drain failure / panic). No activeId gate here:
    // startup failures arrive before the invoke resolves and sets
    // activeId, so gating would silently drop the very case this
    // banner is designed for. The backend never emits for a stopped
    // session, so stale-event bleed isn't a real risk.
    unlistenMeetingSourceFailed = await listen<{
      sessionId: number;
      sourceKind: string;
      reason: string;
      // #617: typed flag from the backend so we don't substring-
      // match on `reason`. `true` for both mid-session DeviceLost
      // and pre-warm DeviceLost; `false` for whisper panics,
      // start_stream failures, generic drain errors.
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
      // Three failure classes — distinguished by the typed
      // `deviceLost` flag and a fallback `reason` substring on
      // session-start (which has no typed equivalent yet):
      //   - device-lost — user's mic / AirPods disconnected at
      //     pre-warm or mid-session.
      //   - session-start — pre-warm or start_stream failure that
      //     ISN'T a device-lost (e.g. whisper init failed).
      //   - other — generic mid-session drain failure or whisper
      //     panic (#591).
      // Multi-source intent: the user picked a mic AND opted into
      // system audio, AND system audio was supported on this host.
      // Mirrors the include-source logic in meeting-sessions.svelte.ts.
      // When this is true and only one source failed, the other is
      // still capturing — claiming "recording stopped" would be a lie.
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

    unlistenAudioDeviceLost = await listen<{
      sessionId: number;
      sourceKind: string;
      lostDevice: string;
      newDevice?: string;
    }>(Events.AudioDeviceLost, (e) => {
      console.debug(
        "[AudioDeviceLost]",
        e.payload.sourceKind,
        e.payload.lostDevice,
        "→",
        e.payload.newDevice ?? "no fallback",
      );
      if (e.payload.newDevice) {
        meeting.sourceFailedNotice = `Microphone "${e.payload.lostDevice}" disconnected — switched to "${e.payload.newDevice}".`;
      } else {
        meeting.sourceFailedNotice = `Microphone "${e.payload.lostDevice}" disconnected — recording stopped.`;
      }
    });

    unlistenAudioDeviceRestored = await listen<{
      sessionId: number;
      sourceKind: string;
      restoredDevice: string;
    }>(Events.AudioDeviceRestored, (e) => {
      console.debug(
        "[AudioDeviceRestored]",
        e.payload.sourceKind,
        e.payload.restoredDevice,
      );
      meeting.sourceFailedNotice = null;
    });

    unlistenMeetingAppendFailed = await listen<{ error: string }>(
      Events.DictationMeetingAppendFailed,
      (e) => {
        console.warn("[DictationMeetingAppendFailed]", e.payload.error);
        // Show a warning banner so the user knows the meeting session log is
        // missing this utterance, even though it landed on the clipboard (#696).
        meeting.appendFailedNotice =
          "A transcription couldn't be saved to your meeting session. The text is still on your clipboard.";
      },
    );

    window.addEventListener("keydown", _keydownHandler);
  });

  onDestroy(() => {
    unlistenToggle?.();
    unlistenMenuGoto?.();
    unlistenSettingsGoto?.();
    unlistenPttPress?.();
    unlistenPttRelease?.();
    unlistenDownloadDone?.();
    unlistenAppProfileActivated?.();
    unlistenMeetingSourceFailed?.();
    unlistenAudioDeviceLost?.();
    unlistenAudioDeviceRestored?.();
    unlistenMeetingSessionStarted?.();
    unlistenMeetingAppendFailed?.();
    if (pttPressTimer !== null) {
      clearTimeout(pttPressTimer);
      pttPressTimer = null;
    }
    window.removeEventListener("keydown", _keydownHandler);
    if (meetingActivePollHandle !== null) {
      clearInterval(meetingActivePollHandle);
      meetingActivePollHandle = null;
    }
    dictation.cleanup();
  });
</script>
