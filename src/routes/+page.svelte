<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";
  import ControlsSection from "$lib/ControlsSection.svelte";
  import ResultBlock from "$lib/ResultBlock.svelte";
  import HistoryPanel from "$lib/HistoryPanel.svelte";
  import ReplacementsPanel from "$lib/ReplacementsPanel.svelte";
  import VocabularyPanel from "$lib/VocabularyPanel.svelte";
  import ModelPickerPanel from "$lib/ModelPickerPanel.svelte";
  import MacosDiagnosticPanel from "$lib/MacosDiagnosticPanel.svelte";
  import MeetingSessionsPanel from "$lib/MeetingSessionsPanel.svelte";
  import type {
    AudioSource,
    AudioSourceListing,
    DictationResult,
    DownloadProgress,
    HistoryEntry,
    IpcError,
    MacosPermissionDiagnostic,
    MacosPermissionResetResult,
    ModelCard,
    ModelSelectNotice,
    ReplacementRule,
    VocabularyTerm,
    MeetingSession,
  } from "$lib/types";

  // Page size for the history view. Hard-cap on the Rust side is 500;
  // 25 is plenty per page for a dictation history that grows linearly
  // with the user's actual usage (handful per day).
  const HISTORY_PAGE_SIZE = 25;

  let sources = $state<AudioSourceListing[]>([]);
  let sourcesLoaded = $state(false);
  // Selected source id. Mic devices use their device name; the
  // system-audio entry uses the literal string `"system"`. Mapped to
  // an `AudioSource` for `start_dictation` in `start()`.
  let selected = $state<string | null>(null);
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

  let replacements = $state<ReplacementRule[]>([]);
  let replacementsLoaded = $state(false);
  let replacementsError = $state<string | null>(null);
  let newFind = $state("");
  let newReplace = $state("");
  let findInputEl = $state<HTMLInputElement | null>(null);

  let vocabulary = $state<VocabularyTerm[]>([]);
  let vocabularyLoaded = $state(false);
  let vocabularyError = $state<string | null>(null);
  let newVocab = $state("");
  let vocabInputEl = $state<HTMLInputElement | null>(null);

  let models = $state<ModelCard[]>([]);
  let modelsLoaded = $state(false);
  let modelsError = $state<string | null>(null);
  let modelsRestartNotice = $state<ModelSelectNotice>(null);

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

  // Per-card transient state for the download flow. Two parallel
  // `Map<id, …>`s keep the per-row status independent of the catalog
  // array's order, so a `model:download-progress` event for one card
  // doesn't have to walk the whole list to find its target. The Maps
  // are intentionally swapped wholesale on each update (`new Map(prev)`)
  // to trip Svelte's reactivity — Svelte 5 runes don't observe
  // mutations on built-in Maps.
  let downloading = $state<Map<string, DownloadProgress>>(new Map());
  let downloadFailed = $state<Map<string, string>>(new Map());

  let unlistenDownloadProgress: UnlistenFn | null = null;
  let unlistenDownloadDone: UnlistenFn | null = null;
  let unlistenDownloadFailed: UnlistenFn | null = null;

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

  // Scroll the model picker section into view. Used by the "Set up
  // your first model" banner and the click-through on the
  // transcription-unavailable error chip.
  function scrollToModelPicker() {
    const heading = document.getElementById("models-heading");
    if (heading) heading.scrollIntoView({ behavior: "smooth", block: "start" });
  }

  let unlistenToggle: UnlistenFn | null = null;
  let unlistenPttPress: UnlistenFn | null = null;
  let unlistenPttRelease: UnlistenFn | null = null;

  // Keep the document title in sync with recording state. Helps users who
  // have the window in the background — at-a-glance signal that the mic
  // is hot. Tauri exposes `window.document` like a regular browser.
  $effect(() => {
    document.title = recording ? "Hush ● Recording" : "Hush";
  });

  onMount(async () => {
    // Check the first-run flag before anything else — if this is a
    // fresh install, we want the welcome modal up before the user
    // tries to use the app and runs into a permission prompt with
    // no context. The fetch is independent of the others so it can
    // race in parallel.
    void invoke<boolean>("get_first_run_completed").then((done) => {
      if (!done) showFirstRun = true;
    });

    // Fire all five fetches concurrently rather than sequentially —
    // the user-visible time-to-paint is bounded by the slowest single
    // call instead of the sum. Each fetch handles its own loading
    // and error state so a slow one (history, in particular) doesn't
    // block the rest of the page.
    await Promise.all([
      loadSources(),
      refreshHistory(),
      refreshReplacements(),
      refreshVocabulary(),
      refreshModels(),
      loadMacosDiagnostic(),
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

    // Model-download events from the backend. The progress event
    // fires per-chunk during the download; done / failed are
    // terminal. The frontend's job is just to mirror these into the
    // two Maps so the per-card UI can switch between idle / progress /
    // failed / downloaded states. After `done` we re-fetch the
    // catalog so the card transitions to "downloaded" without a page
    // reload.
    type DownloadProgressEvent = { id: string; bytesReceived: number; bytesTotal: number | null };
    type DownloadStatusEvent = { id: string; message: string | null };

    unlistenDownloadProgress = await listen<DownloadProgressEvent>(
      "model:download-progress",
      (e) => {
        const next = new Map(downloading);
        next.set(e.payload.id, {
          received: e.payload.bytesReceived,
          total: e.payload.bytesTotal,
        });
        downloading = next;
      }
    );
    unlistenDownloadDone = await listen<DownloadStatusEvent>("model:download-done", (e) => {
      const next = new Map(downloading);
      next.delete(e.payload.id);
      downloading = next;
      // Refresh so the catalog's `isDownloaded` flips for this row.
      void refreshModels();
    });
    unlistenDownloadFailed = await listen<DownloadStatusEvent>("model:download-failed", (e) => {
      const nextDownloading = new Map(downloading);
      nextDownloading.delete(e.payload.id);
      downloading = nextDownloading;
      const nextFailed = new Map(downloadFailed);
      nextFailed.set(
        e.payload.id,
        e.payload.message ?? "Download failed for an unspecified reason."
      );
      downloadFailed = nextFailed;
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
    unlistenPttPress?.();
    unlistenPttRelease?.();
    unlistenDownloadProgress?.();
    unlistenDownloadDone?.();
    unlistenDownloadFailed?.();
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
      // first mic in the list. We don't auto-pick the system-audio
      // entry — that's an explicit user choice the picker exposes.
      const mics = sources.filter((s) => s.kind === "microphone");
      const def = mics.find((s) => s.isDefault) ?? mics[0];
      if (def) selected = def.id;
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

  async function refreshReplacements() {
    replacementsError = null;
    try {
      replacements = await invoke<ReplacementRule[]>("replacements_list");
    } catch (e) {
      replacementsError = formatError(e);
    } finally {
      replacementsLoaded = true;
    }
  }

  async function addReplacement(e: Event) {
    e.preventDefault();
    if (newFind.trim().length === 0) return; // empty find is a no-op rule
    try {
      const created = await invoke<ReplacementRule>("replacement_create", {
        findText: newFind,
        replaceText: newReplace,
        // Default sort_order=0 so the new rule sorts by id (insertion
        // order). A reorder UI lands when users ask for it.
        sortOrder: 0,
      });
      replacements = [...replacements, created];
      newFind = "";
      newReplace = "";
      // Return focus to the find input so a keyboard-only user can
      // type the next rule without Tabbing back from the bottom of
      // the list.
      findInputEl?.focus();
    } catch (err) {
      replacementsError = formatError(err);
    }
  }

  async function deleteReplacement(rule: ReplacementRule) {
    try {
      await invoke("replacement_delete", { id: rule.id });
      // Optimistic update; a background refresh would re-align if any
      // drift, but the trait contract guarantees no-op on missing id so
      // a re-fetch is unnecessary in the happy path.
      replacements = replacements.filter((r) => r.id !== rule.id);
    } catch (err) {
      replacementsError = formatError(err);
    }
  }

  async function refreshVocabulary() {
    vocabularyError = null;
    try {
      vocabulary = await invoke<VocabularyTerm[]>("vocabulary_list");
    } catch (e) {
      vocabularyError = formatError(e);
    } finally {
      vocabularyLoaded = true;
    }
  }

  async function addVocabulary(e: Event) {
    e.preventDefault();
    const trimmed = newVocab.trim();
    if (trimmed.length === 0) return;
    try {
      const created = await invoke<VocabularyTerm>("vocabulary_create", {
        term: trimmed,
      });
      vocabulary = [...vocabulary, created];
      newVocab = "";
      vocabInputEl?.focus(); // same focus pattern as the replacements form
    } catch (err) {
      // Surface unique-constraint violations as a friendlier message
      // than the raw "UNIQUE constraint failed: dictionary_terms.term"
      // that bubbles up from sqlx.
      const formatted = formatError(err);
      vocabularyError = formatted.toLowerCase().includes("unique")
        ? `"${trimmed}" is already in your vocabulary.`
        : formatted;
    }
  }

  async function deleteVocabulary(term: VocabularyTerm) {
    try {
      await invoke("vocabulary_delete", { id: term.id });
      vocabulary = vocabulary.filter((t) => t.id !== term.id);
    } catch (err) {
      vocabularyError = formatError(err);
    }
  }

  async function refreshModels() {
    modelsError = null;
    try {
      models = await invoke<ModelCard[]>("model_list");
    } catch (e) {
      modelsError = formatError(e);
    } finally {
      modelsLoaded = true;
    }
  }

  async function selectModel(card: ModelCard) {
    try {
      const result = await invoke<{ loaded: boolean }>("model_select", { id: card.id });
      // Local card state moves the Default badge regardless of load
      // outcome — the selection has persisted either way.
      models = models.map((m) => ({ ...m, isSelected: m.id === card.id }));
      // `loaded === true` means the backend hot-swapped to the new
      // transcriber and the user can record immediately. `false`
      // means the file isn't on disk yet (or the whisper feature is
      // off in this build); selection persists, but they need to
      // Download before they can use it. The notice pill below
      // branches on this.
      modelsRestartNotice = result.loaded
        ? "loaded"
        : card.isDownloaded
          ? "needs-restart"
          : "needs-download";
    } catch (err) {
      modelsError = formatError(err);
    }
  }

  async function downloadModel(card: ModelCard) {
    // Clear any previous failure for this card before retrying — keeps
    // the per-card error chip from sticking around after the user
    // clicks Try again.
    if (downloadFailed.has(card.id)) {
      const next = new Map(downloadFailed);
      next.delete(card.id);
      downloadFailed = next;
    }

    try {
      await invoke("model_download", { id: card.id });
      // Only show the optimistic progress chip *after* the backend has
      // accepted the request (closes the retry-race half of #48).
      // Pre-invoke optimistic state caused a flash of progress on
      // synchronous IPC failure (e.g. SHA-256 not configured) — the
      // chip would appear and disappear in the same tick. Setting it
      // here means the failure path simply never shows the chip.
      const next = new Map(downloading);
      next.set(card.id, { received: 0, total: card.sizeMb * 1024 * 1024 });
      downloading = next;
    } catch (err) {
      // The IpcError::Settings("...SHA-256 not configured...") path
      // surfaces here. Re-shape into a friendlier per-card message
      // rather than the raw `settings: ...` from formatError.
      const formatted = formatError(err);
      const friendly = formatted.toLowerCase().includes("sha-256")
        ? `Auto-download is not yet configured for ${card.displayName}. Place ${card.filename} in the models directory manually for now.`
        : formatted;
      const fail = new Map(downloadFailed);
      fail.set(card.id, friendly);
      downloadFailed = fail;
    }
  }

  async function cancelDownload(card: ModelCard) {
    try {
      await invoke("model_cancel_download", { id: card.id });
      // The backend will fire `model:download-failed` with a
      // "cancelled" message; the existing handler removes the
      // download chip and shows the error. We do nothing optimistic
      // here — letting the event flow drive the state keeps a single
      // source of truth.
    } catch (err) {
      modelsError = formatError(err);
    }
  }

  async function removeModel(card: ModelCard) {
    try {
      await invoke("model_remove", { id: card.id });
      await refreshModels(); // card flips back to "not downloaded"
    } catch (err) {
      modelsError = formatError(err);
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
  // Backend: `diagnose_macos_permissions` returns a snapshot (bundle
  // id, hint copy, and whether reset is supported); `reset_macos_permissions`
  // wraps the `tccutil reset` recipe documented in
  // docs/macos-permissions.md. We only show the UI section when the
  // diagnostic comes back with `canReset: true`, which is currently
  // macOS-only — non-macOS platforms get the section hidden entirely
  // rather than greyed-out, since there's nothing actionable.
  let macosDiagnostic = $state<MacosPermissionDiagnostic | null>(null);
  let macosDiagnosticOpen = $state(false);
  let macosResetMessage = $state<string | null>(null);
  let macosResetting = $state(false);

  // Meeting Mode (Phase C scaffold; refs #33 / #109). The repo is
  // empty until the streaming pump (#110) starts inserting sessions,
  // but the panel reads through the same IPC surface real sessions
  // will use, so the moment data starts flowing the panel populates
  // with no further wiring.
  let meetingSessions = $state<MeetingSession[]>([]);
  let meetingSessionsLoaded = $state(false);
  let meetingSessionsError = $state<string | null>(null);

  async function refreshMeetingSessions() {
    try {
      meetingSessions = await invoke<MeetingSession[]>("meeting_sessions_list");
      meetingSessionsError = null;
    } catch (e) {
      meetingSessionsError = e instanceof Error ? e.message : "Failed to load meeting sessions.";
    } finally {
      meetingSessionsLoaded = true;
    }
  }

  async function deleteMeetingSession(session: MeetingSession) {
    try {
      await invoke("meeting_session_delete", { id: session.id });
      meetingSessions = meetingSessions.filter((s) => s.id !== session.id);
    } catch (e) {
      meetingSessionsError = e instanceof Error ? e.message : "Failed to delete session.";
    }
  }

  async function loadMacosDiagnostic() {
    if (macosDiagnostic !== null) return; // cached after first load
    try {
      macosDiagnostic = await invoke<MacosPermissionDiagnostic>(
        "diagnose_macos_permissions",
      );
    } catch (e) {
      console.error("diagnose_macos_permissions failed:", e);
    }
  }

  async function runMacosReset() {
    macosResetting = true;
    macosResetMessage = null;
    try {
      const result = await invoke<MacosPermissionResetResult>(
        "reset_macos_permissions",
      );
      macosResetMessage = result.summary;
    } catch (e) {
      macosResetMessage =
        e instanceof Error ? e.message : "Reset failed — see logs.";
    } finally {
      macosResetting = false;
    }
  }

  /// Format a byte count as "12.4 MB" — used for download progress.
  /// We deliberately don't use units smaller than MB because the
  /// smallest model is ~75 MB; KB resolution would just be noise.
  function formatMb(bytes: number): string {
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  }

  function formatTimestamp(iso: string): string {
    // The backend stores `YYYY-MM-DDTHH:MM:SSZ`. JS Date parses ISO-8601
    // natively; locale formatting follows the user's system.
    const date = new Date(iso);
    if (Number.isNaN(date.getTime())) return iso;
    return date.toLocaleString();
  }

  /// Map a tagged IPC error to a user-facing string. Recovery hints are
  /// embedded here rather than in the Rust enum's Display because the
  /// hint copy is product-shaped (what the user *does next*), not
  /// engineering-shaped (what went wrong technically).
  function formatError(e: unknown): string {
    if (typeof e === "object" && e !== null && "kind" in e) {
      const ipc = e as IpcError;
      switch (ipc.kind) {
        case "transcription-unavailable":
          return (
            "No transcription model is loaded yet. Pick one from the " +
            "Models section below and click Download — Hush will fetch " +
            "and verify it, then prompt you to restart."
          );
        case "audio":
          return `Microphone error: ${ipc.message ?? "unknown"}. Try selecting a different input device.`;
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
        <h3>Input Monitoring (macOS)</h3>
        <p>
          Hush's push-to-talk hotkey uses a low-level keyboard hook so
          it works while another app is focused. macOS calls this
          "Input Monitoring" and asks once. If you missed the prompt
          or declined, push-to-talk will silently do nothing until you
          grant the permission in System Settings. The toggle hotkey
          uses a different API and works without it.
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

<main class="container">
  <h1>Hush</h1>
  <p class="tagline">Press, talk, paste. Local Whisper transcription.</p>

  <!--
    Hotkey hint card. Defaults are baked here for M2; once the settings
    panel lands (M3) this becomes a fetched value and the env-var
    override notes go away.
  -->
  <aside class="hint hint-sticky" aria-label="Keyboard shortcuts">
    <strong>Shortcuts:</strong>
    <kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd> to toggle,
    or hold <kbd>Right Ctrl</kbd> to push-to-talk.
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
    onScrollToModelPicker={scrollToModelPicker}
  />

  {#if result}
    <ResultBlock {result} />
  {/if}

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

  <ReplacementsPanel
    {replacements}
    {replacementsLoaded}
    {replacementsError}
    bind:newFind
    bind:newReplace
    bind:inputEl={findInputEl}
    onSubmit={addReplacement}
    onDelete={deleteReplacement}
  />

  <ModelPickerPanel
    {models}
    {modelsLoaded}
    {modelsError}
    {modelsRestartNotice}
    {downloading}
    {downloadFailed}
    {formatMb}
    onSelect={selectModel}
    onDownload={downloadModel}
    onCancel={cancelDownload}
    onRemove={removeModel}
  />

  <VocabularyPanel
    {vocabulary}
    {vocabularyLoaded}
    {vocabularyError}
    bind:newVocab
    bind:inputEl={vocabInputEl}
    onSubmit={addVocabulary}
    onDelete={deleteVocabulary}
  />

  {#if macosDiagnostic?.canReset}
    <MacosDiagnosticPanel
      {macosDiagnostic}
      bind:macosDiagnosticOpen
      {macosResetMessage}
      {macosResetting}
      onOpenPrivacyPane={openPrivacyPane}
      onReset={runMacosReset}
    />
  {/if}

  <MeetingSessionsPanel
    sessions={meetingSessions}
    sessionsLoaded={meetingSessionsLoaded}
    sessionsError={meetingSessionsError}
    onDelete={deleteMeetingSession}
  />
</main>

<style>
:root {
  font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
  font-size: 16px;
  line-height: 24px;
  color: #0f0f0f;
  background-color: #f6f6f6;
  font-synthesis: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
}

.container {
  max-width: 36rem;
  margin: 0 auto;
  padding: 4vh 1.5rem;
  text-align: center;
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
