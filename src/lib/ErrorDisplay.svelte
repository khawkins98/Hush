<script lang="ts">
  import type { ErrorActionKey, ErrorDisplay } from "./errors";

  type Props = {
    error: ErrorDisplay;
    /// Optional scope label rendered before the headline. Lets a
    /// per-panel error pre-fix the surface (e.g. "Meeting:") so the
    /// user sees which section is reporting. Pre-#199 panels did
    /// this with `<strong>` + a colon — kept for parity.
    scope?: string;
    /// Optional handler for the one-click recovery button. The
    /// button only renders when both `error.actionKey` /
    /// `error.actionLabel` *and* `onAction` are set. The parent maps
    /// the key to a concrete navigation/handler. Without this prop
    /// the error renders headline + hint only — backwards-compatible
    /// with every existing call site.
    onAction?: (key: ErrorActionKey) => void;
  };

  let { error, scope, onAction }: Props = $props();

  let showAction = $derived(
    error.actionKey !== undefined
    && error.actionLabel !== undefined
    && onAction !== undefined,
  );
</script>

<div class="error-card scoped-error" role="alert">
  <p class="error-headline">
    {#if scope}<strong class="error-scope">{scope}:</strong>{/if}
    <span class="error-headline-text">{error.headline}</span>
  </p>
  {#if error.hint}
    <p class="error-hint">{error.hint}</p>
  {/if}
  {#if showAction}
    <button
      type="button"
      class="error-action"
      onclick={() => onAction?.(error.actionKey!)}
    >
      {error.actionLabel}
    </button>
  {/if}
  {#if error.details}
    <details class="error-details">
      <summary>Technical details</summary>
      <p class="error-details-body">{error.details}</p>
    </details>
  {/if}
</div>

<style>
.error-card {
  margin: 0.75rem 0;
  padding: 0.85rem 1rem;
  background-color: #fee;
  border: 1px solid var(--danger);
  border-radius: 8px;
  color: #8a0000;
  text-align: left;
  line-height: 1.5;
}

.error-headline {
  margin: 0 0 0.35rem;
  font-size: 0.95rem;
}

.error-scope {
  margin-right: 0.4rem;
  font-weight: 600;
}

.error-headline-text {
  font-weight: 500;
}

.error-hint {
  margin: 0;
  font-size: 0.88rem;
  color: #6b1010;
  /* Slightly less saturated than the headline so the eye reads
     headline first, hint second. */
  opacity: 0.92;
}

.error-action {
  margin-top: 0.6rem;
  padding: 0.35rem 0.85rem;
  background-color: var(--danger);
  border: 1px solid var(--danger);
  border-radius: 6px;
  color: #ffffff;
  font-family: inherit;
  font-size: 0.85rem;
  font-weight: 600;
  cursor: pointer;
  transition: background-color 0.12s, transform 0.05s;
}
.error-action:hover {
  background-color: #b03030;
  border-color: #b03030;
}
.error-action:active {
  transform: translateY(1px);
}
.error-action:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}

.error-details {
  margin-top: 0.55rem;
  font-size: 0.78rem;
  color: #7a3030;
}
.error-details summary {
  cursor: pointer;
  user-select: none;
  padding: 0.1rem 0;
}
.error-details summary:hover {
  color: #5a2020;
}
.error-details-body {
  margin: 0.4rem 0 0;
  /* Monospace + word-break so the long context-chain strings the
     backend produces stay readable inside the narrow card. */
  font-family:
    ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 0.78rem;
  white-space: pre-wrap;
  word-break: break-word;
  color: #6a2828;
  line-height: 1.45;
}

/* When the error card lands inside a panel that already has its
   own left-border accent (history, meetings, etc.), the
   `scoped-error` class gives it a touch more inset so it nests
   cleanly. Mirrors the pre-#199 `.scoped-error` shape. */
.scoped-error {
  padding-left: 1rem;
}

@media (prefers-color-scheme: dark) {
  .error-card {
    background-color: #4a1a1a;
    border-color: var(--danger);
    color: #ffd0d0;
  }
  .error-hint {
    color: #ffb0b0;
  }
  .error-details {
    color: #d8a0a0;
  }
  .error-details-body {
    color: #c89898;
  }
}
</style>
