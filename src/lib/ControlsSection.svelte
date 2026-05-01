<script lang="ts">
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import Select from "./Select.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
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
    error: ErrorDisplayShape | null;
    onStart: () => void | Promise<void>;
    onStop: () => void | Promise<void>;
    // Shared callback used by both the setup-banner "Open Settings →
    // Model" button and the inline model chip.
    onScrollToModelPicker: () => void;
    // Active model display name; null when no model is loaded.
    // Renders an inline model chip above the audio picker so the two
    // session-config controls are visually co-located.
    activeModelName: string | null;
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
    activeModelName,
  }: Props = $props();

  // Derived: separate the mic devices from the system-audio entry so
  // the picker can group them (mics first, then system audio with a
  // visual divider). Disabled mic-less platforms still get the
  // system-audio entry — when it's not yet supported, the option is
  // rendered disabled with a "coming soon" suffix.
  let mics = $derived(sources.filter((s) => s.kind === "microphone"));
  let systemAudio = $derived(sources.find((s) => s.kind === "system-audio"));

  // Total picker option count, including the disabled system-audio
  // entry. Used to size the "no audio sources at all" empty state.
  let pickableCount = $derived(mics.length + (systemAudio ? 1 : 0));

  // Can the user actually start? At least one *supported* source must
  // exist. Mics are always supported when present; the system-audio
  // entry is supported only when the backend says so.
  let hasUsableSource = $derived(
    mics.length > 0 || (systemAudio?.isSupported ?? false),
  );

  // Build the groups array for the custom Select component, mirroring
  // the old <optgroup> structure. The system-audio entry renders as a
  // disabled option when the backend reports it unsupported.
  let sourceGroups = $derived([
    {
      label: "Microphone",
      options: mics.map((m) => ({
        value: m.id,
        label: m.name + (m.isDefault ? " (default)" : ""),
      })),
    },
    ...(systemAudio
      ? [
          {
            label: "System audio",
            options: [
              {
                value: systemAudio.id,
                label:
                  systemAudio.name +
                  (systemAudio.isSupported
                    ? ""
                    : " (coming soon on this platform)"),
                disabled: !systemAudio.isSupported,
              },
            ],
          },
        ]
      : []),
  ]);
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
  <!--
    Unified session-config row: model chip (left) + audio source
    picker (right). Co-locating these two "what are you recording
    with?" controls replaces the old pattern where the model chip
    floated in the page header and the source lived below. Now they
    read as a single setup strip above the action button.
  -->
  <div class="config-row">
    <div class="config-field">
      <label class="field-label" for="audio-source-select">Audio source</label>
      {#if !sourcesLoaded}
        <p class="empty-devices">Loading sources…</p>
      {:else if pickableCount === 0}
        <p class="empty-devices">
          No audio sources detected. On macOS, grant microphone access in
          System Settings → Privacy &amp; Security. On Linux, check that
          PulseAudio / PipeWire is running.
        </p>
      {:else}
        <Select
          id="audio-source-select"
          groups={sourceGroups}
          value={selected}
          onchange={(v) => (selected = v)}
          disabled={recording || busy}
        />
      {/if}
    </div>

    {#if activeModelName}
      <div class="config-field">
        <span class="field-label">Model</span>
        <button
          type="button"
          class="model-chip"
          onclick={onScrollToModelPicker}
          aria-label="Active model: {activeModelName}. Click to change."
          title="Change transcription model"
        >
          <span class="model-name">{activeModelName}</span>
          <svg
            class="model-chevron"
            width="10"
            height="10"
            viewBox="0 0 10 10"
            aria-hidden="true"
            fill="none"
          >
            <path
              d="M3 4l2 2 2-2"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      </div>
    {/if}
  </div>

  {#if !recording}
    <button
      class="start-btn"
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
        <span class="rec-dot idle" aria-hidden="true"></span> Start recording
      {/if}
    </button>
  {:else}
    <button
      class="start-btn stop"
      onclick={onStop}
      disabled={busy}
      aria-label="Stop recording and transcribe"
    >
      <span class="rec-dot stop" aria-hidden="true"></span> Stop and transcribe
    </button>
  {/if}

  <!--
    aria-live so screen readers announce the recording state change
    when the hotkey toggles it from elsewhere on the desktop.
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
  <ErrorDisplay {error} />
{/if}

<style>
/* ── First-time-setup banner ─────────────────────────────── */
.setup-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  padding: 0.85rem 1rem;
  margin: 0 0 1rem;
  background-color: var(--info-bg);
  border: 1px solid var(--info-border);
  border-radius: var(--radius-md);
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
  color: var(--info-text);
}

.setup-banner-text span {
  font-size: 0.85rem;
  color: var(--info-text);
  opacity: 0.85;
}

.setup-banner button {
  flex-shrink: 0;
  white-space: nowrap;
}

/* ── Controls container ──────────────────────────────────── */
.controls {
  display: flex;
  flex-direction: column;
  gap: 0.85rem;
  align-items: stretch;
}

/* ── Config row: audio source + model ───────────────────── */
.config-row {
  display: grid;
  /* Model chip is narrower; audio source fills remaining space. */
  grid-template-columns: 1fr auto;
  gap: 0.6rem;
  align-items: end;
}

.config-field {
  display: flex;
  flex-direction: column;
  gap: 0.3rem;
}

.field-label {
  font-size: 0.8rem;
  font-weight: 500;
  color: var(--text-muted);
  letter-spacing: 0.01em;
}

/* ── Model chip ──────────────────────────────────────────── */
.model-chip {
  height: var(--control-height);
  display: inline-flex;
  align-items: center;
  gap: 0.35rem;
  padding: 0 0.85rem;
  background: var(--bg-surface);
  border: 1px solid var(--border-input);
  border-radius: var(--radius-md);
  color: var(--text-secondary);
  font-family: inherit;
  font-size: 0.88rem;
  font-weight: 500;
  cursor: pointer;
  white-space: nowrap;
  transition: background-color 0.12s, border-color 0.12s, color 0.12s;
}

.model-chip:hover {
  background: var(--bg-elevated);
  border-color: var(--accent-hover);
  color: var(--text-primary);
}

.model-chip:focus-visible {
  outline: none;
  border-color: var(--border-focus);
  box-shadow: 0 0 0 3px var(--accent-subtle);
}

.model-name {
  max-width: 9rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.model-chevron {
  color: var(--text-muted);
  flex-shrink: 0;
}

/* ── Empty devices notice ────────────────────────────────── */
.empty-devices {
  margin: 0;
  padding: 0.65rem 0.85rem;
  background-color: var(--warning-bg);
  border: 1px solid var(--warning-border);
  border-radius: var(--radius-md);
  color: var(--warning-text);
  font-size: 0.9rem;
  line-height: 1.4;
}

/* ── Start / Stop button ─────────────────────────────────── */
.start-btn {
  border-radius: var(--radius-md);
  border: 1px solid var(--border-input);
  height: var(--control-height);
  padding: 0 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: var(--text-primary);
  background-color: var(--bg-surface);
  cursor: pointer;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  transition: border-color 0.15s, background-color 0.15s, box-shadow 0.15s;
  width: 100%;
}

.start-btn:hover:not(:disabled) {
  border-color: var(--accent-hover);
  box-shadow: 0 0 0 3px var(--accent-subtle);
}

.start-btn:focus-visible {
  outline: none;
  border-color: var(--border-focus);
  box-shadow: 0 0 0 3px var(--accent-subtle);
}

.start-btn:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}

.start-btn.stop {
  background-color: var(--danger);
  color: white;
  border-color: var(--danger);
}

.start-btn.stop:hover:not(:disabled) {
  background-color: #c02e2e;
  border-color: #c02e2e;
  box-shadow: 0 0 0 3px rgba(216, 58, 58, 0.18);
}

/* Separate primary button style used by the setup-banner. */
button.primary {
  background-color: var(--accent);
  color: var(--text-on-accent);
  border: 1px solid var(--accent);
  border-radius: var(--radius-md);
  padding: 0.45rem 1rem;
  font-size: 0.88rem;
  font-family: inherit;
  font-weight: 600;
  cursor: pointer;
  transition: background-color 0.12s;
}

button.primary:hover:not(:disabled) {
  background-color: var(--accent-hover);
  border-color: var(--accent-hover);
}

/* ── Recording dot / spinner ─────────────────────────────── */
.rec-dot {
  width: 0.55rem;
  height: 0.55rem;
  border-radius: 50%;
  display: inline-block;
  flex-shrink: 0;
}

.rec-dot.idle {
  background-color: var(--text-secondary);
  opacity: 0.6;
}

.rec-dot.stop {
  background-color: white;
}

/* ── Status line ─────────────────────────────────────────── */
.status {
  margin: 0;
  min-height: 1.4em;
  font-size: 0.88rem;
  color: var(--text-muted);
  text-align: center;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.45rem;
}

.recording-dot {
  width: 0.65rem;
  height: 0.65rem;
  border-radius: 50%;
  background-color: var(--danger);
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
</style>
