<!--
  Dictation usage stats summary (#293). Sits above the History
  list; gives the user a satisfying "look how much you've done"
  overview using data already in the SQLite history table.

  Hidden when `sessionCount === 0` so a fresh install doesn't
  show a row of zeros — the History panel's existing empty-state
  copy already tells the user "your first transcript will land
  here."

  Time saved + keystrokes saved are derived estimates labelled
  with a "~" prefix so users read them as ballpark figures, not
  exact telemetry. The 40 wpm typing baseline is the long-
  established average for casual typists; spoken word
  generation typically lands around 130–150 wpm, so a 3.5×
  multiplier on raw dictation time is a defensible "saved" claim.
-->
<script lang="ts">
  import type { DictationStats } from "./types";

  type Props = {
    stats: DictationStats | null;
  };

  let { stats }: Props = $props();

  const TYPING_WPM = 40;

  // Derived "time saved at 40 wpm" formatted as `Hh Mm`. Returns
  // null for zero-word inputs so the tile can be hidden.
  function formatTimeSaved(words: number): string | null {
    if (words <= 0) return null;
    const totalMinutes = words / TYPING_WPM;
    const hours = Math.floor(totalMinutes / 60);
    const minutes = Math.round(totalMinutes - hours * 60);
    if (hours === 0 && minutes === 0) {
      return "<1m";
    }
    if (hours === 0) {
      return `${minutes}m`;
    }
    return `${hours}h ${minutes}m`;
  }

  // Visible only when at least one session exists. Pre-#293 this
  // surface didn't exist, so the empty-state contract didn't
  // need to handle a "show but blank" case.
  let visible = $derived(stats !== null && stats.sessionCount > 0);
  let timeSaved = $derived(stats ? formatTimeSaved(stats.wordCount) : null);
</script>

{#if visible && stats}
  <section class="dictation-stats" aria-label="Transcription usage statistics">
    <p class="stats-hero">
      You've dictated
      <strong>{stats.wordCount.toLocaleString()}</strong>
      {stats.wordCount === 1 ? "word" : "words"} across
      <strong>{stats.sessionCount.toLocaleString()}</strong>
      {stats.sessionCount === 1 ? "session" : "sessions"}.
    </p>
    <ul class="stats-tiles">
      <li class="stats-tile">
        <span class="tile-value" data-testid="stats-sessions"
          >{stats.sessionCount.toLocaleString()}</span
        >
        <span class="tile-label">{stats.sessionCount === 1 ? "Session" : "Sessions"}</span>
      </li>
      <li class="stats-tile">
        <span class="tile-value" data-testid="stats-words"
          >{stats.wordCount.toLocaleString()}</span
        >
        <span class="tile-label">{stats.wordCount === 1 ? "Word" : "Words"}</span>
      </li>
      {#if timeSaved}
        <li class="stats-tile">
          <span class="tile-value" data-testid="stats-time-saved">~{timeSaved}</span>
          <span class="tile-label">Saved</span>
          <span class="tile-sub">est. at 40 wpm typing</span>
        </li>
      {/if}
      <li class="stats-tile">
        <span class="tile-value" data-testid="stats-keystrokes"
          >~{stats.totalChars.toLocaleString()}</span
        >
        <span class="tile-label">Keystrokes</span>
        <span class="tile-sub">est. not typed by hand</span>
      </li>
    </ul>
  </section>
{/if}

<style>
  .dictation-stats {
    margin: 0 0 1.25rem;
    padding: 0.85rem 1rem;
    background-color: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 10px;
  }
  .stats-hero {
    margin: 0 0 0.75rem;
    font-size: 0.95rem;
    color: var(--text-primary);
    line-height: 1.4;
  }
  .stats-hero strong {
    color: var(--text-primary);
    font-weight: 600;
  }
  .stats-tiles {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(8rem, 1fr));
    gap: 0.5rem;
  }
  .stats-tile {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    padding: 0.5rem 0.65rem;
    background-color: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
  }
  .tile-value {
    font-size: 1.15rem;
    font-weight: 600;
    color: var(--text-primary);
    line-height: 1.15;
  }
  .tile-label {
    font-size: 0.75rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-top: 0.05rem;
  }
  .tile-sub {
    font-size: 0.72rem;
    color: var(--text-muted);
    line-height: 1.3;
  }
</style>
