<script lang="ts">
  import { backOut, cubicIn } from "svelte/easing";
  import { fade, fly } from "svelte/transition";

  import { audio } from "$lib/state/audio.svelte";
  import { dictation } from "$lib/state/dictation.svelte";
  import { meeting } from "$lib/state/meeting-sessions.svelte";

  import AudioSourcePicker from "./AudioSourcePicker.svelte";
  import AudioWaveform from "./AudioWaveform.svelte";
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import MacosPermsPill from "./MacosPermsPill.svelte";
  import ModelChip from "./ModelChip.svelte";
  import RecordPanel from "./RecordPanel.svelte";
  import ResultBlock from "./ResultBlock.svelte";
  import { joinUtterances } from "./transcript-format";
  import { motionDuration } from "./motion";
  import type { PermissionsHealth } from "./types";

  type Props = {
    isMacOS: boolean;
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
    macosCapable,
    allPermsGranted,
    anyPermsDenied,
    onStart,
    onStop,
    onScrollToModelPicker,
    onOpenPermissionsTab,
  }: Props = $props();

  let mics = $derived(audio.sources.filter((s) => s.kind === "microphone"));
  let systemAudio = $derived(audio.sources.find((s) => s.kind === "system-audio"));
  let hasUsableSource = $derived(
    mics.length > 0 || (systemAudio?.isSupported ?? false),
  );
  let willRecordMeeting = $derived(
    !dictation.recording
      && audio.selected !== null
      && audio.selected !== "system"
      && (systemAudio?.isSupported ?? false),
  );
  let selectedSourceLabel = $derived.by(() => {
    if (audio.selected === null) return null;
    if (audio.selected === "system") return systemAudio?.name ?? "System Audio";
    return audio.sources.find((s) => s.id === audio.selected)?.name ?? null;
  });
  let activeModelName = $derived(dictation.activeModel?.displayName ?? null);

  // True when a meeting session is auto-running but dictation is completely
  // idle (not recording, starting, stopping, or transcribing). In this state
  // the RecordPanel is hidden — the meeting-active banner below provides the
  // sole control point and live transcript. When dictation is busy (stopping
  // or transcribing), the banner must NOT show so RecordPanel's "Working"
  // state remains visible.
  let meetingOnlyActive = $derived(
    meeting.activeId !== null && !dictation.recording && !dictation.busy,
  );

  // Live transcript text for the meeting-active banner — same derivation
  // as RecordPanel's liveTranscriptText but sourced from the module-level
  // meeting state directly so we don't have to thread an extra prop.
  let meetingLiveText = $derived.by(() => {
    if (!meeting.activeDetail) return "";
    const finals = meeting.activeDetail.utterances ?? [];
    const partials = meeting.activeDetail.currentPartials ?? [];
    return joinUtterances([...finals, ...partials], "\n");
  });
</script>

<section id="dictation-section" class="page-section">
  <header class="section-header">
    <h1>Transcribe</h1>
    <p class="tagline">Press, talk, paste. Local Whisper transcription.</p>
  </header>

  {#if dictation.noModelInstalled}
    <aside class="setup-banner" role="status" aria-label="First-time setup">
      <div class="setup-banner-text">
        <strong>Set up your first model</strong>
        <span>Hush needs a Whisper model to transcribe.</span>
      </div>
      <button class="primary" onclick={onScrollToModelPicker}>
        Choose a model
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
  {#if meetingOnlyActive}
    <!--
      Meeting-active banner: visible when the meeting pump is
      running but dictation is idle — i.e., meeting was auto-
      started (or started via the History panel) and the user
      has not also pressed Record for dictation. Surfaces the
      live transcript and the Stop button that would otherwise
      only be reachable by navigating to History.
    -->
    <aside
      class="meeting-active-banner"
      role="status"
      aria-label="Meeting recording in progress"
      in:fly={{ y: -6, duration: motionDuration(200), easing: backOut }}
      out:fade={{ duration: motionDuration(150), easing: cubicIn }}
    >
      <header class="meeting-active-header">
        <span class="meeting-active-dot" aria-hidden="true"></span>
        <strong class="meeting-active-title">Meeting recording in progress</strong>
        <button
          class="meeting-active-stop"
          onclick={() => meeting.stopSession()}
          aria-label="Stop meeting recording"
        >
          Stop
        </button>
      </header>
      <div class="meeting-active-waveform" aria-hidden="true">
        <AudioWaveform mode="recording" metering />
      </div>
      {#if meetingLiveText.trim().length > 0}
        <p
          class="meeting-active-transcript"
          aria-live="polite"
          data-testid="meeting-active-transcript"
        >
          {meetingLiveText}
        </p>
      {:else}
        <p class="meeting-active-waiting">Waiting for speech…</p>
      {/if}
    </aside>
  {/if}

  {#if !meetingOnlyActive}
  <RecordPanel
    recording={dictation.recording}
    busy={dictation.busy}
    transcribing={dictation.transcribing}
    {hasUsableSource}
    noModelInstalled={dictation.noModelInstalled}
    {willRecordMeeting}
    recordMode={dictation.recordMode}
    {selectedSourceLabel}
    {activeModelName}
    error={dictation.error}
    meetingActiveDetail={meeting.activeDetail}
    {onStart}
    {onStop}
    onOpenPermissions={onOpenPermissionsTab}
  >
    {#snippet leftAdjunct()}
      <AudioSourcePicker
        sources={audio.sources}
        sourcesLoaded={audio.sourcesLoaded}
        bind:selected={audio.selected}
        recording={dictation.recording}
        busy={dictation.busy}
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
    {#if isMacOS}<kbd>⌃</kbd> + <kbd>⌥</kbd>{:else}<kbd>Ctrl</kbd> + <kbd>Alt</kbd>{/if}
    + <kbd>H</kbd> to toggle,
    or hold
    {#if isMacOS}<kbd>Right ⌘</kbd>{:else}<kbd>Right Ctrl</kbd>{/if}
    to push-to-talk.
  </p>
  {/if}

  {#if dictation.result}
    <div
      in:fly={{ y: 8, duration: motionDuration(200), easing: backOut }}
      out:fade={{ duration: motionDuration(150), easing: cubicIn }}
    >
      <ResultBlock result={dictation.result} />
    </div>
  {/if}

  <MacosPermsPill
    capable={macosCapable}
    allGranted={allPermsGranted}
    anyDenied={anyPermsDenied}
    onOpenPermissions={onOpenPermissionsTab}
  />

  {#if dictation.error}
    <ErrorDisplay
      error={dictation.error}
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

  /* Recording-state banner shown when the meeting pump is active
     but dictation is idle. Red-tinted to match the HUD's recording
     indicator and Signal clearly that something is happening. */
  .meeting-active-banner {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    padding: 0.85rem 1rem;
    background-color: var(--recording-bg, rgba(220, 38, 38, 0.08));
    border: 1px solid var(--recording-border, rgba(220, 38, 38, 0.3));
    border-radius: var(--radius-md);
  }
  .meeting-active-header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  /* Pulsing red dot — mirrors the HUD's recording indicator. */
  .meeting-active-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background-color: #dc2626;
    flex-shrink: 0;
    animation: meeting-pulse 1.4s ease-in-out infinite;
  }
  @keyframes meeting-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.35; }
  }
  .meeting-active-title {
    flex: 1;
    font-size: 0.9rem;
    color: var(--text-primary);
  }
  .meeting-active-stop {
    flex-shrink: 0;
    background-color: #dc2626;
    color: #fff;
    border: none;
    border-radius: var(--radius-md);
    padding: 0.35rem 0.85rem;
    font-size: 0.82rem;
    font-family: inherit;
    font-weight: 600;
    cursor: pointer;
    transition: background-color 0.12s;
  }
  .meeting-active-stop:hover:not(:disabled) {
    background-color: #b91c1c;
  }
  .meeting-active-waveform {
    height: 48px;
    /* Waveform bars inherit the red recording palette so they match
       the HUD pill rather than the default purple dictation gradient. */
    --audio-waveform-bar-color: #dc2626;
  }
  .meeting-active-transcript {
    font-size: 0.88rem;
    line-height: 1.55;
    color: var(--text-primary);
    white-space: pre-wrap;
    margin: 0;
    max-height: 8rem;
    overflow-y: auto;
  }
  .meeting-active-waiting {
    font-size: 0.85rem;
    color: var(--text-muted);
    margin: 0;
    font-style: italic;
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
