<!--
  Technical status line (#411 phase F5).

  Small, muted line that surfaces the active device + model so the
  user can confirm at a glance which setup will be used by the
  next dictation. Reads `🎤 Built-in Microphone · whisper-medium`.

  All data already lives in frontend state — the audio source is
  derived from the parent's `sources` array + selected id; the
  model is the parent's `activeModelName`. No IPC, no backend.

  Toggle lives in Settings → General → Advanced. Persistence is
  localStorage via `lib/status-line.ts`; cross-window propagation
  uses the `Events.StatusLine` Tauri event so a toggle from the
  Settings window updates the open main window without a reload.
-->
<script lang="ts">
  type Props = {
    /// Human-readable label for the active audio source (e.g.
    /// "Built-in Microphone" or "System Audio"). When `null` the
    /// device half collapses to "—" so the line still indicates
    /// the source slot is empty without flashing on/off mid-pick.
    audioSourceLabel: string | null;
    /// Active Whisper model display name; `null` while none is
    /// loaded. Same "—" treatment as above when empty.
    modelName: string | null;
  };

  let { audioSourceLabel, modelName }: Props = $props();
</script>

<p
  class="status-line"
  data-testid="audio-status-line"
  aria-label="Active audio source and model"
>
  <span class="status-line-icon" aria-hidden="true">🎤</span>
  <span class="status-line-device">{audioSourceLabel ?? "—"}</span>
  <span class="status-line-sep" aria-hidden="true">·</span>
  <span class="status-line-model">{modelName ?? "—"}</span>
</p>

<style>
  .status-line {
    margin: 0.5rem 0 0;
    font-size: 0.72rem;
    color: var(--text-muted, #888);
    text-align: center;
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.01em;
    line-height: 1.2;
    /* Don't compete with the waveform above — flatten any inherited
       paragraph spacing so the line reads as a caption. */
    padding: 0;
  }
  .status-line-sep {
    margin: 0 0.4rem;
    opacity: 0.6;
  }
  .status-line-icon {
    margin-right: 0.3rem;
    /* Emoji baselines vary; nudge it up a hair so it sits with the
       text rather than dropping below the cap line. */
    transform: translateY(-1px);
    display: inline-block;
  }
</style>
