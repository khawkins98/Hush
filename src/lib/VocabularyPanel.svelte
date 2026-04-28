<script lang="ts">
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import type { VocabularyTerm } from "./types";

  type Props = {
    vocabulary: VocabularyTerm[];
    vocabularyLoaded: boolean;
    vocabularyError: ErrorDisplayShape | null;
    newVocab: string;
    inputEl?: HTMLInputElement | null;
    onSubmit: (e: Event) => void | Promise<void>;
    onDelete: (term: VocabularyTerm) => void | Promise<void>;
  };

  let {
    vocabulary,
    vocabularyLoaded,
    vocabularyError,
    newVocab = $bindable(),
    inputEl = $bindable(),
    onSubmit,
    onDelete,
  }: Props = $props();

  // Per-row click-to-confirm. First click arms the row's Delete
  // button (label flips to "Click to confirm"); second click within
  // 5 s fires `onDelete`; the timer clears the armed state so a
  // stale click later doesn't catch the user. Same pattern as
  // History's clear-all (#198) and the meeting Stop confirm.
  let confirmingId = $state<number | null>(null);
  let confirmTimer: number | undefined;

  function handleDelete(term: VocabularyTerm) {
    if (confirmingId === term.id) {
      window.clearTimeout(confirmTimer);
      confirmingId = null;
      void onDelete(term);
      return;
    }
    window.clearTimeout(confirmTimer);
    confirmingId = term.id;
    confirmTimer = window.setTimeout(() => {
      confirmingId = null;
    }, 5000);
  }
</script>

<section class="vocabulary panel-vocabulary" aria-labelledby="vocabulary-heading">
  <header class="history-header">
    <h2 id="vocabulary-heading">
      <span class="panel-tag panel-tag-vocabulary" aria-hidden="true">V</span>
      Vocabulary
      <span class="panel-subtitle">biases the recognition</span>
    </h2>
  </header>
  <p class="hint-prose">
    Words Whisper should be primed to recognise — proper nouns,
    jargon, names it otherwise mishears. Joined into the model's
    initial prompt on every transcription. Different from
    Replacements above: vocabulary biases the <em>recognition</em>;
    replacements rewrite the <em>output</em>.
  </p>

  {#if vocabularyError}
    <ErrorDisplay error={vocabularyError} scope="Vocabulary" />
  {/if}

  <form class="replacement-form" onsubmit={onSubmit}>
    <input
      type="text"
      bind:this={inputEl}
      bind:value={newVocab}
      placeholder="Term (e.g. Tauri, ggml, Beingpax)…"
      aria-label="Vocabulary term"
    />
    <button type="submit" disabled={newVocab.trim().length === 0}>Add</button>
  </form>

  {#if !vocabularyLoaded}
    <p class="loading-skeleton">Loading vocabulary…</p>
  {:else if vocabulary.length === 0}
    <p class="empty-history">
      No vocabulary terms yet — add a word above and Whisper
      will be more likely to spell it correctly next time.
    </p>
  {:else}
    <ul class="replacement-list">
      {#each vocabulary as term (term.id)}
        <li class="replacement-row">
          <code class="replacement-find">{term.term}</code>
          <button
            class="ghost danger"
            class:confirming={confirmingId === term.id}
            onclick={() => handleDelete(term)}
            aria-label={confirmingId === term.id
              ? `Click again to confirm deleting ${term.term}`
              : `Delete vocabulary term ${term.term}`}
            data-testid="vocab-delete-{term.id}"
          >
            {confirmingId === term.id ? "Click to confirm" : "Delete"}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
.vocabulary {
  margin-top: 2.5rem;
  text-align: left;
  border-left: 3px solid #e1e1e1;
  padding-left: 1rem;
  padding-bottom: 0.25rem;
}

.panel-vocabulary {
  border-left-color: #d8a64a;
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
.panel-tag-vocabulary {
  background-color: #fff0d4;
  color: #6a4500;
}

.panel-subtitle {
  margin-left: 0.6rem;
  font-size: 0.7em;
  font-weight: 400;
  color: #888;
  font-style: italic;
}

input,
button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  transition: border-color 0.15s, background-color 0.15s;
}

button {
  cursor: pointer;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
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

/* Armed state: first click flips the button into a higher-contrast
   "click to confirm" affordance. Same red palette as the resting
   danger state but opaque so the user reads "this is the confirm
   click" without ambiguity. Auto-resets after 5 s. */
button.ghost.danger.confirming {
  background-color: #fbeaea;
  border-color: #d83a3a;
  color: #8a0000;
  font-weight: 600;
}

/* Error rendering migrated to the shared ErrorDisplay component
   (#199 + follow-up). */

.empty-history {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: #fafafa;
  border: 1px dashed #d1d1d1;
  border-radius: 8px;
  color: #666;
  font-size: 0.9rem;
  text-align: center;
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

.replacement-form {
  display: flex;
  gap: 0.5rem;
  align-items: center;
  margin-bottom: 1rem;
  flex-wrap: wrap;
}

.replacement-form input[type="text"] {
  flex: 1;
  min-width: 8rem;
  padding: 0.5em 0.85em;
  font-size: 0.9rem;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.replacement-form button {
  padding: 0.5em 1.2em;
  font-size: 0.9rem;
}

.replacement-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}

.replacement-row {
  display: flex;
  gap: 0.6rem;
  align-items: center;
  padding: 0.55rem 0.8rem;
  background-color: white;
  border: 1px solid #e1e1e1;
  border-radius: 6px;
  font-size: 0.85rem;
}

.replacement-find {
  background-color: #f4f4f4;
  padding: 0.1em 0.5em;
  border-radius: 4px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  white-space: pre;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 12rem;
  flex-shrink: 1;
  min-width: 0;
}

.replacement-row .ghost {
  margin-left: auto;
}

@media (prefers-color-scheme: dark) {
  input,
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
  .history-header h2 {
    color: #d8d8d8;
  }
  .empty-history {
    background-color: #1f1f1f;
    border-color: #3a3a3a;
    color: #999;
  }
  .hint-prose {
    color: #aaa;
  }
  .replacement-row {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .replacement-find {
    background-color: #1f1f1f;
    color: #f0f0f0;
  }
}
</style>
