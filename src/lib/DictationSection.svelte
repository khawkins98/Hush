<!--
  Dictation page-section render. Top-to-bottom layout: setup
  banner (when no model installed) → centerpiece RecordPanel
  (waveform on top + button + adjuncts row + status) → shortcut
  hint → result block → macOS perms pill.

  The pre-r3 sidebar/content two-column grid is gone — the
  source picker and model chip are now adjunct snippets passed
  into RecordPanel so they render flanking the centerpiece
  button on a single row. The page reads top-to-bottom rather
  than fighting between columns.

  Render-only by design: the orchestrator owns dictation IPC and
  hotkey listeners. This section composes the leaves and computes
  the cross-leaf deriveds (`hasUsableSource`, `badgeVisible`,
  `willRecordMeeting`, `selectedSourceLabel`).
-->
<script lang="ts">
  import { backOut, cubicIn } from "svelte/easing";
  import { fade, fly } from "svelte/transition";

  import AudioSourcePicker from "./AudioSourcePicker.svelte";
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import MacosPermsPill from "./MacosPermsPill.svelte";
  import ModelChip from "./ModelChip.svelte";
  import RecordPanel from "./RecordPanel.svelte";
  import ResultBlock from "./ResultBlock.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import { motionDuration } from "./motion";
  import type {
    AudioSourceListing,
    DictationResult,
    MeetingSessionDetail,
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
    /// Live meeting-pump session detail — finalized utterances +
    /// in-flight partials. Threaded through to RecordPanel which
    /// renders it as a live transcript while recording. `null`
    /// when no meeting session is active.
    meetingActiveDetail?: MeetingSessionDetail | null;
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
    meetingActiveDetail = null,
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

  // System audio is always available via CoreAudio tap (#600) — no
  // Screen Recording permission needed.  Badge is never shown.
  const badgeVisible = false;
  const badgeIsStale = false;

  let willRecordMeeting = $derived(
    !recording
      && selected !== null
      && selected !== "system"
      && (systemAudio?.isSupported ?? false),
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

  <!--
    Centerpiece dictation area. RecordPanel renders the waveform
    and the circular Record button; the source dropdown +
    model chip are passed in as adjunct snippets so they render
    flanking the button on a single row. No sidebar, no two-
    column grid — the page now reads top-to-bottom.
  -->
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
    {meetingActiveDetail}
    {onStart}
    {onStop}
    onOpenPermissions={onOpenPermissionsTab}
  >
    {#snippet leftAdjunct()}
      <AudioSourcePicker
        {sources}
        {sourcesLoaded}
        bind:selected
        {recording}
        {busy}
      />
    {/snippet}
    {#snippet rightAdjunct()}
      <ModelChip {activeModelName} {onScrollToModelPicker} />
    {/snippet}
  </RecordPanel>

  <!--
    Keyboard-shortcut hint sits under the recording area as a
    contextual reminder. Quieter visual treatment so it doesn't
    compete with the centerpiece waveform + button above.
  -->
  <p class="shortcut-hint" aria-label="Keyboard shortcuts">
    <kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd> to toggle,
    or hold
    {#if isMacOS}<kbd>Right ⌘</kbd>{:else}<kbd>Right Ctrl</kbd>{/if}
    to push-to-talk.
  </p>

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

  {#if error}
    <ErrorDisplay
      {error}
      onAction={(key) => {
        if (key === "open-model-settings") {
          onScrollToModelPicker();
        }
      }}
    />
  {/if}
</section>

<style>
  /* Widen the dictation section to 52 rem so the centerpiece +
     adjuncts row reads. History stays at 36 rem (single column).
     `:global()` because the `.page-section` selector is owned by
     `+page.svelte` and its scoping hash differs from this leaf. */
  :global(#dictation-section) {
    max-width: 52rem;
    /* Centre the centerpiece on ultra-wide windows so it doesn't
       hug the sidebar edge with empty space on the right. The
       page-section's pre-r3 default `margin: 0 auto` was dropped
       in #479 slice 1 to let History fill the column; centering
       is reinstated here per-section because the centerpiece
       composition reads better bounded. */
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }

  /* Section-header subtitle. Lifted from +page.svelte when the
     History tagline was removed and this became the only consumer
     — keeping it in the parent file would have left a Svelte
     unused-selector warning since Svelte component styles are
     scoped per file. */
  .tagline {
    color: var(--text-muted);
    margin: 0 0 1.25rem;
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

  /* Inline keyboard-shortcut hint, contextually placed below the
     record area. Pre-r2 this was an info-box-styled `.hint
     hint-sticky` strip pinned above the section; visually heavy
     for a contextual reminder. The new treatment is text-only —
     muted body copy with kbd chips that match the hint's
     quieter weight. */
  .shortcut-hint {
    margin: 0;
    text-align: center;
    font-size: 0.82rem;
    line-height: 1.6;
    color: var(--text-muted);
  }
  .shortcut-hint kbd {
    display: inline-block;
    padding: 0.05rem 0.4rem;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.85em;
    color: var(--text-secondary);
    background-color: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    margin: 0 0.1rem;
  }
</style>
