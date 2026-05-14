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

  Permission/onboarding state lives in the `permissions` and `onboarding`
  state modules (#722); this component writes to them directly instead
  of forwarding via bind: props. `isMacOS` remains a bindable prop
  because it is only set here and read by the parent layout glyph.
-->
<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { platform } from "@tauri-apps/plugin-os";
  import { onDestroy, onMount } from "svelte";

  import { Events } from "$lib/events";
  import { audio } from "$lib/state/audio.svelte";
  import { dictation, TRAILING_SILENCE_MS } from "$lib/state/dictation.svelte";
  import { history } from "$lib/state/history.svelte";
  import { meeting } from "$lib/state/meeting-sessions.svelte";
  import { onboarding } from "$lib/state/onboarding.svelte";
  import { permissions } from "$lib/state/permissions.svelte";
  import type { SettingsTab } from "$lib/SettingsPanel.svelte";
  import { nav } from "$lib/state/nav.svelte";

  type Props = {
    /// Resolved platform — true when running on macOS. Set during onMount
    /// from `@tauri-apps/plugin-os`'s `platform()` call.
    isMacOS: boolean;
    /// Keyboard handler to register on `window` during onMount. Passed
    /// by the parent so handleGlobalKeydown can close over paletteOpen.
    onGlobalKeydown: (e: KeyboardEvent) => void;
  };

  let {
    isMacOS = $bindable(false),
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
  let unlistenAudioDeviceLost: UnlistenFn | null = null;
  let unlistenAudioDeviceRestored: UnlistenFn | null = null;
  // The three meeting-session listeners (session-started,
  // source-failed, append-failed) are owned by `meeting.svelte.ts`
  // and cleaned up via the single returned function (#700).
  let cleanupMeetingListeners: (() => void) | null = null;

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
      permissions.permStatuses?.microphone === "granted" &&
      audio.sources.filter((s) => s.kind === "microphone").length === 0
    ) {
      void dictation.loadSources();
    }
  });

  $effect(() => {
    if (dictation.pendingPermissionsDialogIntro !== null) {
      permissions.openDialog(dictation.pendingPermissionsDialogIntro);
      dictation.clearPendingPermissionsDialog();
    }
  });

  $effect(() => {
    if (meeting.pendingPermissionsDialogIntro !== null) {
      permissions.openDialog(meeting.pendingPermissionsDialogIntro);
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
    await onboarding.check();

    // Register meeting-session listeners before the initial refresh
    // so events that fire in the narrow window between refresh and
    // listener setup are never lost. The three listeners (session-
    // started, source-failed, append-failed) live in the meeting
    // state module since all three update only meeting state (#700).
    cleanupMeetingListeners = await meeting.initSessionListeners();

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
    cleanupMeetingListeners?.();
    unlistenAudioDeviceLost?.();
    unlistenAudioDeviceRestored?.();
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
