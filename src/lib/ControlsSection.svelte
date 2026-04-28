<script lang="ts">
  import type { AudioSourceListing } from "./types";

  type Props = {
    sources: AudioSourceListing[];
    sourcesLoaded: boolean;
    /// Selected source id. Mic devices use their device name; the
    /// system-audio entry uses the literal string `"system"`. The
    /// parent page maps this to an `AudioSource` argument when calling
    /// `start_dictation`.
    selected: string | null;
    recording: boolean;
    busy: boolean;
    transcribing: boolean;
    noModelInstalled: boolean;
    error: string | null;
    onStart: () => void | Promise<void>;
    onStop: () => void | Promise<void>;
    onScrollToModelPicker: () => void;
  };

  let {
    sources,
    sourcesLoaded,
    selected = $bindable(),
    recording,
    busy,
    transcribing,
    noModelInstalled,
    error,
    onStart,
    onStop,
    onScrollToModelPicker,
  }: Props = $props();

  // Derived: separate the mic devices from the system-audio entry so
  // the picker can group them (mics first, then system audio with a
  // visual divider). Disabled mic-less platforms still get the
  // system-audio entry — when it's not yet supported, the option is
  // rendered disabled with a "coming soon" suffix.
  let mics = $derived(sources.filter((s) => s.kind === "microphone"));
  let systemAudio = $derived(sources.find((s) => s.kind === "system-audio"));

  // Total picker option count, including the disabled system-audio
  // entry. Used to size the "no audio sources at all" empty state —
  // we should never get here in practice (the backend always pushes
  // a system-audio listing) but it's a safety net for the UI to not
  // render an empty `<select>`.
  let pickableCount = $derived(mics.length + (systemAudio ? 1 : 0));

  // Can the user actually start? At least one *supported* source must
  // exist. Mics are always supported when present; the system-audio
  // entry is supported only when the backend says so. A platform with
  // zero mics AND no system-audio support would have nothing usable.
  let hasUsableSource = $derived(
    mics.length > 0 || (systemAudio?.isSupported ?? false),
  );
</script>

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
        Hush needs a Whisper model to transcribe. Open Settings →
        Model to pick one — Whisper Base is a solid default.
      </span>
    </div>
    <button class="primary" onclick={onScrollToModelPicker}>
      Open Settings → Model
    </button>
  </aside>
{/if}

<section class="controls">
  <label>
    Audio source
    {#if !sourcesLoaded}
      <p class="empty-devices">Loading sources…</p>
    {:else if pickableCount === 0}
      <p class="empty-devices">
        No audio sources detected. On macOS, grant microphone access in
        System Settings → Privacy &amp; Security. On Linux, check that
        PulseAudio / PipeWire is running.
      </p>
    {:else}
      <!--
        Two groups: mic devices (always supported) and the system-audio
        entry (currently disabled on every platform — see #33 for the
        per-OS roadmap). Splitting via <optgroup> makes the structure
        clear to assistive tech; the disabled attribute on the
        not-yet-supported option means the user can't accidentally
        pick it and hit a runtime error.
      -->
      <select bind:value={selected} disabled={recording || busy}>
        <optgroup label="Microphone">
          {#each mics as mic (mic.id)}
            <option value={mic.id}>
              {mic.name}{mic.isDefault ? " (default)" : ""}
            </option>
          {/each}
        </optgroup>
        {#if systemAudio}
          <optgroup label="System audio">
            <option value={systemAudio.id} disabled={!systemAudio.isSupported}>
              {systemAudio.name}{systemAudio.isSupported
                ? ""
                : " (coming soon — #33)"}
            </option>
          </optgroup>
        {/if}
      </select>
    {/if}
  </label>

  {#if !recording}
    <button
      onclick={onStart}
      disabled={busy || !hasUsableSource || noModelInstalled}
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
    <button class="stop" onclick={onStop} disabled={busy} aria-label="Stop recording and transcribe">
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

<style>
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

@media (prefers-color-scheme: dark) {
  label,
  .status {
    color: #aaa;
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
  .error {
    /* Increased contrast over the previous #ffa0a0 — flagged in the
       UX review as likely below WCAG AA on dark mode. */
    background-color: #4a1a1a;
    border-color: #d83a3a;
    color: #ffd0d0;
  }
}
</style>
