<!--
  Dictation page-section render. Wraps the section header, the
  keyboard-shortcut hint, ControlsSection, the conditional
  ResultBlock (with F6 transitions), and the MacosPermsPill.

  Render-only by design: every dictation IPC handler and hotkey
  listener stays in the orchestrator because they touch state
  that lives across multiple sections (refreshHistory, the
  meeting-pump upgrade path, the permissions recovery surface).
  Pulling them out would entangle the seam more than it unwinds.
-->
<script lang="ts">
  import { backOut, cubicIn } from "svelte/easing";
  import { fade, fly } from "svelte/transition";

  import ControlsSection from "./ControlsSection.svelte";
  import MacosPermsPill from "./MacosPermsPill.svelte";
  import ResultBlock from "./ResultBlock.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import { motionDuration } from "./motion";
  import type {
    AudioSourceListing,
    DictationResult,
    PermissionsHealth,
  } from "./types";

  type Props = {
    /// Whether the host platform is macOS — drives the PTT-key
    /// glyph in the keyboard-shortcut hint.
    isMacOS: boolean;
    /// Audio source list + load state (loaded by orchestrator).
    sources: AudioSourceListing[];
    sourcesLoaded: boolean;
    /// Bindable picker selection — proxied straight through to
    /// `ControlsSection` so the orchestrator's `selectedAsAudioSource`
    /// helper sees the same value.
    selected: string | null;
    /// Live recording state (orchestrator owns the writes).
    recording: boolean;
    /// IPC-in-flight guard.
    busy: boolean;
    /// Derived mid-transcription flag.
    transcribing: boolean;
    /// Models list reports nothing downloaded — drives the
    /// no-model setup banner inside `ControlsSection`.
    noModelInstalled: boolean;
    /// Last error to surface inline (passed through to
    /// `ControlsSection`'s `<ErrorDisplay>`).
    error: ErrorDisplayShape | null;
    /// Dictation result for the inline transcript card. Null
    /// after dismiss / before any session lands.
    result: DictationResult | null;
    /// Active recording mode — drives the `mic only / mic + system
    /// audio` label inside the recording status pill.
    recordMode: "dictation" | "meeting" | null;
    /// Active model display name — `null` while none loaded.
    activeModelName: string | null;
    /// Three-state Screen Recording health for the mic-only
    /// badge inside `ControlsSection`.
    permissionHealth: PermissionsHealth | null;
    /// macOS perm-pill props (loaded by `PermissionHealthSection`,
    /// passed through here).
    macosCapable: boolean;
    allPermsGranted: boolean;
    anyPermsDenied: boolean;
    /// Action callbacks — owned by the orchestrator because each
    /// reaches into cross-section state (open Settings tab,
    /// scroll the model picker into view, kick off the start /
    /// stop IPC fan-out).
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

  <ControlsSection
    {sources}
    {sourcesLoaded}
    bind:selected
    {recording}
    {busy}
    {transcribing}
    {noModelInstalled}
    {error}
    {onStart}
    {onStop}
    {onScrollToModelPicker}
    {activeModelName}
    screenRecordingHealth={permissionHealth?.screenRecording ?? null}
    onOpenPermissions={onOpenPermissionsTab}
    {recordMode}
  />

  {#if result}
    <!--
      F6: spring-out fly + fade on appear, plain fade on dismiss.
      The transcript rising up from below mirrors the speech-to-
      text "result emerges" mental model; the exit just dissolves
      so the user doesn't see it slide off-screen mid-cleanup.
      `motionDuration` honours prefers-reduced-motion (collapses
      to 0 ms there).
    -->
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
</section>

<style>
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
    /* Sticky so the hotkey hint stays visible as the page grows.
       The UX review flagged that the original (non-sticky) card
       scrolls off once the user has built up some history /
       replacements / vocabulary. */
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
