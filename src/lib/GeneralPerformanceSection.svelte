<!--
  Advanced → Performance subsection: inference-threads and mic-gain
  sliders extracted from GeneralTab (#709). Uses the two-cell slider
  pattern: oninput updates the display label in real time during drag;
  onchange (fires on release) persists via IPC.
-->
<script lang="ts">
  import { generalRuntime as gr } from "./state/general-runtime.svelte";
</script>

<section class="settings-group" aria-labelledby="settings-performance-heading">
  <h2 id="settings-performance-heading" class="group-heading">Performance</h2>
  <label class="slider-row">
    <span class="toggle-label">
      <span class="toggle-name">
        Transcription threads:
        <span
          data-testid="settings-inference-threads-value"
          aria-live="polite"
        >{gr.inferenceThreadsDisplay}</span>
        {#if gr.inferenceThreadsBusy}
          <span class="row-note" aria-live="polite">Saving…</span>
        {/if}
      </span>
      <span id="settings-inference-threads-desc" class="toggle-desc">
        How many CPU threads whisper.cpp uses per chunk. More threads
        finish each chunk faster on a multi-core CPU but compete with
        other apps for cores. The default (4) suits most laptops; bump
        it up if transcription lags on a larger model, drop it if you
        want Hush to run quietly alongside heavy workloads.
      </span>
    </span>
    <input
      type="range"
      min="1"
      max="16"
      step="1"
      data-testid="settings-inference-threads-slider"
      aria-label="Transcription threads"
      aria-describedby="settings-inference-threads-desc"
      aria-valuetext={`${gr.inferenceThreadsDisplay} threads`}
      disabled={gr.inferenceThreadsBusy}
      value={gr.inferenceThreadsDisplay}
      oninput={gr.onInferenceThreadsInput}
      onchange={gr.onInferenceThreadsChange}
    />
  </label>
  {#if gr.inferenceThreadsError}
    <p class="settings-error">{gr.inferenceThreadsError}</p>
  {/if}
  <label class="slider-row">
    <span class="toggle-label">
      <span class="toggle-name">
        Microphone boost:
        <span
          data-testid="settings-mic-gain-db-value"
          aria-live="polite"
        >{gr.micGainDbDisplay === 0 ? "Off (0 dB)" : `+${gr.micGainDbDisplay} dB`}</span>
        {#if gr.micGainDbBusy}
          <span class="row-note" aria-live="polite">Saving…</span>
        {/if}
      </span>
      <span id="settings-mic-gain-db-desc" class="toggle-desc">
        Amplify microphone input before transcription. Useful if your
        voice comes through quietly. 0 = no boost; 6 dB ≈ double the
        perceived volume; 20 dB is the maximum safe boost. Has no
        effect on system-audio capture.
      </span>
    </span>
    <input
      type="range"
      min="0"
      max="20"
      step="1"
      data-testid="settings-mic-gain-db-slider"
      aria-label="Microphone boost"
      aria-describedby="settings-mic-gain-db-desc"
      aria-valuetext={gr.micGainDbDisplay === 0 ? "No boost" : `+${gr.micGainDbDisplay} dB`}
      disabled={gr.micGainDbBusy}
      value={gr.micGainDbDisplay}
      oninput={gr.onMicGainDbInput}
      onchange={gr.onMicGainDbChange}
    />
  </label>
  {#if gr.micGainDbError}
    <p class="settings-error">{gr.micGainDbError}</p>
  {/if}
</section>
