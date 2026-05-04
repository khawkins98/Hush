<script lang="ts">
  import type { ErrorDisplay } from "./errors";

  type Props = {
    error: ErrorDisplay;
    /// Optional scope label rendered before the headline. Lets a
    /// per-panel error pre-fix the surface (e.g. "Meeting:") so the
    /// user sees which section is reporting. Pre-#199 panels did
    /// this with `<strong>` + a colon — kept for parity.
    scope?: string;
  };

  let { error, scope }: Props = $props();
</script>

<div class="error-card scoped-error" role="alert">
  <p class="error-headline">
    {#if scope}<strong class="error-scope">{scope}:</strong>{/if}
    <span class="error-headline-text">{error.headline}</span>
  </p>
  {#if error.hint}
    <p class="error-hint">{error.hint}</p>
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
