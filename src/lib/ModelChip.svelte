<!--
  Active-model chip — the right-flank adjunct beside the Record
  button. Pulled out of `AudioSourcePicker` in #468 r3 when the
  layout dropped the sidebar in favour of a single row with the
  source dropdown and the model chip flanking the centerpiece
  button. Same markup the chip had inside `AudioSourcePicker`,
  same event semantics; this is just a colocation move.
-->
<script lang="ts">
  type Props = {
    /// Active model display name. `null` while none is loaded —
    /// the chip is hidden then; the no-model setup banner upstream
    /// owns that affordance.
    activeModelName: string | null;
    onScrollToModelPicker: () => void;
  };

  let { activeModelName, onScrollToModelPicker }: Props = $props();
</script>

{#if activeModelName}
  <div class="model-field">
    <span class="field-label">Model</span>
    <button
      type="button"
      class="model-chip"
      onclick={onScrollToModelPicker}
      aria-label="Active model: {activeModelName}. Click to change."
      title="Change transcription model"
    >
      <span class="model-name">{activeModelName}</span>
      <svg
        class="model-chevron"
        width="10"
        height="10"
        viewBox="0 0 10 10"
        aria-hidden="true"
        fill="none"
      >
        <path
          d="M3 4l2 2 2-2"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
    </button>
  </div>
{/if}

<style>
  .model-field {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    min-width: 0;
  }

  .field-label {
    font-size: 0.68rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .model-chip {
    width: 100%;
    height: var(--control-height);
    display: inline-flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.35rem;
    padding: 0 0.85rem;
    background: var(--bg-surface);
    border: 1px solid var(--border-input);
    border-radius: var(--radius-md);
    color: var(--text-secondary);
    font-family: inherit;
    font-size: 0.88rem;
    font-weight: 500;
    cursor: pointer;
    white-space: nowrap;
    transition: background-color 0.12s, border-color 0.12s, color 0.12s;
  }
  .model-chip:hover {
    background: var(--bg-elevated);
    border-color: var(--accent-hover);
    color: var(--text-primary);
  }
  .model-chip:focus-visible {
    outline: none;
    border-color: var(--border-focus);
    box-shadow: 0 0 0 3px var(--accent-subtle);
  }
  .model-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1 1 auto;
    min-width: 0;
    text-align: left;
  }
  .model-chevron {
    color: var(--text-muted);
    flex-shrink: 0;
  }
</style>
