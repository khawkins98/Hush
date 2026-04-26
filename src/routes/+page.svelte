<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";

  // Mirror the camelCase serde renames on the Rust side.
  type AudioDevice = { id: string; name: string; isDefault: boolean };
  type ForegroundApp = { appName: string; windowTitle: string };
  type DictationResult = { text: string; foreground: ForegroundApp | null };
  type IpcError = { kind: string; message?: string };
  type HistoryEntry = {
    id: number;
    transcript: string;
    appName: string | null;
    windowTitle: string | null;
    model: string;
    durationMs: number | null;
    createdAt: string;
  };
  type ReplacementRule = {
    id: number;
    findText: string;
    replaceText: string;
    sortOrder: number;
  };
  type VocabularyTerm = {
    id: number;
    term: string;
  };
  // Mirrors `ModelCard` on the Rust side. `metadata` is flattened by
  // serde so all the catalog fields land at the top level.
  type ModelCard = {
    id: string;
    displayName: string;
    filename: string;
    sizeMb: number;
    speedRating: number;
    accuracyRating: number;
    description: string;
    isDefault: boolean;
    isDownloaded: boolean;
    isSelected: boolean;
    expectedPath: string;
  };

  // Page size for the history view. Hard-cap on the Rust side is 500;
  // 25 is plenty per page for a dictation history that grows linearly
  // with the user's actual usage (handful per day).
  const HISTORY_PAGE_SIZE = 25;

  let devices = $state<AudioDevice[]>([]);
  let devicesLoaded = $state(false);
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
  // Notice pill shown after the user picks a model. Three flavours:
  //   - "loaded"         : backend hot-swapped; ready to record now.
  //   - "needs-download" : selection persisted but the model file is
  //                        not on disk yet — user has to Download.
  //   - "needs-restart"  : the file is on disk but hot-swap returned
  //                        false (whisper feature off, or some other
  //                        backend reason). Restart picks it up. Rare
  //                        in practice; covers the edge case so the
  //                        message stays accurate.
  //   - null             : no notice currently visible.
  type ModelSelectNotice = "loaded" | "needs-download" | "needs-restart" | null;
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
  let downloading = $state<Map<string, { received: number; total: number | null }>>(new Map());
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
      loadDevices(),
      refreshHistory(),
      refreshReplacements(),
      refreshVocabulary(),
      refreshModels(),
      loadMacosDiagnostic(),
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
      await invoke("start_dictation", { deviceId: selected });
      recording = true;
    } catch (e) {
      error = formatError(e);
    } finally {
      busy = false;
    }
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

  async function loadDevices() {
    try {
      devices = await invoke<AudioDevice[]>("list_input_devices");
      const def = devices.find((d) => d.isDefault) ?? devices[0];
      if (def) selected = def.id;
    } catch (e) {
      error = formatError(e);
    } finally {
      devicesLoaded = true;
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
  type MacosPermissionDiagnostic = {
    bundleId: string;
    microphoneHint: string;
    inputMonitoringHint: string;
    canReset: boolean;
  };
  type MacosPermissionResetResult = {
    anyReset: boolean;
    summary: string;
  };

  let macosDiagnostic = $state<MacosPermissionDiagnostic | null>(null);
  let macosDiagnosticOpen = $state(false);
  let macosResetMessage = $state<string | null>(null);
  let macosResetting = $state(false);

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

  {#if noModelInstalled}
    <!--
      No model is on disk yet. Banner replaces the bottom-of-page
      hunt and the "transcription not set up" error-after-click flow.
      Click → scroll to the picker; from there the user clicks
      Download on a card and the auto-download path takes over.
    -->
    <aside class="setup-banner" role="status" aria-label="First-time setup">
      <div class="setup-banner-text">
        <strong>Set up your first model</strong>
        <span>
          Hush needs a Whisper model to transcribe. Pick one from the
          Models section below — Whisper Base is a solid default.
        </span>
      </div>
      <button class="primary" onclick={scrollToModelPicker}>
        Choose a model
      </button>
    </aside>
  {/if}

  <section class="controls">
    <label>
      Input device
      {#if !devicesLoaded}
        <p class="empty-devices">Loading devices…</p>
      {:else if devices.length === 0}
        <p class="empty-devices">
          No microphones detected. On macOS, grant microphone access in
          System Settings → Privacy &amp; Security. On Linux, check that
          PulseAudio / PipeWire is running.
        </p>
      {:else}
        <select bind:value={selected} disabled={recording || busy}>
          {#each devices as device (device.id)}
            <option value={device.id}>
              {device.name}{device.isDefault ? " (default)" : ""}
            </option>
          {/each}
        </select>
      {/if}
    </label>

    {#if !recording}
      <button
        onclick={start}
        disabled={busy || devices.length === 0 || noModelInstalled}
        aria-label={busy
          ? "Working"
          : noModelInstalled
            ? "Choose a model first"
            : "Start recording"}
        title={noModelInstalled ? "Choose a model first" : undefined}
      >
        {#if transcribing}
          <span class="spinner" aria-hidden="true"></span> Transcribing…
        {:else}
          ● Start recording
        {/if}
      </button>
    {:else}
      <button class="stop" onclick={stop} disabled={busy} aria-label="Stop recording and transcribe">
        ■ Stop and transcribe
      </button>
    {/if}

    <!--
      aria-live so screen readers announce the recording state change
      when the hotkey toggles it from elsewhere on the desktop. Visually
      this is the same `🔴 Recording…` cue that gives sighted users
      feedback that the mic is hot when the window is in the background.
    -->
    <p class="status" aria-live="polite">
      {#if recording}
        <span class="recording-dot" aria-hidden="true"></span> Recording…
        release the hotkey or press Stop to transcribe.
      {:else if transcribing}
        Transcribing — this can take a few seconds for short clips,
        longer for big models.
      {/if}
    </p>
  </section>

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}

  {#if result}
    <section class="result" aria-live="polite">
      <h2>Transcription</h2>
      <p class="text">{result.text || "(empty)"}</p>
      {#if result.foreground}
        <p class="meta">
          Captured while focused on <em>{result.foreground.appName}</em>
          {#if result.foreground.windowTitle}— {result.foreground.windowTitle}{/if}
        </p>
      {/if}
      <p class="meta">Already on your clipboard. Paste with ⌘V / Ctrl+V.</p>
    </section>
  {/if}

  <section class="history panel-history" aria-labelledby="history-heading">
    <header class="history-header">
      <h2 id="history-heading">
        <span class="panel-tag" aria-hidden="true">H</span>
        History
      </h2>
      <div class="search-wrap">
        <input
          type="search"
          placeholder="Search transcriptions…"
          value={historyQuery}
          oninput={onSearchInput}
          aria-label="Search history"
        />
        {#if historySearching}
          <span class="search-spinner" aria-label="Searching" role="status"></span>
        {/if}
      </div>
    </header>

    {#if historyError}
      <p class="error scoped-error" role="alert">
        <strong>History:</strong>
        {historyError}
      </p>
    {/if}

    {#if !historyLoaded}
      <p class="loading-skeleton">Loading history…</p>
    {:else if historyEntries.length === 0}
      <p class="empty-history">
        {#if historyQuery.trim().length > 0}
          No matches for "<em>{historyQuery}</em>". Try a shorter query.
        {:else}
          No transcriptions yet — press the hotkey or click Start above.
        {/if}
      </p>
    {:else}
      <ul class="history-list" data-version={historyVersion}>
        {#each historyEntries as entry (entry.id)}
          <li class="history-row">
            <p class="history-text">{entry.transcript}</p>
            <p class="history-meta">
              {formatTimestamp(entry.createdAt)}
              {#if entry.appName}· {entry.appName}{/if}
              {#if entry.model}· {entry.model}{/if}
            </p>
            <div class="history-actions">
              <button class="ghost" onclick={() => copyHistoryEntry(entry)}>
                Copy
              </button>
              <button class="ghost danger" onclick={() => deleteHistoryEntry(entry)}>
                Delete
              </button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="replacements panel-replacements" aria-labelledby="replacements-heading">
    <header class="history-header">
      <h2 id="replacements-heading">
        <span class="panel-tag panel-tag-replacements" aria-hidden="true">R</span>
        Replacements
        <span class="panel-subtitle">rewrites the output</span>
      </h2>
    </header>
    <p class="hint-prose">
      Find/replace pairs applied to every transcription before it's
      copied to the clipboard. Useful for stripping fillers
      (<code>um </code> → <code>(empty)</code>) or fixing names the
      model misrecognises. Literal substrings, case-sensitive.
    </p>

    {#if replacementsError}
      <p class="error scoped-error" role="alert">
        <strong>Replacements:</strong>
        {replacementsError}
      </p>
    {/if}

    <form class="replacement-form" onsubmit={addReplacement}>
      <input
        type="text"
        bind:this={findInputEl}
        bind:value={newFind}
        placeholder="Find…"
        aria-label="Find text"
      />
      <span class="arrow" aria-hidden="true">→</span>
      <input
        type="text"
        bind:value={newReplace}
        placeholder="Replace with… (blank deletes)"
        aria-label="Replace with"
      />
      <button type="submit" disabled={newFind.trim().length === 0}>Add</button>
    </form>

    {#if !replacementsLoaded}
      <p class="loading-skeleton">Loading replacements…</p>
    {:else if replacements.length === 0}
      <p class="empty-history">
        No replacement rules yet — add one above to clean up
        future transcriptions automatically.
      </p>
    {:else}
      <ul class="replacement-list">
        {#each replacements as rule (rule.id)}
          <li class="replacement-row">
            <code class="replacement-find">{rule.findText}</code>
            <span class="arrow" aria-hidden="true">→</span>
            <code class="replacement-replace">
              {rule.replaceText.length === 0 ? "(empty)" : rule.replaceText}
            </code>
            <button
              class="ghost danger"
              onclick={() => deleteReplacement(rule)}
              aria-label="Delete replacement {rule.findText} to {rule.replaceText}"
            >
              Delete
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="models panel-models" aria-labelledby="models-heading">
    <header class="history-header">
      <h2 id="models-heading">
        <span class="panel-tag panel-tag-models" aria-hidden="true">M</span>
        Model
      </h2>
    </header>
    <p class="hint-prose">
      Pick a Whisper variant. Bigger models are slower but more
      accurate. Hush expects model files in
      <code class="path-hint" title={models[0]?.expectedPath ?? ""}
        >&lt;app-data&gt;/models/</code
      >; download them from
      <a
        href="https://huggingface.co/ggerganov/whisper.cpp/tree/main"
        target="_blank"
        rel="noopener noreferrer">whisper.cpp on Hugging Face</a
      > and place them in that folder.
    </p>

    {#if modelsError}
      <p class="error scoped-error" role="alert">
        <strong>Model:</strong>
        {modelsError}
      </p>
    {/if}

    {#if modelsRestartNotice === "loaded"}
      <p class="restart-notice notice-loaded" role="status">
        ✓ Loaded. Ready to record.
      </p>
    {:else if modelsRestartNotice === "needs-download"}
      <p class="restart-notice notice-warn" role="status">
        Saved as default — but this model isn't downloaded yet. Click
        <strong>Download</strong> on the card below to fetch it.
      </p>
    {:else if modelsRestartNotice === "needs-restart"}
      <p class="restart-notice" role="status">
        Saved. Restart Hush to use the new model.
      </p>
    {/if}

    {#if !modelsLoaded}
      <p class="loading-skeleton">Loading models…</p>
    {/if}

    <ul class="model-grid">
      {#each models as card (card.id)}
        {@const inFlight = downloading.get(card.id) ?? null}
        {@const failure = downloadFailed.get(card.id) ?? null}
        <li
          class="model-card"
          class:selected={card.isSelected}
          class:unavailable={!card.isDownloaded && !inFlight}
        >
          <!--
            The card body is a `<button>` so the user can click any
            card to set it as default — including ones that aren't
            downloaded yet (the `selectModel` handler persists the
            selection and the notice pill above tells the user they
            need to Download next). Action buttons (Download, Cancel,
            Try again, Remove) live in a sibling `<footer>` below;
            keeping them out of the card-body button avoids invalid
            nested-button HTML.
          -->
          <button
            type="button"
            class="model-card-button"
            onclick={() => selectModel(card)}
            aria-label={card.isDownloaded
              ? `Select ${card.displayName}`
              : `Select ${card.displayName} (will need Download to use)`}
            aria-pressed={card.isSelected}
          >
            <header class="model-card-head">
              <h3 class="model-name">
                {card.displayName}
                {#if card.isSelected}
                  <span class="badge default-badge">Default</span>
                {/if}
              </h3>
              {#if card.isSelected}
                <span class="model-card-current" aria-hidden="true">●</span>
              {/if}
            </header>
            <p class="model-stats">
              <span>{card.sizeMb} MB</span>
              <span class="stat">
                Speed
                <span class="bars" aria-label="{card.speedRating} of 10">
                  {#each Array(10) as _, i}
                    <span class:on={i < card.speedRating}></span>
                  {/each}
                </span>
                {card.speedRating.toFixed(1)}
              </span>
              <span class="stat">
                Accuracy
                <span class="bars" aria-label="{card.accuracyRating} of 10">
                  {#each Array(10) as _, i}
                    <span class:on={i < card.accuracyRating}></span>
                  {/each}
                </span>
                {card.accuracyRating.toFixed(1)}
              </span>
            </p>
            <p class="model-desc">{card.description}</p>
          </button>

          <!-- Per-card action footer: Download / Cancel / Try again / Remove. -->
          <footer class="model-card-actions">
            {#if inFlight}
              <!--
                Active download: progress bar + Cancel.

                When `total` is null the download size is unknown, so
                the bar enters indeterminate state — `aria-valuenow`
                / `aria-valuemax` are omitted (per WAI-ARIA, a
                progressbar without a numeric `valuenow` is treated
                as indeterminate). The `aria-valuetext` provides the
                screen-reader-friendly version of what's drawn, so
                the announcement matches the visible state instead
                of stating a fake "0 of 100" reading. Closes the
                progress-bar a11y half of #48.
              -->
              <div class="download-progress" role="progressbar"
                aria-valuemin="0"
                aria-valuemax={inFlight.total ?? undefined}
                aria-valuenow={inFlight.total ? inFlight.received : undefined}
                aria-valuetext={inFlight.total
                  ? `${Math.round((inFlight.received / inFlight.total) * 100)}% — ${formatMb(inFlight.received)} of ${formatMb(inFlight.total)}`
                  : `Downloading ${formatMb(inFlight.received)} (size unknown)`}
                aria-label="Downloading {card.displayName}"
              >
                <div
                  class="download-progress-bar"
                  style:width={inFlight.total
                    ? `${Math.min(100, (inFlight.received / inFlight.total) * 100)}%`
                    : "100%"}
                ></div>
              </div>
              <span class="download-progress-text">
                {formatMb(inFlight.received)}{#if inFlight.total} / {formatMb(inFlight.total)}{/if}
              </span>
              <button class="ghost danger" onclick={() => cancelDownload(card)}>
                Cancel
              </button>
            {:else if failure}
              <!-- Failure: error chip + Try again. -->
              <p class="model-failure" role="alert">{failure}</p>
              <button class="ghost" onclick={() => downloadModel(card)}>
                Try again
              </button>
            {:else if card.isDownloaded}
              <!-- Downloaded: a small Remove button so the user can
                   reclaim disk if they change their mind. -->
              <button class="ghost danger" onclick={() => removeModel(card)}>
                Remove
              </button>
            {:else}
              <!-- Not downloaded, no in-flight or failure. -->
              <button class="ghost primary" onclick={() => downloadModel(card)}>
                Download
              </button>
            {/if}
          </footer>
        </li>
      {/each}
    </ul>
  </section>

  <section class="vocabulary panel-vocabulary" aria-labelledby="vocabulary-heading">
    <header class="history-header">
      <h2 id="vocabulary-heading">
        <span class="panel-tag panel-tag-vocabulary" aria-hidden="true">V</span>
        Vocabulary
        <span class="panel-subtitle">biases the recognition</span>
      </h2>
    </header>
    <p class="hint-prose">
      Words Whisper should be primed to recognise — proper nouns,
      jargon, names it otherwise mishears. Joined into the model's
      initial prompt on every transcription. Different from
      Replacements above: vocabulary biases the <em>recognition</em>;
      replacements rewrite the <em>output</em>.
    </p>

    {#if vocabularyError}
      <p class="error scoped-error" role="alert">
        <strong>Vocabulary:</strong>
        {vocabularyError}
      </p>
    {/if}

    <form class="replacement-form" onsubmit={addVocabulary}>
      <input
        type="text"
        bind:this={vocabInputEl}
        bind:value={newVocab}
        placeholder="Term (e.g. Tauri, ggml, Beingpax)…"
        aria-label="Vocabulary term"
      />
      <button type="submit" disabled={newVocab.trim().length === 0}>Add</button>
    </form>

    {#if !vocabularyLoaded}
      <p class="loading-skeleton">Loading vocabulary…</p>
    {:else if vocabulary.length === 0}
      <p class="empty-history">
        No vocabulary terms yet — add a word above and Whisper
        will be more likely to spell it correctly next time.
      </p>
    {:else}
      <ul class="replacement-list">
        {#each vocabulary as term (term.id)}
          <li class="replacement-row">
            <code class="replacement-find">{term.term}</code>
            <button
              class="ghost danger"
              onclick={() => deleteVocabulary(term)}
              aria-label="Delete vocabulary term {term.term}"
            >
              Delete
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  {#if macosDiagnostic?.canReset}
    <!--
      macOS permission diagnostic — only rendered when the backend
      reports `canReset: true` (effectively `cfg!(target_os = "macos")`
      on the Rust side). Linux/Windows users see this section hidden
      entirely; there's no permission story to diagnose for them.
      The disclosure starts collapsed because most users don't need
      it; it's the recovery path for the stuck-permission state.
    -->
    <section class="macos-diagnostic" aria-labelledby="macos-diag-heading">
      <details bind:open={macosDiagnosticOpen}>
        <summary id="macos-diag-heading">
          macOS permissions — diagnostic and reset
        </summary>
        <div class="macos-diagnostic-body">
          <p class="macos-diag-bundle">
            <strong>Bundle id:</strong>
            <code>{macosDiagnostic.bundleId}</code>
            — this is what System Settings → Privacy &amp; Security keys
            against. If you don't see Hush listed under Microphone or
            Input Monitoring, the binary may not be registering under
            this bundle id (common on unsigned dev builds).
          </p>
          <p>
            <strong>Microphone:</strong> {macosDiagnostic.microphoneHint}
          </p>
          <p>
            <strong>Input Monitoring:</strong> {macosDiagnostic.inputMonitoringHint}
          </p>
          <div class="macos-diag-actions">
            <button
              type="button"
              class="ghost"
              onclick={() => openPrivacyPane("microphone")}
            >
              Open Microphone settings
            </button>
            <button
              type="button"
              class="ghost"
              onclick={() => openPrivacyPane("input-monitoring")}
            >
              Open Input Monitoring settings
            </button>
            <button
              type="button"
              class="primary"
              onclick={runMacosReset}
              disabled={macosResetting}
            >
              {macosResetting ? "Resetting…" : "Reset permissions and re-prompt"}
            </button>
          </div>
          {#if macosResetMessage}
            <p class="macos-diag-reset-result" role="status">
              {macosResetMessage}
            </p>
          {/if}
          <p class="macos-diag-doc-pointer">
            Full troubleshooting recipe (including the
            <code>tccutil</code> commands this button wraps) is in
            <a
              href="https://github.com/khawkins98/Hush/blob/main/docs/macos-permissions.md"
              target="_blank"
              rel="noopener noreferrer"
            >docs/macos-permissions.md</a>.
          </p>
        </div>
      </details>
    </section>
  {/if}
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

/*
  First-time-setup banner. Renders only when the catalog has loaded
  and no model is on disk. Sits above the controls row so it's the
  first action-shaped surface a fresh-install user reads — replaces
  the previous "click Start, get a confusing error" flow.
*/
.setup-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  padding: 0.85rem 1rem;
  margin: 0 0 1rem;
  background-color: #eef2ff;
  border: 1px solid #c7d2fe;
  border-radius: 8px;
}

.setup-banner-text {
  display: flex;
  flex-direction: column;
  gap: 0.15rem;
  flex: 1;
  min-width: 0;
}

.setup-banner-text strong {
  font-size: 0.95rem;
  color: #1e1b4b;
}

.setup-banner-text span {
  font-size: 0.85rem;
  color: #3730a3;
}

.setup-banner button {
  flex-shrink: 0;
  white-space: nowrap;
}

@media (prefers-color-scheme: dark) {
  .setup-banner {
    background-color: #1e1b4b;
    border-color: #4338ca;
  }
  .setup-banner-text strong {
    color: #e0e7ff;
  }
  .setup-banner-text span {
    color: #c7d2fe;
  }
}

.controls {
  display: flex;
  flex-direction: column;
  gap: 1rem;
  align-items: stretch;
}

label {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  text-align: left;
  font-size: 0.85rem;
  color: #555;
}

.empty-devices {
  margin: 0;
  padding: 0.65rem 0.85rem;
  background-color: #fff7e6;
  border: 1px solid #f0c87b;
  border-radius: 6px;
  color: #6a4a00;
  font-size: 0.9rem;
  line-height: 1.4;
}

select,
button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  transition: border-color 0.15s, background-color 0.15s;
}

button {
  cursor: pointer;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
}

button:hover:not(:disabled) {
  border-color: #396cd8;
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

button.stop {
  background-color: #d83a3a;
  color: white;
  border-color: #d83a3a;
}

.status {
  margin: 0;
  min-height: 1.4em;
  font-size: 0.95rem;
  color: #555;
  text-align: center;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.45rem;
}

.recording-dot {
  width: 0.7rem;
  height: 0.7rem;
  border-radius: 50%;
  background-color: #d83a3a;
  display: inline-block;
  animation: pulse 1.2s ease-in-out infinite;
}

@keyframes pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.55; transform: scale(0.85); }
}

@media (prefers-reduced-motion: reduce) {
  .recording-dot,
  .spinner {
    animation: none;
  }
}

.spinner {
  width: 0.85rem;
  height: 0.85rem;
  border: 2px solid currentColor;
  border-right-color: transparent;
  border-radius: 50%;
  display: inline-block;
  animation: spin 0.8s linear infinite;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

.error {
  margin-top: 1.5rem;
  padding: 0.75rem 1rem;
  background-color: #fee;
  border: 1px solid #d83a3a;
  border-radius: 8px;
  color: #8a0000;
  text-align: left;
  line-height: 1.5;
}

.result {
  margin-top: 2rem;
  padding: 1rem 1.25rem;
  background-color: white;
  border: 1px solid #d1d1d1;
  border-radius: 12px;
  text-align: left;
}

.result h2 {
  margin: 0 0 0.5rem;
  font-size: 1rem;
  color: #555;
  font-weight: 600;
}

.result .text {
  margin: 0 0 0.75rem;
  font-size: 1.1rem;
  line-height: 1.5;
  white-space: pre-wrap;
  word-break: break-word;
}

.result .meta {
  margin: 0.25rem 0;
  font-size: 0.85rem;
  color: #666;
}

.history {
  margin-top: 2.5rem;
  text-align: left;
}

.history-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: 1rem;
}

.history-header h2 {
  margin: 0;
  font-size: 1.1rem;
  font-weight: 600;
  color: #333;
}

.history-header input[type="search"] {
  flex: 1;
  max-width: 18rem;
  padding: 0.5em 0.85em;
  font-size: 0.9rem;
}

.history-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.history-row {
  padding: 0.75rem 1rem;
  background-color: white;
  border: 1px solid #e1e1e1;
  border-radius: 8px;
}

.history-text {
  margin: 0 0 0.35rem;
  font-size: 0.95rem;
  line-height: 1.45;
  white-space: pre-wrap;
  word-break: break-word;
}

.history-meta {
  margin: 0 0 0.5rem;
  font-size: 0.8rem;
  color: #6b6b6b;
}

.history-actions {
  display: flex;
  gap: 0.4rem;
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

button.ghost.danger {
  color: #b03030;
  border-color: #e1b8b8;
}

button.ghost.danger:hover:not(:disabled) {
  background-color: #fbeaea;
  border-color: #d83a3a;
}

.empty-history {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: #fafafa;
  border: 1px dashed #d1d1d1;
  border-radius: 8px;
  color: #666;
  font-size: 0.9rem;
  text-align: center;
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

.replacements,
.vocabulary,
.models {
  margin-top: 2.5rem;
  text-align: left;
  /* Per-panel accent stripe + slightly inset padding so each section
     reads visually distinct as the page grows. The vocabulary review
     flagged that Replacements / Vocabulary look near-identical and
     are easy to mis-target. Accent + section-tag pill differentiate
     them without resorting to icons. */
  border-left: 3px solid #e1e1e1;
  padding-left: 1rem;
  padding-bottom: 0.25rem;
}

.panel-replacements {
  border-left-color: #6a8cf0;
}
.panel-vocabulary {
  border-left-color: #d8a64a;
}
.panel-models {
  border-left-color: #4a8a4a;
}
.panel-history {
  margin-top: 2.5rem;
  text-align: left;
  border-left: 3px solid #c0c0c0;
  padding-left: 1rem;
  padding-bottom: 0.25rem;
}

.panel-tag {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.4em;
  height: 1.4em;
  border-radius: 5px;
  font-size: 0.75em;
  font-weight: 700;
  background-color: #e8e8e8;
  color: #444;
  margin-right: 0.5rem;
}
.panel-tag-replacements {
  background-color: #dde5ff;
  color: #2c3e8f;
}
.panel-tag-vocabulary {
  background-color: #fff0d4;
  color: #6a4500;
}
.panel-tag-models {
  background-color: #d6ecd6;
  color: #1f5a1f;
}

.panel-subtitle {
  margin-left: 0.6rem;
  font-size: 0.7em;
  font-weight: 400;
  color: #888;
  font-style: italic;
}

.scoped-error {
  /* `.error` already provides the red box; `strong` inside scopes
     the message to a section. The two together give the user a
     visual cue (red) and a textual cue (section name). */
  padding-left: 1rem;
}
.scoped-error strong {
  margin-right: 0.4rem;
}

.loading-skeleton {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: #fafafa;
  border-radius: 6px;
  color: #999;
  font-size: 0.9rem;
  text-align: center;
  font-style: italic;
}

.search-wrap {
  position: relative;
  display: flex;
  align-items: center;
  gap: 0.4rem;
}

.search-spinner {
  width: 0.7rem;
  height: 0.7rem;
  border: 2px solid #b0b0b0;
  border-right-color: transparent;
  border-radius: 50%;
  display: inline-block;
  animation: spin 0.8s linear infinite;
}

.path-hint {
  background-color: #eef2ff;
  padding: 0.05em 0.4em;
  border-radius: 4px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.restart-notice {
  margin: 0.5rem 0 1rem;
  padding: 0.6rem 0.85rem;
  background-color: #e8f5e8;
  border: 1px solid #b8d8b8;
  border-radius: 6px;
  color: #1f5a1f;
  font-size: 0.9rem;
}

/* Three flavours of post-select notice. The default green (above)
   covers the "needs-restart" edge case. `notice-loaded` is the happy
   path — saturated green to read as success. `notice-warn` is amber
   — selection persisted but user has work left (Download). */
.notice-loaded {
  background-color: #d1f0d1;
  border-color: #8fc88f;
  color: #1a4a1a;
}

.notice-warn {
  background-color: #fef3c7;
  border-color: #fcd34d;
  color: #92400e;
}

@media (prefers-color-scheme: dark) {
  .notice-loaded {
    background-color: #14532d;
    border-color: #166534;
    color: #bbf7d0;
  }
  .notice-warn {
    background-color: #422006;
    border-color: #92400e;
    color: #fde68a;
  }
}

.model-grid {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}

.model-card {
  border-radius: 12px;
  background-color: white;
  border: 1px solid #e1e1e1;
  transition: border-color 0.15s, background-color 0.15s;
}

.model-card.selected {
  border-color: #6a8cf0;
  background-color: #f5f8ff;
  box-shadow: 0 0 0 1px #6a8cf0;
}

.model-card.unavailable {
  opacity: 0.55;
}

.model-card-button {
  width: 100%;
  display: block;
  background: transparent;
  border: none;
  padding: 0.85rem 1.1rem;
  text-align: left;
  border-radius: 12px;
  cursor: pointer;
  font: inherit;
  color: inherit;
}

.model-card-button:disabled {
  cursor: default;
}

.model-card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 0.5rem;
}

.model-name {
  margin: 0;
  font-size: 1rem;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 0.6rem;
}

.badge {
  font-size: 0.7rem;
  font-weight: 500;
  padding: 0.05rem 0.45rem;
  border-radius: 999px;
  background-color: #c7d2fe;
  color: #2c3e8f;
}

.model-card-current {
  color: #6a8cf0;
  font-size: 0.85rem;
}

.model-stats {
  display: flex;
  flex-wrap: wrap;
  gap: 1rem;
  margin: 0.5rem 0 0.4rem;
  font-size: 0.8rem;
  color: #555;
  align-items: center;
}

.model-stats .stat {
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
}

.bars {
  display: inline-flex;
  gap: 2px;
}

.bars span {
  width: 5px;
  height: 9px;
  border-radius: 1px;
  background-color: #d8d8d8;
  display: inline-block;
}

.bars span.on {
  background-color: #6a8cf0;
}

.model-desc {
  margin: 0;
  font-size: 0.85rem;
  color: #444;
  line-height: 1.45;
}

.model-card-actions {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0 1.1rem 0.85rem;
  flex-wrap: wrap;
}

button.ghost.primary {
  border-color: #6a8cf0;
  color: #2c3e8f;
}

button.ghost.primary:hover:not(:disabled) {
  background-color: #eef2ff;
  border-color: #4a6cd0;
}

.download-progress {
  flex: 1;
  min-width: 6rem;
  height: 6px;
  background-color: #e8e8e8;
  border-radius: 3px;
  overflow: hidden;
}

.download-progress-bar {
  height: 100%;
  background-color: #6a8cf0;
  transition: width 0.15s ease-out;
}

.download-progress-text {
  font-size: 0.8rem;
  color: #555;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.model-failure {
  flex: 1;
  margin: 0;
  padding: 0.4rem 0.6rem;
  background-color: #fee;
  border: 1px solid #d83a3a;
  border-radius: 4px;
  color: #8a0000;
  font-size: 0.85rem;
}

.hint-prose {
  margin: 0 0 1rem;
  font-size: 0.85rem;
  color: #555;
  line-height: 1.5;
}

.hint-prose code {
  background-color: #eef2ff;
  padding: 0.05em 0.4em;
  border-radius: 4px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.9em;
}

.replacement-form {
  display: flex;
  gap: 0.5rem;
  align-items: center;
  margin-bottom: 1rem;
  flex-wrap: wrap;
}

.replacement-form input[type="text"] {
  flex: 1;
  min-width: 8rem;
  padding: 0.5em 0.85em;
  font-size: 0.9rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.replacement-form button {
  padding: 0.5em 1.2em;
  font-size: 0.9rem;
}

.arrow {
  color: #888;
  font-weight: 600;
  flex-shrink: 0;
}

.replacement-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}

.replacement-row {
  display: flex;
  gap: 0.6rem;
  align-items: center;
  padding: 0.55rem 0.8rem;
  background-color: white;
  border: 1px solid #e1e1e1;
  border-radius: 6px;
  font-size: 0.85rem;
}

.replacement-find,
.replacement-replace {
  background-color: #f4f4f4;
  padding: 0.1em 0.5em;
  border-radius: 4px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  white-space: pre;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 12rem;
  flex-shrink: 1;
  min-width: 0;
}

.replacement-row .ghost {
  margin-left: auto;
}

@media (prefers-color-scheme: dark) {
  :root {
    color: #f0f0f0;
    background-color: #1a1a1a;
  }
  .tagline,
  label,
  .status,
  .result h2,
  .result .meta {
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
  .empty-devices {
    background-color: #3a2e10;
    border-color: #7a5a20;
    color: #f0d090;
  }
  select,
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button:hover:not(:disabled) {
    border-color: #6a8cf0;
  }
  .result {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .error {
    /* Increased contrast over the previous #ffa0a0 — flagged in the
       UX review as likely below WCAG AA on dark mode. */
    background-color: #4a1a1a;
    border-color: #d83a3a;
    color: #ffd0d0;
  }
  .history-header h2 {
    color: #d8d8d8;
  }
  .history-row {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .history-meta {
    color: #9a9a9a;
  }
  button.ghost {
    border-color: #3a3a3a;
    color: #f0f0f0;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
  button.ghost.danger {
    color: #ff9090;
    border-color: #5a2020;
  }
  button.ghost.danger:hover:not(:disabled) {
    background-color: #3a1818;
    border-color: #d83a3a;
  }
  .empty-history {
    background-color: #1f1f1f;
    border-color: #3a3a3a;
    color: #999;
  }
  .restart-notice {
    background-color: #1a3a1a;
    border-color: #2a5a2a;
    color: #c8e8c8;
  }
  .model-card {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .model-card.selected {
    background-color: #2a3050;
    border-color: #6a8cf0;
  }
  .model-stats {
    color: #aaa;
  }
  .model-desc {
    color: #d0d0d0;
  }
  .bars span {
    background-color: #3a3a3a;
  }
  .bars span.on {
    background-color: #8aa0ff;
  }
  .badge {
    background-color: #3a4a7a;
    color: #d0d8ff;
  }
  .path-hint {
    background-color: #1e2a4a;
    color: #c0d0ff;
  }
  .download-progress {
    background-color: #3a3a3a;
  }
  .download-progress-bar {
    background-color: #8aa0ff;
  }
  .download-progress-text {
    color: #aaa;
  }
  .model-failure {
    background-color: #4a1a1a;
    border-color: #d83a3a;
    color: #ffd0d0;
  }
  button.ghost.primary {
    border-color: #6a8cf0;
    color: #c0d0ff;
  }
  button.ghost.primary:hover:not(:disabled) {
    background-color: #1e2a4a;
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
  .hint-prose {
    color: #aaa;
  }
  .hint-prose code {
    background-color: #1e2a4a;
    color: #c0d0ff;
  }
  .replacement-row {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .replacement-find,
  .replacement-replace {
    background-color: #1f1f1f;
    color: #f0f0f0;
  }
  .arrow {
    color: #888;
  }
}
</style>
