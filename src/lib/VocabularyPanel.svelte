<script lang="ts">
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import type { LanguageStyle, PackStatus, VocabularyTerm } from "./types";

  type Props = {
    vocabulary: VocabularyTerm[];
    vocabularyLoaded: boolean;
    vocabularyError: ErrorDisplayShape | null;
    packs: PackStatus[];
    languageStyle: LanguageStyle;
    newVocab: string;
    inputEl?: HTMLInputElement | null;
    onSubmit: (e: Event) => void | Promise<void>;
    onDelete: (term: VocabularyTerm) => void | Promise<void>;
    onTogglePack: (slug: string, enable: boolean) => void | Promise<void>;
    onSetLanguageStyle: (style: LanguageStyle) => void | Promise<void>;
  };

  let {
    vocabulary,
    vocabularyLoaded,
    vocabularyError,
    packs,
    languageStyle,
    newVocab = $bindable(),
    inputEl = $bindable(),
    onSubmit,
    onDelete,
    onTogglePack,
    onSetLanguageStyle,
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

  const STYLE_LABELS: Record<LanguageStyle, string> = {
    american: "American English",
    british: "British English",
    oxford: "Oxford English",
  };
</script>

<!-- Language style -->
<section class="vocabulary panel-language-style" aria-labelledby="language-style-heading">
  <header class="history-header">
    <h2 id="language-style-heading">Language Style</h2>
  </header>
  <p class="hint-prose">
    Sets the spelling style hint in Whisper's initial prompt. American is the
    model default. British and Oxford add a short prefix ("Use British English
    spelling.") that nudges the model toward the appropriate spellings.
  </p>
  <div class="style-options" role="radiogroup" aria-label="Language style">
    {#each (["american", "british", "oxford"] as LanguageStyle[]) as style}
      <label class="style-option" class:selected={languageStyle === style}>
        <input
          type="radio"
          name="language-style"
          value={style}
          checked={languageStyle === style}
          onchange={() => void onSetLanguageStyle(style)}
        />
        {STYLE_LABELS[style]}
      </label>
    {/each}
  </div>
</section>

<!-- Preset packs -->
{#if packs.length > 0}
  <section class="vocabulary panel-packs" aria-labelledby="packs-heading">
    <header class="history-header">
      <h2 id="packs-heading">Preset Packs</h2>
    </header>
    <p class="hint-prose">
      Opt-in vocabulary and replacement bundles. Enabled packs supplement your
      personal vocabulary — their terms are deduplicated against your own so
      your spellings always win.
    </p>
    <ul class="pack-list">
      {#each packs as pack (pack.slug)}
        <li class="pack-row">
          <label class="pack-label">
            <input
              type="checkbox"
              checked={pack.enabled}
              onchange={(e) =>
                void onTogglePack(pack.slug, (e.target as HTMLInputElement).checked)}
              aria-label={`${pack.enabled ? "Disable" : "Enable"} ${pack.name} pack`}
            />
            <span class="pack-name">{pack.name}</span>
          </label>
          <span class="pack-description">{pack.description}</span>
          <span class="pack-counts">
            {pack.vocabularyCount} terms · {pack.replacementCount} replacements
          </span>
        </li>
      {/each}
    </ul>
  </section>
{/if}

<!-- Personal vocabulary -->
<section class="vocabulary panel-vocabulary" aria-labelledby="vocabulary-heading">
  <header class="history-header">
    <h2 id="vocabulary-heading">Personal Vocabulary</h2>
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
  border-left: 3px solid var(--border);
  padding-left: 1rem;
  padding-bottom: 0.25rem;
}

.panel-vocabulary {
  border-left-color: #d8a64a;
}

.panel-packs {
  border-left-color: #5b8dd9;
}

.panel-language-style {
  border-left-color: #6cba7d;
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
  color: var(--text-primary);
}

input,
button {
  border-radius: 8px;
  border: 1px solid var(--border-input);
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: var(--text-primary);
  background-color: var(--bg-surface);
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
  border-color: var(--accent-hover);
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
  border: 1px solid var(--border-input);
}

button.ghost:hover:not(:disabled) {
  background-color: var(--bg-app);
}

button.ghost.danger {
  color: var(--danger);
  border-color: var(--danger-border);
}

button.ghost.danger:hover:not(:disabled) {
  background-color: var(--danger-bg);
  border-color: var(--danger);
}

/* Armed state: first click flips the button into a higher-contrast
   "click to confirm" affordance. Same red palette as the resting
   danger state but opaque so the user reads "this is the confirm
   click" without ambiguity. Auto-resets after 5 s. */
button.ghost.danger.confirming {
  background-color: var(--danger-bg);
  border-color: var(--danger);
  color: #8a0000;
  font-weight: 600;
}

.empty-history {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: var(--bg-surface);
  border: 1px dashed var(--border-input);
  border-radius: 8px;
  color: var(--text-muted);
  font-size: 0.9rem;
  text-align: center;
}

.loading-skeleton {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: var(--bg-surface);
  border-radius: 6px;
  color: var(--text-muted);
  font-size: 0.9rem;
  text-align: center;
  font-style: italic;
}

.hint-prose {
  margin: 0 0 1rem;
  font-size: 0.85rem;
  color: var(--text-muted);
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
  background-color: var(--bg-surface);
  border: 1px solid var(--border);
  border-radius: 6px;
  font-size: 0.85rem;
}

.replacement-find {
  background-color: var(--bg-sidebar);
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

/* Language style radio group */
.style-options {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
  margin-bottom: 0.5rem;
}

.style-option {
  display: flex;
  align-items: center;
  gap: 0.4rem;
  padding: 0.45em 1em;
  border: 1px solid var(--border-input);
  border-radius: 8px;
  font-size: 0.875rem;
  cursor: pointer;
  background-color: var(--bg-surface);
  transition: border-color 0.15s, background-color 0.15s;
}

.style-option:hover {
  border-color: var(--accent-hover);
}

.style-option.selected {
  border-color: var(--accent);
  background-color: color-mix(in srgb, var(--accent) 10%, var(--bg-surface));
}

.style-option input[type="radio"] {
  width: auto;
  padding: 0;
  margin: 0;
  border: none;
  background: none;
  accent-color: var(--accent);
}

/* Preset pack list */
.pack-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.pack-row {
  display: grid;
  grid-template-columns: 1fr;
  gap: 0.2rem;
  padding: 0.7rem 0.9rem;
  background-color: var(--bg-surface);
  border: 1px solid var(--border);
  border-radius: 8px;
  font-size: 0.875rem;
}

.pack-label {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-weight: 600;
  color: var(--text-primary);
  cursor: pointer;
}

.pack-label input[type="checkbox"] {
  width: auto;
  padding: 0;
  border: none;
  background: none;
  accent-color: var(--accent);
}

.pack-name {
  font-size: 0.9rem;
}

.pack-description {
  font-size: 0.8rem;
  color: var(--text-muted);
  line-height: 1.4;
}

.pack-counts {
  font-size: 0.75rem;
  color: var(--text-muted);
  font-variant-numeric: tabular-nums;
}

@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) input,
  :root:not([data-theme="light"]) button {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) button:hover:not(:disabled) {
    border-color: var(--accent);
  }
  :root:not([data-theme="light"]) button.ghost {
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
  :root:not([data-theme="light"]) button.ghost.danger {
    color: #ff9090;
  }
  :root:not([data-theme="light"]) button.ghost.danger:hover:not(:disabled) {
    background-color: #3a1818;
    border-color: var(--danger);
  }
  :root:not([data-theme="light"]) .history-header h2 {
    color: #d8d8d8;
  }
  :root:not([data-theme="light"]) .empty-history {
    background-color: #1f1f1f;
    border-color: #3a3a3a;
    color: #999;
  }
  :root:not([data-theme="light"]) .hint-prose {
    color: #aaa;
  }
  :root:not([data-theme="light"]) .replacement-row {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) .replacement-find {
    background-color: #1f1f1f;
    color: #f0f0f0;
  }
  :root:not([data-theme="light"]) .style-option {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) .pack-row {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) .pack-label {
    color: #d8d8d8;
  }
}
:root[data-theme="dark"] input,
:root[data-theme="dark"] button {
  background-color: #2a2a2a;
  border-color: #3a3a3a;
}
:root[data-theme="dark"] button:hover:not(:disabled) {
  border-color: var(--accent);
}
:root[data-theme="dark"] button.ghost {
  border-color: #3a3a3a;
}
:root[data-theme="dark"] button.ghost:hover:not(:disabled) {
  background-color: #353535;
}
:root[data-theme="dark"] button.ghost.danger {
  color: #ff9090;
}
:root[data-theme="dark"] button.ghost.danger:hover:not(:disabled) {
  background-color: #3a1818;
  border-color: var(--danger);
}
:root[data-theme="dark"] .history-header h2 {
  color: #d8d8d8;
}
:root[data-theme="dark"] .empty-history {
  background-color: #1f1f1f;
  border-color: #3a3a3a;
  color: #999;
}
:root[data-theme="dark"] .hint-prose {
  color: #aaa;
}
:root[data-theme="dark"] .replacement-row {
  background-color: #2a2a2a;
  border-color: #3a3a3a;
}
:root[data-theme="dark"] .replacement-find {
  background-color: #1f1f1f;
  color: #f0f0f0;
}
:root[data-theme="dark"] .style-option {
  background-color: #2a2a2a;
  border-color: #3a3a3a;
}
:root[data-theme="dark"] .pack-row {
  background-color: #2a2a2a;
  border-color: #3a3a3a;
}
:root[data-theme="dark"] .pack-label {
  color: #d8d8d8;
}
</style>
