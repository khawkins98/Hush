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
  let historyQuery = $state("");
  let historyError = $state<string | null>(null);
  // Sentinel that any history-touching command bumps so we can react
  // to an external invalidation (e.g. a successful stop_dictation
  // inserted a new row).
  let historyVersion = $state(0);

  // `recording` is "audio is being captured", `busy` covers both the
  // start handshake AND the post-stop transcription window. Splitting
  // out `transcribing` lets the UI distinguish "starting up" (~ms) from
  // "Whisper is working" (seconds), which deserves a visible spinner.
  let transcribing = $derived(busy && !recording && !!result === false);

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
    try {
      devices = await invoke<AudioDevice[]>("list_input_devices");
      const def = devices.find((d) => d.isDefault) ?? devices[0];
      if (def) selected = def.id;
    } catch (e) {
      error = formatError(e);
    } finally {
      devicesLoaded = true;
    }

    await refreshHistory();

    // Hotkey lives in the backend (`hotkey::register_default`); on every
    // press the backend emits `hotkey:toggle`. We dispatch start vs stop
    // here against the frontend's own recording flag so the toggle
    // semantics live next to the UI state they affect.
    unlistenToggle = await listen("hotkey:toggle", () => {
      if (busy) return; // ignore presses while a transcription is in flight
      if (recording) void stop();
      else void start();
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

  async function refreshHistory() {
    historyError = null;
    try {
      historyEntries = await invoke<HistoryEntry[]>("history_search", {
        query: historyQuery,
        limit: HISTORY_PAGE_SIZE,
        offset: 0,
      });
      historyVersion += 1;
    } catch (e) {
      historyError = formatError(e);
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
            "Transcription isn't set up yet. The model picker is coming in " +
            "the next milestone — for now, set HUSH_MODEL_PATH to a Whisper " +
            "GGUF file and run with `cargo tauri dev --features whisper`. " +
            "(See README for setup help.)"
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

<main class="container">
  <h1>Hush</h1>
  <p class="tagline">Press, talk, paste. Local Whisper transcription.</p>

  <!--
    Hotkey hint card. Defaults are baked here for M2; once the settings
    panel lands (M3) this becomes a fetched value and the env-var
    override notes go away.
  -->
  <aside class="hint" aria-label="Keyboard shortcuts">
    <strong>Shortcuts:</strong>
    <kbd>⌘/Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Space</kbd> to toggle,
    or hold <kbd>Right Ctrl</kbd> to push-to-talk.
  </aside>

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
        disabled={busy || devices.length === 0}
        aria-label={busy ? "Working" : "Start recording"}
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
    <section class="result">
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

  <section class="history" aria-labelledby="history-heading">
    <header class="history-header">
      <h2 id="history-heading">History</h2>
      <input
        type="search"
        placeholder="Search transcriptions…"
        value={historyQuery}
        oninput={onSearchInput}
        aria-label="Search history"
      />
    </header>

    {#if historyError}
      <p class="error" role="alert">{historyError}</p>
    {/if}

    {#if historyEntries.length === 0}
      <p class="empty-history">
        {#if historyQuery.trim().length > 0}
          No matches for "<em>{historyQuery}</em>".
        {:else}
          No transcriptions yet. Press the hotkey or click Start to record one.
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
}
</style>
