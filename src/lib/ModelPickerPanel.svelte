<script lang="ts">
  import type { ModelCard, ModelSelectNotice, DownloadProgress } from "./types";

  type Props = {
    models: ModelCard[];
    modelsLoaded: boolean;
    modelsError: string | null;
    modelsRestartNotice: ModelSelectNotice;
    downloading: Map<string, DownloadProgress>;
    downloadFailed: Map<string, string>;
    formatMb: (bytes: number) => string;
    onSelect: (card: ModelCard) => void | Promise<void>;
    onDownload: (card: ModelCard) => void | Promise<void>;
    onCancel: (card: ModelCard) => void | Promise<void>;
    onRemove: (card: ModelCard) => void | Promise<void>;
  };

  let {
    models,
    modelsLoaded,
    modelsError,
    modelsRestartNotice,
    downloading,
    downloadFailed,
    formatMb,
    onSelect,
    onDownload,
    onCancel,
    onRemove,
  }: Props = $props();
</script>

<section class="models panel-models" aria-labelledby="models-heading">
  <header class="history-header">
    <h2 id="models-heading">
      <span class="panel-tag panel-tag-models" aria-hidden="true">M</span>
      Model
    </h2>
  </header>
  <p class="hint-prose">
    Pick a Whisper variant. Bigger models are slower but more
    accurate. Hush expects model files in
    <code class="path-hint" title={models[0]?.expectedPath ?? ""}
      >&lt;app-data&gt;/models/</code
    >; download them from
    <a
      href="https://huggingface.co/ggerganov/whisper.cpp/tree/main"
      target="_blank"
      rel="noopener noreferrer">whisper.cpp on Hugging Face</a
    > and place them in that folder.
  </p>

  {#if modelsError}
    <p class="error scoped-error" role="alert">
      <strong>Model:</strong>
      {modelsError}
    </p>
  {/if}

  {#if modelsRestartNotice === "loaded"}
    <p class="restart-notice notice-loaded" role="status">
      ✓ Loaded — ready to record now (no restart needed).
    </p>
  {:else if modelsRestartNotice === "needs-download"}
    <p class="restart-notice notice-warn" role="status">
      Saved as default — but this model isn't downloaded yet. Click
      <strong>Download</strong> on the card below to fetch it.
    </p>
  {:else if modelsRestartNotice === "needs-restart"}
    <p class="restart-notice" role="status">
      Saved. Restart Hush to use the new model.
    </p>
  {/if}

  {#if !modelsLoaded}
    <p class="loading-skeleton">Loading models…</p>
  {/if}

  <ul class="model-grid">
    {#each models as card (card.id)}
      {@const inFlight = downloading.get(card.id) ?? null}
      {@const failure = downloadFailed.get(card.id) ?? null}
      <li
        class="model-card"
        class:selected={card.isSelected}
        class:unavailable={!card.isDownloaded && !inFlight}
      >
        <!--
          The card body is a `<button>` so the user can click any
          card to set it as default — including ones that aren't
          downloaded yet (the `selectModel` handler persists the
          selection and the notice pill above tells the user they
          need to Download next). Action buttons (Download, Cancel,
          Try again, Remove) live in a sibling `<footer>` below;
          keeping them out of the card-body button avoids invalid
          nested-button HTML.
        -->
        <button
          type="button"
          class="model-card-button"
          onclick={() => onSelect(card)}
          aria-label={card.isDownloaded
            ? `Select ${card.displayName}`
            : `Select ${card.displayName} (will need Download to use)`}
          aria-pressed={card.isSelected}
        >
          <header class="model-card-head">
            <h3 class="model-name">
              {card.displayName}
              {#if card.isSelected}
                <span class="badge default-badge">Default</span>
              {/if}
            </h3>
            {#if card.isSelected}
              <span class="model-card-current" aria-hidden="true">●</span>
            {/if}
          </header>
          <p class="model-stats">
            <span>{card.sizeMb} MB</span>
            <span class="stat">
              Speed
              <span class="bars" aria-label="{card.speedRating} of 10">
                {#each Array(10) as _, i}
                  <span class:on={i < card.speedRating}></span>
                {/each}
              </span>
              {card.speedRating.toFixed(1)}
            </span>
            <span class="stat">
              Accuracy
              <span class="bars" aria-label="{card.accuracyRating} of 10">
                {#each Array(10) as _, i}
                  <span class:on={i < card.accuracyRating}></span>
                {/each}
              </span>
              {card.accuracyRating.toFixed(1)}
            </span>
          </p>
          <p class="model-desc">{card.description}</p>
        </button>

        <!-- Per-card action footer: Download / Cancel / Try again / Remove. -->
        <footer class="model-card-actions">
          {#if inFlight}
            <!--
              Active download: progress bar + Cancel.

              When `total` is null the download size is unknown, so
              the bar enters indeterminate state — `aria-valuenow`
              / `aria-valuemax` are omitted (per WAI-ARIA, a
              progressbar without a numeric `valuenow` is treated
              as indeterminate). The `aria-valuetext` provides the
              screen-reader-friendly version of what's drawn, so
              the announcement matches the visible state instead
              of stating a fake "0 of 100" reading. Closes the
              progress-bar a11y half of #48.
            -->
            <div class="download-progress" role="progressbar"
              aria-valuemin="0"
              aria-valuemax={inFlight.total ?? undefined}
              aria-valuenow={inFlight.total ? inFlight.received : undefined}
              aria-valuetext={inFlight.total
                ? `${Math.round((inFlight.received / inFlight.total) * 100)}% — ${formatMb(inFlight.received)} of ${formatMb(inFlight.total)}`
                : `Downloading ${formatMb(inFlight.received)} (size unknown)`}
              aria-label="Downloading {card.displayName}"
            >
              <div
                class="download-progress-bar"
                style:width={inFlight.total
                  ? `${Math.min(100, (inFlight.received / inFlight.total) * 100)}%`
                  : "100%"}
              ></div>
            </div>
            <span class="download-progress-text">
              {formatMb(inFlight.received)}{#if inFlight.total} / {formatMb(inFlight.total)}{/if}
            </span>
            <button class="ghost danger" onclick={() => onCancel(card)}>
              Cancel
            </button>
          {:else if failure}
            <!-- Failure: error chip + Try again. -->
            <p class="model-failure" role="alert">{failure}</p>
            <button class="ghost" onclick={() => onDownload(card)}>
              Try again
            </button>
          {:else if card.isDownloaded}
            <!-- Downloaded: a small Remove button so the user can
                 reclaim disk if they change their mind. -->
            <button class="ghost danger" onclick={() => onRemove(card)}>
              Remove
            </button>
          {:else}
            <!-- Not downloaded, no in-flight or failure. -->
            <button class="ghost primary" onclick={() => onDownload(card)}>
              Download
            </button>
          {/if}
        </footer>
      </li>
    {/each}
  </ul>
</section>

<style>
.models {
  margin-top: 2.5rem;
  text-align: left;
  border-left: 3px solid #e1e1e1;
  padding-left: 1rem;
  padding-bottom: 0.25rem;
}

.panel-models {
  border-left-color: #4a8a4a;
}

.history-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: 1rem;
}

.history-header h2 {
  margin: 0;
  font-size: 1.1rem;
  font-weight: 600;
  color: #333;
}

.panel-tag {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.4em;
  height: 1.4em;
  border-radius: 5px;
  font-size: 0.75em;
  font-weight: 700;
  background-color: #e8e8e8;
  color: #444;
  margin-right: 0.5rem;
}
.panel-tag-models {
  background-color: #d6ecd6;
  color: #1f5a1f;
}

button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  cursor: pointer;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  transition: border-color 0.15s, background-color 0.15s;
}

button:hover:not(:disabled) {
  border-color: #396cd8;
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

button.ghost {
  padding: 0.3em 0.75em;
  font-size: 0.8rem;
  font-weight: 500;
  background-color: transparent;
  border: 1px solid #d1d1d1;
}

button.ghost:hover:not(:disabled) {
  background-color: #f0f0f0;
}

button.ghost.danger {
  color: #b03030;
  border-color: #e1b8b8;
}

button.ghost.danger:hover:not(:disabled) {
  background-color: #fbeaea;
  border-color: #d83a3a;
}

button.ghost.primary {
  border-color: #6a8cf0;
  color: #2c3e8f;
}

button.ghost.primary:hover:not(:disabled) {
  background-color: #eef2ff;
  border-color: #4a6cd0;
}

.error {
  margin-top: 1.5rem;
  padding: 0.75rem 1rem;
  background-color: #fee;
  border: 1px solid #d83a3a;
  border-radius: 8px;
  color: #8a0000;
  text-align: left;
  line-height: 1.5;
}

.scoped-error {
  padding-left: 1rem;
}
.scoped-error strong {
  margin-right: 0.4rem;
}

.loading-skeleton {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: #fafafa;
  border-radius: 6px;
  color: #999;
  font-size: 0.9rem;
  text-align: center;
  font-style: italic;
}

.hint-prose {
  margin: 0 0 1rem;
  font-size: 0.85rem;
  color: #555;
  line-height: 1.5;
}

.hint-prose code {
  background-color: #eef2ff;
  padding: 0.05em 0.4em;
  border-radius: 4px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.9em;
}

.path-hint {
  background-color: #eef2ff;
  padding: 0.05em 0.4em;
  border-radius: 4px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.restart-notice {
  margin: 0.5rem 0 1rem;
  padding: 0.6rem 0.85rem;
  background-color: #e8f5e8;
  border: 1px solid #b8d8b8;
  border-radius: 6px;
  color: #1f5a1f;
  font-size: 0.9rem;
}

/* Three flavours of post-select notice. The default green (above)
   covers the "needs-restart" edge case. `notice-loaded` is the happy
   path — saturated green to read as success. `notice-warn` is amber
   — selection persisted but user has work left (Download). */
.notice-loaded {
  background-color: #d1f0d1;
  border-color: #8fc88f;
  color: #1a4a1a;
}

.notice-warn {
  background-color: #fef3c7;
  border-color: #fcd34d;
  color: #92400e;
}

.model-grid {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}

.model-card {
  border-radius: 12px;
  background-color: white;
  border: 1px solid #e1e1e1;
  transition: border-color 0.15s, background-color 0.15s;
}

.model-card.selected {
  border-color: #6a8cf0;
  background-color: #f5f8ff;
  box-shadow: 0 0 0 1px #6a8cf0;
}

.model-card.unavailable {
  opacity: 0.55;
}

.model-card-button {
  width: 100%;
  display: block;
  background: transparent;
  border: none;
  padding: 0.85rem 1.1rem;
  text-align: left;
  border-radius: 12px;
  cursor: pointer;
  font: inherit;
  color: inherit;
}

.model-card-button:disabled {
  cursor: default;
}

.model-card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 0.5rem;
}

.model-name {
  margin: 0;
  font-size: 1rem;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 0.6rem;
}

.badge {
  font-size: 0.7rem;
  font-weight: 500;
  padding: 0.05rem 0.45rem;
  border-radius: 999px;
  background-color: #c7d2fe;
  color: #2c3e8f;
}

.model-card-current {
  color: #6a8cf0;
  font-size: 0.85rem;
}

.model-stats {
  display: flex;
  flex-wrap: wrap;
  gap: 1rem;
  margin: 0.5rem 0 0.4rem;
  font-size: 0.8rem;
  color: #555;
  align-items: center;
}

.model-stats .stat {
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
}

.bars {
  display: inline-flex;
  gap: 2px;
}

.bars span {
  width: 5px;
  height: 9px;
  border-radius: 1px;
  background-color: #d8d8d8;
  display: inline-block;
}

.bars span.on {
  background-color: #6a8cf0;
}

.model-desc {
  margin: 0;
  font-size: 0.85rem;
  color: #444;
  line-height: 1.45;
}

.model-card-actions {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0 1.1rem 0.85rem;
  flex-wrap: wrap;
}

.download-progress {
  flex: 1;
  min-width: 6rem;
  height: 6px;
  background-color: #e8e8e8;
  border-radius: 3px;
  overflow: hidden;
}

.download-progress-bar {
  height: 100%;
  background-color: #6a8cf0;
  transition: width 0.15s ease-out;
}

.download-progress-text {
  font-size: 0.8rem;
  color: #555;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.model-failure {
  flex: 1;
  margin: 0;
  padding: 0.4rem 0.6rem;
  background-color: #fee;
  border: 1px solid #d83a3a;
  border-radius: 4px;
  color: #8a0000;
  font-size: 0.85rem;
}

@media (prefers-color-scheme: dark) {
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button:hover:not(:disabled) {
    border-color: #6a8cf0;
  }
  button.ghost {
    border-color: #3a3a3a;
    color: #f0f0f0;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
  button.ghost.danger {
    color: #ff9090;
    border-color: #5a2020;
  }
  button.ghost.danger:hover:not(:disabled) {
    background-color: #3a1818;
    border-color: #d83a3a;
  }
  button.ghost.primary {
    border-color: #6a8cf0;
    color: #c0d0ff;
  }
  button.ghost.primary:hover:not(:disabled) {
    background-color: #1e2a4a;
  }
  .history-header h2 {
    color: #d8d8d8;
  }
  .error {
    background-color: #4a1a1a;
    border-color: #d83a3a;
    color: #ffd0d0;
  }
  .restart-notice {
    background-color: #1a3a1a;
    border-color: #2a5a2a;
    color: #c8e8c8;
  }
  .notice-loaded {
    background-color: #14532d;
    border-color: #166534;
    color: #bbf7d0;
  }
  .notice-warn {
    background-color: #422006;
    border-color: #92400e;
    color: #fde68a;
  }
  .hint-prose {
    color: #aaa;
  }
  .hint-prose code {
    background-color: #1e2a4a;
    color: #c0d0ff;
  }
  .path-hint {
    background-color: #1e2a4a;
    color: #c0d0ff;
  }
  .model-card {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .model-card.selected {
    background-color: #2a3050;
    border-color: #6a8cf0;
  }
  .model-stats {
    color: #aaa;
  }
  .model-desc {
    color: #d0d0d0;
  }
  .bars span {
    background-color: #3a3a3a;
  }
  .bars span.on {
    background-color: #8aa0ff;
  }
  .badge {
    background-color: #3a4a7a;
    color: #d0d8ff;
  }
  .download-progress {
    background-color: #3a3a3a;
  }
  .download-progress-bar {
    background-color: #8aa0ff;
  }
  .download-progress-text {
    color: #aaa;
  }
  .model-failure {
    background-color: #4a1a1a;
    border-color: #d83a3a;
    color: #ffd0d0;
  }
}
</style>
