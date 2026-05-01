<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { platform } from "@tauri-apps/plugin-os";
  import { onDestroy, onMount } from "svelte";

  import ControlsSection from "$lib/ControlsSection.svelte";
  import ResultBlock from "$lib/ResultBlock.svelte";
  import HistoryPanel from "$lib/HistoryPanel.svelte";
  import FirstRunModal from "$lib/FirstRunModal.svelte";
  import MacosPermsPill from "$lib/MacosPermsPill.svelte";
  import { formatErrorDisplay, type ErrorDisplay } from "$lib/errors";
  import { Events } from "$lib/events";
  import { formatTimestamp } from "$lib/format";
  import type {
    AudioSource,
    AudioSourceListing,
    DictationResult,
    HistoryEntry,
    MacosPermissionDiagnostic,
    ModelCard,
    MeetingExportFormat,
    MeetingSession,
    MeetingSessionDetail,
    PermissionStatuses,
  } from "$lib/types";

  // Page size for the history view. Hard-cap on the Rust side is 500;
  // 25 is plenty per page for a dictation history that grows linearly
  // with the user's actual usage (handful per day).
  const HISTORY_PAGE_SIZE = 25;

  // Sidebar nav state. Drives which content block renders in the

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
  let error = $state<ErrorDisplay | null>(null);

  let historyEntries = $state<HistoryEntry[]>([]);
  let historyLoaded = $state(false);
  let historyQuery = $state("");
  let historySearching = $state(false);
  let historyError = $state<ErrorDisplay | null>(null);
  // Unfiltered total — `historyEntries` shows the current page /
  // filtered slice, so the total drives the sidebar counter and
  // the "Clear all N" confirmation copy. Fetched via
  // `history_count` alongside every list/search refresh.
  let historyTotalCount = $state(0);
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
  // never shows again on this install. Modal markup, focus trap,
  // keydown handling, and styles live in `FirstRunModal.svelte`.
  let showFirstRun = $state(false);

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

  // Platform check used to pick the right modifier glyph in the
  // shortcut hint (Right ⌘ on macOS, Right Ctrl elsewhere). PTT is
  // on by default everywhere as of #194; this flag isn't gating
  // visibility anymore, just glyph copy.
  //
  // Resolved asynchronously via `@tauri-apps/plugin-os` (#272) —
  // replaces a deprecated `navigator.platform` regex match.
  // Defaults to `false` until the IPC round-trip lands; the only
  // visible consequence of the brief default is a single re-render
  // of the modifier-glyph kbd when the IPC resolves, which is
  // imperceptible in practice (the `onMount` runs before the
  // hotkey hint section paints in any non-pathological case).
  let isMacOS = $state(false);

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
    void emit(Events.UiRecordingState, recording);
  });

  onMount(async () => {
    // Platform glyph (#272). Resolves quickly; the kbd render
    // gates on the resulting bool. Failure leaves the default
    // `false` (Right Ctrl glyph) — same fallback `navigator.platform`
    // would have produced on a non-macOS host anyway.
    try {
      isMacOS = (await platform()) === "macos";
    } catch (e) {
      console.warn("[hush] platform() probe failed; defaulting to non-macOS glyph", e);
    }

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
    unlistenToggle = await listen(Events.HotkeyToggle, () => {
      if (busy) return; // ignore presses while a transcription is in flight
      if (recording) void stop();
      else void start();
    });

    // Native menu bar dispatches View → Section selections through
    // this event (#164 Phase 2). Sections are now always rendered in
    // one scrollable page, so we scroll to the anchor instead of
    // switching a tab.
    unlistenMenuGoto = await listen<string>(Events.MenuGotoSection, (e) => {
      const payload = e.payload;
      const sectionId =
        payload === "meetings" || payload === "history"
          ? "history-section"
          : "dictation-section";
      document.getElementById(sectionId)?.scrollIntoView({ behavior: "smooth" });
    });

    // Model-download events from the backend. The progress event
    // The Settings window owns the per-card download UI; here we
    // only listen for `model:download-done` so the Dictation tab's
    // "no model installed" banner disappears once a download in the
    // other window completes. Tauri broadcasts events to every
    // window, so the same backend emit reaches both surfaces.
    unlistenDownloadDone = await listen<{ id: string }>(Events.ModelDownloadDone, () => {
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
    }>(Events.MeetingSourceFailed, (e) => {
      const next = new Set(meetingDroppedSources);
      next.add(e.payload.sourceKind);
      meetingDroppedSources = next;
    });

    // Push-to-talk: the rdev listener in `hotkey::ptt` emits these
    // events on key-down and key-up of the configured PTT key.
    unlistenPttPress = await listen(Events.HotkeyPttPress, () => {
      if (busy || recording) return;
      void start();
    });
    unlistenPttRelease = await listen(Events.HotkeyPttRelease, () => {
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
      error = formatErrorDisplay(e);
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
      error = formatErrorDisplay(e);
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
      error = formatErrorDisplay(e);
    } finally {
      sourcesLoaded = true;
    }
  }

  async function refreshHistory() {
    historyError = null;
    historySearching = true;
    try {
      // Fetch the current page and the unfiltered total in
      // parallel — the total drives the "Clear all N"
      // confirmation copy and the sidebar counter.
      const [entries, total] = await Promise.all([
        invoke<HistoryEntry[]>("history_search", {
          query: historyQuery,
          limit: HISTORY_PAGE_SIZE,
          offset: 0,
        }),
        invoke<number>("history_count"),
      ]);
      historyEntries = entries;
      historyTotalCount = total;
      historyVersion += 1;
    } catch (e) {
      historyError = formatErrorDisplay(e);
    } finally {
      historyLoaded = true;
      historySearching = false;
    }
  }

  /// Debounce the search input so we don't fire SQLite queries on
  /// every keystroke. 200ms is the empirical sweet spot — fast
  /// enough that the user feels the list react, slow enough that
  /// holding a key doesn't queue dozens of queries.
  ///
  /// Cross-stream search (#357 phase 2): both `refreshHistory` and
  /// `refreshMeetingSessions` see the new query — the latter
  /// reads `historyQuery` directly inside `refreshMeetingSessions`,
  /// so we just fire the two refreshes in parallel.
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  function onSearchInput(e: Event) {
    historyQuery = (e.target as HTMLInputElement).value;
    if (searchTimer !== null) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      void refreshHistory();
      void refreshMeetingSessions();
    }, 200);
  }

  async function copyHistoryEntry(entry: HistoryEntry) {
    try {
      await navigator.clipboard.writeText(entry.transcript);
    } catch (e) {
      historyError = {
        headline: "Copy failed",
        hint: "Hush couldn't write to the clipboard. Try copying again, or paste from this entry's text directly.",
        details: String(e),
      };
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
      historyError = formatErrorDisplay(e);
    }
  }

  /// Export a single dictation transcript as CSV (#357 phase 3a).
  /// Two-step round-trip:
  ///   1. tauri-plugin-dialog's `save()` runs the OS Save File
  ///      picker; the user picks the location.
  ///   2. The backend writes the CSV body directly to that path.
  /// We deliberately avoid `tauri-plugin-fs` — its broad
  /// fs surface is more than this single feature needs. The
  /// backend writing the file keeps the trust boundary at the
  /// IPC and lets the capability stay narrow (`dialog:allow-save`
  /// only).
  ///
  /// Cancelling the picker is a no-op (no toast, no error).
  /// Failures inside the IPC route to the existing history-error
  /// region — same channel `history_search` and the rest use.
  async function exportDictationCsv(entry: HistoryEntry) {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const datePart = entry.createdAt.slice(0, 10);
      const path = await save({
        defaultPath: `hush-dictation-${datePart}.csv`,
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (path === null) {
        // User cancelled the dialog — quietly do nothing.
        return;
      }
      await invoke("history_export_row_csv", { id: entry.id, path });
    } catch (e) {
      historyError = formatErrorDisplay(e);
    }
  }

  /// Export a single meeting session in the user's chosen format
  /// (#357 phase 3b). Same two-step shape as the dictation export:
  /// dialog plugin picks the path, the backend writes the bytes.
  /// Format-specific filename stem ("hush-meeting-<date>.txt" /
  /// ".csv" / ".json") so the OS picker pre-populates a
  /// recognisable name. Cancellation is silent.
  async function exportMeetingSession(
    session: MeetingSession,
    format: MeetingExportFormat,
  ) {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const datePart = session.startedAt.slice(0, 10);
      const ext = format === "text" ? "txt" : format;
      const filterName =
        format === "text" ? "Plain text" : format === "csv" ? "CSV" : "JSON";
      const path = await save({
        defaultPath: `hush-meeting-${datePart}.${ext}`,
        filters: [{ name: filterName, extensions: [ext] }],
      });
      if (path === null) {
        return;
      }
      await invoke("meeting_session_export", {
        id: session.id,
        format,
        path,
      });
    } catch (e) {
      meetingSessionsError = formatErrorDisplay(e);
    }
  }

  async function clearAllHistory() {
    try {
      const removed = await invoke<number>("history_clear");
      historyEntries = [];
      historyTotalCount = 0;
      historyVersion += 1;
      historyError = null;
      // Confirmation feedback is surfaced inline by the panel — the
      // confirm prompt closes automatically when the IPC fires.
      // Logging the removed count is enough for now; a future toast
      // / status pill can render `Cleared {removed} transcripts.` if
      // the silent confirm path turns out to feel ambiguous.
      void removed;
    } catch (e) {
      historyError = formatErrorDisplay(e);
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
    try {
      await invoke("mark_first_run_completed");
    } catch (e) {
      // Best-effort: if the persist fails, the user sees the
      // welcome again on next launch, which is annoying but not
      // broken. Logged for diagnostics.
      console.error("mark_first_run_completed failed:", e);
    }
  }

  async function openPrivacyPane(
    target: "microphone" | "input-monitoring" | "screen-recording",
  ) {
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
      // Input Monitoring's `not-determined` is acceptable —
      // happens between PTT being enabled (default-on per #194)
      // and the user actually pressing the combo for the first
      // time. Only an explicit `denied` downgrades the pill.
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

  // Meeting Mode session list. Populated from the meetings repo via
  // `meeting_sessions_list`; rows are appended by the streaming pump
  // (`SessionManager`) as it persists chunks. The Meetings panel
  // reads through the same IPC surface real sessions use.
  let meetingSessions = $state<MeetingSession[]>([]);
  let meetingSessionsLoaded = $state(false);
  let meetingSessionsError = $state<ErrorDisplay | null>(null);
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
      // Use the search-aware IPC so a non-empty query filters the
      // returned sessions in lockstep with `history_search`. The
      // backend treats an empty `query` as "no filter" and falls
      // through to a plain `list()`, so this works the same as the
      // pre-#357 `meeting_sessions_list` call when the search box
      // is empty.
      const [sessions, active] = await Promise.all([
        invoke<MeetingSession[]>("meeting_sessions_search", {
          query: historyQuery,
        }),
        invoke<{ active: number | null }>("meeting_active_session"),
      ]);
      meetingSessions = sessions;
      meetingActiveId = active.active;
      meetingSessionsError = null;
    } catch (e) {
      meetingSessionsError = formatErrorDisplay(e);
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
      meetingSessionsError = formatErrorDisplay(e);
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
      meetingSessionsError = formatErrorDisplay(e);
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
      meetingSessionsError = formatErrorDisplay(e);
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
        meetingSessionsError = {
          headline: "No audio sources selected",
          hint: "Pick at least one source (microphone or system audio) before starting a session.",
        };
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
      meetingSessionsError = formatErrorDisplay(e);
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
      meetingSessionsError = formatErrorDisplay(e);
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
      await emit(Events.SettingsGotoTab, tab);
    } catch (e) {
      console.warn("[hush] open settings tab failed", e);
    }
  }

  // Error formatting moved to `lib/errors.ts` (#205): the
  // `formatErrorDisplay` helper used throughout this file routes
  // every error through one source of truth. The local
  // `formatError(e) → string` that lived here was deleted; the
  // remaining string-shaped error surface (`firstRunResetMessage`)
  // builds its copy directly.
</script>

<FirstRunModal
  show={showFirstRun}
  onDismiss={dismissFirstRun}
  onOpenPrivacyPane={openPrivacyPane}
/>

<header class="app-bar">
  <div class="brand">
    <img
      class="brand-icon"
      src="/app-icon.png"
      srcset="/app-icon.png 1x, /app-icon@2x.png 2x"
      alt=""
      aria-hidden="true"
      width="22"
      height="22"
    />
    <span class="brand-name">Hush</span>
  </div>
  <button
    type="button"
    class="settings-btn"
    onclick={() => openSettingsTab("general")}
    title="Settings (⌘,)"
  >
    Settings <kbd>⌘,</kbd>
  </button>
</header>

<main class="app-main">
  <section id="dictation-section" class="page-section">
    <header class="section-header">
      <h1>Dictation</h1>
      <p class="tagline">Press, talk, paste. Local Whisper transcription.</p>
    </header>

    <aside class="hint hint-sticky" aria-label="Keyboard shortcuts">
      <strong>Shortcuts:</strong>
      <kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd> to toggle,
      or hold
      {#if isMacOS}<kbd>Right ⌘</kbd>{:else}<kbd>Right Ctrl</kbd>{/if}
      to push-to-talk.
    </aside>

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
      activeModelName={activeModel?.displayName ?? null}
    />

    {#if result}
      <ResultBlock {result} />
    {/if}

    <MacosPermsPill
      capable={macosCapable}
      allGranted={allPermsGranted}
      anyDenied={anyPermsDenied}
      onOpenPermissions={() => openSettingsTab("permissions")}
    />
  </section>

  <section id="history-section" class="page-section">
    <header class="section-header">
      <h1>History</h1>
      <p class="tagline">Every transcript Hush has captured, searchable.</p>
    </header>
    <HistoryPanel
      {historyEntries}
      {historyLoaded}
      {historyQuery}
      {historySearching}
      {historyError}
      {historyVersion}
      {historyTotalCount}
      meetingSessions={meetingSessions}
      meetingSessionsLoaded={meetingSessionsLoaded}
      {models}
      {formatTimestamp}
      {onSearchInput}
      onCopy={copyHistoryEntry}
      onDelete={deleteHistoryEntry}
      onExportDictationCsv={exportDictationCsv}
      onMeetingDelete={deleteMeetingSession}
      onMeetingLoadDetail={loadMeetingSessionDetail}
      onMeetingExport={exportMeetingSession}
      onClearAll={clearAllHistory}
    />
  </section>
</main>

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
  color: var(--text-primary);
  background-color: var(--bg-app);
  font-synthesis: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
}

/* Single-scroll layout: top bar + main content column. */
.app-bar {
  position: sticky;
  top: 0;
  z-index: 10;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0.6rem 1.5rem;
  background-color: var(--bg-sidebar, #f0f0f3);
  border-bottom: 1px solid var(--border, #e1e1e1);
}

.brand {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}
.brand-icon {
  width: 22px;
  height: 22px;
  border-radius: 5px;
  image-rendering: -webkit-optimize-contrast;
}
.brand-name {
  font-weight: 600;
  font-size: 0.95rem;
  letter-spacing: -0.01em;
}

.settings-btn {
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  padding: 0.3rem 0.75rem;
  background: none;
  border: 1px solid var(--border-input, #d1d1d6);
  border-radius: var(--radius-md, 8px);
  font-family: inherit;
  font-size: 0.85rem;
  font-weight: 500;
  color: var(--text-secondary, #555);
  cursor: pointer;
  transition: background-color 0.12s, border-color 0.12s;
}
.settings-btn:hover {
  background-color: rgba(44, 62, 143, 0.07);
  border-color: var(--accent, #5b7ee5);
  color: var(--text-primary, #111);
}
.settings-btn:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}
.settings-btn kbd {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.78em;
  color: var(--text-muted, #888);
}

.app-main {
  padding: 0 1.5rem 4rem;
  text-align: left;
  overflow-y: auto;
  height: calc(100vh - 45px); /* subtract app-bar height */
  box-sizing: border-box;
}

.page-section {
  max-width: 36rem;
  margin: 0 auto;
  padding-top: 2.5rem;
}

.page-section + .page-section {
  border-top: 1px solid var(--border, #e1e1e1);
  margin-top: 2rem;
  padding-top: 2.5rem;
}

@media (prefers-color-scheme: dark) {
  .app-bar {
    border-bottom-color: #2f2f33;
  }
  .brand-name { color: #e8e8e8; }
  .settings-btn {
    color: #a0a0a0;
    border-color: #3a3a3a;
  }
  .settings-btn:hover {
    background-color: rgba(150, 170, 240, 0.1);
    border-color: var(--accent);
    color: #e8e8e8;
  }
  .page-section + .page-section {
    border-top-color: #2f2f33;
  }
}

.section-header {
  margin-bottom: 1.5rem;
}
.section-header h1 {
  margin: 0 0 0.25rem;
  font-size: 1.75rem;
  letter-spacing: -0.02em;
}

.tagline {
  color: var(--text-muted);
  margin: 0 0 1.25rem;
}

.hint {
  margin: 0 0 2rem;
  padding: 0.75rem 1rem;
  background-color: var(--info-bg);
  border: 1px solid var(--info-border);
  border-radius: var(--radius-md);
  color: var(--info-text);
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

.hint kbd {
  display: inline-block;
  padding: 0.05rem 0.4rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.85em;
  background-color: var(--bg-surface);
  border: 1px solid var(--info-border);
  border-radius: var(--radius-sm);
  margin: 0 0.1rem;
}

</style>
