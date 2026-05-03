<!--
  Dictation page-section render. Now lays out the section as a
  two-column grid (#468 slice C / #411 Phase A.2): a sidebar
  column for `AudioSourcePicker` (source + model chip) and a
  content column for `RecordPanel` (record button, status,
  waveform, F5 status line) plus the result block and the macOS
  permissions banner.

  Render-only by design: the orchestrator owns dictation IPC and
  hotkey listeners. This section composes the leaves and computes
  the cross-leaf deriveds (`hasUsableSource`, `badgeVisible`,
  `willRecordMeeting`, `selectedSourceLabel`) that pre-#468 lived
  on `ControlsSection`.
-->
<script lang="ts">
  import { backOut, cubicIn } from "svelte/easing";
  import { fade, fly } from "svelte/transition";

  import AudioSourcePicker from "./AudioSourcePicker.svelte";
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import MacosPermsPill from "./MacosPermsPill.svelte";
  import RecordPanel from "./RecordPanel.svelte";
  import ResultBlock from "./ResultBlock.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import { motionDuration } from "./motion";
  import type {
    AudioSourceListing,
    DictationResult,
    PermissionsHealth,
  } from "./types";

  type Props = {
    isMacOS: boolean;
    sources: AudioSourceListing[];
    sourcesLoaded: boolean;
    selected: string | null;
    recording: boolean;
    busy: boolean;
    transcribing: boolean;
    noModelInstalled: boolean;
    error: ErrorDisplayShape | null;
    result: DictationResult | null;
    recordMode: "dictation" | "meeting" | null;
    activeModelName: string | null;
    permissionHealth: PermissionsHealth | null;
    macosCapable: boolean;
    allPermsGranted: boolean;
    anyPermsDenied: boolean;
    onStart: () => void | Promise<void>;
    onStop: () => void | Promise<void>;
    onScrollToModelPicker: () => void;
    onOpenPermissionsTab: () => void;
  };

  let {
    isMacOS,
    sources,
    sourcesLoaded,
    selected = $bindable(),
    recording,
    busy,
    transcribing,
    noModelInstalled,
    error,
    result,
    recordMode,
    activeModelName,
    permissionHealth,
    macosCapable,
    allPermsGranted,
    anyPermsDenied,
    onStart,
    onStop,
    onScrollToModelPicker,
    onOpenPermissionsTab,
  }: Props = $props();

  // Cross-leaf deriveds — computed once here so each leaf
  // receives concrete props rather than re-deriving from
  // `sources` + `selected` + the screen-recording health.
  let mics = $derived(sources.filter((s) => s.kind === "microphone"));
  let systemAudio = $derived(sources.find((s) => s.kind === "system-audio"));

  let hasUsableSource = $derived(
    mics.length > 0 || (systemAudio?.isSupported ?? false),
  );

  let screenRecordingHealth = $derived(
    permissionHealth?.screenRecording ?? null,
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

  let selectedSourceLabel = $derived.by(() => {
    if (selected === null) return null;
    if (selected === "system") return systemAudio?.name ?? "System Audio";
    return sources.find((s) => s.id === selected)?.name ?? null;
  });
</script>

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

  <div class="main-layout">
    <aside
      class="sidebar"
      class:locked={recording}
      aria-label="Session configuration"
    >
      <AudioSourcePicker
        {sources}
        {sourcesLoaded}
        bind:selected
        {recording}
        {busy}
        {activeModelName}
        {onScrollToModelPicker}
      />
    </aside>

    <div class="content">
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
        onOpenPermissions={onOpenPermissionsTab}
      />

      {#if result}
        <div
          in:fly={{ y: 8, duration: motionDuration(200), easing: backOut }}
          out:fade={{ duration: motionDuration(150), easing: cubicIn }}
        >
          <ResultBlock {result} />
        </div>
      {/if}

      <MacosPermsPill
        capable={macosCapable}
        allGranted={allPermsGranted}
        anyDenied={anyPermsDenied}
        onOpenPermissions={onOpenPermissionsTab}
      />
    </div>
  </div>

  {#if error}
    <ErrorDisplay {error} />
  {/if}
</section>

<style>
  /* Widen the dictation section beyond the default 36rem so the
     two-column grid (200 px sidebar + 1fr content) has room to
     breathe. History stays at 36rem because it's a single column.
     `:global()` because the .page-section selector is owned by
     +page.svelte and its scoping hash differs from this leaf. */
  :global(#dictation-section) {
    max-width: 52rem;
  }

  .main-layout {
    display: grid;
    grid-template-columns: 200px 1fr;
    gap: 1.5rem;
    align-items: start;
  }

  /* Sidebar column — Rogue Amoeba "panel carved out of the page
     chrome" idiom. The tonal step from `--bg-app` to
     `--bg-sidebar` does the visual delimiting on its own; the
     pre-r2 hairline was barely visible in either theme so the
     boundary now relies on the surface tone difference instead.
     The padding gives the bg some room to read around the
     controls. */
  .sidebar {
    background: var(--bg-sidebar);
    padding: 0.85rem 1rem;
    border-radius: var(--radius-md);
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
    /* Children may have intrinsic widths wider than the 200 px
       column (long device names etc). `min-width: 0` lets them
       shrink + ellipsis within the column rather than overflow
       into the content column. */
    min-width: 0;
    /* Rogue Amoeba-style frozen-while-active treatment: when the
       parent flips `recording=true` the sidebar dims + locks so
       the eye reads "the configuration is committed for this
       session". Underlying inputs are already `disabled` for
       keyboard a11y; this is the visual reinforcement. */
    transition: opacity 250ms ease;
  }
  .sidebar.locked {
    opacity: 0.5;
    pointer-events: none;
  }
  @media (prefers-reduced-motion: reduce) {
    .sidebar {
      transition: none;
    }
  }

  .content {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
    min-width: 0;
  }

  /* Below ~520 px the sidebar's 200 px would crowd the content
     column. Stack instead — the tonal step from `--bg-sidebar`
     still delimits the panel without needing a separate
     boundary rule. */
  @media (max-width: 520px) {
    .main-layout {
      grid-template-columns: 1fr;
      gap: 1rem;
    }
  }

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

  /* Setup banner's primary button — colocated here because it's
     the only consumer left after slice B trimmed ControlsSection. */
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
