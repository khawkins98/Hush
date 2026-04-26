<script lang="ts">
  import type { DictationResult } from "./types";

  type Props = {
    result: DictationResult;
  };
  let { result }: Props = $props();
</script>

<section class="result" aria-live="polite">
  <h2>Transcription</h2>
  <p class="text">{result.text || "(empty)"}</p>
  {#if result.foreground}
    <p class="meta">
      Captured while focused on <em>{result.foreground.appName}</em>
      {#if result.foreground.windowTitle}— {result.foreground.windowTitle}{/if}
    </p>
  {/if}
  <p class="meta">Already on your clipboard. Paste with ⌘V / Ctrl+V.</p>
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
