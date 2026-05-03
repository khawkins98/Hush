<!--
  Sidebar-shaped session config: audio source picker + active
  model chip. Pulled out of `ControlsSection` in #468 slice B so
  the two-column layout (slice C) can place this in the sidebar
  column while `RecordPanel` lives in the content column.
-->
<script lang="ts">
  import Select from "./Select.svelte";
  import type { AudioSourceListing } from "./types";

  type Props = {
    sources: AudioSourceListing[];
    sourcesLoaded: boolean;
    /// Selected source id. Mic devices use their device name; the
    /// system-audio entry uses the literal string `"system"`.
    selected: string | null;
    recording: boolean;
    busy: boolean;
    /// Active model display name; null when no model is loaded.
    /// The chip is hidden in that case — the no-model setup
    /// banner upstream takes over the affordance.
    activeModelName: string | null;
    onScrollToModelPicker: () => void;
  };

  let {
    sources,
    sourcesLoaded,
    selected = $bindable(),
    recording,
    busy,
    activeModelName,
    onScrollToModelPicker,
  }: Props = $props();

  let mics = $derived(sources.filter((s) => s.kind === "microphone"));
  let systemAudio = $derived(sources.find((s) => s.kind === "system-audio"));
  let pickableCount = $derived(mics.length + (systemAudio ? 1 : 0));

  // <Select>'s group/option shape, mirroring the old <optgroup>
  // structure. The system-audio entry renders disabled when the
  // backend reports it unsupported.
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

<style>
  /* CSS cloned verbatim out of the pre-#468 ControlsSection so
     slice B is visually byte-identical. Slice C's two-column
     layout will likely retune some of these (sidebar grid drops
     the `1fr auto` row layout). */
  .config-row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 0.6rem;
    align-items: end;
  }

  .config-field {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  /* Panic-flavoured all-caps section label — same idiom Nova
     and Transmit use for sidebar group headers. Tight letter-
     spacing reads as "section title" rather than "form label";
     the muted colour keeps it from competing with the picker. */
  .field-label {
    font-size: 0.68rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

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
</style>
