<script lang="ts">
  import { writeText } from "@tauri-apps/plugin-clipboard-manager";
  import {
    EXPORT_FORMAT_LABELS,
    exportAs,
    readStoredFormat,
    rememberFormat,
    type ExportFormat,
  } from "./export-formats";
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

  // Export-format picker (#427 Item 4). Plain text already lands on
  // the clipboard automatically via the dictation-stop path; the
  // picker re-writes the clipboard with a chosen format on click,
  // for SRT subtitles, Markdown notes, etc. Last pick is sticky so
  // a user who exports as Markdown daily doesn't re-pick every time.
  let copyConfirmation = $state<string | null>(null);
  let copyConfirmationTimer: ReturnType<typeof setTimeout> | null = null;

  // The four offered formats, in display order. `as const` so the
  // literal types are preserved through the iteration.
  const FORMATS = ["plain", "markdown", "srt", "vtt"] as const satisfies readonly ExportFormat[];

  async function copyAs(format: ExportFormat) {
    const body = exportAs(format, {
      text: result.text,
      durationMs: result.durationMs,
    });
    try {
      await writeText(body);
      rememberFormat(format);
      copyConfirmation = format;
      if (copyConfirmationTimer !== null) {
        clearTimeout(copyConfirmationTimer);
      }
      // Auto-clear the confirmation so the picker returns to its
      // neutral state. 1.6 s is long enough to read but short
      // enough that a fast follow-up re-export doesn't queue up
      // confirmations.
      copyConfirmationTimer = setTimeout(() => {
        copyConfirmation = null;
        copyConfirmationTimer = null;
      }, 1600);
    } catch (e) {
      console.warn("[hush] export-as clipboard write failed", e);
      copyConfirmation = null;
    }
  }

  // The most recently used format gets a subtle highlight so users
  // who export as the same shape repeatedly see their preference
  // surface. Read at script-evaluation time so a result that
  // arrives after a previous export already shows the right
  // highlight on first paint.
  let lastUsedFormat = $state<ExportFormat>(readStoredFormat());
  $effect(() => {
    if (copyConfirmation !== null) {
      lastUsedFormat = copyConfirmation as ExportFormat;
    }
  });
</script>

<section class="result" aria-live="polite">
  <h2>Transcript</h2>
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
    <!--
      Export-format picker. Re-writes the clipboard with the chosen
      conversion. Plain text is the default landing format (covered
      by the auto-copy path above) but exposed here too so the
      flow is symmetric — a user who wants to sanity-check the
      "copy" affordance can click Plain and see the same thing.
      SRT/WebVTT use `result.durationMs` as the cue duration —
      single-block subtitle covering the whole capture, since a
      `DictationResult` doesn't carry per-segment timestamps. The
      meeting-session export path (in `ExportOptionsDialog`) has
      richer per-segment data and a separate code path.
    -->
    <div
      class="export-picker"
      role="group"
      aria-label="Copy transcript as a different format"
      data-testid="export-picker"
    >
      <span class="export-label">Copy as:</span>
      {#each FORMATS as format (format)}
        <button
          type="button"
          class="export-option"
          class:active={lastUsedFormat === format}
          class:confirmed={copyConfirmation === format}
          data-testid={`export-format-${format}`}
          onclick={() => copyAs(format)}
        >
          {EXPORT_FORMAT_LABELS[format]}
          {#if copyConfirmation === format}
            <span class="check" aria-hidden="true">✓</span>
          {/if}
        </button>
      {/each}
    </div>
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

/* Export-format picker: a row of small button chips below the
   transcript meta. Active (last-used) chip gets a subtle accent
   border; the just-confirmed chip flashes a check + accent fill. */
.export-picker {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.4rem;
  margin-top: 0.6rem;
}
.export-label {
  font-size: 0.8rem;
  color: #777;
  margin-right: 0.15rem;
}
.export-option {
  appearance: none;
  border: 1px solid #d1d1d8;
  background-color: white;
  color: #333;
  padding: 0.18rem 0.55rem;
  font-size: 0.78rem;
  font-family: inherit;
  font-weight: 500;
  border-radius: 999px;
  cursor: pointer;
  transition: background-color 0.12s, border-color 0.12s, color 0.12s;
  display: inline-flex;
  align-items: center;
  gap: 0.25rem;
}
.export-option:hover:not(:disabled) {
  border-color: var(--accent, #7c6ff7);
  color: var(--text-primary, #111);
}
.export-option.active {
  border-color: var(--accent, #7c6ff7);
  color: var(--accent, #7c6ff7);
}
.export-option.confirmed {
  background-color: var(--accent-subtle, rgba(124, 111, 247, 0.18));
  color: var(--accent-hover, #5c4fd4);
  border-color: var(--accent, #7c6ff7);
}
.export-option .check {
  font-weight: 700;
}
.export-option:focus-visible {
  outline: 2px solid var(--accent, #7c6ff7);
  outline-offset: 2px;
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
  .export-label {
    color: #aaa;
  }
  .export-option {
    background-color: #2a2a2d;
    border-color: #3a3a3e;
    color: #d8d8d8;
  }
}
</style>
