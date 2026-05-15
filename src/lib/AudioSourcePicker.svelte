<!--
  Audio source dropdown — the left-flank adjunct beside the
  Record button. Pre-r3 this also rendered the active-model chip;
  the chip moved to its own `ModelChip.svelte` when the layout
  dropped the sidebar in favour of a single row with source + model
  flanking the centerpiece button.
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
  };

  let {
    sources,
    sourcesLoaded,
    selected = $bindable(),
    recording,
    busy,
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

<div class="source-field">
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

<style>
  .source-field {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    min-width: 0;
  }
  .source-field :global(.select-trigger) {
    width: 100%;
  }

  .field-label {
    font-size: 0.68rem;
    font-weight: 600;
    color: var(--text-label);
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
</style>
