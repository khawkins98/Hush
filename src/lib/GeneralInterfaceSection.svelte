<script lang="ts">
  import { onMount } from "svelte";

  import { generalSettings as gs } from "./state/general-settings.svelte";
  import "./settings-tab.css";

  onMount(() => {
    void gs.load();
  });
</script>

<section class="settings-group" aria-labelledby="settings-interface-heading">
  <h2 id="settings-interface-heading" class="group-heading">Interface</h2>
  <label class="toggle-row">
    <input
      type="checkbox"
      data-testid="settings-hud-toggle"
      disabled={gs.hudBusy}
      checked={gs.hudEnabled}
      onchange={(e) => gs.setHudEnabled((e.target as HTMLInputElement).checked)}
    />
    <span class="toggle-label">
      <span class="toggle-name">Show recording HUD</span>
      <span class="toggle-desc">
        The floating pill that appears in the top-right corner
        while Hush is capturing audio. Off hides it for both
        dictation and meeting mode; recording itself is
        unaffected.
      </span>
    </span>
  </label>
  {#if gs.hudError}
    <p class="settings-error">{gs.hudError}</p>
  {/if}

  <label class="toggle-row">
    <input
      type="checkbox"
      data-testid="settings-sound-cues-toggle"
      disabled={gs.soundCuesBusy}
      checked={gs.soundCuesEnabled}
      onchange={(e) => gs.setSoundCuesEnabled((e.target as HTMLInputElement).checked)}
    />
    <span class="toggle-label">
      <span class="toggle-name">Audio cues</span>
      <span class="toggle-desc">
        Plays a short chime when recording starts and a
        second chime when the transcript is on the
        clipboard. Honours your system volume and Do Not
        Disturb. Off keeps Hush silent.
      </span>
    </span>
  </label>
  {#if gs.soundCuesError}
    <p class="settings-error">{gs.soundCuesError}</p>
  {/if}

  <div
    class="sound-cue-subtoggles"
    class:is-disabled={!gs.soundCuesEnabled}
    aria-label="Per-event audio cues"
  >
    <div class="toggle-row toggle-row-sub">
      <label class="toggle-row-inner">
        <input
          type="checkbox"
          data-testid="settings-sound-cue-start-toggle"
          disabled={!gs.soundCuesEnabled || gs.soundCueStartBusy}
          checked={gs.soundCueStartEnabled}
          onchange={(e) =>
            gs.setSoundCueStartEnabled((e.target as HTMLInputElement).checked)}
        />
        <span class="toggle-label">
          <span class="toggle-name">Recording-start cue</span>
          <span class="toggle-desc">
            Plays a chime the moment the mic goes hot.
          </span>
        </span>
      </label>
      <button
        type="button"
        class="cue-preview-btn"
        data-testid="settings-cue-preview-start"
        onclick={() => gs.previewSoundCue("start")}
        aria-label="Preview the recording-start cue"
        title="Preview the recording-start cue"
      >▶</button>
    </div>
    <div class="toggle-row toggle-row-sub">
      <label class="toggle-row-inner">
        <input
          type="checkbox"
          data-testid="settings-sound-cue-complete-toggle"
          disabled={!gs.soundCuesEnabled || gs.soundCueCompleteBusy}
          checked={gs.soundCueCompleteEnabled}
          onchange={(e) =>
            gs.setSoundCueCompleteEnabled((e.target as HTMLInputElement).checked)}
        />
        <span class="toggle-label">
          <span class="toggle-name">Transcription-complete cue</span>
          <span class="toggle-desc">
            Plays a chime once the transcript is on the clipboard
            — the "safe to paste" signal.
          </span>
        </span>
      </label>
      <button
        type="button"
        class="cue-preview-btn"
        data-testid="settings-cue-preview-done"
        onclick={() => gs.previewSoundCue("done")}
        aria-label="Preview the transcription-complete cue"
        title="Preview the transcription-complete cue"
      >▶</button>
    </div>
  </div>
  {#if gs.soundCueSubError}
    <p class="settings-error">{gs.soundCueSubError}</p>
  {/if}
</section>
