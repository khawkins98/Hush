<!--
  Compatibility wrapper around `AudioSourcePicker` + `RecordPanel`
  introduced in #468 slice B. Keeps the existing prop interface
  for `DictationSection` so this slice is a pure refactor — slice
  C will replace this wrapper with direct imports of the two
  leaves laid out in a grid.

  Owns:
  - The first-time-setup banner (shown when no model installed)
  - The cross-leaf derived values (badgeVisible, willRecordMeeting,
    hasUsableSource, selectedSourceLabel) so each leaf receives
    only concrete props
  - The trailing `<ErrorDisplay>` block
-->
<script lang="ts">
  import AudioSourcePicker from "./AudioSourcePicker.svelte";
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import RecordPanel from "./RecordPanel.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import type { AudioSourceListing, PermissionHealth } from "./types";

  type Props = {
    sources: AudioSourceListing[];
    sourcesLoaded: boolean;
    selected: string | null;
    recording: boolean;
    busy: boolean;
    transcribing: boolean;
    noModelInstalled: boolean;
    error: ErrorDisplayShape | null;
    onStart: () => void | Promise<void>;
    onStop: () => void | Promise<void>;
    onScrollToModelPicker: () => void;
    activeModelName: string | null;
    screenRecordingHealth?: PermissionHealth | null;
    onOpenPermissions?: () => void;
    recordMode?: "dictation" | "meeting" | null;
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
    screenRecordingHealth = null,
    onOpenPermissions,
    recordMode = null,
  }: Props = $props();

  // Cross-leaf deriveds — computed once here so AudioSourcePicker
  // and RecordPanel stay decoupled from each other's source lists
  // and selection state.
  let mics = $derived(sources.filter((s) => s.kind === "microphone"));
  let systemAudio = $derived(sources.find((s) => s.kind === "system-audio"));

  let hasUsableSource = $derived(
    mics.length > 0 || (systemAudio?.isSupported ?? false),
  );

  let badgeVisible = $derived(
    !recording
      && selected !== null
      && selected !== "system"
      && (screenRecordingHealth === "stale"
        || screenRecordingHealth === "not-granted"),
  );
  let badgeIsStale = $derived(screenRecordingHealth === "stale");

  let willRecordMeeting = $derived(
    !recording
      && selected !== null
      && selected !== "system"
      && screenRecordingHealth === "confirmed",
  );

  // Resolve the picker's selected id to its display name for the
  // F5 status line.
  let selectedSourceLabel = $derived.by(() => {
    if (selected === null) return null;
    if (selected === "system") return systemAudio?.name ?? "System Audio";
    return sources.find((s) => s.id === selected)?.name ?? null;
  });
</script>

{#if noModelInstalled}
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
  <AudioSourcePicker
    {sources}
    {sourcesLoaded}
    bind:selected
    {recording}
    {busy}
    {activeModelName}
    {onScrollToModelPicker}
  />

  <RecordPanel
    {recording}
    {busy}
    {transcribing}
    {hasUsableSource}
    {noModelInstalled}
    {willRecordMeeting}
    {badgeVisible}
    {badgeIsStale}
    {recordMode}
    {selectedSourceLabel}
    {activeModelName}
    {error}
    {onStart}
    {onStop}
    {onOpenPermissions}
  />
</section>

{#if error}
  <ErrorDisplay {error} />
{/if}

<style>
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

  .controls {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
    align-items: stretch;
  }

  /* Primary button used by the setup-banner. */
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
</style>
