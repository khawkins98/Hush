<script lang="ts">
  import { formatDuration } from "./format";
  import type { DictationResult } from "./types";

  type Props = {
    result: DictationResult;
  };
  let { result }: Props = $props();

  // Empty `text` means whisper either heard nothing or emitted only
  // bracket sentinels (`[BLANK_AUDIO]`, `[NOISE]`, …) that the
  // backend stripped (#196). Render a friendly explanation with a
  // recovery hint rather than a technical empty / placeholder
  // string — gives the user something to act on.
  let isEmpty = $derived(result.text.length === 0);
  let durationLabel = $derived(formatDuration(result.durationMs));
</script>

<section class="result" aria-live="polite">
  <h2>Transcription</h2>
  {#if isEmpty}
    <p class="text empty-result">
      No audio detected. Try speaking closer to the mic, or check
      that the right input source is selected.
    </p>
  {:else}
    <p class="text">{result.text}</p>
  {/if}
  {#if durationLabel}
    <p class="meta">Recorded for {durationLabel}.</p>
  {/if}
  {#if result.foreground}
    <p class="meta">
      Captured while focused on <em>{result.foreground.appName}</em>
      {#if result.foreground.windowTitle}— {result.foreground.windowTitle}{/if}
    </p>
  {/if}
  {#if !isEmpty}
    <p class="meta">Already on your clipboard. Paste with ⌘V / Ctrl+V.</p>
  {/if}
</section>

<style>
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

.result .empty-result {
  /* Empty state: smaller, dimmer, italic — not as eye-catching as
     a real transcript but still legible and clearly *the result*
     rather than an error. The recovery hint reads as guidance,
     not a failure. */
  font-size: 0.95rem;
  font-style: italic;
  color: #666;
}

.result .meta {
  margin: 0.25rem 0;
  font-size: 0.85rem;
  color: #666;
}

@media (prefers-color-scheme: dark) {
  .result {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .result h2,
  .result .meta {
    color: #aaa;
  }
}
</style>
