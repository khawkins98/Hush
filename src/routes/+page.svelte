<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";
  import AppSidebar from "$lib/AppSidebar.svelte";
  import type { AppSection } from "$lib/types";
  import ControlsSection from "$lib/ControlsSection.svelte";
  import ResultBlock from "$lib/ResultBlock.svelte";
  import HistoryPanel from "$lib/HistoryPanel.svelte";
  import MeetingSessionsPanel from "$lib/MeetingSessionsPanel.svelte";
  import { formatTimestamp } from "$lib/format";
  import type {
    AudioSource,
    AudioSourceListing,
    DictationResult,
    HistoryEntry,
    IpcError,
    MacosPermissionDiagnostic,
    ModelCard,
    MeetingSession,
    MeetingSessionDetail,
    PermissionStatuses,
  } from "$lib/types";

  // Page size for the history view. Hard-cap on the Rust side is 500;
  // 25 is plenty per page for a dictation history that grows linearly
  // with the user's actual usage (handful per day).
  const HISTORY_PAGE_SIZE = 25;

  // Sidebar nav state (Phase 1 of the IA redesign). Drives which
  // content block renders in the main pane; the hot-path Dictation
  // section is the default landing tab. Stays page-local for v1 —
  // a Phase 4 follow-up may persist the last-active section in
  // settings so the window reopens to wherever the user was.
  let activeSection = $state<AppSection>("dictation");

  let sources = $state<AudioSourceListing[]>([]);
  let sourcesLoaded = $state(false);
  // Selected source id. Mic devices use their device name; the
  // system-audio entry uses the literal string `"system"`. Mapped to
  // an `AudioSource` for `start_dictation` in `start()`.
  let selected = $state<string | null>(null);

  // Independent state for the meeting panel's source picker
  // (#122 Phase 3). Lives on the page rather than inside the
  // panel so the parent can read it when constructing the
  // `meeting_start_manual` source list, and so the picker state
  // survives panel-level re-renders.
  //
  // - `meetingMicId`: which microphone the meeting captures from.
  //   Initialised to the host default mic in `loadSources` and
  //   bound through the panel's dropdown.
  // - `meetingIncludeSystemAudio`: whether the meeting also
  //   captures system audio in parallel. Default `true` when the
  //   backend reports `is_supported`, `false` otherwise. A
  //   meeting's typical canonical config is mic + system audio
  //   (you on mic, remote participants via SCK loopback) so the
  //   default is "all-on" — power users can deselect.
  let meetingMicId = $state<string | null>(null);
  let meetingIncludeSystemAudio = $state<boolean>(false);
  let recording = $state(false);
  let busy = $state(false);
  let result = $state<DictationResult | null>(null);
  let error = $state<string | null>(null);

  let historyEntries = $state<HistoryEntry[]>([]);
  let historyLoaded = $state(false);
  let historyQuery = $state("");
  let historySearching = $state(false);
  let historyError = $state<string | null>(null);
  // Sentinel that any history-touching command bumps so we can react
  // to an external invalidation (e.g. a successful stop_dictation
  // inserted a new row).
  let historyVersion = $state(0);

  // Models state on the main window is read-only and used solely
  // for the "no model installed" banner on the Dictation tab. The
  // Settings window owns the full picker (download / select /
  // remove). We keep just enough state here to drive
  // `noModelInstalled` and refresh on the broadcast
  // `model:download-done` event.
  let models = $state<ModelCard[]>([]);
  let modelsLoaded = $state(false);

  // First-run welcome flow. Renders on the first launch (regardless
  // of platform — the welcome explains permissions that exist
  // everywhere; the macOS-specific Input Monitoring section is the
  // most useful but the rest is fine to show universally). The
  // deep-link buttons on macOS open System Settings; on other
  // platforms the backend's `open_macos_privacy_pane` is a no-op.
  // Once dismissed, the flag persists in `settings` and the modal
  // never shows again on this install.
  let showFirstRun = $state(false);

  // Modal element ref + the focused-element-before-modal stash. The
  // ref backs the focus trap (so Tab cycles within the modal instead
  // of escaping to the rest of the page); the stash lets us restore
  // focus to whatever the user was on before the welcome appeared
  // when they dismiss it.
  let firstRunCardEl: HTMLElement | undefined = $state();
  let firstRunPreviousFocus: HTMLElement | null = null;

  // Listener for the broadcast `model:download-done` event. The
  // Settings window's picker drives the actual download UX; we only
  // listen here so the Dictation tab's "no model installed" banner
  // disappears once a download completes in the other window.
  let unlistenDownloadDone: UnlistenFn | null = null;

  // `recording` is "audio is being captured", `busy` covers both the
  // start handshake AND the post-stop transcription window. Splitting
  // out `transcribing` lets the UI distinguish "starting up" (~ms) from
  // "Whisper is working" (seconds), which deserves a visible spinner.
  let transcribing = $derived(busy && !recording && !!result === false);

  // True when the catalog has loaded *and* no model file is on disk
  // yet. Drives the prominent "Set up your first model" banner and
  // the disabled state on the Start button. Gated on `modelsLoaded`
  // so we don't flash the banner before the catalog fetch resolves
  // (false-positive "no model" while the page is still booting).
  let noModelInstalled = $derived(
    modelsLoaded && models.length > 0 && !models.some((m) => m.isDownloaded),
  );

  // The currently-loaded model — `isSelected && isDownloaded`. We
  // can show the display name + a "Change" affordance on the
  // Dictation screen so the user always knows which model their
  // recordings will hit. `null` when no model is loaded yet (the
  // setup banner above takes over in that case so we don't render
  // a duplicate affordance).
  let activeModel = $derived(
    models.find((m) => m.isSelected && m.isDownloaded) ?? null,
  );

  // Push-to-talk is disabled by default on macOS — rdev 0.5 hard-aborts
  // on macOS 26+ (see `src-tauri/src/hotkey/ptt.rs` module header).
  // Power users can re-enable with `HUSH_PTT_ENABLE=1`, but the
  // default surface should not advertise the Right-Ctrl hold or the
  // shortcut card reads as broken to most macOS users.
  let isMacOS = typeof navigator !== "undefined"
    && /Mac|iPhone|iPad/i.test(navigator.platform);

  // Open Settings → Model. Used by the "Set up your first model"
  // banner and the click-through on the transcription-unavailable
  // error chip. Pre-IA-redesign this scrolled to a same-page
  // `#models-heading`; the picker has lived in the Settings window
  // since #163-#167 so the on-page scroll became a silent no-op.
  function openModelSettings() {
    void openSettingsTab("model");
  }

  let unlistenToggle: UnlistenFn | null = null;
  let unlistenPttPress: UnlistenFn | null = null;
  let unlistenPttRelease: UnlistenFn | null = null;
  let unlistenMenuGoto: UnlistenFn | null = null;

  // Keep the document title in sync with recording state. Helps users who
  // have the window in the background — at-a-glance signal that the mic
  // is hot. Tauri exposes `window.document` like a regular browser.
  $effect(() => {
    document.title = recording ? "Hush ● Recording" : "Hush";
  });

  // Push recording state to the backend so the tray's "Start / Stop
  // Recording" menu item label can mirror the UI. The tray module
  // listens for `ui:recording-state` and swaps its label. Best-effort:
  // a failed emit just leaves the tray label stale until the next
  // toggle, which is harmless.
  $effect(() => {
    void emit("ui:recording-state", recording);
  });

  onMount(async () => {
    // Check the first-run flag BEFORE the Promise.all — round-7 UX
    // reviewer caught a real timing bug: when the flag fetch raced
    // against `Promise.all`, a fresh-install user could see the
    // no-model setup banner (which depends on the model-list fetch
    // that's part of Promise.all) BEFORE the welcome modal landed.
    // That meant the modal explaining permissions and the dictation
    // flow would appear after the user had already started clicking
    // around looking for the record button. Awaiting the flag
    // synchronously makes the modal beat the rest of the UI to first
    // paint — the cost is one extra IPC round-trip (cheap; this is a
    // single SQLite read of a boolean).
    try {
      const done = await invoke<boolean>("get_first_run_completed");
      if (!done) showFirstRun = true;
    } catch (e) {
      // Don't block the rest of the page on a settings-fetch failure.
      // The welcome modal is a one-time UX nicety; if SQLite can't
      // even answer, the user has bigger problems and the model
      // banner / error chips will surface them anyway.
      console.error("get_first_run_completed failed:", e);
    }

    // Fire all five fetches concurrently rather than sequentially —
    // the user-visible time-to-paint is bounded by the slowest single
    // call instead of the sum. Each fetch handles its own loading
    // and error state so a slow one (history, in particular) doesn't
    // block the rest of the page.
    await Promise.all([
      loadSources(),
      refreshHistory(),
      refreshModels(),
      loadMacosCapabilityFlag(),
      refreshMeetingSessions(),
    ]);

    // Hotkey lives in the backend (`hotkey::register_default`); on every
    // press the backend emits `hotkey:toggle`. We dispatch start vs stop
    // here against the frontend's own recording flag so the toggle
    // semantics live next to the UI state they affect.
    unlistenToggle = await listen("hotkey:toggle", () => {
      if (busy) return; // ignore presses while a transcription is in flight
      if (recording) void stop();
      else void start();
    });

    // Native menu bar dispatches View → Section selections through
    // this event (#164 Phase 2). Payload is a string matching the
    // `AppSection` union; an unknown value is ignored so the
    // frontend stays robust to a future menu entry the page
    // doesn't yet know about.
    unlistenMenuGoto = await listen<string>("menu:goto-section", (e) => {
      const payload = e.payload;
      if (
        payload === "dictation" ||
        payload === "meetings" ||
        payload === "history"
      ) {
        activeSection = payload;
      }
    });

    // Model-download events from the backend. The progress event
    // The Settings window owns the per-card download UI; here we
    // only listen for `model:download-done` so the Dictation tab's
    // "no model installed" banner disappears once a download in the
    // other window completes. Tauri broadcasts events to every
    // window, so the same backend emit reaches both surfaces.
    unlistenDownloadDone = await listen<{ id: string }>("model:download-done", () => {
      void refreshModels();
    });

    // Pump-side per-source failures during a meeting. The backend
    // emits `meeting:source-failed` when a TCC revoke, device
    // unplug, or inference panic forces it to drop a source for
    // the rest of the session. We accumulate the kinds in
    // `meetingDroppedSources`; the panel reads that set to render
    // a struck-through "this side stopped capturing" affordance
    // in the active-session source line.
    unlistenMeetingSourceFailed = await listen<{
      sessionId: number;
      sourceKind: string;
      reason: string;
    }>("meeting:source-failed", (e) => {
      const next = new Set(meetingDroppedSources);
      next.add(e.payload.sourceKind);
      meetingDroppedSources = next;
    });

    // Push-to-talk: the rdev listener in `hotkey::ptt` emits these
    // events on key-down and key-up of the configured PTT key.
    unlistenPttPress = await listen("hotkey:ptt-press", () => {
      if (busy || recording) return;
      void start();
    });
    unlistenPttRelease = await listen("hotkey:ptt-release", () => {
      // Only stop if we are actually recording. A spurious release (e.g.
      // the user released the key after a press the UI ignored because
      // it was busy) must not call `stop_dictation` against an empty
      // session; the IPC layer would error and the UI would show that.
      if (!recording || busy) return;
      void stop();
    });
  });

  onDestroy(() => {
    unlistenToggle?.();
    unlistenMenuGoto?.();
    unlistenPttPress?.();
    unlistenPttRelease?.();
    unlistenDownloadDone?.();
    unlistenMeetingSourceFailed?.();
  });

  async function start() {
    error = null;
    result = null;
    busy = true;
    try {
      await invoke("start_dictation", { source: selectedAsAudioSource() });
      recording = true;
    } catch (e) {
      error = formatError(e);
    } finally {
      busy = false;
    }
  }

  // Resolve the picker's `selected` string id to the discriminated
  // `AudioSource` shape the backend expects. The literal `"system"`
  // id is the system-audio sentinel; everything else is a microphone
  // device id (cpal identifies devices by name today). Returns `null`
  // for the no-selection case so the backend uses its own default.
  function selectedAsAudioSource(): AudioSource | null {
    if (selected === null) return null;
    if (selected === "system") return { kind: "system-audio" };
    return { kind: "microphone", deviceId: selected };
  }

  async function stop() {
    busy = true;
    try {
      result = await invoke<DictationResult>("stop_dictation");
      recording = false;
      // Backend persists the row on a fire-and-forget task; refresh
      // shortly after so the new entry shows up. Small delay so the
      // INSERT has a chance to commit; on a slow disk this could miss
      // the new row, but the next interaction will catch it.
      setTimeout(() => void refreshHistory(), 150);
      // If a meeting session is active, the backend just appended
      // this transcript as an utterance under it (fire-and-forget
      // path in stop_dictation). Refresh the panel after a similar
      // delay so the new utterance appears in the list.
      if (meetingActiveId !== null) {
        setTimeout(() => void refreshMeetingSessions(), 200);
      }
    } catch (e) {
      error = formatError(e);
      // Even if transcription failed, the recording itself stopped on the
      // Rust side — surface that so the UI is never stuck in "recording".
      recording = false;
    } finally {
      busy = false;
    }
  }

  async function loadSources() {
    try {
      sources = await invoke<AudioSourceListing[]>("audio_list_sources");
      // Default to the host's default microphone, falling back to the
      // first mic in the list. The dictation hot path uses this; the
      // meeting panel has its own `meetingMicId` defaulted similarly.
      const mics = sources.filter((s) => s.kind === "microphone");
      const def = mics.find((s) => s.isDefault) ?? mics[0];
      if (def) {
        selected = def.id;
        meetingMicId = def.id;
      }
      // Meeting's "also record system audio" defaults to ON when the
      // backend reports support — meetings really do want both sides
      // of a call by default. Power users can uncheck.
      const sys = sources.find((s) => s.kind === "system-audio");
      meetingIncludeSystemAudio = sys?.isSupported ?? false;
    } catch (e) {
      error = formatError(e);
    } finally {
      sourcesLoaded = true;
    }
  }

  async function refreshHistory() {
    historyError = null;
    historySearching = true;
    try {
      historyEntries = await invoke<HistoryEntry[]>("history_search", {
        query: historyQuery,
        limit: HISTORY_PAGE_SIZE,
        offset: 0,
      });
      historyVersion += 1;
    } catch (e) {
      historyError = formatError(e);
    } finally {
      historyLoaded = true;
      historySearching = false;
    }
  }

  /// Debounce the search input so we don't fire a SQLite query on every
  /// keystroke. 200ms is the empirical sweet spot — fast enough that the
  /// user feels the list react, slow enough that holding a key doesn't
  /// queue dozens of queries.
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  function onSearchInput(e: Event) {
    historyQuery = (e.target as HTMLInputElement).value;
    if (searchTimer !== null) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      void refreshHistory();
    }, 200);
  }

  async function copyHistoryEntry(entry: HistoryEntry) {
    try {
      await navigator.clipboard.writeText(entry.transcript);
    } catch (e) {
      historyError = `Copy failed: ${String(e)}`;
    }
  }

  async function deleteHistoryEntry(entry: HistoryEntry) {
    try {
      await invoke("history_delete", { id: entry.id });
      // Optimistic update so the row disappears immediately. A
      // background refresh re-aligns with the db state in case the
      // delete succeeded but our optimistic view drifted.
      historyEntries = historyEntries.filter((e) => e.id !== entry.id);
      void refreshHistory();
    } catch (e) {
      historyError = formatError(e);
    }
  }

  // Read-only models refresh — the Settings window owns the picker;
  // we just need `models` populated enough for the Dictation tab's
  // "no model installed" banner to derive correctly.
  async function refreshModels() {
    try {
      models = await invoke<ModelCard[]>("model_list");
    } catch (e) {
      // Silent fail: the banner errs on the side of "show the
      // Dictation hot path" if the catalog can't load. The user
      // will hit a real error from `start_dictation` if the
      // selected model is genuinely missing.
      console.warn("[hush] model_list failed on main window", e);
    } finally {
      modelsLoaded = true;
    }
  }

  async function dismissFirstRun() {
    showFirstRun = false;
    // Restore focus to whatever the user was on before the modal
    // opened. Defensive: the previously-focused element may have
    // been removed from the DOM, in which case `.focus()` is a no-op
    // and the browser falls back to body, which is fine.
    firstRunPreviousFocus?.focus();
    firstRunPreviousFocus = null;
    try {
      await invoke("mark_first_run_completed");
    } catch (e) {
      // Best-effort: if the persist fails, the user sees the
      // welcome again on next launch, which is annoying but not
      // broken. Logged for diagnostics.
      console.error("mark_first_run_completed failed:", e);
    }
  }

  /// Selector for the focusable elements we cycle between in the
  /// welcome modal. Excludes elements with `tabindex="-1"` so the
  /// dialog wrapper itself (which is not focusable by users) does
  /// not enter the rotation.
  const FOCUSABLE_SELECTOR =
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  /// Trap Tab cycling inside the welcome modal (closes #48 focus
  /// trap). Tab from the last focusable wraps to the first;
  /// Shift+Tab from the first wraps to the last. Escape dismisses
  /// (per WAI-ARIA guidance for `role="dialog"` `aria-modal="true"`).
  function handleFirstRunKeydown(event: KeyboardEvent) {
    if (!showFirstRun) return;
    if (event.key === "Escape") {
      event.preventDefault();
      void dismissFirstRun();
      return;
    }
    if (event.key !== "Tab" || !firstRunCardEl) return;
    const focusable = firstRunCardEl.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR);
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  // Auto-focus the first focusable element when the modal opens, and
  // remember what was focused before so we can restore it on
  // dismiss. The effect intentionally runs whenever `showFirstRun`
  // flips — including back to false — but only acts on the
  // open transition.
  $effect(() => {
    if (showFirstRun && firstRunCardEl) {
      firstRunPreviousFocus =
        document.activeElement instanceof HTMLElement ? document.activeElement : null;
      // Focus the first action button so a keyboard-only user lands
      // on something useful (the "Open Microphone settings" button)
      // rather than the dialog wrapper.
      const first = firstRunCardEl.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
      first?.focus();
    }
  });

  async function openPrivacyPane(target: "microphone" | "input-monitoring") {
    try {
      await invoke("open_macos_privacy_pane", { target });
    } catch (e) {
      // No-op on non-macOS; user is unlikely to see this branch.
      console.error("open_macos_privacy_pane failed:", e);
    }
  }

  // ---- macOS permission diagnostic surface ----
  //
  // The macOS Permissions diagnostic + reset UI lives in the
  // Settings window (Phase 3). Here we keep just enough to drive
  // the Dictation-tab status hint:
  //  - `macosCapable`: are we on a host where macOS perm
  //    diagnostics apply at all (ie. `canReset === true`)?
  //  - `permStatuses`: live grant state from
  //    `diagnose_macos_permissions` — drives green pill vs yellow
  //    hint vs missing-list when something is denied.
  let macosCapable = $state(false);
  let permStatuses = $state<PermissionStatuses | null>(null);
  // True iff all three perms (mic, screen recording, input
  // monitoring) report `granted`. When true, the hint becomes a
  // small green "Permissions OK" pill instead of the yellow
  // recovery card.
  let allPermsGranted = $derived(
    !!permStatuses
      && permStatuses.microphone === "granted"
      && permStatuses.screenRecording === "granted"
      // PTT is opt-in on macOS 26+ and most users won't grant
      // Input Monitoring; treat its `not-determined` state as
      // acceptable. Only `denied` for mic / screen recording or a
      // sticky `denied` for input monitoring downgrades the pill.
      && permStatuses.inputMonitoring !== "denied",
  );

  // True iff something is *actually* wrong (any perm flagged
  // `denied`). On a fresh install nothing is `denied` yet —
  // everything's `not-determined` — so showing a yellow "Trouble?"
  // hint pre-emptively reads as "something is broken" when nothing
  // is. The Dictation hint should only appear when there's
  // something the user can act on.
  let anyPermsDenied = $derived(
    !!permStatuses
      && (permStatuses.microphone === "denied"
        || permStatuses.screenRecording === "denied"
        || permStatuses.inputMonitoring === "denied"),
  );

  // Meeting Mode (Phase C scaffold; refs #33 / #109). The repo is
  // empty until the streaming pump (#110) starts inserting sessions,
  // but the panel reads through the same IPC surface real sessions
  // will use, so the moment data starts flowing the panel populates
  // with no further wiring.
  let meetingSessions = $state<MeetingSession[]>([]);
  let meetingSessionsLoaded = $state(false);
  let meetingSessionsError = $state<string | null>(null);
  // Active session id from `meeting_active_session`. `null` means no
  // session in flight; non-null means the panel renders the Stop
  // button + a live status line.
  let meetingActiveId = $state<number | null>(null);
  // Live transcript for the active session — populated on a 3 s
  // poll while a session is in flight, cleared on stop. The poll
  // is cheap (one SELECT for the session row + one for its
  // utterances), much simpler than wiring a Tauri event for
  // pump-side appends. If polling becomes a bottleneck we can
  // promote to events without changing the consumer; the panel
  // just reads `meetingActiveDetail` regardless of how it gets
  // populated.
  let meetingActiveDetail = $state<MeetingSessionDetail | null>(null);
  let meetingActivePollHandle: ReturnType<typeof setInterval> | null = null;
  // Source kinds that have failed mid-session. Populated by the
  // `meeting:source-failed` Tauri event the pump emits when a
  // per-source path drops out (TCC revoke, device unplug,
  // inference panic). The panel renders these as struck-through
  // entries in the active-session source line so the user knows
  // capture is no longer working from that side. Cleared on each
  // session start so a fresh meeting starts with a clean slate.
  let meetingDroppedSources = $state<Set<string>>(new Set());
  let unlistenMeetingSourceFailed: UnlistenFn | null = null;
  // Disables the Start/Stop buttons during in-flight IPC calls so
  // a stale double-click can't race against itself. Same rationale
  // as the dictation flow's `busy` flag.
  let meetingBusy = $state(false);

  async function refreshMeetingSessions() {
    try {
      const [sessions, active] = await Promise.all([
        invoke<MeetingSession[]>("meeting_sessions_list"),
        invoke<{ active: number | null }>("meeting_active_session"),
      ]);
      meetingSessions = sessions;
      meetingActiveId = active.active;
      meetingSessionsError = null;
    } catch (e) {
      meetingSessionsError = formatError(e);
    } finally {
      meetingSessionsLoaded = true;
    }
  }

  /**
   * Pull the active session's full detail (utterances + metadata)
   * for the live-transcript view. Errors are swallowed onto the
   * meeting error region — a transient SQLite hiccup shouldn't tear
   * down the panel; the next poll tick recovers.
   */
  async function refreshActiveDetail(id: number) {
    try {
      meetingActiveDetail = await invoke<MeetingSessionDetail>(
        "meeting_session_get",
        { id },
      );
    } catch (e) {
      // Don't blow away the existing detail on a single failed poll;
      // the panel keeps showing whatever we last successfully read.
      meetingSessionsError = formatError(e);
    }
  }

  // Poll the active session's detail every 3s while a session is in
  // flight. The pump (#126) lands utterances every ~10s, so a 3s
  // poll surfaces them with at most ~3s of additional latency —
  // fine for human reading.
  //
  // `$effect` re-runs whenever `meetingActiveId` changes. On
  // session start: kick off an immediate fetch + start the
  // interval. On session stop: clear the interval and the detail.
  $effect(() => {
    if (meetingActivePollHandle !== null) {
      clearInterval(meetingActivePollHandle);
      meetingActivePollHandle = null;
    }
    const id = meetingActiveId;
    if (id === null) {
      meetingActiveDetail = null;
      return;
    }
    void refreshActiveDetail(id);
    meetingActivePollHandle = setInterval(() => {
      void refreshActiveDetail(id);
    }, 3000);
    return () => {
      if (meetingActivePollHandle !== null) {
        clearInterval(meetingActivePollHandle);
        meetingActivePollHandle = null;
      }
    };
  });

  async function deleteMeetingSession(session: MeetingSession) {
    try {
      await invoke("meeting_session_delete", { id: session.id });
      meetingSessions = meetingSessions.filter((s) => s.id !== session.id);
    } catch (e) {
      meetingSessionsError = formatError(e);
    }
  }

  /**
   * Lazy-loader for a historical session's full detail. Used by
   * the panel's expand-on-click affordance (#122 PR5). Errors are
   * surfaced through the meeting error region — same channel the
   * sessions list uses for its own load failures.
   */
  async function loadMeetingSessionDetail(
    id: number,
  ): Promise<MeetingSessionDetail> {
    try {
      const detail = await invoke<MeetingSessionDetail>(
        "meeting_session_get",
        { id },
      );
      meetingSessionsError = null;
      return detail;
    } catch (e) {
      meetingSessionsError = formatError(e);
      throw e;
    }
  }

  async function startMeetingSession() {
    meetingBusy = true;
    try {
      // Phase 3 of #122: meetings default to capturing mic +
      // system audio in parallel, the canonical "you on mic,
      // remote participants via SCK loopback" config. Each axis
      // is independently togglable in the panel's picker; here we
      // resolve the picker state into the wire shape the backend's
      // pump expects (Vec<AudioSource>).
      const sources: AudioSource[] = [];
      if (meetingMicId !== null) {
        sources.push({ kind: "microphone", deviceId: meetingMicId });
      }
      const sys = sources_findSystemAudio();
      if (meetingIncludeSystemAudio && sys?.isSupported) {
        sources.push({ kind: "system-audio" });
      }
      if (sources.length === 0) {
        meetingSessionsError =
          "Pick at least one audio source before starting a session.";
        return;
      }
      // Reset the dropped-sources set: each fresh meeting starts
      // with both sides assumed live; the listener re-populates on
      // any failures the new pump emits.
      meetingDroppedSources = new Set();
      // Without a per-platform foreground-app fetch wired up yet,
      // passing `null` falls through to the manager's "manual"
      // label. A future iteration captures the active foreground
      // app via active-win-pos-rs at click time.
      await invoke("meeting_start_manual", { sources, appName: null });
      await refreshMeetingSessions();
    } catch (e) {
      // Use the shared formatError so the actual `IpcError::MeetingSessions`
      // message (which already names the permission gap or the conflicting
      // session) reaches the user — `e instanceof Error` is false for
      // tagged IPC errors, so a plain `e.message` check would silently
      // mask the helpful copy.
      meetingSessionsError = formatError(e);
    } finally {
      meetingBusy = false;
    }
  }

  // Lookup helper for the system-audio listing. Inlining this at
  // each call site would either duplicate the filter or force an
  // ordering dependency on `sources` updates; the helper keeps the
  // intent — "is the system-audio entry supported on this host?" —
  // readable without recomputing.
  function sources_findSystemAudio(): AudioSourceListing | undefined {
    return sources.find((s) => s.kind === "system-audio");
  }

  async function stopMeetingSession() {
    meetingBusy = true;
    try {
      await invoke("meeting_stop_manual");
      await refreshMeetingSessions();
    } catch (e) {
      meetingSessionsError = formatError(e);
    } finally {
      meetingBusy = false;
    }
  }

  // Drives the Dictation-tab permissions hint:
  //  - `macosCapable` decides whether to show the hint at all
  //  - `permStatuses` decides green vs yellow rendering
  // The full diagnostic (with reset action) renders in the
  // Settings window's Permissions tab.
  async function loadMacosCapabilityFlag() {
    try {
      const res = await invoke<MacosPermissionDiagnostic>("diagnose_macos_permissions");
      macosCapable = res.canReset;
      permStatuses = res.statuses;
    } catch (e) {
      console.error("diagnose_macos_permissions failed:", e);
    }
  }

  /// Map a tagged IPC error to a user-facing string. Recovery hints are
  /// embedded here rather than in the Rust enum's Display because the
  /// hint copy is product-shaped (what the user *does next*), not
  // Open the Settings window and (best-effort) deep-link to a
  // specific tab. The settings page listens for
  // `settings:goto-tab` after mount; emitting before invoke risks
  // racing the listener registration, so we order
  // open → small tick → emit. Tauri events are broadcast to every
  // window, so the settings window picks this up regardless of
  // whether it was already open.
  async function openSettingsTab(tab: string) {
    try {
      await invoke("open_settings");
      // Two animation frames: enough time for the settings window
      // to mount + register its listener on the bus, well under
      // human perception (~32 ms). Cheaper than polling for a
      // ready signal and good enough for this UI affordance.
      await new Promise((r) => setTimeout(r, 50));
      await emit("settings:goto-tab", tab);
    } catch (e) {
      console.warn("[hush] open settings tab failed", e);
    }
  }

  /// engineering-shaped (what went wrong technically).
  function formatError(e: unknown): string {
    if (typeof e === "object" && e !== null && "kind" in e) {
      const ipc = e as IpcError;
      switch (ipc.kind) {
        case "transcription-unavailable":
          return (
            "No transcription model is loaded yet. Open Settings → " +
            "Model and pick one — Hush will fetch and verify it, " +
            "then load it without a restart."
          );
        case "audio":
          return `Audio capture error: ${ipc.message ?? "unknown"}. Check your microphone and Screen Recording permissions, or try a different input device.`;
        case "transcription":
          return `Transcription failed: ${ipc.message ?? "unknown"}. The model may be incompatible — try a different one.`;
        case "clipboard":
          return `Couldn't write to the clipboard: ${ipc.message ?? "unknown"}.`;
        case "internal":
          return `Internal error: ${ipc.message ?? "unknown"}. Please restart Hush.`;
        default:
          return ipc.message ? `${ipc.kind}: ${ipc.message}` : ipc.kind;
      }
    }
    return String(e);
  }
</script>

<!--
  Window-level keydown listener for the welcome modal. Lives outside
  the `{#if showFirstRun}` block because `<svelte:window>` cannot be
  placed inside ordinary blocks. The handler itself is gated on
  `showFirstRun`, so it is a no-op when the modal isn't visible.
-->
<svelte:window onkeydown={handleFirstRunKeydown} />

{#if showFirstRun}
  <!--
    First-run welcome modal. Static content; no fetches behind it.
    Backdrop intercepts clicks so the user has to engage with the
    Got It button rather than dismissing accidentally. The two
    permission sections call `open_macos_privacy_pane(...)` which
    is a no-op on Linux / Windows — those users can still click
    "Got it" and proceed without harm.

    A11y plumbing (closes #48):
    - Escape dismisses (the keydown listener is at window level
      above; gated on `showFirstRun`).
    - Tab cycles within the modal via `handleFirstRunKeydown`, so a
      keyboard user cannot accidentally focus elements behind the
      backdrop.
    - Auto-focus lands on the first action button on open; on
      dismiss, focus restores to whatever was focused before.
  -->
  <div class="first-run-backdrop" role="dialog" aria-modal="true" aria-labelledby="first-run-heading">
    <article class="first-run-card" bind:this={firstRunCardEl} tabindex="-1">
      <header>
        <h2 id="first-run-heading">Welcome to Hush</h2>
        <p class="first-run-tagline">
          Local, private voice-to-text. Here's what to know about
          permissions and privacy before you start.
        </p>
      </header>

      <section class="first-run-section">
        <h3>Microphone</h3>
        <p>
          Hush records audio only while you've explicitly started a
          dictation session. The first time you record, your OS will
          ask you to grant Hush microphone access. Without it, the
          dictation pipeline can't capture what you say.
        </p>
        <button class="ghost" onclick={() => openPrivacyPane("microphone")}>
          Open Microphone settings
        </button>
      </section>

      <section class="first-run-section">
        <h3>Input Monitoring (macOS — push-to-talk only)</h3>
        <p>
          Push-to-talk (hold <kbd>Right ⌘</kbd> while you speak) is
          <strong>opt-in</strong>: launching Hush with
          <code>HUSH_PTT_ENABLE=1</code> turns it on and macOS will
          prompt for Input Monitoring on first use. The toggle hotkey
          (<kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd>) and the
          on-screen Start button work without it.
        </p>
        <button class="ghost" onclick={() => openPrivacyPane("input-monitoring")}>
          Open Input Monitoring settings
        </button>
      </section>

      <footer class="first-run-footer">
        <p class="first-run-meta">
          Hush makes no other network requests except when you click
          Download on a model card. No telemetry, no cloud transcription,
          no analytics.
        </p>
        <button class="primary" onclick={dismissFirstRun}>Got it</button>
      </footer>
    </article>
  </div>
{/if}

<div class="app-shell">
  <AppSidebar
    active={activeSection}
    onSelect={(s) => (activeSection = s)}
    historyCount={historyEntries.length}
    meetingsCount={meetingSessions.length}
    activeMeetingInProgress={meetingActiveId !== null}
  />

  <main class="app-main" data-active-section={activeSection}>
    {#if activeSection === "dictation"}
      <header class="section-header">
        <h1>Dictation</h1>
        <p class="tagline">Press, talk, paste. Local Whisper transcription.</p>
      </header>

      <!--
        Hotkey hint card. Stays sticky inside the Dictation pane so
        the shortcut stays visible as the result block grows. PTT
        clause hides on macOS where it's disabled by default (#161).
      -->
      <aside class="hint hint-sticky" aria-label="Keyboard shortcuts">
        <strong>Shortcuts:</strong>
        <kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd> to toggle,
        or hold
        {#if isMacOS}<kbd>Right ⌘</kbd>{:else}<kbd>Right Ctrl</kbd>{/if}
        to push-to-talk.
      </aside>

      {#if activeModel}
        <!--
          Active-model chip. Single button so the entire pill is
          the click target — the "Model: X · Change" three-piece
          treatment in #196 read as disjointed; one chip with a
          quiet chevron is cleaner. Click → Settings → Model.
        -->
        <button
          type="button"
          class="active-model-chip"
          onclick={openModelSettings}
          aria-label="Active model: {activeModel.displayName}. Click to change."
          title="Change transcription model"
        >
          <span class="active-model-name">{activeModel.displayName}</span>
          <span class="active-model-chevron" aria-hidden="true">›</span>
        </button>
      {/if}

      <ControlsSection
        {sources}
        {sourcesLoaded}
        bind:selected
        {recording}
        {busy}
        {transcribing}
        {noModelInstalled}
        {error}
        onStart={start}
        onStop={stop}
        onScrollToModelPicker={openModelSettings}
      />

      {#if result}
        <ResultBlock {result} />
      {/if}

      {#if macosCapable}
        {#if allPermsGranted}
          <!--
            Green pill: AVFoundation / CoreGraphics / IOKit all
            report `granted`. Stays compact so it doesn't crowd
            the Dictation surface; click-through still leads into
            the Settings window for users who want to verify or
            adjust.
          -->
          <p class="permissions-pill permissions-pill-ok" data-testid="perms-pill-ok">
            <span class="dot" aria-hidden="true"></span>
            macOS permissions OK.
            <button
              type="button"
              class="link-button"
              onclick={() => void openSettingsTab("permissions")}
            >View</button>
          </p>
        {:else if anyPermsDenied}
          <!--
            Yellow hint: at least one permission is *denied* (a
            real, actionable problem). On a fresh install where
            mic / screen-recording are still `not-determined`
            because the user hasn't tried recording yet, the hint
            stays hidden — pre-emptively asking "Trouble?" reads
            as "something is broken" when nothing actually is.
          -->
          <p class="permissions-hint" data-testid="perms-hint-yellow">
            On macOS, dictation needs Microphone access (and Screen
            Recording for system-audio capture in meetings). Trouble?
            <button
              type="button"
              class="link-button"
              onclick={() => void openSettingsTab("permissions")}
            >Open the Permissions diagnostic</button>.
          </p>
        {/if}
      {/if}
    {:else if activeSection === "meetings"}
      <header class="section-header">
        <h1>Meetings</h1>
        <p class="tagline">
          Long-running multi-source capture with searchable transcripts.
        </p>
      </header>
      <MeetingSessionsPanel
        sessions={meetingSessions}
        sessionsLoaded={meetingSessionsLoaded}
        sessionsError={meetingSessionsError}
        activeSessionId={meetingActiveId}
        activeDetail={meetingActiveDetail}
        busy={meetingBusy}
        {sources}
        {sourcesLoaded}
        droppedSources={meetingDroppedSources}
        bind:meetingMicId
        bind:meetingIncludeSystemAudio
        onDelete={deleteMeetingSession}
        onStart={startMeetingSession}
        onStop={stopMeetingSession}
        onLoadDetail={loadMeetingSessionDetail}
      />
    {:else if activeSection === "history"}
      <header class="section-header">
        <h1>History</h1>
        <p class="tagline">Every dictation transcript, searchable.</p>
      </header>
      <HistoryPanel
        {historyEntries}
        {historyLoaded}
        {historyQuery}
        {historySearching}
        {historyError}
        {historyVersion}
        {formatTimestamp}
        {onSearchInput}
        onCopy={copyHistoryEntry}
        onDelete={deleteHistoryEntry}
      />
    {/if}
  </main>
</div>

<style>
:root {
  /* System font stack — picks San Francisco on macOS, Segoe UI on
     Windows, the distro default on Linux. Inter / Avenir were
     close-enough fallbacks but rendered noticeably "off" on macOS,
     so the app deliberately uses whatever the host considers
     native instead. The trailing emoji families let macOS render
     coloured emoji inline; Linux fonts handle the rest. */
  font-family:
    -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen,
    Ubuntu, Cantarell, "Helvetica Neue", Arial, sans-serif,
    "Apple Color Emoji", "Segoe UI Emoji";

  /* Layer 1 of "feel native". Two CSS primitives, no per-OS code:
     - `color-scheme` opts into the user agent's native dark-mode
       rendering for form controls, scrollbars, and the document
       background. Without this, scrollbars on macOS render as the
       light-mode style even when the rest of the app is dark.
     - `accent-color: auto` makes checkboxes / radios / range
       sliders / progress bars pick up the user's OS accent (the
       Mac highlight blue, the Windows accent, the GNOME accent)
       instead of the browser default cobalt. One line, real
       impact on perceived nativeness. */
  color-scheme: light dark;
  accent-color: auto;

  font-size: 16px;
  line-height: 24px;
  color: #0f0f0f;
  background-color: #f6f6f6;
  font-synthesis: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
}

/* Phase 1 IA redesign: app shell is a flex row of sidebar +
   content. The container's centred-with-max-width treatment moves
   to .app-main so each section reads at the same comfortable
   measure. */
.app-shell {
  display: flex;
  align-items: stretch;
  min-height: 100vh;
}

.app-main {
  flex: 1;
  min-width: 0;
  padding: 3vh 2rem 4vh;
  text-align: left;
}

.section-header {
  max-width: 36rem;
  margin: 0 auto 1.5rem;
}
.section-header h1 {
  margin: 0 0 0.25rem;
  font-size: 1.6rem;
  letter-spacing: -0.01em;
}

/* Centred content column inside the main pane. Each section's
   children inherit this measure via the existing per-component
   styles (HistoryPanel etc. use width:auto inside a max-width
   parent). */
.app-main > :not(.section-header) {
  max-width: 36rem;
  margin-left: auto;
  margin-right: auto;
}

.permissions-hint {
  margin: 1.25rem auto 0;
  padding: 0.75rem 1rem;
  background-color: #fff7e6;
  border: 1px solid #ffd591;
  border-radius: 8px;
  color: #8a5a00;
  font-size: 0.85rem;
  line-height: 1.5;
}
.permissions-pill {
  margin: 1.25rem auto 0;
  padding: 0.5rem 0.85rem;
  background-color: #e7f8ec;
  border: 1px solid #b6e5c5;
  border-radius: 999px;
  color: #2a6b3c;
  font-size: 0.8rem;
  line-height: 1.4;
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  /* Override the .app-main centring fallback so the pill renders
     compact on the left rather than full-width. */
  max-width: max-content;
  margin-left: auto;
  margin-right: auto;
}
.permissions-pill .dot {
  width: 0.55rem;
  height: 0.55rem;
  border-radius: 50%;
  background-color: #2eaa53;
  box-shadow: 0 0 0 2px rgba(46, 170, 83, 0.18);
  display: inline-block;
}
.link-button {
  background: none;
  border: none;
  padding: 0;
  color: inherit;
  font: inherit;
  text-decoration: underline;
  cursor: pointer;
}
.link-button:hover {
  text-decoration: none;
}
@media (prefers-color-scheme: dark) {
  .permissions-hint {
    background-color: #3a2c00;
    border-color: #6b5300;
    color: #ffd591;
  }
  .permissions-pill {
    background-color: #1a3a23;
    border-color: #2a6b3c;
    color: #b6e5c5;
  }
  .permissions-pill .dot {
    background-color: #4ad07a;
    box-shadow: 0 0 0 2px rgba(74, 208, 122, 0.2);
  }
}

h1 {
  margin: 0 0 0.25rem;
  font-size: 2.5rem;
  letter-spacing: -0.02em;
}

.tagline {
  color: #555;
  margin: 0 0 1.25rem;
}

.hint {
  margin: 0 0 2rem;
  padding: 0.75rem 1rem;
  background-color: #eef2ff;
  border: 1px solid #c7d2fe;
  border-radius: 8px;
  color: #2c3e8f;
  font-size: 0.9rem;
  text-align: left;
  line-height: 1.5;
}

.hint-sticky {
  /* Sticky so the hotkey hint stays visible as the page grows. The
     UX review flagged that the original (non-sticky) card scrolls
     off once the user has built up some history / replacements /
     vocabulary. */
  position: sticky;
  top: 0.75rem;
  z-index: 5;
  box-shadow: 0 2px 4px rgba(0, 0, 0, 0.05);
}

.active-model-chip {
  /* Quiet pill that surfaces the active model + opens the picker.
     The whole chip is the click target; a small chevron at the
     trailing edge hints "click to change" without shouting. Sits
     between the shortcut hint and the controls section. */
  margin: 0.5rem 0 1rem;
  padding: 0.3rem 0.7rem;
  font-size: 0.85rem;
  font-family: inherit;
  color: #444;
  background-color: #f3f3f5;
  border: 1px solid #e1e1e4;
  border-radius: 999px;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  transition: background-color 0.12s, border-color 0.12s, color 0.12s;
}
.active-model-chip:hover {
  background-color: #e8e8ec;
  border-color: #d4d4d9;
  color: #222;
}
.active-model-chip:focus-visible {
  outline: 2px solid #6a8cf0;
  outline-offset: 2px;
}
.active-model-name {
  font-weight: 500;
}
.active-model-chevron {
  font-size: 1.05em;
  color: #999;
  margin-left: 0.05rem;
  /* Vertically nudge the chevron — Apple-style "›" sits a hair
     above the optical baseline at this font size. */
  transform: translateY(-0.05em);
}
.active-model-chip:hover .active-model-chevron {
  color: #555;
}

@media (prefers-color-scheme: dark) {
  .active-model-chip {
    color: #b8b8b8;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .active-model-chip:hover {
    background-color: #353535;
    border-color: #4a4a4a;
    color: #e8e8e8;
  }
  .active-model-chevron {
    color: #777;
  }
  .active-model-chip:hover .active-model-chevron {
    color: #b8b8b8;
  }
}

.hint kbd {
  display: inline-block;
  padding: 0.05rem 0.4rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.85em;
  background-color: white;
  border: 1px solid #c7d2fe;
  border-radius: 4px;
  margin: 0 0.1rem;
}

.first-run-backdrop {
  position: fixed;
  inset: 0;
  background-color: rgba(15, 15, 15, 0.55);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
  padding: 1.5rem;
}

.first-run-card {
  background-color: #ffffff;
  border-radius: 12px;
  padding: 1.5rem 1.75rem;
  max-width: 30rem;
  width: 100%;
  max-height: calc(100vh - 3rem);
  overflow-y: auto;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.18);
  text-align: left;
}

.first-run-card h2 {
  margin: 0 0 0.35rem;
  font-size: 1.5rem;
  letter-spacing: -0.01em;
}

.first-run-tagline {
  margin: 0 0 1.25rem;
  color: #555;
  font-size: 0.95rem;
}

.first-run-section {
  margin-bottom: 1.25rem;
  padding-bottom: 1.25rem;
  border-bottom: 1px solid #eee;
}

.first-run-section:last-of-type {
  border-bottom: none;
}

.first-run-section h3 {
  margin: 0 0 0.35rem;
  font-size: 1rem;
  font-weight: 600;
}

.first-run-section p {
  margin: 0 0 0.6rem;
  font-size: 0.9rem;
  color: #444;
  line-height: 1.5;
}

.first-run-footer {
  margin-top: 0.75rem;
  display: flex;
  align-items: flex-end;
  gap: 1rem;
  justify-content: space-between;
  flex-wrap: wrap;
}

.first-run-meta {
  flex: 1;
  margin: 0;
  font-size: 0.8rem;
  color: #6a6a6a;
  line-height: 1.45;
}

button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  cursor: pointer;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  transition: border-color 0.15s, background-color 0.15s;
}

button:hover:not(:disabled) {
  border-color: #396cd8;
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

button.ghost {
  padding: 0.3em 0.75em;
  font-size: 0.8rem;
  font-weight: 500;
  background-color: transparent;
  border: 1px solid #d1d1d1;
}

button.ghost:hover:not(:disabled) {
  background-color: #f0f0f0;
}

button.primary {
  background-color: #6a8cf0;
  color: white;
  border-color: #6a8cf0;
  font-weight: 600;
}

button.primary:hover:not(:disabled) {
  background-color: #4a6cd0;
  border-color: #4a6cd0;
}

@media (prefers-color-scheme: dark) {
  :root {
    color: #f0f0f0;
    background-color: #1a1a1a;
  }
  .tagline {
    color: #aaa;
  }
  .hint {
    background-color: #1e2a4a;
    border-color: #3a4a7a;
    color: #c0d0ff;
  }
  .hint kbd {
    background-color: #0f1a2e;
    border-color: #3a4a7a;
    color: #f0f0f0;
  }
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button:hover:not(:disabled) {
    border-color: #6a8cf0;
  }
  button.ghost {
    border-color: #3a3a3a;
    color: #f0f0f0;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
  .first-run-backdrop {
    background-color: rgba(0, 0, 0, 0.65);
  }
  .first-run-card {
    background-color: #1f1f1f;
    color: #f0f0f0;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
  }
  .first-run-tagline,
  .first-run-section p,
  .first-run-meta {
    color: #c0c0c0;
  }
  .first-run-section {
    border-bottom-color: #2e2e2e;
  }
}
</style>
