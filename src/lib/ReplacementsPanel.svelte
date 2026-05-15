<script lang="ts">
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import type { ReplacementRule } from "./types";

  type Props = {
    replacements: ReplacementRule[];
    replacementsLoaded: boolean;
    replacementsError: ErrorDisplayShape | null;
    newFind: string;
    newReplace: string;
    inputEl?: HTMLInputElement | null;
    onSubmit: (e: Event) => void | Promise<void>;
    onDelete: (rule: ReplacementRule) => void | Promise<void>;
  };

  let {
    replacements,
    replacementsLoaded,
    replacementsError,
    newFind = $bindable(),
    newReplace = $bindable(),
    inputEl = $bindable(),
    onSubmit,
    onDelete,
  }: Props = $props();

  // Per-row click-to-confirm. First click arms; second click within
  // 5 s fires. Same shape as VocabularyPanel + History clear-all
  // (#198) so destructive actions feel consistent across panels.
  let confirmingId = $state<number | null>(null);
  let confirmTimer: number | undefined;

  function handleDelete(rule: ReplacementRule) {
    if (confirmingId === rule.id) {
      window.clearTimeout(confirmTimer);
      confirmingId = null;
      void onDelete(rule);
      return;
    }
    window.clearTimeout(confirmTimer);
    confirmingId = rule.id;
    confirmTimer = window.setTimeout(() => {
      confirmingId = null;
    }, 5000);
  }
</script>

<section class="replacements panel-replacements" aria-labelledby="replacements-heading">
  <header class="history-header">
    <h2 id="replacements-heading">Replacements</h2>
  </header>
  <p class="hint-prose">
    Find/replace pairs applied to every transcription before it's
    copied to the clipboard. Useful for stripping fillers
    (<code>um </code> → <code>(empty)</code>) or fixing names the
    model misrecognises. Literal substrings, case-sensitive.
  </p>

  {#if replacementsError}
    <ErrorDisplay error={replacementsError} scope="Replacements" />
  {/if}

  <form class="replacement-form" onsubmit={onSubmit}>
    <input
      type="text"
      bind:this={inputEl}
      bind:value={newFind}
      placeholder="Find…"
      aria-label="Find text"
    />
    <span class="arrow" aria-hidden="true">→</span>
    <input
      type="text"
      bind:value={newReplace}
      placeholder="Replace with… (blank deletes)"
      aria-label="Replace with"
    />
    <button type="submit" disabled={newFind.trim().length === 0}>Add</button>
  </form>

  {#if !replacementsLoaded}
    <p class="loading-skeleton">Loading replacements…</p>
  {:else if replacements.length === 0}
    <p class="empty-history">
      No replacement rules yet — add one above to clean up
      future transcripts automatically.
    </p>
  {:else}
    <ul class="replacement-list">
      {#each replacements as rule (rule.id)}
        <li class="replacement-row">
          <code class="replacement-find">{rule.findText}</code>
          <span class="arrow" aria-hidden="true">→</span>
          <code class="replacement-replace">
            {rule.replaceText.length === 0 ? "(empty)" : rule.replaceText}
          </code>
          <button
            class="ghost danger"
            class:confirming={confirmingId === rule.id}
            onclick={() => handleDelete(rule)}
            aria-label={confirmingId === rule.id
              ? `Click again to confirm deleting ${rule.findText} → ${rule.replaceText}`
              : `Delete replacement ${rule.findText} to ${rule.replaceText}`}
            data-testid="replacement-delete-{rule.id}"
          >
            {confirmingId === rule.id ? "Click to confirm" : "Delete"}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
.replacements {
  margin-top: 2.5rem;
  text-align: left;
  /* Per-panel accent stripe + slightly inset padding so each section
     reads visually distinct as the page grows. */
  border-left: 3px solid var(--border);
  padding-left: 1rem;
  padding-bottom: 0.25rem;
}

.panel-replacements {
  border-left-color: var(--accent);
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

.hint-prose code {
  background-color: var(--info-bg);
  color: var(--info-text);
  padding: 0.05em 0.4em;
  border-radius: 4px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.9em;
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

.arrow {
  color: var(--text-muted);
  font-weight: 600;
  flex-shrink: 0;
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

.replacement-find,
.replacement-replace {
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

</style>
